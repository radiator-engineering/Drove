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
/// same plan are still created (D29). Destructive actions are always applied;
/// [`up`] uses [`apply_plan_gated`] instead to hold them behind `--yes`.
pub fn apply_plan(backend: &dyn Backend, ir: &Ir, plan: &Plan) -> Result<Vec<(String, Outcome)>> {
    Ok(apply_plan_gated(backend, ir, plan, true)?.0)
}

/// Like [`apply_plan`], but when `approve` is false every destructive action
/// (a topology-change `ClosePane`, D22) is left unapplied and reported as
/// [`Outcome::Skipped`] — the same `--yes` gate a task's `run` sits behind.
/// Returns the per-action outcomes together with the backend ids created
/// along the way, so a caller can record ownership from what actually ran.
fn apply_plan_gated(
    backend: &dyn Backend,
    ir: &Ir,
    plan: &Plan,
    approve: bool,
) -> Result<(Vec<(String, Outcome)>, ApplyState)> {
    let mut state = ApplyState::default();
    let mut outcomes = Vec::with_capacity(plan.actions.len());
    for action in &plan.actions {
        let outcome = if action.destructive && !approve {
            Outcome::Skipped
        } else {
            apply_action(backend, ir, &mut state, action)?
        };
        outcomes.push((action.address.clone(), outcome));
    }
    Ok((outcomes, state))
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
        });
    }

    let was_in_sync = plan.actions.is_empty();

    // Step 3: run the plan's tasks, then apply its backend actions and record
    // the resources that came into being so the next run sees them in sync.
    let tasks = execute_plan_tasks(profile, plan, ctx, state, approve)?;
    let (outcomes, applied) = apply_plan_gated(backend, ir, plan, approve)?;
    record_ownership(state, ctx.profile, ir, plan, &outcomes, &applied)?;

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

/// Records ownership of the resources a plan just applied, so the next
/// `build_plan` sees them as owned and converged. The backend id comes from
/// what this apply created, else the action's own backend id (a rename or
/// restart of an already-known resource), else what local state already held.
/// A `Detach` drops the resource; an `AdoptPane` records the caller pane even
/// though no backend verb ran (D24).
fn record_ownership(
    state: &mut LocalState,
    profile: &str,
    ir: &Ir,
    plan: &Plan,
    outcomes: &[(String, Outcome)],
    applied: &ApplyState,
) -> Result<()> {
    enum Change {
        Upsert(String, ManagedResource),
        Remove(String),
    }
    let mut changes: Vec<Change> = Vec::new();
    let existing = state.profile(profile).cloned().unwrap_or_default();

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

    for (action, (_, outcome)) in plan.actions.iter().zip(outcomes) {
        let address = action.address.as_str();
        match action.kind {
            Action::Core(CoreAction::AdoptPane) => {
                if let (Ok(digest), Ok(parent)) =
                    (digest_of(ir, "pane", address), pane_group_id(ir, address))
                {
                    let backend_id =
                        resolve(address, &applied.pane_ids, action.backend_id.as_deref());
                    changes.push(Change::Upsert(
                        address.to_owned(),
                        ManagedResource {
                            kind: "pane".into(),
                            backend_id,
                            parent: Some(parent),
                            digest: digest.to_owned(),
                            adopted: Some(true),
                            last_outcome: None,
                        },
                    ));
                }
                continue;
            }
            Action::Core(CoreAction::Detach) => {
                changes.push(Change::Remove(address.to_owned()));
                continue;
            }
            _ => {}
        }

        if *outcome != Outcome::Applied {
            continue;
        }

        match action.kind {
            Action::Core(CoreAction::CreateWorkspace | CoreAction::RenameWorkspace) => {
                if let Ok(digest) = digest_of(ir, "workspace", address) {
                    let backend_id = resolve(
                        address,
                        &applied.workspace_ids,
                        action.backend_id.as_deref(),
                    );
                    changes.push(Change::Upsert(
                        address.to_owned(),
                        ManagedResource {
                            kind: "workspace".into(),
                            backend_id,
                            parent: None,
                            digest: digest.to_owned(),
                            adopted: None,
                            last_outcome: None,
                        },
                    ));
                }
            }
            Action::Herdr(
                HerdrAction::CreateTab | HerdrAction::RenameTab | HerdrAction::SetRatio,
            ) => {
                if let (Some(digest), Some(workspace)) = (
                    group_topology_digest(ir, address),
                    group_workspace(ir, address),
                ) {
                    let backend_id =
                        resolve(address, &applied.group_ids, action.backend_id.as_deref());
                    changes.push(Change::Upsert(
                        address.to_owned(),
                        ManagedResource {
                            kind: "placement".into(),
                            backend_id,
                            parent: Some(workspace.to_owned()),
                            digest: digest.to_owned(),
                            adopted: None,
                            last_outcome: None,
                        },
                    ));
                }
            }
            Action::Core(
                CoreAction::CreatePane | CoreAction::RenamePane | CoreAction::RestartCommand,
            )
            | Action::Herdr(HerdrAction::SplitPane) => {
                if let (Ok(digest), Ok(parent)) =
                    (digest_of(ir, "pane", address), pane_group_id(ir, address))
                {
                    let backend_id =
                        resolve(address, &applied.pane_ids, action.backend_id.as_deref());
                    let adopted = existing
                        .resources
                        .get(address)
                        .and_then(|resource| resource.adopted);
                    changes.push(Change::Upsert(
                        address.to_owned(),
                        ManagedResource {
                            kind: "pane".into(),
                            backend_id,
                            parent: Some(parent),
                            digest: digest.to_owned(),
                            adopted,
                            last_outcome: None,
                        },
                    ));
                }
            }
            Action::Herdr(HerdrAction::StartAgent) | Action::Core(CoreAction::PromptAgent) => {
                if let (Ok(digest), Ok(pane)) =
                    (digest_of(ir, "agent", address), agent_parent(ir, address))
                {
                    let backend_id =
                        resolve(&pane, &applied.pane_ids, action.backend_id.as_deref());
                    changes.push(Change::Upsert(
                        address.to_owned(),
                        ManagedResource {
                            kind: "agent".into(),
                            backend_id,
                            parent: Some(pane),
                            digest: digest.to_owned(),
                            adopted: None,
                            last_outcome: None,
                        },
                    ));
                }
            }
            // A topology-change `ClosePane` is immediately followed by a
            // `SplitPane` in the same plan that re-records the pane under its
            // new group, so there is nothing to remove here.
            _ => {}
        }
    }

    if changes.is_empty() {
        return Ok(());
    }
    let managed = state.profile_mut(profile);
    for change in changes {
        match change {
            Change::Upsert(id, resource) => {
                managed.resources.insert(id, resource);
            }
            Change::Remove(id) => {
                managed.resources.remove(&id);
            }
        }
    }
    state.save()
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

    /// A fake Herdr for the `up` flow (D43): it hands out backend ids for
    /// every workspace, Herdr tab and pane it is asked to create, records the
    /// verbs it receives, and answers `ensure_session` with a state the test
    /// sets.
    struct RecordingHerdr {
        session: SessionState,
        calls: Mutex<Vec<String>>,
        next_id: Mutex<u32>,
    }

    impl RecordingHerdr {
        fn running() -> Self {
            Self {
                session: SessionState::Running,
                calls: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }
        }

        fn cannot_start(hint: &str) -> Self {
            Self {
                session: SessionState::CannotStart { hint: hint.into() },
                calls: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
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
            self.record(format!("create_workspace:{label}"));
            Ok(self.id("w"))
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
            _ratios: &[f64],
        ) -> Result<String> {
            self.record(format!("create_tab:{workspace_id}:{label}"));
            Ok(self.id("t"))
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
