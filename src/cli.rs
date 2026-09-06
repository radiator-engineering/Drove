//! Command-line interface.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    backend::{Backend, herdr::HerdrClient},
    dsl::{compile, find_drovefile},
    executor::{
        ExecutionContext, HostCommandRunner, TaskOutcome, down, execute_plan_tasks, list_tasks,
        run_named_task,
    },
    model::Profile,
    planner::{ActionKind, Plan, SyncStatus, build_plan},
    state::LocalState,
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
        /// Not yet implemented: the planner in this PR never proposes a replace.
        #[arg(long)]
        allow_replace: bool,

        /// Approve any task `run`/hook argv this apply needs to execute.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Print the compiled intermediate representation (schema version 2).
    Render,
    /// Run one task and its `after` prerequisites; with no task, list every
    /// declared task and its last recorded outcome.
    Run {
        task: Option<String>,

        /// Approve the task's (and any hook's) argv digest before running.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Run `on_stop` hooks, then detach every resource this profile owns.
    Down {
        /// Also close owned panes on the backend; without it, detach only.
        #[arg(long)]
        purge: bool,

        /// Approve any `on_stop` hook argv this teardown needs to run.
        #[arg(long, short = 'y')]
        yes: bool,
    },
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

    let repo_root = drovefile
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| current.clone());

    if let Some(Command::Run { task, yes }) = &cli.command {
        return run_command(profile, &repo_root, task.as_deref(), *yes, cli.json);
    }
    if let Some(Command::Down { purge, yes }) = &cli.command {
        return down_command(
            profile,
            cli.socket.as_deref(),
            cli.session.as_deref(),
            &repo_root,
            *purge,
            *yes,
        );
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

    let mut state = LocalState::load(&repo_root)?;
    // Backends cannot read ownership tokens back yet (PR 3), so a resource's
    // observed state comes only from local state's own record of the last
    // apply (D16's declared fallback) until live discovery lands.
    let snapshot = state
        .profile(&cli.profile)
        .map(|managed| managed.to_snapshot(&cli.profile, Backend::caller_pane_id(&client)))
        .unwrap_or_default();
    let plan = build_plan(profile, &snapshot)?;

    match cli.command.unwrap_or(Command::Up {
        allow_replace: false,
        yes: false,
    }) {
        Command::Render | Command::Run { .. } | Command::Down { .. } => {
            unreachable!("handled above")
        }
        Command::Status | Command::Plan => {
            print_plan(&plan, cli.json)?;
            Ok(if plan.status == SyncStatus::InSync {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        Command::Up { yes, .. } => {
            print_plan(&plan, cli.json)?;
            if plan
                .actions
                .iter()
                .any(|action| action.kind == ActionKind::Conflict)
            {
                // Reconciling workspaces/tabs/panes/agents against the
                // backend is out of scope here (PR 3/5); only the plan's
                // `RunTask` actions execute (item 1/5 of this PR's brief).
                return Ok(ExitCode::from(2));
            }

            let ctx = ExecutionContext {
                repo_root: &repo_root,
                profile: &cli.profile,
                runner: &HostCommandRunner,
            };
            let results = execute_plan_tasks(profile, &plan, &ctx, &mut state, yes)?;
            report_task_outcomes(&results)
        }
    }
}

fn run_command(
    profile: &Profile,
    repo_root: &Path,
    task: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<ExitCode> {
    let mut state = LocalState::load(repo_root)?;
    match task {
        None => {
            let tasks = list_tasks(profile, &state);
            if json {
                let rows: Vec<_> = tasks
                    .iter()
                    .map(|(name, outcome)| {
                        serde_json::json!({"task": name, "last_outcome": outcome})
                    })
                    .collect();
                println!("{}", serde_json::to_string(&rows)?);
            } else if tasks.is_empty() {
                println!("no tasks declared in profile `{}`", profile.name);
            } else {
                for (name, outcome) in &tasks {
                    println!("{name}: {}", outcome.as_deref().unwrap_or("never run"));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(name) => {
            let ctx = ExecutionContext {
                repo_root,
                profile: &profile.name,
                runner: &HostCommandRunner,
            };
            let results = run_named_task(profile, name, &ctx, &mut state, yes)?;
            report_task_outcomes(&results)
        }
    }
}

fn down_command(
    profile: &Profile,
    socket: Option<&Path>,
    session: Option<&str>,
    repo_root: &Path,
    purge: bool,
    yes: bool,
) -> Result<ExitCode> {
    let mut state = LocalState::load(repo_root)?;
    let client = HerdrClient::discover(socket, session);
    let ctx = ExecutionContext {
        repo_root,
        profile: &profile.name,
        runner: &HostCommandRunner,
    };
    let backend: Option<&dyn Backend> = if purge { Some(&client) } else { None };
    let report = down(profile, &ctx, &mut state, yes, purge, backend)?;
    for id in &report.detached {
        println!("detached {id}");
    }
    if report.detached.is_empty() {
        println!("nothing owned by profile `{}`", profile.name);
    }
    Ok(ExitCode::SUCCESS)
}

fn report_task_outcomes(results: &[(String, TaskOutcome)]) -> Result<ExitCode> {
    let mut blocked = false;
    let mut failed = false;
    for (name, outcome) in results {
        println!("{}", describe_outcome(name, *outcome));
        match outcome {
            TaskOutcome::Blocked => blocked = true,
            TaskOutcome::Ran(false) => failed = true,
            _ => {}
        }
    }
    Ok(if blocked || failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn describe_outcome(name: &str, outcome: TaskOutcome) -> String {
    match outcome {
        TaskOutcome::Skipped => format!("{name}: skipped (check passed)"),
        TaskOutcome::Ran(true) => format!("{name}: ran"),
        TaskOutcome::Ran(false) => format!("{name}: failed"),
        TaskOutcome::Blocked => {
            format!("{name}: blocked (needs approval; re-run with --yes)")
        }
    }
}

fn print_plan(plan: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    print!("{}", plan.render());
    Ok(())
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

    #[test]
    fn clap_parses_run_with_optional_task() {
        let cli = Cli::try_parse_from(["drove", "run"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Run { task: None, .. })));

        let cli = Cli::try_parse_from(["drove", "run", "scaffold", "--yes"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                task: Some(name),
                yes: true
            }) if name == "scaffold"
        ));
    }

    #[test]
    fn clap_parses_down_with_purge() {
        let cli = Cli::try_parse_from(["drove", "down", "--purge"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Down {
                purge: true,
                yes: false
            })
        ));
    }
}
