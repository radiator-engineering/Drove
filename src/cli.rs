//! Command-line interface.

use std::{
    ffi::OsString,
    io::IsTerminal,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::{
    backend::{Backend, herdr, radiator, select},
    dsl::{compile, find_drovefile},
    executor::{
        ExecutionContext, HostCommandRunner, TaskOutcome, UpOutcome, down, list_tasks,
        run_named_task, up,
    },
    model::{DroveConfig, Profile},
    planner::{Action, CoreAction, Plan, SyncStatus, build_plan},
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

    /// Profile declared in Drovefile; an alias of the positional PROFILE
    /// (D42). Giving both and disagreeing is an error.
    // Named `profile_flag` (not `profile`) so clap gives it an arg id
    // distinct from every subcommand's own positional `profile` field —
    // sharing the field name would collide the two args under one id.
    #[arg(long = "profile", global = true)]
    profile_flag: Option<String>,

    /// Backend to reconcile onto (`herdr`, `radiator`); overrides `backend(...)`
    /// in the Drovefile (D32).
    #[arg(long, global = true)]
    backend: Option<String>,

    /// Named target instance for the selected backend (Herdr session,
    /// Radiator hub); overrides `herdr.session(...)`/`radiator.hub(...)`
    /// in the Drovefile (D32).
    #[arg(long, global = true)]
    target: Option<String>,

    /// Explicit backend socket path override.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    /// Named Herdr session; an alias of `--target` for the Herdr backend.
    #[arg(long, global = true)]
    session: Option<String>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// Profile declared in Drovefile (D42); only consulted when no
    /// subcommand consumes its own positional PROFILE.
    profile_arg: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report drift without changing the backend.
    Status { profile: Option<String> },
    /// Print the ordered reconciliation plan.
    Plan { profile: Option<String> },
    /// Reconcile the selected profile, bring its workspace to the front, and
    /// (from outside Herdr) attach to the session.
    Up {
        /// Not yet implemented: the planner in this PR never proposes a replace.
        #[arg(long)]
        allow_replace: bool,

        /// Approve any task `run`/hook argv this apply needs to execute, and
        /// any destructive backend action (D22).
        #[arg(long, short = 'y')]
        yes: bool,

        /// Workspace to bring to the front; defaults to the profile's first
        /// declared workspace (D43 step 4).
        #[arg(long)]
        workspace: Option<String>,

        /// Reconcile only: do not focus a workspace or attach to the session.
        #[arg(long)]
        no_focus: bool,

        profile: Option<String>,
    },
    /// Print the compiled intermediate representation (schema version 3); a
    /// v2 Drovefile also prints its deprecation warnings and v3 form (D31).
    Render { profile: Option<String> },
    /// Run one task and its `after` prerequisites; with no task, list every
    /// declared task and its last recorded outcome.
    Run {
        task: Option<String>,

        /// Approve the task's (and any hook's) argv digest before running.
        #[arg(long, short = 'y')]
        yes: bool,

        profile: Option<String>,
    },
    /// Warn about a stale `was =` declaration or a task with no `check`
    /// (D26, D34). Always exits 0.
    Lint { profile: Option<String> },
    /// Run `on_stop` hooks, then detach every resource this profile owns.
    Down {
        /// Also close owned panes on the backend; without it, detach only.
        #[arg(long)]
        purge: bool,

        /// Approve any `on_stop` hook argv this teardown needs to run.
        #[arg(long, short = 'y')]
        yes: bool,

        profile: Option<String>,
    },
    /// List every declared profile with its backend, target and reachability
    /// (D42).
    Ls,
}

pub fn run() -> Result<ExitCode> {
    run_with(Cli::parse())
}

/// The positional PROFILE, wherever it landed: on the subcommand (`drove
/// status monitoring`) or, with no subcommand, at the top level (`drove
/// monitoring`).
fn positional_profile(cli: &Cli) -> Option<&str> {
    let from_command = match &cli.command {
        Some(Command::Status { profile }) => profile.as_deref(),
        Some(Command::Plan { profile }) => profile.as_deref(),
        Some(Command::Up { profile, .. }) => profile.as_deref(),
        Some(Command::Render { profile }) => profile.as_deref(),
        Some(Command::Run { profile, .. }) => profile.as_deref(),
        Some(Command::Lint { profile }) => profile.as_deref(),
        Some(Command::Down { profile, .. }) => profile.as_deref(),
        Some(Command::Ls) | None => None,
    };
    from_command.or(cli.profile_arg.as_deref())
}

/// `--profile` is a no-warning alias of the positional PROFILE (D42); giving
/// both and disagreeing is an error.
fn requested_profile(cli: &Cli) -> Result<Option<&str>> {
    match (positional_profile(cli), cli.profile_flag.as_deref()) {
        (Some(positional), Some(flag)) if positional != flag => {
            bail!(
                "profile given as both `{positional}` (positional) and `--profile {flag}`; they must match"
            )
        }
        (Some(positional), _) => Ok(Some(positional)),
        (None, flag) => Ok(flag),
    }
}

/// Resolves which profile this invocation targets (D42): the requested name
/// if declared, else `default` if declared, else the file's only profile.
/// Anything else prints every declared profile and returns `None` so the
/// caller exits 2.
fn resolve_profile<'a>(requested: Option<&str>, config: &'a DroveConfig) -> Option<&'a Profile> {
    if let Some(name) = requested {
        let found = config.profiles.get(name);
        if found.is_none() {
            print_profile_list(&format!("unknown profile `{name}`"), config);
        }
        return found;
    }
    if let Some(profile) = config.profiles.get("default") {
        return Some(profile);
    }
    if config.profiles.len() == 1 {
        return config.profiles.values().next();
    }
    print_profile_list("no profile given", config);
    None
}

fn print_profile_list(reason: &str, config: &DroveConfig) {
    let names: Vec<&str> = config.profiles.keys().map(String::as_str).collect();
    println!("{reason}; declared profiles: {}", names.join(", "));
}

/// `drove run PROFILE` (no task) parses identically to `drove run TASK`:
/// `Run`'s `task` positional comes before its `profile` positional, so clap
/// binds a single bare word to `task`. If that word names a declared
/// profile and no declared task shares the name, reinterpret it as the
/// profile instead (D42).
fn disambiguate_run_positional(cli: &mut Cli, config: &DroveConfig) {
    let Some(Command::Run { task, profile, .. }) = &mut cli.command else {
        return;
    };
    if profile.is_some() {
        return;
    }
    let Some(name) = task.as_deref() else {
        return;
    };
    let names_a_profile = config.profiles.contains_key(name);
    let names_a_task = config
        .profiles
        .values()
        .any(|declared| declared.tasks.iter().any(|t| t.name == name));
    if names_a_profile && !names_a_task {
        *profile = task.take();
    }
}

fn run_with(mut cli: Cli) -> Result<ExitCode> {
    let current = std::env::current_dir().context("cannot read current directory")?;
    let drovefile = match &cli.file {
        Some(path) => path.clone(),
        None => find_drovefile(&current)?,
    };
    let compiled = compile(&drovefile)?;
    disambiguate_run_positional(&mut cli, &compiled.config);

    if matches!(cli.command, Some(Command::Ls)) {
        return ls_command(&cli, &compiled.config, cli.json);
    }

    let requested = requested_profile(&cli)?;
    let Some(profile) = resolve_profile(requested, &compiled.config) else {
        return Ok(ExitCode::from(2));
    };

    if matches!(cli.command, Some(Command::Render { .. })) {
        let ir = profile.to_ir();
        if cli.json {
            println!("{}", serde_json::to_string(&ir)?);
        } else {
            println!("{}", ir.to_json_pretty()?);
        }
        // A v2 Drovefile compiles through the shims (D31); show its warnings
        // and the equivalent v3 form so the author can migrate. Both go to
        // stderr so `--json` stdout stays a clean IR document.
        print_warnings(&compiled.warnings);
        if !compiled.warnings.is_empty() {
            eprint!("\nv3 form:\n\n{}", v3form::render(&compiled.config));
        }
        return Ok(ExitCode::SUCCESS);
    }

    let repo_root = drovefile
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| current.clone());

    if let Some(Command::Run { task, yes, .. }) = &cli.command {
        return run_command(profile, &repo_root, task.as_deref(), *yes, cli.json);
    }

    if matches!(cli.command, Some(Command::Lint { .. })) {
        return lint_command(profile, &repo_root, cli.json);
    }

    let (backend_id, target) = resolve_backend(&cli, &compiled.config, profile);

    if let Some(Command::Down { purge, yes, .. }) = &cli.command {
        return down_command(
            profile,
            &backend_id,
            &target,
            &repo_root,
            *purge,
            *yes,
            cli.json,
        );
    }

    // `drove` with no subcommand, and `drove up`, are the one command that
    // gets you there (D43): they start the session if needed and apply the
    // plan, so they route to `up_command` before the read-only reachability
    // check below (which `plan`/`status` keep).
    if let Some((yes, workspace, no_focus)) = up_flags(&cli.command) {
        return up_command(
            profile,
            &backend_id,
            &target,
            &repo_root,
            &profile.name,
            workspace.as_deref(),
            no_focus,
            cli.json,
            yes,
        );
    }

    let client = select::open(&backend_id, &target)?;
    let live_snapshot = match client.snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let socket = backend_socket_display(&backend_id, &target);
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "profile": profile.name,
                        "backend": backend_id,
                        "status": "not_running",
                        "error": error.to_string(),
                        "socket": socket,
                    })
                );
            } else {
                println!(
                    "not running: cannot reach {backend_id} at {} ({error})",
                    socket.display()
                );
            }
            return Ok(ExitCode::from(3));
        }
    };

    let state = LocalState::load(&repo_root)?;
    // Backends cannot read ownership tokens back yet (PR 3), so a resource's
    // observed state comes from local state's own record of the last apply
    // (D16's declared fallback), pruned against the live snapshot first
    // (D48): a recorded resource whose backend id no longer exists is
    // dropped so the planner sees it as absent and plans its creation again,
    // rather than trusting a stale id from a session that was wiped and
    // restarted. `plan` and `status` never write the prune back.
    let managed = state.profile(&profile.name).cloned().unwrap_or_default();
    let (pruned, dropped) = crate::state::prune_missing(&managed, &live_snapshot);
    let snapshot = pruned.to_snapshot(&profile.name, client.caller_pane_id());
    let plan = build_plan(profile, &snapshot)?;

    // Only `status` and `plan` reach here: Render/Run/Down/Lint/Ls returned
    // above, and Up (with the no-subcommand default) routed through
    // `up_command`. Both are read-only: print the plan and report drift.
    debug_assert!(matches!(
        cli.command,
        Some(Command::Status { .. }) | Some(Command::Plan { .. })
    ));
    // Only `status` annotates what the prune dropped (D48); `plan` renders
    // the plan alone, unchanged.
    let is_status = matches!(cli.command, Some(Command::Status { .. }));
    print_plan(&plan, cli.json, is_status.then_some(dropped.as_slice()))?;
    print_warnings(&compiled.warnings);
    Ok(if plan.status == SyncStatus::InSync {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// The `up` flags for `drove` with no subcommand (all defaults) and for an
/// explicit `drove up`; `None` for any other subcommand.
fn up_flags(command: &Option<Command>) -> Option<(bool, Option<String>, bool)> {
    match command {
        None => Some((false, None, false)),
        Some(Command::Up {
            yes,
            workspace,
            no_focus,
            ..
        }) => Some((*yes, workspace.clone(), *no_focus)),
        _ => None,
    }
}

/// `drove ls` (D42): every declared profile with its resolved backend,
/// target name and whether that target answers a `ping` (a `snapshot()`
/// round trip, the same reachability probe `status`/`plan` already use).
fn ls_command(cli: &Cli, config: &DroveConfig, json: bool) -> Result<ExitCode> {
    #[derive(serde::Serialize)]
    struct Row {
        profile: String,
        backend: String,
        target: Option<String>,
        reachable: bool,
    }

    let mut rows = Vec::with_capacity(config.profiles.len());
    for profile in config.profiles.values() {
        let (backend_id, target) = resolve_backend(cli, config, profile);
        let client = select::open(&backend_id, &target)?;
        let reachable = client.snapshot().is_ok();
        rows.push(Row {
            profile: profile.name.clone(),
            backend: backend_id,
            target: target.name.clone(),
            reachable,
        });
    }

    if json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        for row in &rows {
            println!(
                "{}: backend={} target={} reachable={}",
                row.profile,
                row.backend,
                row.target.as_deref().unwrap_or("-"),
                row.reachable
            );
        }
    }
    Ok(ExitCode::SUCCESS)
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
            report_task_outcomes(&results, json)
        }
    }
}

/// `drove lint` (D26, D34): a stale `was =` that matches nothing live, and a
/// task with no `check`. Minimal on purpose — always exits 0, warnings only.
fn lint_command(profile: &Profile, repo_root: &Path, json: bool) -> Result<ExitCode> {
    let state = LocalState::load(repo_root)?;
    let snapshot = state
        .profile(&profile.name)
        .map(|managed| managed.to_snapshot(&profile.name, None))
        .unwrap_or_default();

    let is_live = |name: &str| {
        snapshot
            .resources
            .get(name)
            .and_then(|observed| observed.owner.as_ref())
            .is_some_and(|owner| owner.profile == profile.name)
    };

    let mut warnings = Vec::new();
    for workspace in &profile.workspaces {
        if let Some(was) = &workspace.was
            && !is_live(was)
        {
            warnings.push(format!(
                "workspace `{}` declares was = \"{was}\" which matches nothing live",
                workspace.name
            ));
        }
        for tab in &workspace.tabs {
            for pane in &tab.panes {
                if let Some(was) = &pane.was
                    && !is_live(was)
                {
                    warnings.push(format!(
                        "pane `{}` declares was = \"{was}\" which matches nothing live",
                        pane.name
                    ));
                }
            }
        }
    }
    for task in &profile.tasks {
        if task.check.is_none() {
            warnings.push(format!(
                "task `{}` has no `check`; every run is treated as unconverged",
                task.name
            ));
        }
    }

    if json {
        println!("{}", serde_json::to_string(&warnings)?);
    } else if warnings.is_empty() {
        println!("no lint warnings in profile `{}`", profile.name);
    } else {
        for warning in &warnings {
            println!("warning: {warning}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `drove down` (D19, D47): runs every `on_stop` hook and detaches every
/// owned resource, then — on the Herdr backend, when the resolved target
/// names a session other than `default` (D46 precedence) — stops and
/// deletes that session. The detach is saved to local state before the
/// session is touched, so a `stop_session` failure (a missing `herdr`
/// binary, or a failed delete) still leaves the resources detached and a
/// retry of `down` idempotent.
fn down_command(
    profile: &Profile,
    backend_id: &str,
    target: &select::Target,
    repo_root: &Path,
    purge: bool,
    yes: bool,
    json: bool,
) -> Result<ExitCode> {
    let mut state = LocalState::load(repo_root)?;
    let client = select::open(backend_id, target)?;
    let ctx = ExecutionContext {
        repo_root,
        profile: &profile.name,
        runner: &HostCommandRunner,
    };
    let backend: Option<&dyn Backend> = if purge { Some(client.as_ref()) } else { None };
    let report = down(profile, &ctx, &mut state, yes, purge, backend)?;

    if !json {
        for id in &report.detached {
            println!("detached {id}");
        }
        if report.detached.is_empty() {
            println!("nothing owned by profile `{}`", profile.name);
        }
    }

    let named_session = target
        .name
        .as_deref()
        .filter(|name| *name != "default")
        .zip(client.herdr());
    let session = match named_session {
        Some((name, herdr)) => Some((name.to_owned(), herdr.stop_session(name)?)),
        None => None,
    };

    if json {
        let mut body = serde_json::json!({
            "detached": report.detached,
            "hooks_run": report.hooks_run.iter().map(|(name, success)| {
                serde_json::json!({"resource": name, "success": success})
            }).collect::<Vec<_>>(),
        });
        if let Some((name, stop)) = &session {
            body["session"] = serde_json::json!({
                "name": name,
                "stopped": stop.stopped,
                "deleted": stop.deleted,
            });
        }
        println!("{body}");
    } else if let Some((name, stop)) = &session {
        if stop.stopped {
            println!("stopped session {name}");
        } else {
            println!("deleted session {name}");
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// `drove up` / `drove [PROFILE]` (D43): start the session if it is not
/// running, apply the plan (tasks and backend actions), bring the target
/// workspace to the front, and — from a terminal outside Herdr — attach to the
/// session so the caller lands in it.
#[allow(clippy::too_many_arguments)]
fn up_command(
    profile: &Profile,
    backend_id: &str,
    target: &select::Target,
    repo_root: &Path,
    profile_arg: &str,
    workspace: Option<&str>,
    no_focus: bool,
    json: bool,
    yes: bool,
) -> Result<ExitCode> {
    let client = select::open(backend_id, target)?;
    let mut state = LocalState::load(repo_root)?;
    // D48: probe the target once, up front. When it is already reachable,
    // prune local state against its live snapshot and save the pruned set
    // before anything is applied, so a resource whose backend id the
    // session no longer has (stopped and restarted, wiping Herdr's
    // workspaces and id counter) is planned as a fresh create instead of
    // trusted as already there. When the target isn't reachable yet (first
    // start), there is nothing live to prune against; `up` below starts the
    // session and applies against local state as recorded, unchanged from
    // today. The same probe result is reused for the Radiator reachability
    // check below, rather than reaching the backend a second time.
    let live_snapshot = client.snapshot();
    if let Ok(live_snapshot) = &live_snapshot {
        let managed = state.profile(profile_arg).cloned().unwrap_or_default();
        let (pruned, dropped) = crate::state::prune_missing(&managed, live_snapshot);
        if !dropped.is_empty() {
            state.profiles.insert(profile_arg.to_owned(), pruned);
            state.save()?;
        }
    }
    let snapshot = state
        .profile(profile_arg)
        .map(|managed| managed.to_snapshot(profile_arg, client.caller_pane_id()))
        .unwrap_or_default();
    let plan = build_plan(profile, &snapshot)?;

    // A `Conflict` never applies anything and exits 2 (D43 step 3).
    if plan
        .actions
        .iter()
        .any(|action| action.kind == Action::Core(CoreAction::Conflict))
    {
        print_plan(&plan, json, None)?;
        return Ok(ExitCode::from(2));
    }

    let is_herdr = backend_id == select::HERDR_BACKEND;
    // A Radiator hub has no headless-start verb (D37): if it is unreachable,
    // fail with the hub name rather than applying against a dead socket.
    if !is_herdr && live_snapshot.is_err() {
        let hub = target.name.as_deref().unwrap_or(radiator::DEFAULT_HUB_NAME);
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "profile": profile.name,
                    "backend": backend_id,
                    "status": "not_running",
                    "hub": hub,
                })
            );
        } else {
            println!("cannot reach the Radiator hub `{hub}`; start it, then re-run `drove`");
        }
        return Ok(ExitCode::from(1));
    }

    // The session name for the headless start and the attach; a workspace to
    // bring to the front (the flag, else the profile's first workspace).
    let session = target.name.as_deref().unwrap_or("default");
    let focus_target = workspace.or_else(|| profile.workspaces.first().map(|w| w.name.as_str()));
    // `--json` implies `--no-focus` (D43 step 4).
    let do_focus = !no_focus && !json;

    let ctx = ExecutionContext {
        repo_root,
        profile: profile_arg,
        runner: &HostCommandRunner,
    };
    let report = up(
        client.as_ref(),
        profile,
        &profile.to_ir(),
        &plan,
        &ctx,
        &mut state,
        yes,
        session,
        focus_target,
        do_focus,
    )?;

    if let UpOutcome::CannotStart { hint } = &report.outcome {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "profile": profile.name,
                    "backend": backend_id,
                    "status": "cannot_start",
                    "hint": hint,
                })
            );
        } else {
            println!("cannot reach the Herdr session `{session}`; start it with:\n    {hint}");
        }
        return Ok(ExitCode::from(1));
    }

    print_up_summary(profile, &report, json)?;

    let blocked = report.blocked_destructive
        || report
            .tasks
            .iter()
            .any(|(_, outcome)| matches!(outcome, TaskOutcome::Blocked | TaskOutcome::Ran(false)));
    if blocked {
        if report.blocked_destructive {
            eprintln!(
                "warning: a destructive action needs approval; re-run with --yes to apply it"
            );
        }
        return Ok(ExitCode::from(1));
    }

    // Step 4: from a terminal outside Herdr, land the caller in the session.
    if do_focus && is_herdr && should_attach() {
        return attach_session(session);
    }
    Ok(ExitCode::SUCCESS)
}

/// The `--json` body for the `up` summary (D43 step 5), built pure so its
/// shape is testable without capturing stdout.
fn up_summary_json(profile: &Profile, report: &crate::executor::UpReport) -> serde_json::Value {
    let tasks: Vec<_> = report
        .tasks
        .iter()
        .map(
            |(name, outcome)| serde_json::json!({"task": name, "outcome": outcome_label(*outcome)}),
        )
        .collect();
    let mut object = serde_json::json!({
        "profile": profile.name,
        "focused": report.focused,
        "tasks": tasks,
    });
    match &report.outcome {
        UpOutcome::Reconciled {
            created,
            changed,
            tasks_run,
        } => {
            object["status"] = serde_json::json!("in_sync");
            object["created"] = serde_json::json!(created);
            object["changed"] = serde_json::json!(changed);
            object["tasks_run"] = serde_json::json!(tasks_run);
        }
        UpOutcome::AlreadyRunning => {
            object["status"] = serde_json::json!("already_running");
        }
        UpOutcome::CannotStart { .. } => unreachable!("handled before summary"),
    }
    object
}

/// The one summary line D43 step 5 prints (or its `--json` form).
fn print_up_summary(
    profile: &Profile,
    report: &crate::executor::UpReport,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", up_summary_json(profile, report));
        return Ok(());
    }
    match &report.outcome {
        UpOutcome::Reconciled {
            created,
            changed,
            tasks_run,
        } => println!(
            "profile {}: {created} created, {changed} changed, {tasks_run} tasks run, in sync",
            profile.name
        ),
        UpOutcome::AlreadyRunning => {
            println!(
                "profile {}: already running, brought to front",
                profile.name
            )
        }
        UpOutcome::CannotStart { .. } => unreachable!("handled before summary"),
    }
    Ok(())
}

/// Whether `up` should replace this process with a session attach: only from a
/// terminal outside Herdr (`HERDR_ENV` unset) whose stdout is a TTY (D43 step
/// 4).
fn should_attach() -> bool {
    std::env::var_os("HERDR_ENV").is_none() && std::io::stdout().is_terminal()
}

/// Replaces this process with `herdr session attach NAME` (D43 step 4). On
/// Unix `exec` replaces the process and returns only on failure; on Windows,
/// which has no `exec`, it spawns, waits, and forwards the child's exit code.
/// Isolated here so the rest of `up` stays testable without a real terminal.
fn attach_session(name: &str) -> Result<ExitCode> {
    let bin = std::env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"));
    let mut command = std::process::Command::new(bin);
    command.args(["session", "attach", name]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns if it fails to replace the process.
        Err(command.exec()).context("cannot attach to the Herdr session")
    }
    #[cfg(windows)]
    {
        let status = command
            .status()
            .context("cannot attach to the Herdr session")?;
        Ok(ExitCode::from(
            status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
        ))
    }
}

fn report_task_outcomes(results: &[(String, TaskOutcome)], json: bool) -> Result<ExitCode> {
    let mut blocked = false;
    let mut failed = false;
    for (_, outcome) in results {
        match outcome {
            TaskOutcome::Blocked => blocked = true,
            TaskOutcome::Ran(false) => failed = true,
            _ => {}
        }
    }
    if json {
        let rows: Vec<_> = results
            .iter()
            .map(|(name, outcome)| {
                serde_json::json!({"task": name, "outcome": outcome_label(*outcome)})
            })
            .collect();
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        for (name, outcome) in results {
            println!("{}", describe_outcome(name, *outcome));
        }
    }
    Ok(if blocked || failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn outcome_label(outcome: TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Skipped => "skipped",
        TaskOutcome::Ran(true) => "ran",
        TaskOutcome::Ran(false) => "failed",
        TaskOutcome::Blocked => "blocked",
    }
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

/// Resolves the backend id and target per D46's six-level order, gathering
/// the CLI/environment/Drovefile inputs the pure `select::resolve` needs.
fn resolve_backend(cli: &Cli, config: &DroveConfig, profile: &Profile) -> (String, select::Target) {
    let cli_inputs = select::CliInputs {
        backend: cli.backend.as_deref(),
        target: cli.target.as_deref(),
        session: cli.session.as_deref(),
        socket: cli.socket.as_deref(),
    };
    let explicit_env_inputs = select::ExplicitEnvInputs {
        drove_backend: std::env::var("DROVE_BACKEND").ok(),
        drove_session: std::env::var("DROVE_SESSION").ok(),
        drove_target: std::env::var("DROVE_TARGET").ok(),
    };
    let ambient_env_inputs = select::AmbientEnvInputs {
        herdr_session: std::env::var("HERDR_SESSION").ok(),
        radiator_hub: std::env::var("RADIATOR_HUB").ok(),
        ambient_radiator: radiator::selected_by_environment(),
    };
    let profile_inputs = select::ProfileInputs {
        backend: profile.backend.as_deref(),
        session: profile.session.as_deref(),
    };
    select::resolve(
        cli_inputs,
        &explicit_env_inputs,
        &ambient_env_inputs,
        profile_inputs,
        config.backend.as_deref(),
        &config.target,
    )
}

/// The socket path a backend will actually connect to, for diagnostics —
/// computed with the same pure resolvers `select::open` calls internally,
/// since `Backend` (out of scope here) exposes no `socket_path()` accessor.
fn backend_socket_display(backend_id: &str, target: &select::Target) -> PathBuf {
    match backend_id {
        select::RADIATOR_BACKEND => {
            radiator::resolve_socket_path(target.socket.as_deref(), target.name.as_deref())
        }
        _ => herdr::resolve_socket_path(target.socket.as_deref(), target.name.as_deref()),
    }
}

/// Prints a plan, or (from `status`) a plan plus the identities D48 pruned
/// from local state before it was built — dropped because their recorded
/// backend id no longer exists in the live snapshot, so the plan below now
/// recreates them. `pruned` is `None` for every caller but `status`, so
/// `plan`'s JSON output carries no `"pruned"` key and is unchanged.
fn print_plan(plan: &Plan, json: bool, pruned: Option<&[String]>) -> Result<()> {
    if json {
        let mut value = serde_json::to_value(plan)?;
        if let Some(pruned) = pruned
            && let Some(object) = value.as_object_mut()
        {
            object.insert("pruned".into(), serde_json::json!(pruned));
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    for identity in pruned.unwrap_or_default() {
        println!("recreate {identity}: backend id no longer exists; recreating");
    }
    print!("{}", plan.render());
    Ok(())
}

/// Prints each compile-time deprecation warning to stderr (D31), so a warning
/// never corrupts a `--json` stdout document.
fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}

/// Renders a compiled config back into an equivalent v3 Drovefile (D31): the
/// `drove render` "v3 form" block for a v2 Drovefile. It prints the resolved
/// profiles, so `extends`/`without` are already applied and every workspace
/// uses `panes = [herdr.tab(...)]` with `caller_pane` for adoption. Helper
/// functions and `load(...)` from the original source are not reconstructed.
mod v3form {
    use std::fmt::Write;

    use crate::model::{
        Agent, DroveConfig, Pane, Profile, Readiness, SplitDirection, Tab, Task, Workspace,
    };

    pub fn render(config: &DroveConfig) -> String {
        let mut out = String::new();
        let mut declared = false;
        if let Some(backend) = &config.backend {
            let _ = writeln!(out, "backend({})", quote(backend));
            declared = true;
        }
        if let Some(session) = &config.target.herdr_session {
            let _ = writeln!(out, "herdr.session({})", quote(session));
            declared = true;
        }
        if let Some(hub) = &config.target.radiator_hub {
            let _ = writeln!(out, "radiator.hub({})", quote(hub));
            declared = true;
        }
        if declared {
            out.push('\n');
        }
        for profile in config.profiles.values() {
            out.push_str(&render_profile(profile));
            out.push('\n');
        }
        out
    }

    fn render_profile(profile: &Profile) -> String {
        let mut args = vec![format!("name = {}", quote(&profile.name))];
        if !profile.workspaces.is_empty() {
            let items: Vec<String> = profile.workspaces.iter().map(render_workspace).collect();
            args.push(format!("workspaces = {}", list_block(&items, 1)));
        }
        if !profile.tasks.is_empty() {
            let items: Vec<String> = profile.tasks.iter().map(render_task).collect();
            args.push(format!("tasks = {}", list_block(&items, 1)));
        }
        call("profile", &args, 0)
    }

    fn render_workspace(workspace: &Workspace) -> String {
        let mut args = vec![quote(&workspace.name)];
        if let Some(label) = &workspace.label {
            args.push(format!("label = {}", quote(label)));
        }
        if workspace.cwd != std::path::Path::new(".") {
            args.push(format!(
                "cwd = {}",
                quote(&workspace.cwd.display().to_string())
            ));
        }
        if !workspace.env.is_empty() {
            args.push(format!("env = {}", render_env(&workspace.env)));
        }
        let panes: Vec<String> = workspace.tabs.iter().map(render_tab).collect();
        args.push(format!("panes = {}", list_block(&panes, 2)));
        call("workspace", &args, 1)
    }

    fn render_tab(tab: &Tab) -> String {
        let mut args = vec![quote(&tab.name)];
        if let Some(label) = &tab.label {
            args.push(format!("label = {}", quote(label)));
        }
        if tab.split != SplitDirection::Right {
            args.push(format!("split = {}", split_constant(tab.split)));
        }
        if !tab.ratios.is_empty() {
            let ratios: Vec<String> = tab.ratios.iter().map(|r| r.to_string()).collect();
            args.push(format!("ratios = [{}]", ratios.join(", ")));
        }
        let panes: Vec<String> = tab.panes.iter().map(render_pane).collect();
        args.push(format!("panes = {}", list_block(&panes, 3)));
        call("herdr.tab", &args, 2)
    }

    fn render_pane(pane: &Pane) -> String {
        let mut args = vec![quote(&pane.name)];
        if let Some(label) = &pane.label {
            args.push(format!("label = {}", quote(label)));
        }
        if let Some(cwd) = &pane.cwd {
            args.push(format!("cwd = {}", quote(&cwd.display().to_string())));
        }
        if !pane.env.is_empty() {
            args.push(format!("env = {}", render_env(&pane.env)));
        }
        if !pane.serve.is_empty() {
            args.push(format!("serve = {}", render_serve(&pane.serve)));
        }
        if let Some(ready) = &pane.ready {
            args.push(format!("ready = {}", render_ready(ready)));
        }
        if !pane.after.is_empty() {
            args.push(format!("after = {}", render_argv(&pane.after)));
        }
        if let Some(agent) = &pane.agent {
            args.push(format!("agent = {}", render_agent(agent)));
        }
        if let Some(on_start) = &pane.on_start {
            args.push(format!("on_start = {}", render_argv(on_start)));
        }
        if let Some(on_stop) = &pane.on_stop {
            args.push(format!("on_stop = {}", render_argv(on_stop)));
        }
        // `adopt = "caller"` becomes the `caller_pane` constructor (D31).
        let constructor = if pane.adopt.as_deref() == Some("caller") {
            "caller_pane"
        } else {
            "pane"
        };
        call(constructor, &args, 3)
    }

    fn render_task(task: &Task) -> String {
        let mut args = vec![quote(&task.name)];
        if !task.run.is_empty() {
            args.push(format!("run = {}", render_argv(&task.run)));
        }
        if let Some(check) = &task.check {
            args.push(format!("check = {}", render_argv(check)));
        }
        if !task.inputs.is_empty() {
            let inputs: Vec<String> = task
                .inputs
                .iter()
                .map(|p| quote(&p.display().to_string()))
                .collect();
            args.push(format!("inputs = [{}]", inputs.join(", ")));
        }
        if !task.after.is_empty() {
            args.push(format!("after = {}", render_argv(&task.after)));
        }
        if !task.auto {
            args.push("auto = False".to_owned());
        }
        if let Some(on_start) = &task.on_start {
            args.push(format!("on_start = {}", render_argv(on_start)));
        }
        if let Some(on_stop) = &task.on_stop {
            args.push(format!("on_stop = {}", render_argv(on_stop)));
        }
        call("task", &args, 1)
    }

    fn render_ready(ready: &Readiness) -> String {
        match ready {
            Readiness::Output { value } => format!("output({})", quote(value)),
            Readiness::Port { value } => format!("port({value})"),
            Readiness::Cmd { value } => format!("cmd({})", render_argv(value)),
        }
    }

    fn render_agent(agent: &Agent) -> String {
        let mut args = vec![quote(&agent.kind)];
        if !agent.args.is_empty() {
            args.push(format!("args = {}", render_argv(&agent.args)));
        }
        if let Some(prompt) = &agent.prompt {
            args.push(format!("prompt = {}", quote(prompt)));
        }
        if let Some(name) = &agent.name {
            args.push(format!("name = {}", quote(name)));
        }
        format!("agent({})", args.join(", "))
    }

    fn render_serve(serve: &[Vec<String>]) -> String {
        match serve {
            [single] => render_argv(single),
            many => {
                let candidates: Vec<String> = many.iter().map(|c| render_argv(c)).collect();
                format!("any_of({})", candidates.join(", "))
            }
        }
    }

    fn render_argv(argv: &[String]) -> String {
        let items: Vec<String> = argv.iter().map(|s| quote(s)).collect();
        format!("[{}]", items.join(", "))
    }

    fn render_env(env: &std::collections::BTreeMap<String, String>) -> String {
        let entries: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{}: {}", quote(k), quote(v)))
            .collect();
        format!("{{{}}}", entries.join(", "))
    }

    fn split_constant(split: SplitDirection) -> &'static str {
        match split {
            SplitDirection::Right => "herdr.RIGHT",
            SplitDirection::Down => "herdr.DOWN",
        }
    }

    /// Renders `name(arg, arg, ...)` with one argument per line, each indented
    /// `level` steps in from the call itself.
    fn call(name: &str, args: &[String], level: usize) -> String {
        let inner = indent(level + 1);
        let joined = args
            .iter()
            .map(|arg| format!("{inner}{arg}"))
            .collect::<Vec<_>>()
            .join(",\n");
        format!("{name}(\n{joined},\n{})", indent(level))
    }

    /// Renders `[item, item, ...]` with one item per line at `level`.
    fn list_block(items: &[String], level: usize) -> String {
        if items.is_empty() {
            return "[]".to_owned();
        }
        let inner = indent(level);
        let joined = items
            .iter()
            .map(|item| format!("{inner}{item}"))
            .collect::<Vec<_>>()
            .join(",\n");
        format!("[\n{joined},\n{}]", indent(level - 1))
    }

    fn indent(level: usize) -> String {
        "    ".repeat(level)
    }

    fn quote(text: &str) -> String {
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane_digests(config: &crate::model::DroveConfig, profile: &str) -> Vec<(String, String)> {
        let ir = config.profile(profile).expect("profile").to_ir();
        let mut panes: Vec<(String, String)> = ir
            .resources
            .iter()
            .filter(|resource| resource.kind == "pane")
            .map(|resource| (resource.name.clone(), resource.digest.clone()))
            .collect();
        panes.sort();
        panes
    }

    #[test]
    fn v3_form_round_trips_a_v2_drovefile_without_warnings() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("Drovefile"),
            r#"
control = workspace("control", tabs = [
    tab("coordinator", split = "down", ratios = [0.5], panes = [
        pane("controller", adopt = "caller"),
        pane("eventlog", serve = ["eventlog-view.sh", "-f"], ready = output("ready")),
    ]),
])
profile("default", workspaces = [control])
"#,
        )
        .expect("write v2 fixture");

        let v2 = compile(&directory.path().join("Drovefile")).expect("compile v2");
        assert!(!v2.warnings.is_empty(), "the fixture is a v2 form");
        let v3_source = v3form::render(&v2.config);
        // The rewrite uses only the v3 surface.
        assert!(v3_source.contains("herdr.tab("));
        assert!(v3_source.contains("caller_pane("));
        assert!(v3_source.contains("split = herdr.DOWN"));
        assert!(v3_source.contains("panes = ["));
        assert!(!v3_source.contains("tabs = ["));
        assert!(!v3_source.contains("adopt ="));

        // The rewrite recompiles cleanly and yields identical pane content
        // digests, so migrating proposes no restarts (D30).
        std::fs::write(directory.path().join("Drovefile"), &v3_source).expect("write v3 form");
        let v3 = compile(&directory.path().join("Drovefile")).expect("compile v3 form");
        assert!(
            v3.warnings.is_empty(),
            "v3 form still warns: {:?}",
            v3.warnings
        );
        assert_eq!(
            pane_digests(&v2.config, "default"),
            pane_digests(&v3.config, "default"),
        );
    }

    #[test]
    fn clap_defaults_to_up_workflow() {
        let cli = Cli::try_parse_from(["drove"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.profile_flag, None);
        assert_eq!(cli.profile_arg, None);
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
                yes: true,
                profile: None,
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
                yes: false,
                profile: None,
            })
        ));
    }

    #[test]
    fn clap_parses_positional_profile_with_no_subcommand() {
        let cli = Cli::try_parse_from(["drove", "monitoring"]).expect("parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.profile_arg.as_deref(), Some("monitoring"));
        assert_eq!(positional_profile(&cli), Some("monitoring"));
    }

    #[test]
    fn run_disambiguates_a_bare_profile_name_from_a_task_name() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("Drovefile"),
            "profile(name = \"default\", tasks = [task(name = \"scaffold\", run = [\"true\"])])\n\
             profile(name = \"monitoring\")",
        )
        .expect("write Drovefile");
        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");

        // `drove run monitoring`: the bare word names a profile and no
        // declared task, so it is reinterpreted as the profile.
        let mut cli = Cli::try_parse_from(["drove", "run", "monitoring"]).expect("parse");
        disambiguate_run_positional(&mut cli, &compiled.config);
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                task: None,
                profile: Some(ref name),
                ..
            }) if name == "monitoring"
        ));

        // `drove run scaffold`: the bare word names a task, so it stays put
        // even though the file happens to also declare a `default` profile.
        let mut cli = Cli::try_parse_from(["drove", "run", "scaffold"]).expect("parse");
        disambiguate_run_positional(&mut cli, &compiled.config);
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                task: Some(ref name),
                profile: None,
                ..
            }) if name == "scaffold"
        ));

        // `drove run scaffold monitoring`: both positionals already given,
        // so there is nothing to disambiguate.
        let mut cli =
            Cli::try_parse_from(["drove", "run", "scaffold", "monitoring"]).expect("parse");
        disambiguate_run_positional(&mut cli, &compiled.config);
        assert!(matches!(
            cli.command,
            Some(Command::Run {
                task: Some(ref task),
                profile: Some(ref profile),
                ..
            }) if task == "scaffold" && profile == "monitoring"
        ));
    }

    #[test]
    fn clap_parses_up_with_workspace_and_no_focus() {
        let cli = Cli::try_parse_from(["drove", "up", "--workspace", "api", "--no-focus", "--yes"])
            .expect("parse");
        assert!(matches!(
            &cli.command,
            Some(Command::Up { workspace: Some(w), no_focus: true, yes: true, .. }) if w == "api"
        ));
    }

    #[test]
    fn clap_parses_positional_profile_after_a_subcommand() {
        let cli = Cli::try_parse_from(["drove", "status", "monitoring"]).expect("parse");
        assert!(matches!(
            cli.command,
            Some(Command::Status { profile: Some(ref name) }) if name == "monitoring"
        ));
        assert_eq!(positional_profile(&cli), Some("monitoring"));
    }

    #[test]
    fn requested_profile_rejects_disagreeing_flag_and_positional() {
        let cli = Cli::try_parse_from(["drove", "--profile", "core", "monitoring"]).expect("parse");
        let error = requested_profile(&cli).expect_err("disagreeing profile");
        assert!(error.to_string().contains("must match"));
    }

    #[test]
    fn requested_profile_rejects_disagreeing_flag_and_a_subcommand_positional() {
        // Regression: the global `--profile` flag and every subcommand's own
        // positional field must not share a clap arg id, or the derive
        // collides them and one silently overwrites the other.
        let cli =
            Cli::try_parse_from(["drove", "--profile", "default", "lint", "other"]).expect("parse");
        assert_eq!(cli.profile_flag.as_deref(), Some("default"));
        assert_eq!(positional_profile(&cli), Some("other"));
        let error = requested_profile(&cli).expect_err("disagreeing profile");
        assert!(error.to_string().contains("must match"));
    }

    #[test]
    fn requested_profile_accepts_agreeing_flag_and_positional() {
        let cli =
            Cli::try_parse_from(["drove", "--profile", "monitoring", "monitoring"]).expect("parse");
        assert_eq!(requested_profile(&cli).expect("agree"), Some("monitoring"));
    }

    #[test]
    fn clap_parses_ls() {
        let cli = Cli::try_parse_from(["drove", "ls"]).expect("parse");
        assert!(matches!(cli.command, Some(Command::Ls)));
    }

    #[test]
    fn up_flags_cover_the_bare_command_and_explicit_up_only() {
        // `drove` with no subcommand is the up workflow with defaults.
        assert_eq!(up_flags(&None), Some((false, None, false)));
        // Other subcommands are read-only and never route through `up`.
        assert_eq!(up_flags(&Some(Command::Status { profile: None })), None);
        assert_eq!(up_flags(&Some(Command::Plan { profile: None })), None);
        // Explicit `up` carries its flags through.
        let cli = Cli::try_parse_from(["drove", "up", "--workspace", "api"]).expect("parse");
        assert_eq!(
            up_flags(&cli.command),
            Some((false, Some("api".to_owned()), false))
        );
    }

    fn report(outcome: UpOutcome) -> crate::executor::UpReport {
        crate::executor::UpReport {
            outcome,
            tasks: Vec::new(),
            focused: Some("w1".to_owned()),
            blocked_destructive: false,
        }
    }

    #[test]
    fn up_summary_json_carries_the_reconciled_shape() {
        let profile = Profile {
            name: "dev".into(),
            ..Default::default()
        };
        let object = up_summary_json(
            &profile,
            &report(UpOutcome::Reconciled {
                created: 2,
                changed: 1,
                tasks_run: 3,
            }),
        );
        assert_eq!(object["profile"], "dev");
        assert_eq!(object["status"], "in_sync");
        assert_eq!(object["created"], 2);
        assert_eq!(object["changed"], 1);
        assert_eq!(object["tasks_run"], 3);
        assert_eq!(object["focused"], "w1");
    }

    #[test]
    fn up_summary_json_carries_the_already_running_shape() {
        let profile = Profile {
            name: "dev".into(),
            ..Default::default()
        };
        let object = up_summary_json(&profile, &report(UpOutcome::AlreadyRunning));
        assert_eq!(object["status"], "already_running");
        // The reconciled counts are absent in this form.
        assert!(object.get("created").is_none());
    }
}
