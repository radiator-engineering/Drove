//! Command-line interface.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::{
    backend::{Backend, herdr, radiator, select},
    dsl::{compile, find_drovefile},
    executor::{
        ExecutionContext, HostCommandRunner, TaskOutcome, down, execute_plan_tasks, list_tasks,
        run_named_task,
    },
    model::Profile,
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

    /// Profile declared in Drovefile.
    #[arg(long, global = true, default_value = "default")]
    profile: String,

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
    /// Print the compiled intermediate representation (schema version 3); a
    /// v2 Drovefile also prints its deprecation warnings and v3 form (D31).
    Render,
    /// Run one task and its `after` prerequisites; with no task, list every
    /// declared task and its last recorded outcome.
    Run {
        task: Option<String>,

        /// Approve the task's (and any hook's) argv digest before running.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Warn about a stale `was =` declaration or a task with no `check`
    /// (D26, D34). Always exits 0.
    Lint,
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

    if let Some(Command::Run { task, yes }) = &cli.command {
        return run_command(profile, &repo_root, task.as_deref(), *yes, cli.json);
    }

    if matches!(cli.command, Some(Command::Lint)) {
        return lint_command(profile, &repo_root, cli.json);
    }

    let (backend_id, target) = resolve_backend(&cli, &compiled.config);

    if let Some(Command::Down { purge, yes }) = &cli.command {
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

    let client = select::open(&backend_id, &target)?;
    if let Err(error) = client.snapshot() {
        let socket = backend_socket_display(&backend_id, &target);
        if cli.json {
            println!(
                "{}",
                serde_json::json!({
                    "profile": cli.profile,
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

    let mut state = LocalState::load(&repo_root)?;
    // Backends cannot read ownership tokens back yet (PR 3), so a resource's
    // observed state comes only from local state's own record of the last
    // apply (D16's declared fallback) until live discovery lands.
    let snapshot = state
        .profile(&cli.profile)
        .map(|managed| managed.to_snapshot(&cli.profile, client.caller_pane_id()))
        .unwrap_or_default();
    let plan = build_plan(profile, &snapshot)?;

    match cli.command.unwrap_or(Command::Up {
        allow_replace: false,
        yes: false,
    }) {
        Command::Render | Command::Run { .. } | Command::Down { .. } | Command::Lint => {
            unreachable!("handled above")
        }
        Command::Status | Command::Plan => {
            print_plan(&plan, cli.json)?;
            print_warnings(&compiled.warnings);
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
                .any(|action| action.kind == Action::Core(CoreAction::Conflict))
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
            report_task_outcomes(&results, cli.json)
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
    if json {
        println!(
            "{}",
            serde_json::json!({
                "detached": report.detached,
                "hooks_run": report.hooks_run.iter().map(|(name, success)| {
                    serde_json::json!({"resource": name, "success": success})
                }).collect::<Vec<_>>(),
            })
        );
    } else {
        for id in &report.detached {
            println!("detached {id}");
        }
        if report.detached.is_empty() {
            println!("nothing owned by profile `{}`", profile.name);
        }
    }
    Ok(ExitCode::SUCCESS)
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

/// Resolves the backend id and target per D32's four-level order, gathering
/// the CLI/environment/Drovefile inputs the pure `select::resolve` needs.
fn resolve_backend(cli: &Cli, config: &crate::model::DroveConfig) -> (String, select::Target) {
    let cli_inputs = select::CliInputs {
        backend: cli.backend.as_deref(),
        target: cli.target.as_deref(),
        session: cli.session.as_deref(),
        socket: cli.socket.as_deref(),
    };
    let env_inputs = select::EnvInputs {
        drove_backend: std::env::var("DROVE_BACKEND").ok(),
        herdr_session: std::env::var("HERDR_SESSION").ok(),
        radiator_hub: std::env::var("RADIATOR_HUB").ok(),
        ambient_radiator: radiator::selected_by_environment(),
    };
    select::resolve(
        cli_inputs,
        &env_inputs,
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

fn print_plan(plan: &Plan, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
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
