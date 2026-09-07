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
    backend::{Backend, SessionState, herdr, radiator, select},
    dsl::{compile, find_drovefile},
    executor::{
        ExecutionContext, HostCommandRunner, TaskOutcome, UpOutcome, down, list_tasks,
        run_named_task, up,
    },
    ir::Ir,
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

    let (backend_id, target) = resolve_backend(&cli, &compiled.config, profile);

    if matches!(cli.command, Some(Command::Lint { .. })) {
        return lint_command(profile, &backend_id, &target, &repo_root, cli.json);
    }

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
    // D51 point 3: `HERDR_PANE_ID` (or Radiator's `RADIATOR_PANE_ID`) is
    // only trusted when it names a pane the live snapshot just fetched
    // actually lists — a stale value (a closed pane whose id was reused, or
    // the var leaking into an unrelated shell) is dropped, so a pane
    // declaring `adopt = "caller"` plans as a normal create instead of
    // adopting a phantom.
    let caller_pane_id = client
        .caller_pane_id()
        .filter(|id| live_snapshot.panes.iter().any(|pane| &pane.pane_id == id));
    // D54: fold the live snapshot's per-pane `process_info` in so the
    // planner can compare what's actually running to what's declared, not
    // only the declared state to itself. Only when the backend actually
    // reports the capability: Radiator sets a pane's `process_info` to
    // `None` both when nothing is running and when the hub simply omitted
    // it, and merging that unconditionally would read an unreported process
    // as idle and plan a spurious `RestartCommand`.
    let snapshot = pruned.to_snapshot(&profile.name, caller_pane_id);
    let snapshot = if client.capabilities().process_info {
        snapshot.merge_process_info(&live_snapshot)
    } else {
        snapshot
    };
    let plan = build_plan(profile, &snapshot)?;

    // Only `status` and `plan` reach here: Render/Run/Down/Lint/Ls returned
    // above, and Up (with the no-subcommand default) routed through
    // `up_command`. Both are read-only: print the plan and report drift.
    debug_assert!(matches!(
        cli.command,
        Some(Command::Status { .. }) | Some(Command::Plan { .. })
    ));
    // Only `status` annotates what the prune dropped (D48) and any
    // interrupted journal entry (D52 point 4); `plan` renders the plan
    // alone, unchanged.
    let is_status = matches!(cli.command, Some(Command::Status { .. }));
    let interrupted = state.interrupted();
    print_plan(
        &plan,
        cli.json,
        is_status.then_some(dropped.as_slice()),
        is_status.then_some(interrupted.as_slice()),
        is_status.then_some(live_snapshot.process_info_unavailable.as_slice()),
    )?;
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
            // D52 point 4: a previous `run` of this task began but never
            // recorded completion (killed mid-execution) — say so before
            // rerunning it, instead of silently retrying.
            if !json && state.has_interrupted(&format!("task:{name}")) {
                println!("previous run of {name} did not finish; rerunning");
            }
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
/// D51 point 4: `is_live` used to answer purely from local state, so a
/// session that was restarted since the last `up`/`plan` (wiping Herdr's
/// workspaces and ids) still looked live here. `lint` now opens the backend,
/// fetches its snapshot and prunes exactly as `plan`/`status` do first; when
/// the backend is unreachable, it says so and treats nothing as live, the
/// same as the unpruned fallback before this fix.
fn lint_command(
    profile: &Profile,
    backend_id: &str,
    target: &select::Target,
    repo_root: &Path,
    json: bool,
) -> Result<ExitCode> {
    let state = LocalState::load(repo_root)?;
    let managed = state.profile(&profile.name).cloned().unwrap_or_default();
    let client = select::open(backend_id, target)?;
    // An unreachable backend leaves nothing pruned-in as live, the same
    // conservative fallback `up_command` uses when the target isn't
    // reachable yet: `is_live` below then answers `false` for every `was =`.
    let (managed, backend_unreachable) = match client.snapshot() {
        Ok(live) => (crate::state::prune_missing(&managed, &live).0, false),
        Err(_) => (crate::state::ManagedProfile::default(), true),
    };
    let snapshot = managed.to_snapshot(&profile.name, None);

    let is_live = |name: &str| {
        snapshot
            .resources
            .get(name)
            .and_then(|observed| observed.owner.as_ref())
            .is_some_and(|owner| owner.profile == profile.name)
    };

    let mut warnings = Vec::new();
    if backend_unreachable {
        warnings.push(format!(
            "cannot reach {backend_id}; every `was =` is reported as matching nothing live"
        ));
    }
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

/// `drove down` (D19, D47, D50): runs every `on_stop` hook and detaches
/// every owned resource, then — on the Herdr backend, when the resolved
/// target names a session other than `default` (D46 precedence) — stops and
/// deletes that session. The detach is saved to local state before the
/// session is touched, so a `stop_session` failure (a missing `herdr`
/// binary, or a failed delete) still leaves the resources detached and a
/// retry of `down` idempotent.
///
/// Before tearing anything down, the live snapshot is fetched (D50) the same
/// way `up_command` does after D48, and the managed profile is pruned
/// against it: a resource the session no longer has is detached from state
/// without any backend call, reported under `pruned`, so a session that
/// already lost a pane never makes `down` abort. When the snapshot can't be
/// fetched at all (the session isn't running), `down` proceeds without a
/// backend — no `close_pane` calls — and warns; the D47 session stop still
/// runs below.
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

    let live_snapshot = client.snapshot();
    let session_unreachable = live_snapshot.is_err();
    let mut pruned: Vec<String> = Vec::new();
    if let Ok(live_snapshot) = &live_snapshot {
        let managed = state.profile(&profile.name).cloned().unwrap_or_default();
        let (pruned_profile, dropped) = crate::state::prune_missing(&managed, live_snapshot);
        if !dropped.is_empty() {
            state.profiles.insert(profile.name.clone(), pruned_profile);
            state.save()?;
            pruned = dropped;
        }
    }

    let backend: Option<&dyn Backend> = if purge && !session_unreachable {
        Some(client.as_ref())
    } else {
        None
    };
    let report = down(profile, &ctx, &mut state, yes, purge, backend)?;

    if !json {
        if session_unreachable {
            let target_word = if backend_id == select::HERDR_BACKEND {
                "session"
            } else {
                "hub"
            };
            println!("warning: {target_word} not reachable; detaching without closing panes");
        }
        for id in &pruned {
            println!("pruned {id} (not in session)");
        }
        for id in &report.detached {
            println!("detached {id}");
        }
        for (id, message) in &report.close_failed {
            println!("warning: could not close {id}: {message}");
        }
        if report.detached.is_empty() && pruned.is_empty() {
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
            "pruned": pruned,
            "close_failed": report.close_failed.iter().map(|(id, error)| {
                serde_json::json!({"id": id, "error": error})
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

/// Rewrites every workspace, pane, and placement digest in `managed` still
/// recorded under the pre-D53 single-hash format to the resource's current
/// composite digest, when that resource is still declared in `ir` — so the
/// D53 migration amnesty (`crate::planner::is_legacy_digest`) ends after one
/// `up` instead of persisting forever. A resource no longer declared is left
/// alone; the normal detach/prune path handles it. Returns whether anything
/// changed, so the caller knows whether to save.
fn restamp_legacy_digests(managed: &mut crate::state::ManagedProfile, ir: &Ir) -> bool {
    let mut changed = false;
    for (id, resource) in &mut managed.resources {
        if !crate::planner::is_legacy_digest(&resource.digest) {
            continue;
        }
        let fresh = match resource.kind.as_str() {
            "placement" => ir
                .placements
                .iter()
                .find(|group| &group.id == id)
                .map(|group| group.topology_digest.clone()),
            kind => ir
                .resources
                .iter()
                .find(|candidate| candidate.kind == kind && &candidate.name == id)
                .map(|candidate| candidate.digest.clone()),
        };
        if let Some(fresh) = fresh {
            resource.digest = fresh;
            changed = true;
        }
    }
    changed
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
    let ir = profile.to_ir();
    let live_snapshot = client.snapshot();
    if let Ok(live_snapshot) = &live_snapshot {
        let managed = state.profile(profile_arg).cloned().unwrap_or_default();
        let (mut pruned, dropped) = crate::state::prune_missing(&managed, live_snapshot);
        // D53 migration: a resource confirmed still live (it survived the
        // prune above) but still recorded under the pre-D53 single-hash
        // digest can't be diffed by category, so the planner treats it as
        // converged (`planner::is_legacy_digest`) rather than guessing what
        // changed from a format that carries no field breakdown at all. That
        // has to end after one `up`, not stay true forever, so re-stamp the
        // fresh composite digest here — outside the plan entirely, so it
        // never shows up as an edit.
        let restamped = restamp_legacy_digests(&mut pruned, &ir);
        if !dropped.is_empty() || restamped {
            state.profiles.insert(profile_arg.to_owned(), pruned);
            state.save()?;
        }
    }
    // D51 point 3: only trust `HERDR_PANE_ID`/`RADIATOR_PANE_ID` when the
    // live snapshot just fetched (if any) actually lists it; with no live
    // snapshot yet (the target wasn't reachable), there is nothing to
    // confirm it against, so it is not trusted here either.
    let caller_pane_id = client.caller_pane_id().filter(|id| {
        live_snapshot
            .as_ref()
            .is_ok_and(|live| live.panes.iter().any(|pane| &pane.pane_id == id))
    });
    let snapshot = state
        .profile(profile_arg)
        .map(|managed| managed.to_snapshot(profile_arg, caller_pane_id))
        .unwrap_or_default();
    // D54: same live `process_info` merge as `plan`/`status`, gated the same
    // way on the backend's capability, when the target was reachable for
    // the D48 prune above.
    let snapshot = match &live_snapshot {
        Ok(live_snapshot) if client.capabilities().process_info => {
            snapshot.merge_process_info(live_snapshot)
        }
        _ => snapshot,
    };
    let mut plan = build_plan(profile, &snapshot)?;

    let is_herdr = backend_id == select::HERDR_BACKEND;
    // The session name for the headless start and the attach.
    let session = target.name.as_deref().unwrap_or("default");

    // D51 point 1: when the target wasn't reachable at the up-front probe
    // above, `plan` was built from local state exactly as recorded before
    // this run — nothing has pruned it. Herdr wipes every workspace and
    // restarts its id counter on a fresh start (issue 24), so if starting
    // the session here finds it wasn't already running, that picture is
    // stale: re-fetch the now-live snapshot, re-run `prune_missing`, save,
    // and rebuild the plan before the `Conflict` check or anything is
    // applied, discarding the plan built above. Only attempted when the
    // up-front probe found the target unreachable: when it already
    // succeeded, the prune above already ran against a live snapshot and
    // `ensure_session` is guaranteed to find the session already `Running`
    // (no restart to react to), so there is nothing to redo here — this
    // also avoids a second, redundant `ensure_session` call on that path,
    // since `up()` below always makes its own (idempotently, against the
    // now-running session, since it also owns reporting `CannotStart`
    // uniformly when starting fails).
    let mut session_started = false;
    if is_herdr
        && live_snapshot.is_err()
        && let Some(ext) = client.herdr()
        && let SessionState::Started = ext.ensure_session(session)?
    {
        session_started = true;
        if let Ok(live) = client.snapshot() {
            let managed = state.profile(profile_arg).cloned().unwrap_or_default();
            let (pruned, dropped) = crate::state::prune_missing(&managed, &live);
            if !dropped.is_empty() {
                state.profiles.insert(profile_arg.to_owned(), pruned);
            }
            state.save()?;
            // D51 point 3: same cross-check as above, against the snapshot
            // just fetched from the now-started session.
            let caller_pane_id = client
                .caller_pane_id()
                .filter(|id| live.panes.iter().any(|pane| &pane.pane_id == id));
            let snapshot = state
                .profile(profile_arg)
                .map(|managed| managed.to_snapshot(profile_arg, caller_pane_id))
                .unwrap_or_default();
            // D54: same capability-gated `process_info` merge as above,
            // against the snapshot just fetched from the now-started
            // session.
            let snapshot = if client.capabilities().process_info {
                snapshot.merge_process_info(&live)
            } else {
                snapshot
            };
            plan = build_plan(profile, &snapshot)?;
        }
    }

    // A `Conflict` never applies anything and exits 2 (D43 step 3).
    if plan
        .actions
        .iter()
        .any(|action| action.kind == Action::Core(CoreAction::Conflict))
    {
        print_plan(&plan, json, None, None, None)?;
        return Ok(ExitCode::from(2));
    }

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

    // The workspace to bring to the front: the flag, else the profile's
    // first workspace.
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
        &ir,
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

    print_up_summary(profile, &report, session_started, json)?;
    // D52 point 3: one line per failure and per skip, printed after the
    // summary and after everything already applied has been saved.
    if !json {
        for failure in &report.failed {
            println!("failed: {}: {}", failure.address, failure.error);
        }
        for skip in &report.skipped {
            println!("skipped: {}: depends on {}", skip.address, skip.depends_on);
        }
    }

    let blocked = report.blocked_destructive
        || !report.failed.is_empty()
        || report.tasks.iter().any(|(_, outcome)| {
            matches!(
                outcome,
                TaskOutcome::Blocked | TaskOutcome::Ran(false) | TaskOutcome::DependencySkipped
            )
        });
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
fn up_summary_json(
    profile: &Profile,
    report: &crate::executor::UpReport,
    session_started: bool,
) -> serde_json::Value {
    let tasks: Vec<_> = report
        .tasks
        .iter()
        .map(
            |(name, outcome)| serde_json::json!({"task": name, "outcome": outcome_label(*outcome)}),
        )
        .collect();
    let failed: Vec<_> = report
        .failed
        .iter()
        .map(|failure| serde_json::json!({"action": failure.address, "error": failure.error}))
        .collect();
    let skipped: Vec<_> = report
        .skipped
        .iter()
        .map(|skip| serde_json::json!({"action": skip.address, "depends_on": skip.depends_on}))
        .collect();
    let mut object = serde_json::json!({
        "profile": profile.name,
        "focused": report.focused,
        "tasks": tasks,
        "failed": failed,
        "skipped": skipped,
        "session_started": session_started,
    });
    match &report.outcome {
        UpOutcome::Reconciled {
            created,
            changed,
            tasks_run,
        } => {
            object["status"] = if report.failed.is_empty() {
                serde_json::json!("in_sync")
            } else {
                serde_json::json!("partial_failure")
            };
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
    session_started: bool,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", up_summary_json(profile, report, session_started));
        return Ok(());
    }
    match &report.outcome {
        UpOutcome::Reconciled {
            created,
            changed,
            tasks_run,
        } => {
            let sync_word = if report.failed.is_empty() {
                "in sync"
            } else {
                "partial failure"
            };
            println!(
                "profile {}: {created} created, {changed} changed, {tasks_run} tasks run, {sync_word}",
                profile.name
            )
        }
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
            TaskOutcome::Ran(false) | TaskOutcome::DependencySkipped => failed = true,
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
        TaskOutcome::DependencySkipped => "dependency_skipped",
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
        TaskOutcome::DependencySkipped => {
            format!("{name}: skipped (an `after` prerequisite did not succeed)")
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

/// recreates them — a journal entry an earlier apply began but never
/// finished (D52 point 4), and (D51 point 2) the pane ids whose
/// `process_info` this run's snapshot could not fetch. Each is `None` for
/// every caller but `status`, so `plan`'s JSON output carries none of those
/// keys and is unchanged.
fn print_plan(
    plan: &Plan,
    json: bool,
    pruned: Option<&[String]>,
    interrupted: Option<&[(&str, &str)]>,
    process_info_unavailable: Option<&[String]>,
) -> Result<()> {
    if json {
        let mut value = serde_json::to_value(plan)?;
        if let Some(object) = value.as_object_mut() {
            if let Some(pruned) = pruned {
                object.insert("pruned".into(), serde_json::json!(pruned));
            }
            if let Some(interrupted) = interrupted {
                let rows: Vec<_> = interrupted
                    .iter()
                    .map(|(action, digest)| serde_json::json!({"action": action, "digest": digest}))
                    .collect();
                object.insert("interrupted".into(), serde_json::json!(rows));
            }
            if let Some(process_info_unavailable) = process_info_unavailable {
                object.insert(
                    "process_info_unavailable".into(),
                    serde_json::json!(process_info_unavailable),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    for identity in pruned.unwrap_or_default() {
        println!("recreate {identity}: backend id no longer exists; recreating");
    }
    // D52 point 4: a journal entry an earlier apply began but never finished
    // (a killed `run`/hook) is surfaced here instead of silently retried.
    for (action, digest) in interrupted.unwrap_or_default() {
        println!("interrupted {action} ({digest})");
    }
    for pane_id in process_info_unavailable.unwrap_or_default() {
        println!("warning: could not read process info for pane {pane_id}");
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
            failed: Vec::new(),
            skipped: Vec::new(),
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
            true,
        );
        assert_eq!(object["profile"], "dev");
        assert_eq!(object["status"], "in_sync");
        assert_eq!(object["created"], 2);
        assert_eq!(object["changed"], 1);
        assert_eq!(object["tasks_run"], 3);
        assert_eq!(object["focused"], "w1");
        assert_eq!(object["session_started"], true);
    }

    #[test]
    fn up_summary_json_carries_failed_and_skipped_actions() {
        use crate::executor::{FailedAction, SkippedAction};

        let profile = Profile {
            name: "dev".into(),
            ..Default::default()
        };
        let mut report = report(UpOutcome::Reconciled {
            created: 1,
            changed: 0,
            tasks_run: 0,
        });
        report.failed.push(FailedAction {
            address: "ops".into(),
            error: "boom".into(),
        });
        report.skipped.push(SkippedAction {
            address: "ops/main".into(),
            depends_on: "ops".into(),
        });

        let object = up_summary_json(&profile, &report, false);
        assert_eq!(object["failed"][0]["action"], "ops");
        assert_eq!(object["failed"][0]["error"], "boom");
        assert_eq!(object["skipped"][0]["action"], "ops/main");
        assert_eq!(object["skipped"][0]["depends_on"], "ops");
        assert_eq!(
            object["status"], "partial_failure",
            "a partial apply must not report in_sync"
        );
    }

    #[test]
    fn up_summary_json_carries_the_already_running_shape() {
        let profile = Profile {
            name: "dev".into(),
            ..Default::default()
        };
        let object = up_summary_json(&profile, &report(UpOutcome::AlreadyRunning), false);
        assert_eq!(object["status"], "already_running");
        assert_eq!(object["session_started"], false);
        // The reconciled counts are absent in this form.
        assert!(object.get("created").is_none());
    }
}
