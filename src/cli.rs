//! Command-line interface and one-shot reconciliation workflow.

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::{
    bootstrap,
    dsl::{compile, find_drovefile},
    executor::{self, ExecuteOptions},
    herdr::HerdrClient,
    planner::{Action, Plan, SyncStatus, build_plan},
    state::LocalState,
};

#[derive(Debug, Parser)]
#[command(
    name = "drove",
    version,
    about = "Versioned, declarative Herdr workspaces"
)]
pub struct Cli {
    /// Drovefile path; searches parent directories when omitted.
    #[arg(long, global = true)]
    file: Option<PathBuf>,

    /// Profile declared in Drovefile.
    #[arg(long, global = true, default_value = "default")]
    profile: String,

    /// Explicit Herdr API socket path.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Named Herdr session.
    #[arg(long, global = true)]
    session: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report drift without changing Herdr.
    Status,
    /// Print the ordered reconciliation plan.
    Plan,
    /// Reconcile the selected profile, then exit.
    Up {
        /// Permit replacement of managed tabs and their live PTYs.
        #[arg(long)]
        allow_replace: bool,

        /// Approve changed bootstrap task bytes without an interactive prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

pub fn run() -> Result<ExitCode> {
    run_with(Cli::parse())
}

fn run_with(cli: Cli) -> Result<ExitCode> {
    let current = std::env::current_dir().context("cannot read current directory")?;
    let drovefile = match cli.file {
        Some(path) => path,
        None => find_drovefile(&current)?,
    };
    let compiled = compile(&drovefile)?;
    let profile = compiled.config.profile(&cli.profile)?;
    let mut state = LocalState::load(&compiled.repo_root)?;
    let client = HerdrClient::discover(cli.socket.as_deref(), cli.session.as_deref());
    let observed = match crate::planner::ObservedState::gather(&client, &state, &cli.profile) {
        Ok(observed) => observed,
        Err(error) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "profile": cli.profile,
                        "status": "not_running",
                        "error": error.to_string(),
                        "socket": client.socket_path(),
                    })
                );
            } else {
                println!(
                    "not running: cannot reach Herdr at {} ({error})",
                    client.socket_path().display()
                );
            }
            return Ok(ExitCode::from(3));
        }
    };
    let bootstrap_actions = bootstrap::pending_actions(
        profile,
        &compiled.repo_root,
        &compiled.source_digest,
        &state,
    )?;
    let plan = build_plan(profile, &state, &observed, bootstrap_actions)?;

    match cli.command.unwrap_or(Command::Up {
        allow_replace: false,
        yes: false,
    }) {
        Command::Status => {
            print_plan(&plan, cli.json)?;
            Ok(if plan.status == SyncStatus::InSync {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Plan => {
            print_plan(&plan, cli.json)?;
            Ok(if plan.status == SyncStatus::InSync {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Up { allow_replace, yes } => {
            print_plan(&plan, cli.json)?;
            if plan.status == SyncStatus::InSync {
                return Ok(ExitCode::SUCCESS);
            }
            let approve_bootstrap =
                approve_bootstrap_if_needed(profile, &compiled, &state, &plan, yes)?;
            executor::execute(
                &client,
                profile,
                &compiled.source_digest,
                &plan,
                &mut state,
                &ExecuteOptions {
                    allow_replace,
                    approve_bootstrap,
                },
            )?;

            let observed = crate::planner::ObservedState::gather(&client, &state, &cli.profile)?;
            let bootstrap_actions = bootstrap::pending_actions(
                profile,
                &compiled.repo_root,
                &compiled.source_digest,
                &state,
            )?;
            let final_plan = build_plan(profile, &state, &observed, bootstrap_actions)?;
            if final_plan.status != SyncStatus::InSync {
                print_plan(&final_plan, cli.json)?;
                bail!("reconciliation completed but the profile is still out of sync");
            }
            if !cli.json {
                println!("in sync: profile `{}`", cli.profile);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn approve_bootstrap_if_needed(
    profile: &crate::model::Profile,
    compiled: &crate::dsl::CompiledDrovefile,
    state: &LocalState,
    plan: &Plan,
    yes: bool,
) -> Result<bool> {
    let pending = plan
        .actions
        .iter()
        .filter(|action| action.kind == crate::planner::ActionKind::RunBootstrap)
        .filter_map(|action| {
            profile
                .bootstrap
                .iter()
                .find(|task| task.id == action.address)
        })
        .filter_map(|task| {
            bootstrap::task_digest(task, &compiled.repo_root, &compiled.source_digest)
                .ok()
                .filter(|digest| !state.is_approved(digest))
                .map(|digest| (task, digest))
        })
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(false);
    }
    if yes {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        bail!("changed bootstrap tasks require approval; rerun `drove up --yes` after review");
    }
    eprintln!("The following changed bootstrap tasks will execute:");
    for (task, digest) in &pending {
        eprintln!("  {}: {:?} (approval {})", task.id, task.run, &digest[..12]);
    }
    confirm("Approve these task bytes? [y/N] ")
}

fn confirm(prompt: &str) -> Result<bool> {
    eprint!("{prompt}");
    io::stderr().flush().context("cannot flush prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("cannot read approval")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn print_plan(plan: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    match plan.status {
        SyncStatus::InSync => println!("in sync: profile `{}`", plan.profile),
        SyncStatus::OutOfSync => {
            println!(
                "out of sync: profile `{}` ({} action{})",
                plan.profile,
                plan.actions.len(),
                if plan.actions.len() == 1 { "" } else { "s" }
            );
            for action in &plan.actions {
                print_action(action);
            }
        }
    }
    Ok(())
}

fn print_action(action: &Action) {
    let warning = if action.destructive {
        " [replacement approval required]"
    } else {
        ""
    };
    println!(
        "  {:?} {}{} — {}",
        action.kind, action.address, warning, action.reason
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_defaults_to_up_workflow() {
        let cli = Cli::try_parse_from(["drove"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.profile, "default");
    }
}
