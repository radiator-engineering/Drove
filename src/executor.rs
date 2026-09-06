//! Executes tasks and lifecycle hooks: host argv Drove runs directly,
//! approval-gated on content digest, recorded in the local state journal
//! (spec §5, D11-D13, D19).
//!
//! Reconciling workspaces, tabs, panes and agents against a live backend is
//! out of scope here (PRs 3 and 5 fill in the `Backend` methods this module
//! does not call); this module only runs the argv a `task()` or an
//! `on_start`/`on_stop` hook declares, on the host, in the repo root.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Command,
};

use anyhow::Result;

use crate::{
    backend::Backend,
    ir::{Ir, Resource},
    model::{Profile, Task, canonical_digest},
    planner::{ActionKind, Plan},
    state::{LocalState, ManagedResource},
};

/// Runs host argv. A real [`HostCommandRunner`] shells out; tests substitute
/// a fake that records calls instead of touching the filesystem or network.
pub trait CommandRunner {
    fn run(&self, argv: &[String], cwd: &Path, env: &BTreeMap<String, String>) -> Result<bool>;
}

pub struct HostCommandRunner;

impl CommandRunner for HostCommandRunner {
    fn run(&self, argv: &[String], cwd: &Path, env: &BTreeMap<String, String>) -> Result<bool> {
        anyhow::ensure!(!argv.is_empty(), "cannot run an empty argv");
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).current_dir(cwd);
        for (key, value) in env {
            command.env(key, value);
        }
        Ok(command.status()?.success())
    }
}

pub struct ExecutionContext<'a> {
    pub repo_root: &'a Path,
    pub profile: &'a str,
    pub runner: &'a dyn CommandRunner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    /// `check` passed; `run` never executed.
    Skipped,
    /// `run` executed; carries its exit status.
    Ran(bool),
    /// `run` needed approval that isn't recorded yet.
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Start,
    Stop,
}

impl HookEvent {
    fn label(self) -> &'static str {
        match self {
            HookEvent::Start => "on_start",
            HookEvent::Stop => "on_stop",
        }
    }
}

/// Runs one `task()`: `check` first (early cutoff), otherwise the
/// approval-gated `run`, then its `on_start` hook if `run` executed at all.
/// `resource_digest` is the task's IR content digest, recorded in local
/// state so the next `build_plan` sees this task as converged.
pub fn run_task(
    task: &Task,
    resource_digest: &str,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
) -> Result<TaskOutcome> {
    if let Some(check) = &task.check
        && ctx.runner.run(check, ctx.repo_root, &BTreeMap::new())?
    {
        record_task_resource(state, ctx, &task.name, resource_digest, "skipped")?;
        return Ok(TaskOutcome::Skipped);
    }

    let approval_digest = canonical_digest(&task.run)?;
    if approve {
        state.approve(approval_digest.clone());
    }
    if !state.is_approved(&approval_digest) {
        return Ok(TaskOutcome::Blocked);
    }

    state.begin_action(&format!("task:{}", task.name), &approval_digest)?;
    let success = ctx.runner.run(&task.run, ctx.repo_root, &BTreeMap::new())?;
    state.finish_action(&approval_digest, success)?;

    if let Some(hook) = &task.on_start {
        run_hook(
            hook,
            &task.name,
            None,
            HookEvent::Start,
            ctx,
            state,
            approve,
        )?;
    }

    record_task_resource(
        state,
        ctx,
        &task.name,
        resource_digest,
        if success { "ok" } else { "failed" },
    )?;
    Ok(TaskOutcome::Ran(success))
}

/// Runs one `on_start`/`on_stop` argv hook (D13): approval-gated like a
/// task's `run`, with `DROVE_RESOURCE` and (when known) `DROVE_BACKEND_ID`
/// in the environment.
pub fn run_hook(
    argv: &[String],
    resource: &str,
    backend_id: Option<&str>,
    event: HookEvent,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
) -> Result<TaskOutcome> {
    if argv.is_empty() {
        return Ok(TaskOutcome::Skipped);
    }

    let mut env = BTreeMap::new();
    env.insert("DROVE_RESOURCE".to_owned(), resource.to_owned());
    if let Some(id) = backend_id {
        env.insert("DROVE_BACKEND_ID".to_owned(), id.to_owned());
    }

    let approval_digest = canonical_digest(&argv.to_vec())?;
    if approve {
        state.approve(approval_digest.clone());
    }
    if !state.is_approved(&approval_digest) {
        return Ok(TaskOutcome::Blocked);
    }

    state.begin_action(
        &format!("hook:{resource}:{}", event.label()),
        &approval_digest,
    )?;
    let success = ctx.runner.run(argv, ctx.repo_root, &env)?;
    state.finish_action(&approval_digest, success)?;
    Ok(TaskOutcome::Ran(success))
}

fn record_task_resource(
    state: &mut LocalState,
    ctx: &ExecutionContext<'_>,
    name: &str,
    digest: &str,
    outcome: &str,
) -> Result<()> {
    let profile = state.profile_mut(ctx.profile);
    profile.resources.insert(
        name.to_owned(),
        ManagedResource {
            kind: "task".into(),
            backend_id: String::new(),
            parent: None,
            digest: digest.to_owned(),
            adopted: None,
            last_outcome: Some(outcome.to_owned()),
        },
    );
    state.save()
}

/// `name`, last recorded outcome (`None` if it has never run) for every
/// declared task, in declaration order — what `drove run` with no argument
/// prints.
pub fn list_tasks(profile: &Profile, state: &LocalState) -> Vec<(String, Option<String>)> {
    let managed = state.profile(profile.name.as_str());
    profile
        .tasks
        .iter()
        .map(|task| {
            let outcome = managed
                .and_then(|managed| managed.resources.get(&task.name))
                .and_then(|resource| resource.last_outcome.clone());
            (task.name.clone(), outcome)
        })
        .collect()
}

/// Runs `target` and every task it transitively depends on through `after`,
/// in dependency order, and nothing else declared in the profile.
pub fn run_named_task(
    profile: &Profile,
    target: &str,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
) -> Result<Vec<(String, TaskOutcome)>> {
    let tasks_by_name: BTreeMap<&str, &Task> = profile
        .tasks
        .iter()
        .map(|task| (task.name.as_str(), task))
        .collect();
    anyhow::ensure!(
        tasks_by_name.contains_key(target),
        "no task named `{target}`"
    );

    let mut needed: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![target.to_owned()];
    while let Some(name) = stack.pop() {
        if !needed.insert(name.clone()) {
            continue;
        }
        if let Some(task) = tasks_by_name.get(name.as_str()) {
            for dep in &task.after {
                if tasks_by_name.contains_key(dep.as_str()) {
                    stack.push(dep.clone());
                }
            }
        }
    }

    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &needed {
        let after = tasks_by_name[name.as_str()].after.clone();
        edges.insert(name.clone(), after);
    }
    let order = topo_forward(&needed, &edges);

    let ir = profile.to_ir();
    let mut results = Vec::new();
    for name in order {
        let task = tasks_by_name[name.as_str()];
        let digest = digest_of(&ir, "task", &name)?;
        let outcome = run_task(task, digest, ctx, state, approve)?;
        results.push((name, outcome));
    }
    Ok(results)
}

/// Runs every `RunTask` action a [`Plan`] proposes, in the plan's own
/// (already `after`-ordered) order. Every other action kind is left for a
/// future PR once the `Backend` methods it needs (PR 3/5) exist.
pub fn execute_plan_tasks(
    profile: &Profile,
    plan: &Plan,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
) -> Result<Vec<(String, TaskOutcome)>> {
    let tasks_by_name: BTreeMap<&str, &Task> = profile
        .tasks
        .iter()
        .map(|task| (task.name.as_str(), task))
        .collect();
    let ir = profile.to_ir();
    let mut results = Vec::new();
    for action in &plan.actions {
        if action.kind != ActionKind::RunTask {
            continue;
        }
        let Some(task) = tasks_by_name.get(action.address.as_str()) else {
            continue;
        };
        let digest = digest_of(&ir, "task", &action.address)?;
        let outcome = run_task(task, digest, ctx, state, approve)?;
        results.push((action.address.clone(), outcome));
    }
    Ok(results)
}

#[derive(Debug, Clone, Default)]
pub struct DownReport {
    /// Resource identities detached, in the order they were torn down.
    pub detached: Vec<String>,
    /// `(resource identity, hook succeeded)` for every `on_stop` hook run.
    pub hooks_run: Vec<(String, bool)>,
}

/// `drove down` (D19): runs each owned resource's `on_stop` hook (if the
/// profile still declares one), then detaches it — with `purge`, also
/// closes owned panes on the backend — in reverse dependency order.
/// Resources the backend doesn't recognize as owned by this profile (an
/// unmanaged pane) are never touched, because they are never in
/// `state`'s managed set to begin with.
pub fn down(
    profile: &Profile,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
    purge: bool,
    backend: Option<&dyn Backend>,
) -> Result<DownReport> {
    let ir = profile.to_ir();
    let on_stop_hooks = collect_hooks(profile, HookEvent::Stop);
    let managed = state.profile(ctx.profile).cloned().unwrap_or_default();
    let order = teardown_order(&managed.resources, &ir);

    let mut report = DownReport::default();
    for id in order {
        let Some(resource) = managed.resources.get(&id) else {
            continue;
        };
        if let Some(hook) = on_stop_hooks.get(id.as_str()) {
            let backend_id = if resource.backend_id.is_empty() {
                None
            } else {
                Some(resource.backend_id.as_str())
            };
            let outcome = run_hook(hook, &id, backend_id, HookEvent::Stop, ctx, state, approve)?;
            if let TaskOutcome::Ran(success) = outcome {
                report.hooks_run.push((id.clone(), success));
            }
        }

        if purge
            && resource.kind == "pane"
            && let Some(backend) = backend
        {
            backend.close_pane(&resource.backend_id)?;
        }

        state.profile_mut(ctx.profile).resources.remove(&id);
        state.save()?;
        report.detached.push(id);
    }
    Ok(report)
}

fn collect_hooks(profile: &Profile, event: HookEvent) -> BTreeMap<&str, &[String]> {
    let mut hooks = BTreeMap::new();
    for workspace in &profile.workspaces {
        for tab in &workspace.tabs {
            for pane in &tab.panes {
                let hook = match event {
                    HookEvent::Start => &pane.on_start,
                    HookEvent::Stop => &pane.on_stop,
                };
                if let Some(argv) = hook {
                    hooks.insert(pane.name.as_str(), argv.as_slice());
                }
            }
        }
    }
    for task in &profile.tasks {
        let hook = match event {
            HookEvent::Start => &task.on_start,
            HookEvent::Stop => &task.on_stop,
        };
        if let Some(argv) = hook {
            hooks.insert(task.name.as_str(), argv.as_slice());
        }
    }
    hooks
}

/// A resource's identity for the shared namespace (D5): its own name,
/// except a tab, whose identity is scoped to its workspace. Mirrors
/// `planner::identity`, which is private to that module.
fn identity(resource: &Resource) -> String {
    if resource.kind == "tab" {
        format!(
            "{}/{}",
            resource.parent.as_deref().unwrap_or(""),
            resource.name
        )
    } else {
        resource.name.clone()
    }
}

/// Dependent -> its dependencies: a resource's structural parent (a pane
/// depends on its tab, a tab on its workspace, an agent on its pane) plus
/// whatever it names in a declared `after`.
fn dependency_edges(ir: &Ir) -> BTreeMap<String, Vec<String>> {
    let mut edges = BTreeMap::new();
    for resource in &ir.resources {
        let mut deps = Vec::new();
        if let Some(parent) = &resource.parent {
            deps.push(parent.clone());
        }
        if let Some(after) = resource.fields.get("after").and_then(|v| v.as_array()) {
            for value in after {
                if let Some(name) = value.as_str() {
                    deps.push(name.to_owned());
                }
            }
        }
        edges.insert(identity(resource), deps);
    }
    edges
}

fn digest_of<'a>(ir: &'a Ir, kind: &str, name: &str) -> Result<&'a str> {
    ir.resources
        .iter()
        .find(|resource| resource.kind == kind && resource.name == name)
        .map(|resource| resource.digest.as_str())
        .ok_or_else(|| anyhow::anyhow!("no {kind} resource named `{name}`"))
}

/// Dependencies before dependents (a resource's creation order), restricted
/// to `ids`. The DAG is already enforced by `Profile::validate`, so a stray
/// cycle among `ids` alone (there isn't one in practice) just falls back to
/// appending the unresolved remainder in name order.
fn topo_forward(ids: &BTreeSet<String>, edges: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut indegree: BTreeMap<&str, usize> = ids.iter().map(|id| (id.as_str(), 0)).collect();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for id in ids {
        if let Some(deps) = edges.get(id) {
            for dep in deps {
                if ids.contains(dep) {
                    *indegree
                        .get_mut(id.as_str())
                        .expect("every id in `ids` seeds `indegree`") += 1;
                    children.entry(dep.as_str()).or_default().push(id.as_str());
                }
            }
        }
    }

    let mut frontier: BTreeSet<&str> = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order: Vec<String> = Vec::new();
    while let Some(id) = frontier.iter().next().copied() {
        frontier.remove(id);
        order.push(id.to_owned());
        if let Some(kids) = children.get(id) {
            for kid in kids {
                let degree = indegree
                    .get_mut(kid)
                    .expect("`children` only ever names ids seeded into `indegree`");
                *degree -= 1;
                if *degree == 0 {
                    frontier.insert(kid);
                }
            }
        }
    }
    for id in ids {
        if !order.contains(id) {
            order.push(id.clone());
        }
    }
    order
}

fn teardown_order(managed: &BTreeMap<String, ManagedResource>, ir: &Ir) -> Vec<String> {
    let ids: BTreeSet<String> = managed.keys().cloned().collect();
    let edges = dependency_edges(ir);
    let mut order = topo_forward(&ids, &edges);
    order.reverse();
    order
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Mutex};

    use serde_json::json;

    use super::*;
    use crate::planner::{Snapshot, build_plan};

    type Call = (Vec<String>, BTreeMap<String, String>);

    #[derive(Default)]
    struct FakeRunner {
        /// argv joined with a space -> whether it should succeed.
        outcomes: Mutex<BTreeMap<String, bool>>,
        calls: Mutex<Vec<Call>>,
    }

    impl FakeRunner {
        fn succeed(self, argv: &[&str]) -> Self {
            self.outcomes
                .lock()
                .expect("outcomes mutex")
                .insert(argv.join(" "), true);
            self
        }

        fn fail(self, argv: &[&str]) -> Self {
            self.outcomes
                .lock()
                .expect("outcomes mutex")
                .insert(argv.join(" "), false);
            self
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .expect("calls mutex")
                .iter()
                .map(|(argv, _)| argv.clone())
                .collect()
        }

        fn envs_for(&self, argv: &[&str]) -> Option<BTreeMap<String, String>> {
            let key = argv.join(" ");
            self.calls
                .lock()
                .expect("calls mutex")
                .iter()
                .find(|(call, _)| call.join(" ") == key)
                .map(|(_, env)| env.clone())
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            argv: &[String],
            _cwd: &Path,
            env: &BTreeMap<String, String>,
        ) -> Result<bool> {
            self.calls
                .lock()
                .expect("calls mutex")
                .push((argv.to_vec(), env.clone()));
            Ok(*self
                .outcomes
                .lock()
                .expect("outcomes mutex")
                .get(&argv.join(" "))
                .unwrap_or(&true))
        }
    }

    /// Builds a `LocalState` pointed at a fresh temp file directly, rather
    /// than through `LocalState::load`'s env-var-based directory lookup:
    /// that lookup is process-global, and mutating it from a test would
    /// need `std::env::set_var`, which this crate denies (`unsafe_code =
    /// "deny"`) and which would race other tests regardless.
    fn temp_state() -> (LocalState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: dir.path().join("state.json"),
        };
        (state, dir)
    }

    fn profile_from(value: serde_json::Value) -> Profile {
        serde_json::from_value(value).expect("profile fixture")
    }

    fn ctx<'a>(root: &'a Path, runner: &'a dyn CommandRunner) -> ExecutionContext<'a> {
        ExecutionContext {
            repo_root: root,
            profile: "default",
            runner,
        }
    }

    #[test]
    fn check_passing_skips_run() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default().succeed(&["check"]);
        let task = Task {
            name: "scaffold".into(),
            run: vec!["run".into()],
            check: Some(vec!["check".into()]),
            inputs: vec![],
            after: vec![],
            auto: true,
            on_start: None,
            on_stop: None,
        };
        let root = PathBuf::from("/repo");
        let outcome =
            run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, false).expect("run_task");
        assert_eq!(outcome, TaskOutcome::Skipped);
        assert_eq!(runner.calls(), vec![vec!["check".to_owned()]]);
        assert_eq!(
            state
                .profile("default")
                .expect("profile recorded")
                .resources
                .get("scaffold")
                .expect("scaffold recorded")
                .last_outcome
                .as_deref(),
            Some("skipped")
        );
    }

    #[test]
    fn failing_check_runs_the_task_when_approved() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default().fail(&["check"]).succeed(&["run"]);
        let task = Task {
            name: "scaffold".into(),
            run: vec!["run".into()],
            check: Some(vec!["check".into()]),
            inputs: vec![],
            after: vec![],
            auto: true,
            on_start: None,
            on_stop: None,
        };
        let root = PathBuf::from("/repo");
        let outcome =
            run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, true).expect("run_task");
        assert_eq!(outcome, TaskOutcome::Ran(true));
        assert_eq!(
            runner.calls(),
            vec![vec!["check".to_owned()], vec!["run".to_owned()]]
        );
    }

    #[test]
    fn unapproved_task_is_blocked() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let task = Task {
            name: "scaffold".into(),
            run: vec!["run".into()],
            check: None,
            inputs: vec![],
            after: vec![],
            auto: true,
            on_start: None,
            on_stop: None,
        };
        let root = PathBuf::from("/repo");
        let outcome =
            run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, false).expect("run_task");
        assert_eq!(outcome, TaskOutcome::Blocked);
        assert!(runner.calls().is_empty(), "blocked task must not run");
    }

    #[test]
    fn approving_once_covers_a_later_unattended_run() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let task = Task {
            name: "scaffold".into(),
            run: vec!["run".into()],
            check: None,
            inputs: vec![],
            after: vec![],
            auto: true,
            on_start: None,
            on_stop: None,
        };
        let root = PathBuf::from("/repo");
        run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, true).expect("first run");
        let second = run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, false)
            .expect("second run");
        assert_eq!(second, TaskOutcome::Ran(true));
    }

    #[test]
    fn task_on_start_hook_fires_after_run() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let task = Task {
            name: "scaffold".into(),
            run: vec!["run".into()],
            check: None,
            inputs: vec![],
            after: vec![],
            auto: true,
            on_start: Some(vec!["notify".into()]),
            on_stop: None,
        };
        let root = PathBuf::from("/repo");
        run_task(&task, "digest-1", &ctx(&root, &runner), &mut state, true).expect("run");
        assert_eq!(
            runner.calls(),
            vec![vec!["run".to_owned()], vec!["notify".to_owned()]]
        );
        let env = runner.envs_for(&["notify"]).expect("hook env recorded");
        assert_eq!(env.get("DROVE_RESOURCE"), Some(&"scaffold".to_owned()));
    }

    #[test]
    fn hook_reports_backend_id_in_env() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");
        run_hook(
            &["notify".into()],
            "gitlog",
            Some("w1:p2"),
            HookEvent::Stop,
            &ctx(&root, &runner),
            &mut state,
            true,
        )
        .expect("hook");
        let env = runner.envs_for(&["notify"]).expect("env recorded");
        assert_eq!(env.get("DROVE_RESOURCE"), Some(&"gitlog".to_owned()));
        assert_eq!(env.get("DROVE_BACKEND_ID"), Some(&"w1:p2".to_owned()));
    }

    #[test]
    fn run_named_task_runs_only_target_and_its_prerequisites() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [
                {"name": "a", "run": ["run-a"]},
                {"name": "b", "run": ["run-b"], "after": ["a"]},
                {"name": "unrelated", "run": ["run-unrelated"]}
            ]
        }));
        let root = PathBuf::from("/repo");
        let results =
            run_named_task(&profile, "b", &ctx(&root, &runner), &mut state, true).expect("run");
        assert_eq!(
            results.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(
            runner.calls(),
            vec![vec!["run-a".to_owned()], vec!["run-b".to_owned()]]
        );
    }

    #[test]
    fn run_named_task_rejects_unknown_name() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let profile = profile_from(json!({"name": "default", "tasks": []}));
        let root = PathBuf::from("/repo");
        let error = run_named_task(&profile, "missing", &ctx(&root, &runner), &mut state, true)
            .expect_err("unknown task");
        assert!(error.to_string().contains("no task named"));
    }

    #[test]
    fn list_tasks_reports_last_outcome() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "scaffold", "run": ["run"]}, {"name": "never-run", "run": ["run"]}]
        }));
        let root = PathBuf::from("/repo");
        run_named_task(&profile, "scaffold", &ctx(&root, &runner), &mut state, true).expect("run");
        let listed = list_tasks(&profile, &state);
        assert_eq!(
            listed,
            vec![
                ("scaffold".to_owned(), Some("ok".to_owned())),
                ("never-run".to_owned(), None),
            ]
        );
    }

    #[test]
    fn execute_plan_tasks_runs_ready_tasks_from_the_plan() {
        let (mut state, _dir) = temp_state();
        let runner = FakeRunner::default();
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "scaffold", "run": ["run"]}]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        let root = PathBuf::from("/repo");
        let results = execute_plan_tasks(&profile, &plan, &ctx(&root, &runner), &mut state, true)
            .expect("execute");
        assert_eq!(
            results,
            vec![("scaffold".to_owned(), TaskOutcome::Ran(true))]
        );
    }

    fn down_test_profile() -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "panes": [{"name": "gitlog", "serve": [["lazygit"]], "on_stop": ["notify-stop"]}]
                }]
            }]
        }))
    }

    fn seed_managed(state: &mut LocalState, entries: &[(&str, &str, Option<&str>)]) {
        let profile = state.profile_mut("default");
        for (id, kind, parent) in entries {
            profile.resources.insert(
                (*id).to_owned(),
                ManagedResource {
                    kind: (*kind).to_owned(),
                    backend_id: format!("backend-{id}"),
                    parent: parent.map(str::to_owned),
                    digest: "any-digest".into(),
                    adopted: None,
                    last_outcome: None,
                },
            );
        }
        state.save().expect("save seeded state");
    }

    #[test]
    fn down_runs_on_stop_before_detaching_and_leaves_unmanaged_alone() {
        let (mut state, _dir) = temp_state();
        let profile = down_test_profile();
        seed_managed(
            &mut state,
            &[
                ("dev", "workspace", None),
                ("dev/main", "tab", Some("dev")),
                ("gitlog", "pane", Some("dev/main")),
            ],
        );

        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");
        let report = down(
            &profile,
            &ctx(&root, &runner),
            &mut state,
            true,
            false,
            None,
        )
        .expect("down");

        // Reverse dependency order: pane before tab before workspace.
        assert_eq!(report.detached, vec!["gitlog", "dev/main", "dev"]);
        assert_eq!(report.hooks_run, vec![("gitlog".to_owned(), true)]);
        assert_eq!(runner.calls(), vec![vec!["notify-stop".to_owned()]]);

        let managed = state.profile("default").expect("profile recorded");
        assert!(
            managed.resources.is_empty(),
            "every managed resource must be detached"
        );
    }

    #[test]
    fn down_never_touches_a_resource_it_never_managed() {
        let (mut state, _dir) = temp_state();
        let profile = down_test_profile();
        // Nothing seeded: an unmanaged pane the backend might report is
        // simply absent from local state, so `down` has nothing to iterate.
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");
        let report = down(
            &profile,
            &ctx(&root, &runner),
            &mut state,
            true,
            false,
            None,
        )
        .expect("down");
        assert!(report.detached.is_empty());
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn down_blocks_on_stop_hook_without_approval() {
        let (mut state, _dir) = temp_state();
        let profile = down_test_profile();
        seed_managed(&mut state, &[("gitlog", "pane", Some("dev/main"))]);
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");
        let report = down(
            &profile,
            &ctx(&root, &runner),
            &mut state,
            false,
            false,
            None,
        )
        .expect("down");
        assert!(runner.calls().is_empty(), "blocked hook must not run");
        // The resource is still detached even though its hook was blocked:
        // down's job is to stop tracking it, not to force approval.
        assert_eq!(report.detached, vec!["gitlog"]);
        assert!(report.hooks_run.is_empty());
    }
}
