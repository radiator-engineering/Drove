//! Command-line interface.

use std::{path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    backend::herdr::HerdrClient,
    dsl::{compile, find_drovefile},
    planner::{Action, Plan, SyncStatus, build_plan},
};

#[derive(Debug, Parser)]
#[command(
    name = "drove",
    version,
    about = "Versioned, declarative agent workspaces"
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
    /// Report drift without changing the backend.
    Status,
    /// Print the ordered reconciliation plan.
    Plan,
    /// Reconcile the selected profile, then exit.
    Up {
        /// Permit replacement of managed tabs and their live PTYs.
        #[arg(long)]
        allow_replace: bool,

        /// Approve changed task bytes without an interactive prompt.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print the compiled intermediate representation (schema version 2).
    Render,
}

pub fn run() -> Result<ExitCode> {
    run_with(Cli::parse())
}

fn run_with(cli: Cli) -> Result<ExitCode> {
    let current = std::env::current_dir().context("cannot read current directory")?;
    let drovefile = match &cli.file {
        Some(path) => path.clone(),
        None => find_drovefile(&current)?,
    };
    let compiled = compile(&drovefile)?;
    let profile = compiled.config.profile(&cli.profile)?;

    if matches!(cli.command, Some(Command::Render)) {
        let ir = profile.to_ir();
        if cli.json {
            println!("{}", serde_json::to_string(&ir)?);
        } else {
            println!("{}", ir.to_json_pretty()?);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let client = HerdrClient::discover(cli.socket.as_deref(), cli.session.as_deref());
    if let Err(error) = client.snapshot() {
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

    let plan = build_plan(profile)?;
    match cli.command.unwrap_or(Command::Up {
        allow_replace: false,
        yes: false,
    }) {
        Command::Render => unreachable!("handled above"),
        Command::Status | Command::Plan => {
            print_plan(&plan, cli.json)?;
            Ok(if plan.status == SyncStatus::InSync {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Up { .. } => {
            print_plan(&plan, cli.json)?;
            if !cli.json {
                println!("in sync: profile `{}`", cli.profile);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
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
