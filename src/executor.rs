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
    backend::{Backend, PaneSpec, SessionState},
    ir::{Ir, Resource},
    model::{Profile, Task, canonical_digest},
    planner::{Action, CoreAction, HerdrAction, Plan, PlannedAction},
    state::{LocalState, ManagedProfile, ManagedResource},
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
    /// `(resource identity, error message)` for every `close_pane` call that
    /// failed (D50): the resource is still detached and the failure does not
    /// abort the teardown, so a pane the session already lost is treated as
    /// already gone rather than blocking `down`.
    pub close_failed: Vec<(String, String)>,
}

/// `drove down` (D19): runs each owned resource's `on_stop` hook (if the
/// profile still declares one), then detaches it — with `purge`, also
/// closes owned panes on the backend — in reverse dependency order.
/// Resources the backend doesn't recognize as owned by this profile (an
/// unmanaged pane) are never touched, because they are never in
/// `state`'s managed set to begin with. A `close_pane` failure (D50) no
/// longer aborts the loop: the resource is still detached and saved, and
/// the failure is collected in [`DownReport::close_failed`] instead.
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
            && let Err(error) = backend.close_pane(&resource.backend_id)
        {
            report.close_failed.push((id.clone(), error.to_string()));
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

/// One backend call inside an apply loop that failed; collected instead of
/// aborting so independent actions still apply and dependents can be skipped
/// (D52 point 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAction {
    pub address: String,
    pub error: String,
}

/// One action left unattempted because an action it structurally depends on
/// (its placement group, its workspace, its pane) failed or was itself
/// skipped earlier in the same apply (D52 point 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedAction {
    pub address: String,
    pub depends_on: String,
}

/// What became of one planned action when applied to a backend (D29, D52).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The verb ran against the backend.
    Applied,
    /// The action is recorded/handled outside the backend apply loop (a task
    /// run, a detach, a task conflict report), or held back for want of
    /// `--yes` (a destructive action, D22).
    Skipped,
    /// A flavor action whose flavor this backend does not implement. `drove
    /// plan` prints it and `drove status` counts it; it is never dropped.
    Unsupported {
        flavor: &'static str,
        action: Action,
    },
    /// The backend call for this action failed; the error is also collected
    /// into the apply's `failed` list. The loop does not abort (D52 point 2).
    Failed(String),
    /// Not attempted: it depends on an action that failed or was itself
    /// skipped this run (D52 point 2).
    DependencySkipped { depends_on: String },
}

/// Running backend ids gathered while a plan applies: creating a workspace,
/// group or pane yields the id later actions address.
#[derive(Default)]
struct ApplyState {
    workspace_ids: BTreeMap<String, String>,
    group_ids: BTreeMap<String, String>,
    pane_ids: BTreeMap<String, String>,
}

impl ApplyState {
    /// Seeds the resolver with the backend ids a previous run recorded, so an
    /// action against a parent that already converged (and so needs no action
    /// this run) still resolves that parent's id. Without this, a later `up`
    /// that only adds a pane to an existing group cannot find the group's
    /// workspace, since nothing populated `workspace_ids` for it this run.
    fn seeded_from(managed: Option<&crate::state::ManagedProfile>) -> Self {
        let mut state = Self::default();
        let Some(managed) = managed else {
            return state;
        };
        for (address, resource) in &managed.resources {
            if resource.backend_id.is_empty() {
                continue;
            }
            let map = match resource.kind.as_str() {
                "workspace" => &mut state.workspace_ids,
                "placement" => &mut state.group_ids,
                "pane" => &mut state.pane_ids,
                _ => continue,
            };
            map.insert(address.clone(), resource.backend_id.clone());
        }
        state
    }
}

/// Every id this action's target structurally depends on, nearest first: a
/// pane's placement group and that group's workspace, a group's workspace, an
/// agent's pane and that pane's group and workspace. Reuses the same
/// parent relations [`record_action_ownership`] already reads off the IR
/// (D52 point 2) instead of building a new graph.
fn action_dependencies(ir: &Ir, action: &PlannedAction) -> Vec<String> {
    match action.kind {
        Action::Herdr(HerdrAction::CreateTab | HerdrAction::RenameTab | HerdrAction::SetRatio) => {
            group_workspace(ir, &action.address)
                .map(|workspace| vec![workspace.to_owned()])
                .unwrap_or_default()
        }
        Action::Core(
            CoreAction::CreatePane | CoreAction::RenamePane | CoreAction::RestartCommand,
        )
        | Action::Herdr(HerdrAction::SplitPane) => {
            let Ok(group) = pane_group_id(ir, &action.address) else {
                return Vec::new();
            };
            let mut chain = vec![group.clone()];
            if let Some(workspace) = group_workspace(ir, &group) {
                chain.push(workspace.to_owned());
            }
            chain
        }
        Action::Herdr(HerdrAction::StartAgent) | Action::Core(CoreAction::PromptAgent) => {
            let Ok(pane) = agent_parent(ir, &action.address) else {
                return Vec::new();
            };
            let mut chain = vec![pane.clone()];
            if let Ok(group) = pane_group_id(ir, &pane) {
                chain.push(group.clone());
                if let Some(workspace) = group_workspace(ir, &group) {
                    chain.push(workspace.to_owned());
                }
            }
            chain
        }
        _ => Vec::new(),
    }
}

/// Applies every action in `plan` against `backend`, resolving each verb's
/// concrete arguments from `ir`, and returns each action's [`Outcome`] in
/// order. A flavor action on a backend without that flavor is surfaced as
/// [`Outcome::Unsupported`] and the loop continues, so core resources in the
/// same plan are still created (D29). Destructive actions are always applied;
/// [`up`] uses `apply_plan_gated` instead to hold them behind `--yes`.
pub fn apply_plan(backend: &dyn Backend, ir: &Ir, plan: &Plan) -> Vec<(String, Outcome)> {
    apply_plan_gated(backend, ir, plan, true, ApplyState::default(), |_, _, _| {
        true
    })
    .0
}

/// Like [`apply_plan`], but when `approve` is false every destructive action
/// (a topology-change `ClosePane`, D22) is left unapplied and reported as
/// [`Outcome::Skipped`] — the same `--yes` gate a task's `run` sits behind.
///
/// A failing action's error is collected into the returned `failed` list
/// instead of aborting the loop (D52 point 2): every other independent
/// action still applies. An action that depends on one that failed, or was
/// itself skipped this run, is reported as [`Outcome::DependencySkipped`] and
/// collected into `skipped` rather than attempted. `on_action` runs with each
/// action's outcome and the backend ids accumulated so far, right before the
/// next action starts — a caller that saves state there (`up` does) leaves a
/// killed process describing exactly what succeeded (D52 point 1). If
/// `on_action` returns `false` (its own save failed), the loop stops before
/// the next action so nothing more is applied without a record of it.
/// Every action's outcome, the backend ids created along the way, and what
/// failed or was skipped — [`apply_plan_gated`]'s result.
type ApplyPlanResult = (
    Vec<(String, Outcome)>,
    ApplyState,
    Vec<FailedAction>,
    Vec<SkippedAction>,
);

fn apply_plan_gated(
    backend: &dyn Backend,
    ir: &Ir,
    plan: &Plan,
    approve: bool,
    seed: ApplyState,
    mut on_action: impl FnMut(&PlannedAction, &Outcome, &ApplyState) -> bool,
) -> ApplyPlanResult {
    let mut state = seed;
    let mut outcomes: Vec<Option<(String, Outcome)>> =
        (0..plan.actions.len()).map(|_| None).collect();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    let mut blocked: BTreeSet<String> = BTreeSet::new();

    // A `SetRatio` addresses a split gap, so it must run after the panes that
    // create the group's gaps. The plan orders every Herdr tab action ahead of
    // the pane splits (its rank sorts before the pane rank), which is right for
    // `plan`/`status` output but would apply a ratio before its gap exists when
    // a pane is added to an existing group. So apply the ratios last, keeping
    // each action's outcome in its original plan position.
    let is_deferred =
        |action: &PlannedAction| matches!(action.kind, Action::Herdr(HerdrAction::SetRatio));
    let order = plan
        .actions
        .iter()
        .enumerate()
        .filter(|(_, action)| !is_deferred(action))
        .chain(
            plan.actions
                .iter()
                .enumerate()
                .filter(|(_, action)| is_deferred(action)),
        );

    for (index, action) in order {
        let blocking_dependency = action_dependencies(ir, action)
            .into_iter()
            .find(|id| blocked.contains(id));
        let outcome = if action.destructive && !approve {
            Outcome::Skipped
        } else if let Some(depends_on) = blocking_dependency {
            skipped.push(SkippedAction {
                address: action.address.clone(),
                depends_on: depends_on.clone(),
            });
            Outcome::DependencySkipped { depends_on }
        } else {
            match apply_action(backend, ir, &mut state, action) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let message = error.to_string();
                    failed.push(FailedAction {
                        address: action.address.clone(),
                        error: message.clone(),
                    });
                    Outcome::Failed(message)
                }
            }
        };
        if matches!(
            outcome,
            Outcome::Failed(_) | Outcome::DependencySkipped { .. }
        ) {
            blocked.insert(action.address.clone());
        }
        let keep_going = on_action(action, &outcome, &state);
        outcomes[index] = Some((action.address.clone(), outcome));
        if !keep_going {
            break;
        }
    }

    // Ordinarily every slot is filled (`order` visits every action), but
    // `on_action` returning `false` stops the loop before the rest run, so
    // trailing entries stay `None` rather than lying about an outcome they
    // never got.
    let outcomes = outcomes.into_iter().flatten().collect();
    (outcomes, state, failed, skipped)
}

/// What `drove up` did, reported as one summary line (D43 step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpOutcome {
    /// The session was reachable (or just started headlessly) and the plan's
    /// tasks and backend actions were applied.
    Reconciled {
        created: usize,
        changed: usize,
        tasks_run: usize,
    },
    /// Nothing was out of sync; the workspace was only brought to the front.
    AlreadyRunning,
    /// The Herdr session's server was not reachable and could not be started
    /// headlessly (D43 step 2); `hint` is the command to run by hand.
    CannotStart { hint: String },
}

/// The full result of one [`up`] run.
#[derive(Debug, Clone)]
pub struct UpReport {
    pub outcome: UpOutcome,
    /// Per-task run results, for `--json` output and the process exit code.
    pub tasks: Vec<(String, TaskOutcome)>,
    /// The backend id of the workspace brought to the front, if any.
    pub focused: Option<String>,
    /// A destructive action was left unapplied for want of `--yes` (D22).
    pub blocked_destructive: bool,
    /// Backend actions whose call failed (D52 point 2); `up` exits 1 when
    /// this is non-empty.
    pub failed: Vec<FailedAction>,
    /// Actions left unattempted because an action they depend on failed or
    /// was itself skipped this run (D52 point 2).
    pub skipped: Vec<SkippedAction>,
}

/// `drove up` end to end (D43): ensure the session is reachable, run the
/// plan's tasks, apply its backend actions behind the `--yes` gate, record
/// what was created, and bring the target workspace to the front. The caller
/// (`src/cli.rs`) is responsible for the `Conflict` early exit before calling
/// this, for printing the summary, and for the `exec herdr session attach`
/// step, which is not exercised here.
#[allow(clippy::too_many_arguments)]
pub fn up(
    backend: &dyn Backend,
    profile: &Profile,
    ir: &Ir,
    plan: &Plan,
    ctx: &ExecutionContext<'_>,
    state: &mut LocalState,
    approve: bool,
    session: &str,
    focus_workspace: Option<&str>,
    do_focus: bool,
) -> Result<UpReport> {
    // Step 2: make the session reachable. Herdr starts its own server
    // headlessly; a flavorless backend (Radiator) has no such verb, so the
    // caller checks its reachability separately (D43 step 2, D44).
    if let Some(ext) = backend.herdr()
        && let SessionState::CannotStart { hint } = ext.ensure_session(session)?
    {
        return Ok(UpReport {
            outcome: UpOutcome::CannotStart { hint },
            tasks: Vec::new(),
            focused: None,
            blocked_destructive: false,
            failed: Vec::new(),
            skipped: Vec::new(),
        });
    }

    let was_in_sync = plan.actions.is_empty();

    // Step 3: run the plan's tasks, then apply its backend actions, recording
    // ownership and saving state right after each action succeeds (D52
    // point 1): a killed process leaves state describing exactly what was
    // created, instead of discarding it the way a single end-of-loop
    // `record_ownership` pass would.
    let tasks = execute_plan_tasks(profile, plan, ctx, state, approve)?;
    // Seed the resolver with what a previous run recorded, so an action
    // against a parent that already converged still finds its backend id.
    let seed = ApplyState::seeded_from(state.profile(ctx.profile));
    let existing = state.profile(ctx.profile).cloned().unwrap_or_default();
    let mut save_error: Option<anyhow::Error> = None;
    let (outcomes, applied, failed, skipped) = apply_plan_gated(
        backend,
        ir,
        plan,
        approve,
        seed,
        |action, outcome, applied| {
            let managed = state.profile_mut(ctx.profile);
            let changed = record_action_ownership(managed, &existing, ir, action, outcome, applied);
            if changed && let Err(error) = state.save() {
                save_error = Some(error);
                return false;
            }
            true
        },
    );
    if let Some(error) = save_error {
        return Err(error);
    }

    let blocked_destructive = !approve && plan.has_destructive_actions();
    let (created, changed) = count_applied(plan, &outcomes);
    let tasks_run = tasks
        .iter()
        .filter(|(_, outcome)| matches!(outcome, TaskOutcome::Ran(_)))
        .count();

    // Step 4: bring the target workspace to the front.
    let focused = if do_focus {
        focus_first_workspace(backend, state, ctx.profile, &applied, focus_workspace)?
    } else {
        None
    };

    let outcome = if was_in_sync {
        UpOutcome::AlreadyRunning
    } else {
        UpOutcome::Reconciled {
            created,
            changed,
            tasks_run,
        }
    };
    Ok(UpReport {
        outcome,
        tasks,
        focused,
        blocked_destructive,
        failed,
        skipped,
    })
}

/// Splits the plan's applied actions into a created count and a changed count
/// for the summary line. Only [`Outcome::Applied`] actions count; a skipped,
/// unsupported, task, detach, or conflict action does not.
fn count_applied(plan: &Plan, outcomes: &[(String, Outcome)]) -> (usize, usize) {
    let mut created = 0;
    let mut changed = 0;
    for (action, (_, outcome)) in plan.actions.iter().zip(outcomes) {
        if *outcome != Outcome::Applied {
            continue;
        }
        match action.kind {
            Action::Core(CoreAction::CreateWorkspace | CoreAction::CreatePane)
            | Action::Herdr(
                HerdrAction::CreateTab | HerdrAction::SplitPane | HerdrAction::StartAgent,
            ) => created += 1,
            Action::Core(
                CoreAction::RenameWorkspace
                | CoreAction::RenamePane
                | CoreAction::RestartCommand
                | CoreAction::ClosePane
                | CoreAction::PromptAgent,
            )
            | Action::Herdr(HerdrAction::RenameTab | HerdrAction::SetRatio) => changed += 1,
            _ => {}
        }
    }
    (created, changed)
}

/// Brings the profile's target workspace to the front through
/// `workspace.focus` (D43 step 4). The workspace's backend id comes from what
/// this run just created, else from what a previous run recorded in local
/// state (the already-in-sync case). A flavorless backend has no
/// `focus_workspace` verb, so this is a no-op there.
fn focus_first_workspace(
    backend: &dyn Backend,
    state: &LocalState,
    profile: &str,
    applied: &ApplyState,
    workspace: Option<&str>,
) -> Result<Option<String>> {
    let Some(name) = workspace else {
        return Ok(None);
    };
    let Some(ext) = backend.herdr() else {
        return Ok(None);
    };
    let backend_id = applied
        .workspace_ids
        .get(name)
        .cloned()
        .or_else(|| {
            state
                .profile(profile)
                .and_then(|managed| managed.resources.get(name))
                .map(|resource| resource.backend_id.clone())
        })
        .filter(|id| !id.is_empty());
    let Some(backend_id) = backend_id else {
        return Ok(None);
    };
    ext.focus_workspace(&backend_id)?;
    Ok(Some(backend_id))
}

/// Records one applied action's ownership into `managed.resources` — the
/// per-action split of the old whole-plan pass (D52 point 1), called right
/// after each action's outcome is known so a save right after leaves state
/// describing exactly what succeeded. The backend id comes from what this
/// action's own apply created (`applied`), else the action's own already-known
/// backend id (a rename or restart of an already-known resource), else what
/// local state already held (`existing`, a snapshot from before this apply
/// started). A `Detach` drops the resource; an `AdoptPane` records the caller
/// pane even though no backend verb ran (D24) — both regardless of `outcome`,
/// since neither ever touches the backend. Returns whether `managed` actually
/// changed, so the caller only pays for a `state.save()` when there is
/// something to save.
fn record_action_ownership(
    managed: &mut ManagedProfile,
    existing: &ManagedProfile,
    ir: &Ir,
    action: &PlannedAction,
    outcome: &Outcome,
    applied: &ApplyState,
) -> bool {
    let resolve = |name: &str, ids: &BTreeMap<String, String>, own: Option<&str>| -> String {
        ids.get(name)
            .cloned()
            .or_else(|| own.map(str::to_owned))
            .or_else(|| {
                existing
                    .resources
                    .get(name)
                    .map(|resource| resource.backend_id.clone())
            })
            .unwrap_or_default()
    };

    let address = action.address.as_str();
    match action.kind {
        Action::Core(CoreAction::AdoptPane) => {
            let (Ok(digest), Ok(parent)) =
                (digest_of(ir, "pane", address), pane_group_id(ir, address))
            else {
                return false;
            };
            let backend_id = resolve(address, &applied.pane_ids, action.backend_id.as_deref());
            managed.resources.insert(
                address.to_owned(),
                ManagedResource {
                    kind: "pane".into(),
                    backend_id,
                    parent: Some(parent),
                    digest: digest.to_owned(),
                    adopted: Some(true),
                    last_outcome: None,
                },
            );
            return true;
        }
        Action::Core(CoreAction::Detach) => {
            return managed.resources.remove(address).is_some();
        }
        _ => {}
    }

    if *outcome != Outcome::Applied {
        return false;
    }

    match action.kind {
        Action::Core(CoreAction::CreateWorkspace | CoreAction::RenameWorkspace) => {
            let Ok(digest) = digest_of(ir, "workspace", address) else {
                return false;
            };
            let backend_id = resolve(
                address,
                &applied.workspace_ids,
                action.backend_id.as_deref(),
            );
            managed.resources.insert(
                address.to_owned(),
                ManagedResource {
                    kind: "workspace".into(),
                    backend_id,
                    parent: None,
                    digest: digest.to_owned(),
                    adopted: None,
                    last_outcome: None,
                },
            );
            true
        }
        Action::Herdr(HerdrAction::CreateTab | HerdrAction::RenameTab | HerdrAction::SetRatio) => {
            let mut changed = false;
            if let (Some(digest), Some(workspace)) = (
                group_topology_digest(ir, address),
                group_workspace(ir, address),
            ) {
                let backend_id = resolve(address, &applied.group_ids, action.backend_id.as_deref());
                managed.resources.insert(
                    address.to_owned(),
                    ManagedResource {
                        kind: "placement".into(),
                        backend_id,
                        parent: Some(workspace.to_owned()),
                        digest: digest.to_owned(),
                        adopted: None,
                        last_outcome: None,
                    },
                );
                changed = true;
            }
            // A fresh `CreateTab` builds every pane in the group with no
            // per-pane action, so record each one here (its parent is the
            // group) — otherwise the next run would see them unobserved and
            // split them in again.
            if action.kind == Action::Herdr(HerdrAction::CreateTab)
                && let Ok(group) = placement_group(ir, address)
            {
                for pane in &group.panes {
                    if let Ok(digest) = digest_of(ir, "pane", pane) {
                        let backend_id = resolve(pane, &applied.pane_ids, None);
                        managed.resources.insert(
                            pane.clone(),
                            ManagedResource {
                                kind: "pane".into(),
                                backend_id,
                                parent: Some(address.to_owned()),
                                digest: digest.to_owned(),
                                adopted: None,
                                last_outcome: None,
                            },
                        );
                        changed = true;
                    }
                }
            }
            changed
        }
        Action::Core(
            CoreAction::CreatePane | CoreAction::RenamePane | CoreAction::RestartCommand,
        )
        | Action::Herdr(HerdrAction::SplitPane) => {
            let (Ok(digest), Ok(parent)) =
                (digest_of(ir, "pane", address), pane_group_id(ir, address))
            else {
                return false;
            };
            let backend_id = resolve(address, &applied.pane_ids, action.backend_id.as_deref());
            let adopted = existing
                .resources
                .get(address)
                .and_then(|resource| resource.adopted);
            managed.resources.insert(
                address.to_owned(),
                ManagedResource {
                    kind: "pane".into(),
                    backend_id,
                    parent: Some(parent),
                    digest: digest.to_owned(),
                    adopted,
                    last_outcome: None,
                },
            );
            true
        }
        Action::Herdr(HerdrAction::StartAgent) | Action::Core(CoreAction::PromptAgent) => {
            let (Ok(digest), Ok(pane)) =
                (digest_of(ir, "agent", address), agent_parent(ir, address))
            else {
                return false;
            };
            let backend_id = resolve(&pane, &applied.pane_ids, action.backend_id.as_deref());
            managed.resources.insert(
                address.to_owned(),
                ManagedResource {
                    kind: "agent".into(),
                    backend_id,
                    parent: Some(pane),
                    digest: digest.to_owned(),
                    adopted: None,
                    last_outcome: None,
                },
            );
            true
        }
        // A topology-change `ClosePane` is immediately followed by a
        // `SplitPane` in the same plan that re-records the pane under its new
        // group, so there is nothing to remove here.
        _ => false,
    }
}

fn group_topology_digest<'a>(ir: &'a Ir, id: &str) -> Option<&'a str> {
    ir.placements
        .iter()
        .find(|group| group.id == id)
        .map(|group| group.topology_digest.as_str())
}

fn group_workspace<'a>(ir: &'a Ir, id: &str) -> Option<&'a str> {
    ir.placements
        .iter()
        .find(|group| group.id == id)
        .map(|group| group.workspace.as_str())
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
            // A fresh group plans one `CreateTab` and no per-pane splits, so
            // this builds the whole tab: every declared pane, then the ratios.
            let specs = group
                .panes
                .iter()
                .map(|pane| pane_spec(ir, pane))
                .collect::<Result<Vec<_>>>()?;
            // The root Herdr tab id is only ever present for a workspace
            // this same apply created, and only until the first `CreateTab`
            // for it consumes it (D49) — an adopted or pre-existing
            // workspace never has one.
            let existing_tab = ext.take_root_tab(&workspace_id);
            let layout = ext.create_tab(
                &workspace_id,
                &group.label,
                group.split,
                &group.ratios,
                &specs,
                existing_tab.as_deref(),
            )?;
            state
                .group_ids
                .insert(action.address.clone(), layout.tab_id);
            for (pane, pane_id) in group.panes.iter().zip(layout.pane_ids) {
                state.pane_ids.insert(pane.clone(), pane_id);
            }
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
    Ok((workspace_id, pane_spec(ir, pane)?))
}

/// The [`PaneSpec`] for one declared pane: its label, cwd, command and env,
/// independent of any workspace backend id (used when building the panes of a
/// fresh Herdr tab up front, before their splits run).
fn pane_spec(ir: &Ir, pane: &str) -> Result<PaneSpec> {
    let fields = resource_fields(ir, "pane", pane)?;
    Ok(PaneSpec {
        label: string_field(fields, "label").or_else(|| Some(pane.to_owned())),
        cwd: string_field(fields, "cwd").map(std::path::PathBuf::from),
        command: {
            let argv = pane_command(ir, pane);
            (!argv.is_empty()).then_some(argv)
        },
        env: string_map(fields, "env"),
    })
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
    use std::{fs, path::PathBuf, sync::Mutex};

    use anyhow::bail;
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

    /// A backend whose `close_pane` fails for one chosen backend id and
    /// succeeds for every other (D50), so a `down --purge` test can prove a
    /// lost pane no longer aborts the teardown.
    struct CloseFailsBackend {
        fails_for: &'static str,
    }

    impl Backend for CloseFailsBackend {
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
        fn create_pane(&self, _workspace_id: &str, _spec: &PaneSpec) -> Result<String> {
            Ok("p1".into())
        }
        fn close_pane(&self, id: &str) -> Result<()> {
            if id == self.fails_for {
                bail!("pane_not_found: {id}");
            }
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
    }

    #[test]
    fn down_collects_a_close_pane_failure_without_aborting_the_teardown() {
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
        let backend = CloseFailsBackend {
            fails_for: "backend-gitlog",
        };
        let report = down(
            &profile,
            &ctx(&root, &runner),
            &mut state,
            true,
            true,
            Some(&backend),
        )
        .expect("down");

        assert_eq!(report.detached, vec!["gitlog", "dev"]);
        assert_eq!(report.close_failed.len(), 1);
        let (id, message) = &report.close_failed[0];
        assert_eq!(id, "gitlog");
        assert!(message.contains("pane_not_found"));

        let managed = state.profile("default").expect("profile recorded");
        assert!(
            managed.resources.is_empty(),
            "every managed resource must still be detached and saved"
        );
    }

    /// A fake Herdr for the `up` flow (D43): it hands out backend ids for
    /// every workspace, Herdr tab and pane it is asked to create, records the
    /// verbs it receives, and answers `ensure_session` with a state the test
    /// sets.
    struct RecordingHerdr {
        session: SessionState,
        calls: Mutex<Vec<String>>,
        next_id: Mutex<u32>,
        /// When set, every workspace this fake creates is given a root
        /// Herdr tab id (`<workspace>-root`), simulating what a real
        /// `workspace.create` always returns alongside it (D49).
        auto_root_tab: bool,
        root_tabs: Mutex<BTreeMap<String, String>>,
        /// Calls (matched against the same string [`RecordingHerdr::record`]
        /// logs) that fail instead of succeeding, so a test can prove `up`
        /// collects one action's error and keeps applying the rest (D52).
        fail_calls: BTreeSet<&'static str>,
    }

    impl RecordingHerdr {
        fn running() -> Self {
            Self {
                session: SessionState::Running,
                calls: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
                auto_root_tab: false,
                root_tabs: Mutex::new(BTreeMap::new()),
                fail_calls: BTreeSet::new(),
            }
        }

        /// Like [`RecordingHerdr::running`], but the given calls fail
        /// instead of succeeding (D52). A failing `create_workspace`/
        /// `create_tab` still records the call and consumes an id, matching
        /// what recording-then-failing looks like against a real backend.
        fn running_failing(fail_calls: &[&'static str]) -> Self {
            Self {
                fail_calls: fail_calls.iter().copied().collect(),
                ..Self::running()
            }
        }

        /// Like [`RecordingHerdr::running`], but simulates Herdr's own
        /// behavior of always returning a root Herdr tab alongside a
        /// freshly created workspace (D49).
        fn running_with_root_tabs() -> Self {
            Self {
                auto_root_tab: true,
                ..Self::running()
            }
        }

        fn cannot_start(hint: &str) -> Self {
            Self {
                session: SessionState::CannotStart { hint: hint.into() },
                calls: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
                auto_root_tab: false,
                root_tabs: Mutex::new(BTreeMap::new()),
                fail_calls: BTreeSet::new(),
            }
        }

        fn id(&self, prefix: &str) -> String {
            let mut next = self.next_id.lock().expect("id lock");
            let id = format!("{prefix}{next}");
            *next += 1;
            id
        }

        fn record(&self, call: String) {
            self.calls.lock().expect("calls lock").push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl Backend for RecordingHerdr {
        fn snapshot(&self) -> Result<crate::backend::herdr::SessionSnapshot> {
            Ok(Default::default())
        }
        fn caller_pane_id(&self) -> Option<String> {
            None
        }
        fn create_workspace(&self, label: &str, _cwd: &Path) -> Result<String> {
            let call = format!("create_workspace:{label}");
            self.record(call.clone());
            if self.fail_calls.contains(call.as_str()) {
                bail!("boom: {call}");
            }
            let workspace_id = self.id("w");
            if self.auto_root_tab {
                self.root_tabs
                    .lock()
                    .expect("root tabs lock")
                    .insert(workspace_id.clone(), format!("{workspace_id}-root"));
            }
            Ok(workspace_id)
        }
        fn rename_workspace(&self, id: &str, label: &str) -> Result<()> {
            self.record(format!("rename_workspace:{id}:{label}"));
            Ok(())
        }
        fn create_pane(&self, workspace_id: &str, spec: &PaneSpec) -> Result<String> {
            let label = spec.label.clone().unwrap_or_default();
            self.record(format!("create_pane:{workspace_id}:{label}"));
            Ok(self.id("p"))
        }
        fn close_pane(&self, id: &str) -> Result<()> {
            self.record(format!("close_pane:{id}"));
            Ok(())
        }
        fn rename_pane(&self, id: &str, label: &str) -> Result<()> {
            self.record(format!("rename_pane:{id}:{label}"));
            Ok(())
        }
        fn restart_command(&self, id: &str, _argv: &[String]) -> Result<()> {
            self.record(format!("restart_command:{id}"));
            Ok(())
        }
        fn prompt_agent(&self, id: &str, _prompt: &str) -> Result<()> {
            self.record(format!("prompt_agent:{id}"));
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
                workspace_env: true,
                pane_command_at_create: true,
                metadata_tokens: true,
                process_info: true,
                events: true,
                readiness_output: true,
            }
        }
        fn herdr(&self) -> Option<&dyn crate::backend::HerdrExt> {
            Some(self)
        }
    }

    impl crate::backend::HerdrExt for RecordingHerdr {
        fn create_tab(
            &self,
            workspace_id: &str,
            label: &str,
            _split: crate::backend::Split,
            ratios: &[f64],
            panes: &[PaneSpec],
            existing_tab: Option<&str>,
        ) -> Result<crate::backend::TabLayout> {
            self.record(format!(
                "create_tab:{workspace_id}:{label}:existing={existing_tab:?}"
            ));
            let fail_key = format!("create_tab:{label}");
            if self.fail_calls.contains(fail_key.as_str()) {
                bail!("boom: {fail_key}");
            }
            let tab_id = existing_tab.map_or_else(|| self.id("t"), ToOwned::to_owned);
            let pane_ids = panes
                .iter()
                .map(|spec| {
                    let pane_label = spec.label.clone().unwrap_or_default();
                    self.record(format!("tab_pane:{tab_id}:{pane_label}"));
                    self.id("p")
                })
                .collect();
            if !ratios.is_empty() {
                self.record(format!("set_ratio:{tab_id}"));
            }
            Ok(crate::backend::TabLayout { tab_id, pane_ids })
        }

        fn take_root_tab(&self, workspace_id: &str) -> Option<String> {
            self.root_tabs
                .lock()
                .expect("root tabs lock")
                .remove(workspace_id)
        }
        fn split_pane(
            &self,
            tab_id: &str,
            spec: &PaneSpec,
            _split: crate::backend::Split,
        ) -> Result<String> {
            let label = spec.label.clone().unwrap_or_default();
            self.record(format!("split_pane:{tab_id}:{label}"));
            Ok(self.id("p"))
        }
        fn set_ratio(&self, tab_id: &str, _ratios: &[f64]) -> Result<()> {
            self.record(format!("set_ratio:{tab_id}"));
            Ok(())
        }
        fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()> {
            self.record(format!("rename_tab:{tab_id}:{label}"));
            Ok(())
        }
        fn start_agent(
            &self,
            pane_id: &str,
            name: &str,
            _kind: &str,
            _args: &[String],
        ) -> Result<()> {
            self.record(format!("start_agent:{pane_id}:{name}"));
            Ok(())
        }
        fn focus_workspace(&self, id: &str) -> Result<()> {
            self.record(format!("focus:{id}"));
            Ok(())
        }
        fn ensure_session(&self, _name: &str) -> Result<SessionState> {
            Ok(self.session.clone())
        }
        fn stop_session(&self, name: &str) -> Result<crate::backend::SessionStop> {
            self.record(format!("stop_session:{name}"));
            Ok(crate::backend::SessionStop {
                stopped: true,
                deleted: true,
            })
        }
    }

    fn up_profile() -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "editor", "serve": [["bash"]]}]}]
            }]
        }))
    }

    fn up_plan(kinds: &[(Action, &str)]) -> Plan {
        use crate::planner::SyncStatus;
        Plan {
            profile: "default".into(),
            desired_digest: String::new(),
            status: SyncStatus::OutOfSync,
            adopted: BTreeMap::new(),
            actions: kinds
                .iter()
                .map(|(kind, address)| PlannedAction {
                    kind: *kind,
                    address: (*address).to_owned(),
                    backend_id: None,
                    destructive: false,
                    reason: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn up_applies_workspace_and_pane_then_focuses_the_first_workspace() {
        let (mut state, _dir) = temp_state();
        let profile = up_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Core(CoreAction::CreatePane), "editor"),
        ]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c == "create_workspace:dev"),
            "workspace must be created: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("create_pane:")),
            "pane must be created: {calls:?}"
        );
        // The first workspace is brought to the front with the id its create
        // returned.
        assert_eq!(report.focused.as_deref(), Some("w1"));
        assert!(
            calls.iter().any(|c| c == "focus:w1"),
            "the first workspace must be focused: {calls:?}"
        );
        assert_eq!(
            report.outcome,
            UpOutcome::Reconciled {
                created: 2,
                changed: 0,
                tasks_run: 0
            }
        );
        // Ownership is recorded so the next run sees the resources in sync.
        assert!(
            state
                .profile("default")
                .expect("profile recorded")
                .resources
                .contains_key("dev")
        );
    }

    #[test]
    fn up_with_no_focus_applies_but_never_focuses() {
        let (mut state, _dir) = temp_state();
        let profile = up_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Core(CoreAction::CreatePane), "editor"),
        ]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            false,
        )
        .expect("up");

        assert_eq!(report.focused, None);
        let calls = backend.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("focus:")),
            "--no-focus must not focus anything: {calls:?}"
        );
    }

    #[test]
    fn up_already_in_sync_brings_the_workspace_to_the_front() {
        let (mut state, _dir) = temp_state();
        let profile = up_profile();
        let ir = profile.to_ir();
        // A previous run recorded the workspace's backend id; nothing is out
        // of sync now.
        state.profile_mut("default").resources.insert(
            "dev".to_owned(),
            ManagedResource {
                kind: "workspace".into(),
                backend_id: "w1".into(),
                parent: None,
                digest: "any".into(),
                adopted: None,
                last_outcome: None,
            },
        );
        let plan = up_plan(&[]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        assert_eq!(report.outcome, UpOutcome::AlreadyRunning);
        assert_eq!(report.focused.as_deref(), Some("w1"));
        let calls = backend.calls();
        assert_eq!(calls, vec!["focus:w1".to_owned()], "only focus, no creates");
    }

    #[test]
    fn up_reports_the_hint_when_the_session_cannot_be_started() {
        let (mut state, _dir) = temp_state();
        let profile = up_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[(Action::Core(CoreAction::CreateWorkspace), "dev")]);
        let backend = RecordingHerdr::cannot_start("herdr --session dev-session");
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        assert_eq!(
            report.outcome,
            UpOutcome::CannotStart {
                hint: "herdr --session dev-session".to_owned()
            }
        );
        assert!(
            backend.calls().is_empty(),
            "an unstartable session applies nothing and focuses nothing"
        );
    }

    #[test]
    fn up_builds_a_fresh_multi_pane_tab_and_records_every_pane() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "split": "right",
                    "ratios": [0.67],
                    "panes": [{"name": "editor"}, {"name": "tests"}],
                }]
            }]
        }));
        let ir = profile.to_ir();
        // A fresh group plans one CreateTab and no per-pane splits.
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Herdr(HerdrAction::CreateTab), "dev/main"),
        ]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        let calls = backend.calls();
        // Both declared panes are built as part of the one CreateTab, and the
        // ratio is applied once (the executor never emits a bare set_ratio on
        // a one-pane Herdr tab).
        assert!(
            calls.iter().any(|c| c.starts_with("create_tab:")),
            "the Herdr tab must be created: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("tab_pane:")).count(),
            2,
            "both panes must be built into the tab: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("set_ratio:")),
            "the ratio must be applied: {calls:?}"
        );

        // The group and each pane are recorded, so a second run sees them
        // owned instead of splitting them in again.
        let managed = state.profile("default").expect("profile recorded");
        assert!(managed.resources.contains_key("dev/main"), "group recorded");
        assert!(
            managed.resources.contains_key("editor"),
            "first pane recorded"
        );
        assert!(
            managed.resources.contains_key("tests"),
            "second pane recorded"
        );
        assert_eq!(
            report.outcome,
            UpOutcome::Reconciled {
                created: 2,
                changed: 0,
                tasks_run: 0
            }
        );
    }

    /// Two independent workspaces, each with one Herdr tab and one pane — the
    /// shape D52's repro needs: three create-style backend calls
    /// (`CreateWorkspace(dev)`, `CreateWorkspace(ops)`, `CreateTab(dev/main)`)
    /// where the second can be made to fail without touching the third.
    fn two_workspace_profile() -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [
                {"name": "dev", "tabs": [{"name": "main", "panes": [{"name": "editor"}]}]},
                {"name": "ops", "tabs": [{"name": "main", "panes": [{"name": "shell"}]}]},
            ]
        }))
    }

    #[test]
    fn up_records_the_first_action_and_still_applies_an_independent_third_past_a_failed_second() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = two_workspace_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Core(CoreAction::CreateWorkspace), "ops"),
            (Action::Herdr(HerdrAction::CreateTab), "dev/main"),
        ]);
        let backend = RecordingHerdr::running_failing(&["create_workspace:ops"]);
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            None,
            false,
        )
        .expect("up does not abort on a single failed action");

        assert_eq!(
            report.failed.len(),
            1,
            "the second action must be reported failed"
        );
        assert_eq!(report.failed[0].address, "ops");
        assert!(report.failed[0].error.contains("boom"));
        assert!(
            report.skipped.is_empty(),
            "the third action does not depend on `ops`, so nothing is skipped: {:?}",
            report.skipped
        );

        // The independent third action still ran, despite the second one
        // failing (D52 point 2, audit finding 1).
        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("create_tab:")),
            "the independent CreateTab must still apply: {calls:?}"
        );

        // The first action's ownership was recorded (D52 point 1): a killed
        // process after the failure would leave `dev` and `dev/main`
        // describing exactly what succeeded.
        let managed = state.profile("default").expect("profile recorded");
        assert!(managed.resources.contains_key("dev"), "dev recorded");
        assert!(
            managed.resources.contains_key("dev/main"),
            "dev/main recorded"
        );
        assert!(managed.resources.contains_key("editor"), "editor recorded");
        assert!(
            !managed.resources.contains_key("ops"),
            "the failed workspace must not be recorded"
        );
    }

    #[test]
    fn up_skips_an_action_that_depends_on_one_that_failed() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = two_workspace_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "ops"),
            (Action::Herdr(HerdrAction::CreateTab), "ops/main"),
        ]);
        let backend = RecordingHerdr::running_failing(&["create_workspace:ops"]);
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            None,
            false,
        )
        .expect("up");

        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].address, "ops");
        assert_eq!(
            report.skipped.len(),
            1,
            "the dependent Herdr tab must be skipped"
        );
        assert_eq!(report.skipped[0].address, "ops/main");
        assert_eq!(report.skipped[0].depends_on, "ops");

        // The skipped action never touched the backend at all.
        let calls = backend.calls();
        assert!(
            !calls.iter().any(|c| c.starts_with("create_tab:")),
            "a dependency-skipped action must never call the backend: {calls:?}"
        );
        assert!(
            state
                .profile("default")
                .map(|managed| managed.resources.is_empty())
                .unwrap_or(true),
            "neither the failed nor the skipped resource is recorded"
        );
    }

    #[test]
    fn a_rerun_after_a_partial_failure_plans_only_what_is_still_missing() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = two_workspace_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Core(CoreAction::CreateWorkspace), "ops"),
            (Action::Herdr(HerdrAction::CreateTab), "dev/main"),
        ]);
        let backend = RecordingHerdr::running_failing(&["create_workspace:ops"]);
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            None,
            false,
        )
        .expect("up");

        // The next `build_plan` sees `dev` and its Herdr tab as owned and
        // converged, and proposes only what actually failed last time.
        let managed = state.profile("default").cloned().unwrap_or_default();
        let snapshot = managed.to_snapshot("default", None);
        let rerun = build_plan(&profile, &snapshot).expect("plan");

        let addresses: Vec<&str> = rerun
            .actions
            .iter()
            .map(|action| action.address.as_str())
            .collect();
        assert_eq!(
            addresses,
            vec!["ops", "ops/main"],
            "only the failed workspace (and what depends on it) is replanned: {addresses:?}"
        );
    }

    #[test]
    fn up_stops_applying_further_actions_once_a_state_save_fails() {
        let (mut state, dir) = temp_state();
        // Force every `state.save()` to fail: its parent directory component
        // is actually a plain file, so `fs::create_dir_all` cannot create it.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"not a directory").expect("write blocker file");
        state.path = blocker.join("state.json");

        let profile = two_workspace_profile();
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Core(CoreAction::CreateWorkspace), "ops"),
        ]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let result = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            None,
            false,
        );

        assert!(
            result.is_err(),
            "a state save failure must surface as an error, not a silent partial apply"
        );

        // The first action's backend call happened (its outcome had to be
        // known before the save that failed), but the loop must have stopped
        // there instead of going on to apply `ops` without any record of
        // `dev` or a chance to record `ops` either.
        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c == "create_workspace:dev"),
            "the first action still applies before the save fails: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| c == "create_workspace:ops"),
            "no action after the save failure may reach the backend: {calls:?}"
        );
    }

    #[test]
    fn up_reuses_the_freshly_created_workspaces_root_tab_for_its_first_tab_only() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [
                    {"name": "main", "panes": [{"name": "editor"}]},
                    {"name": "second", "panes": [{"name": "logs"}]},
                ]
            }]
        }));
        let ir = profile.to_ir();
        let plan = up_plan(&[
            (Action::Core(CoreAction::CreateWorkspace), "dev"),
            (Action::Herdr(HerdrAction::CreateTab), "dev/main"),
            (Action::Herdr(HerdrAction::CreateTab), "dev/second"),
        ]);
        let backend = RecordingHerdr::running_with_root_tabs();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        let calls = backend.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("create_tab:")
                && c.contains(":main:")
                && c.contains("existing=Some")),
            "the workspace's own root tab must be reused for its first declared tab: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.starts_with("create_tab:")
                && c.contains(":second:")
                && c.contains("existing=None")),
            "only the first declared tab may reuse the root tab: {calls:?}"
        );
    }

    #[test]
    fn up_never_reuses_a_root_tab_for_an_adopted_or_pre_existing_workspace() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "editor"}]}]
            }]
        }));
        let ir = profile.to_ir();
        // The workspace already exists from a previous run, so this plan has
        // no `CreateWorkspace` action for it — only the Herdr tab is being
        // added.
        {
            let managed = state.profile_mut("default");
            managed.resources.insert(
                "dev".to_owned(),
                ManagedResource {
                    kind: "workspace".into(),
                    backend_id: "w1".into(),
                    parent: None,
                    digest: "d".into(),
                    adopted: None,
                    last_outcome: None,
                },
            );
        }
        let plan = up_plan(&[(Action::Herdr(HerdrAction::CreateTab), "dev/main")]);
        let backend = RecordingHerdr::running_with_root_tabs();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        let calls = backend.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("create_tab:") && c.contains("existing=None")),
            "an adopted or pre-existing workspace must never reuse a root tab: {calls:?}"
        );
    }

    #[test]
    fn up_splits_a_pane_into_a_converged_group_using_recorded_ids() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "editor"}, {"name": "tests"}]}]
            }]
        }));
        let ir = profile.to_ir();
        // A previous run recorded the workspace, the group and the first pane;
        // only `tests` is being added now, so its parents need no action this
        // run and their ids live only in recorded state.
        {
            let managed = state.profile_mut("default");
            for (address, kind, backend, parent) in [
                ("dev", "workspace", "w1", None),
                ("dev/main", "placement", "t1", Some("dev")),
                ("editor", "pane", "p1", Some("dev/main")),
            ] {
                managed.resources.insert(
                    address.to_owned(),
                    ManagedResource {
                        kind: kind.into(),
                        backend_id: backend.into(),
                        parent: parent.map(ToOwned::to_owned),
                        digest: "d".into(),
                        adopted: None,
                        last_outcome: None,
                    },
                );
            }
        }
        let plan = up_plan(&[(Action::Herdr(HerdrAction::SplitPane), "tests")]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        let report = up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up must resolve the converged workspace from recorded state");

        let calls = backend.calls();
        // The new pane splits into the group's recorded Herdr tab, and no
        // error is raised for the workspace that was never touched this run.
        assert!(
            calls.iter().any(|c| c.starts_with("split_pane:t1:")),
            "the pane must split into the recorded tab: {calls:?}"
        );
        assert!(
            state
                .profile("default")
                .expect("profile")
                .resources
                .contains_key("tests"),
            "the new pane is recorded"
        );
        assert!(matches!(report.outcome, UpOutcome::Reconciled { .. }));
    }

    #[test]
    fn set_ratio_is_applied_after_the_split_that_creates_its_gap() {
        use crate::planner::HerdrAction;

        let (mut state, _dir) = temp_state();
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "split": "right",
                    "ratios": [0.6],
                    "panes": [{"name": "editor"}, {"name": "tests"}]
                }]
            }]
        }));
        let ir = profile.to_ir();
        {
            let managed = state.profile_mut("default");
            for (address, kind, backend, parent) in [
                ("dev", "workspace", "w1", None),
                ("dev/main", "placement", "t1", Some("dev")),
                ("editor", "pane", "p1", Some("dev/main")),
            ] {
                managed.resources.insert(
                    address.to_owned(),
                    ManagedResource {
                        kind: kind.into(),
                        backend_id: backend.into(),
                        parent: parent.map(ToOwned::to_owned),
                        digest: "d".into(),
                        adopted: None,
                        last_outcome: None,
                    },
                );
            }
        }
        // The planner orders every Herdr tab action ahead of the pane splits,
        // so the ratio comes first in the plan — but applying it before the
        // split exists would fail against a real backend.
        let plan = up_plan(&[
            (Action::Herdr(HerdrAction::SetRatio), "dev/main"),
            (Action::Herdr(HerdrAction::SplitPane), "tests"),
        ]);
        let backend = RecordingHerdr::running();
        let runner = FakeRunner::default();
        let root = PathBuf::from("/repo");

        up(
            &backend,
            &profile,
            &ir,
            &plan,
            &ctx(&root, &runner),
            &mut state,
            true,
            "dev-session",
            Some("dev"),
            true,
        )
        .expect("up");

        let calls = backend.calls();
        let split_at = calls
            .iter()
            .position(|c| c.starts_with("split_pane:"))
            .expect("a split happened");
        let ratio_at = calls
            .iter()
            .position(|c| c.starts_with("set_ratio:"))
            .expect("a ratio was set");
        assert!(
            ratio_at > split_at,
            "the ratio must be applied after the split, whatever the plan order: {calls:?}"
        );
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
        let outcomes = apply_plan(&backend, &ir, &plan);

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
