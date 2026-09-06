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
    backend::{Backend, PaneSpec},
    ir::{Ir, Resource},
    model::{Profile, Task, canonical_digest},
    planner::{Action, CoreAction, HerdrAction, Plan, PlannedAction},
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
    if let Some(check) = &task.check {
        // A check that cannot even start (missing executable, permission
        // denied) answers "not satisfied" rather than aborting `run_task`
        // outright: the task should still get a chance to run.
        let satisfied = ctx
            .runner
            .run(check, ctx.repo_root, &BTreeMap::new())
            .unwrap_or(false);
        if satisfied {
            record_task_resource(state, ctx, &task.name, resource_digest, "skipped", true)?;
            return Ok(TaskOutcome::Skipped);
        }
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
        // The hook's own outcome is intentionally not folded into this
        // task's `TaskOutcome`: it already gets its own approval gate and
        // journal entry (same as `down`'s hooks), but a wrapped task ran
        // (or didn't) independently of whether its post-run notification
        // succeeded.
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

    // D18: only a successful `run` converges the task. Recording the
    // declared digest on failure would make the very next `build_plan` see
    // this task as in sync, so a failing `run` would never be retried.
    record_task_resource(
        state,
        ctx,
        &task.name,
        resource_digest,
        if success { "ok" } else { "failed" },
        success,
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

/// Records the task's last outcome unconditionally, but only records
/// `digest` as its *observed* digest when `converged` is true. On a failed
/// `run`, `converged` is false, so the previously recorded digest (or none,
/// if this is the task's first run) is kept: the task stays out of sync and
/// `build_plan` proposes it again on the next `drove up`/`plan`/`status`.
fn record_task_resource(
    state: &mut LocalState,
    ctx: &ExecutionContext<'_>,
    name: &str,
    digest: &str,
    outcome: &str,
    converged: bool,
) -> Result<()> {
    let profile = state.profile_mut(ctx.profile);
    let observed_digest = if converged {
        digest.to_owned()
    } else {
        profile
            .resources
            .get(name)
            .map(|resource| resource.digest.clone())
            .unwrap_or_default()
    };
    profile.resources.insert(
        name.to_owned(),
        ManagedResource {
            kind: "task".into(),
            backend_id: String::new(),
            parent: None,
            digest: observed_digest,
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
        if action.kind != Action::Core(CoreAction::RunTask) {
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
            // A blocked (unapproved) or failed `on_stop` does not stop the
            // teardown below: `down`'s job is to stop tracking a resource,
            // not to hold it hostage to hook approval. A hook that must run
            // before teardown (e.g. one that kills a background process)
            // needs its digest pre-approved, the same way a task's `run`
            // does.
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
        for group in &workspace.tabs {
            for pane in &group.panes {
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

/// A resource's identity for the shared namespace (D5): its own declared
/// name. Placement groups are not core resources (D29), so they never appear
/// here.
fn identity(resource: &Resource) -> String {
    resource.name.clone()
}

/// Dependent -> its dependencies: a resource's structural parent (a pane
/// depends on its placement group, an agent on its pane) plus whatever it
/// names in a declared `after`.
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

/// What became of one planned action when applied to a backend (D29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The verb ran against the backend.
    Applied,
    /// The action is recorded/handled outside the backend apply loop (a task
    /// run, a detach, a task conflict report).
    Skipped,
    /// A flavor action whose flavor this backend does not implement. `drove
    /// plan` prints it and `drove status` counts it; it is never dropped.
    Unsupported {
        flavor: &'static str,
        action: Action,
    },
}

/// Running backend ids gathered while a plan applies: creating a workspace,
/// group or pane yields the id later actions address.
#[derive(Default)]
struct ApplyState {
    workspace_ids: BTreeMap<String, String>,
    group_ids: BTreeMap<String, String>,
    pane_ids: BTreeMap<String, String>,
}

/// Applies every action in `plan` against `backend`, resolving each verb's
/// concrete arguments from `ir`, and returns each action's [`Outcome`] in
/// order. A flavor action on a backend without that flavor is surfaced as
/// [`Outcome::Unsupported`] and the loop continues, so core resources in the
/// same plan are still created (D29).
pub fn apply_plan(backend: &dyn Backend, ir: &Ir, plan: &Plan) -> Result<Vec<(String, Outcome)>> {
    let mut state = ApplyState::default();
    let mut outcomes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let outcome = apply_action(backend, ir, &mut state, action)?;
        outcomes.push((action.address.clone(), outcome));
    }
    Ok(outcomes)
}

/// Routes one planned action to the backend through a single exhaustive
/// match (D29). A `Herdr(..)`/`Radiator(..)` action whose accessor returns
/// `None` yields [`Outcome::Unsupported`] without touching the backend.
fn apply_action(
    backend: &dyn Backend,
    ir: &Ir,
    state: &mut ApplyState,
    action: &PlannedAction,
) -> Result<Outcome> {
    match action.kind {
        Action::Core(core) => apply_core(backend, ir, state, core, action),
        Action::Herdr(herdr) => {
            let Some(ext) = backend.herdr() else {
                return Ok(Outcome::Unsupported {
                    flavor: "herdr",
                    action: action.kind,
                });
            };
            apply_herdr(ext, ir, state, herdr, action)
        }
        // `RadiatorAction` is empty (spec §8, D37); this arm keeps the match
        // exhaustive so adding a variant forces every backend to answer it.
        Action::Radiator(radiator) => match radiator {},
    }
}

fn apply_core(
    backend: &dyn Backend,
    ir: &Ir,
    state: &mut ApplyState,
    core: CoreAction,
    action: &PlannedAction,
) -> Result<Outcome> {
    match core {
        CoreAction::CreateWorkspace => {
            let fields = resource_fields(ir, "workspace", &action.address)?;
            let label = string_field(fields, "label").unwrap_or_else(|| action.address.clone());
            let cwd = string_field(fields, "cwd").unwrap_or_else(|| ".".to_owned());
            let id = backend.create_workspace(&label, Path::new(&cwd))?;
            state.workspace_ids.insert(action.address.clone(), id);
            Ok(Outcome::Applied)
        }
        CoreAction::RenameWorkspace => {
            let id = backend_id(state.workspace_ids.get(&action.address), action)?;
            let fields = resource_fields(ir, "workspace", &action.address)?;
            let label = string_field(fields, "label").unwrap_or_else(|| action.address.clone());
            backend.rename_workspace(&id, &label)?;
            Ok(Outcome::Applied)
        }
        CoreAction::CreatePane => {
            let (workspace_id, spec) = pane_create_inputs(ir, state, &action.address)?;
            let pane_id = backend.create_pane(&workspace_id, &spec)?;
            state.pane_ids.insert(action.address.clone(), pane_id);
            Ok(Outcome::Applied)
        }
        CoreAction::ClosePane => {
            let id = backend_id(action.backend_id.as_ref(), action)?;
            backend.close_pane(&id)?;
            Ok(Outcome::Applied)
        }
        CoreAction::RenamePane => {
            let id = backend_id(action.backend_id.as_ref(), action)?;
            let fields = resource_fields(ir, "pane", &action.address)?;
            let label = string_field(fields, "label").unwrap_or_else(|| action.address.clone());
            backend.rename_pane(&id, &label)?;
            Ok(Outcome::Applied)
        }
        CoreAction::RestartCommand => {
            let id = backend_id(action.backend_id.as_ref(), action)?;
            let argv = pane_command(ir, &action.address);
            backend.restart_command(&id, &argv)?;
            Ok(Outcome::Applied)
        }
        CoreAction::PromptAgent => {
            let id = backend_id(action.backend_id.as_ref(), action)?;
            let fields = resource_fields(ir, "agent", &action.address)?;
            if let Some(prompt) = string_field(fields, "prompt") {
                backend.prompt_agent(&id, &prompt)?;
            }
            Ok(Outcome::Applied)
        }
        // Adoption records ownership of the caller pane (D24); it needs no
        // backend verb. Detach, RunTask and Conflict are handled outside the
        // backend apply loop (local state, `execute_plan_tasks`, reporting).
        CoreAction::AdoptPane | CoreAction::Detach | CoreAction::RunTask | CoreAction::Conflict => {
            Ok(Outcome::Skipped)
        }
    }
}

fn apply_herdr(
    ext: &dyn crate::backend::HerdrExt,
    ir: &Ir,
    state: &mut ApplyState,
    herdr: HerdrAction,
    action: &PlannedAction,
) -> Result<Outcome> {
    match herdr {
        HerdrAction::CreateTab => {
            let group = placement_group(ir, &action.address)?;
            let workspace_id = backend_id(state.workspace_ids.get(&group.workspace), action)?;
            let tab_id = ext.create_tab(&workspace_id, &group.label, group.split, &group.ratios)?;
            state.group_ids.insert(action.address.clone(), tab_id);
            Ok(Outcome::Applied)
        }
        HerdrAction::RenameTab => {
            let group = placement_group(ir, &action.address)?;
            let tab_id = backend_id(state.group_ids.get(&action.address), action)?;
            ext.rename_tab(&tab_id, &group.label)?;
            Ok(Outcome::Applied)
        }
        HerdrAction::SetRatio => {
            let group = placement_group(ir, &action.address)?;
            let tab_id = backend_id(state.group_ids.get(&action.address), action)?;
            ext.set_ratio(&tab_id, &group.ratios)?;
            Ok(Outcome::Applied)
        }
        HerdrAction::SplitPane => {
            let (_workspace_id, spec) = pane_create_inputs(ir, state, &action.address)?;
            let group_id = pane_group_id(ir, &action.address)?;
            let tab_id = backend_id(state.group_ids.get(&group_id), action)?;
            let group = placement_group(ir, &group_id)?;
            let pane_id = ext.split_pane(&tab_id, &spec, group.split)?;
            state.pane_ids.insert(action.address.clone(), pane_id);
            Ok(Outcome::Applied)
        }
        HerdrAction::StartAgent => {
            let fields = resource_fields(ir, "agent", &action.address)?;
            let pane = agent_parent(ir, &action.address)?;
            let pane_id = backend_id(state.pane_ids.get(&pane), action)?;
            let kind = string_field(fields, "kind").unwrap_or_default();
            let args = string_array(fields, "args");
            ext.start_agent(&pane_id, &action.address, &kind, &args)?;
            Ok(Outcome::Applied)
        }
    }
}

fn resource_fields<'a>(ir: &'a Ir, kind: &str, name: &str) -> Result<&'a serde_json::Value> {
    ir.resources
        .iter()
        .find(|resource| resource.kind == kind && resource.name == name)
        .map(|resource| &resource.fields)
        .ok_or_else(|| anyhow::anyhow!("no {kind} resource named `{name}` in the IR"))
}

fn placement_group<'a>(ir: &'a Ir, id: &str) -> Result<&'a crate::ir::PlacementGroup> {
    ir.placements
        .iter()
        .find(|group| group.id == id)
        .ok_or_else(|| anyhow::anyhow!("no placement group `{id}` in the IR"))
}

fn pane_group_id(ir: &Ir, pane: &str) -> Result<String> {
    ir.resources
        .iter()
        .find(|resource| resource.kind == "pane" && resource.name == pane)
        .and_then(|resource| resource.parent.clone())
        .ok_or_else(|| anyhow::anyhow!("pane `{pane}` has no placement group"))
}

fn agent_parent(ir: &Ir, agent: &str) -> Result<String> {
    ir.resources
        .iter()
        .find(|resource| resource.kind == "agent" && resource.name == agent)
        .and_then(|resource| resource.parent.clone())
        .ok_or_else(|| anyhow::anyhow!("agent `{agent}` has no pane"))
}

fn pane_create_inputs(ir: &Ir, state: &ApplyState, pane: &str) -> Result<(String, PaneSpec)> {
    let group_id = pane_group_id(ir, pane)?;
    let group = placement_group(ir, &group_id)?;
    let workspace_id = state
        .workspace_ids
        .get(&group.workspace)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("workspace `{}` has no backend id yet", group.workspace))?;
    let fields = resource_fields(ir, "pane", pane)?;
    let spec = PaneSpec {
        label: string_field(fields, "label").or_else(|| Some(pane.to_owned())),
        cwd: string_field(fields, "cwd").map(std::path::PathBuf::from),
        command: {
            let argv = pane_command(ir, pane);
            (!argv.is_empty()).then_some(argv)
        },
        env: string_map(fields, "env"),
    };
    Ok((workspace_id, spec))
}

/// The first `serve` candidate's argv (D8: `any_of` tries them in order; the
/// backend runs the first).
fn pane_command(ir: &Ir, pane: &str) -> Vec<String> {
    let Ok(fields) = resource_fields(ir, "pane", pane) else {
        return Vec::new();
    };
    fields
        .get("serve")
        .and_then(|v| v.as_array())
        .and_then(|candidates| candidates.first())
        .and_then(|v| v.as_array())
        .map(|argv| {
            argv.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn backend_id(id: Option<&String>, action: &PlannedAction) -> Result<String> {
    id.cloned()
        .ok_or_else(|| anyhow::anyhow!("no backend id for `{}` yet", action.address))
}

fn string_field(fields: &serde_json::Value, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

fn string_array(fields: &serde_json::Value, key: &str) -> Vec<String> {
    fields
        .get(key)
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn string_map(fields: &serde_json::Value, key: &str) -> BTreeMap<String, String> {
    fields
        .get(key)
        .and_then(|v| v.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default()
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
    fn a_failed_run_is_not_recorded_as_converged() {
        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "scaffold", "run": ["run"]}]
        }));
        let digest = digest_of(&profile.to_ir(), "task", "scaffold")
            .expect("scaffold resource")
            .to_owned();
        let runner = FakeRunner::default().fail(&["run"]);
        let root = PathBuf::from("/repo");
        let outcome = run_task(
            &profile.tasks[0],
            &digest,
            &ctx(&root, &runner),
            &mut state,
            true,
        )
        .expect("run_task");
        assert_eq!(outcome, TaskOutcome::Ran(false));

        // D18: a failed `run` must not look converged to the next
        // `build_plan` — recording `scaffold`'s real IR digest as observed
        // here would be a false convergence.
        let snapshot = state
            .profile("default")
            .expect("profile recorded")
            .to_snapshot("default", None);
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions
                .iter()
                .any(|action| action.address == "scaffold"),
            "a failed task must still be proposed to run again: {plan:?}"
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

        // Reverse dependency order: pane before workspace. The placement
        // group is derived, not a managed resource (D29), so it is not torn
        // down on its own.
        assert_eq!(report.detached, vec!["gitlog", "dev"]);
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

    /// A backend with no Herdr flavor: `herdr()` is `None`, so every
    /// `Herdr(..)` action must come back `Unsupported`. It records the panes
    /// it is asked to create so the test can prove core actions still run.
    #[derive(Default)]
    struct FlavorlessBackend {
        created_panes: Mutex<Vec<String>>,
    }

    impl Backend for FlavorlessBackend {
        fn snapshot(&self) -> Result<crate::backend::herdr::SessionSnapshot> {
            Ok(Default::default())
        }
        fn caller_pane_id(&self) -> Option<String> {
            None
        }
        fn create_workspace(&self, _label: &str, _cwd: &Path) -> Result<String> {
            Ok("w1".into())
        }
        fn rename_workspace(&self, _id: &str, _label: &str) -> Result<()> {
            Ok(())
        }
        fn create_pane(&self, _workspace_id: &str, spec: &PaneSpec) -> Result<String> {
            let name = spec.label.clone().unwrap_or_default();
            self.created_panes.lock().expect("mutex").push(name);
            Ok("p1".into())
        }
        fn close_pane(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        fn rename_pane(&self, _id: &str, _label: &str) -> Result<()> {
            Ok(())
        }
        fn restart_command(&self, _id: &str, _argv: &[String]) -> Result<()> {
            Ok(())
        }
        fn prompt_agent(&self, _id: &str, _prompt: &str) -> Result<()> {
            Ok(())
        }
        fn process_info(&self, _id: &str) -> Result<Option<crate::backend::ProcessInfo>> {
            Ok(None)
        }
        fn report_tokens(&self, _address: &str, _tokens: &BTreeMap<String, String>) -> Result<()> {
            Ok(())
        }
        fn output(&self, _id: &str, _timeout: std::time::Duration) -> Result<String> {
            Ok(String::new())
        }
        fn capabilities(&self) -> crate::backend::Capabilities {
            crate::backend::Capabilities {
                workspace_env: false,
                pane_command_at_create: true,
                metadata_tokens: false,
                process_info: false,
                events: false,
                readiness_output: false,
            }
        }
        // No `herdr()` override: it inherits the default `None`.
    }

    #[test]
    fn herdr_action_on_a_flavorless_backend_is_unsupported_but_core_panes_still_run() {
        use crate::planner::{Action, CoreAction, HerdrAction, PlannedAction, SyncStatus};

        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "editor", "serve": [["bash"]]}]}]
            }]
        }));
        let ir = profile.to_ir();

        let action = |kind, address: &str| PlannedAction {
            kind,
            address: address.to_owned(),
            backend_id: None,
            destructive: false,
            reason: String::new(),
        };
        let plan = Plan {
            profile: "default".into(),
            desired_digest: String::new(),
            status: SyncStatus::OutOfSync,
            adopted: BTreeMap::new(),
            actions: vec![
                action(Action::Core(CoreAction::CreateWorkspace), "dev"),
                action(Action::Herdr(HerdrAction::CreateTab), "dev/main"),
                action(Action::Core(CoreAction::CreatePane), "editor"),
            ],
        };

        let backend = FlavorlessBackend::default();
        let outcomes = apply_plan(&backend, &ir, &plan).expect("apply");

        assert_eq!(outcomes[0].1, Outcome::Applied);
        assert_eq!(
            outcomes[1].1,
            Outcome::Unsupported {
                flavor: "herdr",
                action: Action::Herdr(HerdrAction::CreateTab),
            },
            "a Herdr action on a backend without the flavor must be Unsupported"
        );
        assert_eq!(outcomes[2].1, Outcome::Applied);
        assert_eq!(
            *backend.created_panes.lock().expect("mutex"),
            vec!["editor".to_owned()],
            "the core pane must still be created despite the unsupported Herdr action"
        );
    }
}
