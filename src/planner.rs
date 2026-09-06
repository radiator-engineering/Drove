//! Plans the reconciliation actions needed to converge a `Profile`'s
//! compiled IR against a backend-observed [`Snapshot`] (spec §5, D5, D9,
//! D16, D17, D21, D22).
//!
//! Ownership is decided by comparing each resource's declared identity
//! (`drove_name`/`drove_profile`) and content digest (`drove_digest`,
//! D21) against what the snapshot reports for that identity:
//!
//! - no observed entry: the resource is declared but absent -> create it.
//! - observed, no owner token: unmanaged -> never touched.
//! - observed, owned by this profile, same digest: converged -> no action.
//! - observed, owned by this profile, different digest: a content change
//!   (`RestartCommand`/`RenamePane`/`RenameWorkspace`/...) unless the
//!   resource's structural parent moved, which is a topology change and is
//!   handled as a destructive `ClosePane` followed by a fresh `SplitPane`
//!   (D22).
//! - owned by this profile, no longer declared: `Detach` (leave it running,
//!   stop tracking it; only `drove down` closes owned resources).
//!
//! A resource's identity is its own declared name (D5) except a tab, whose
//! identity is `workspace/tab` (its scope per the model in spec §3); this is
//! deliberately not the same as [`crate::ir::Resource::parent`]-chained
//! address `to_ir` builds for content-digest aggregation.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ir::{Ir, Resource},
    model::Profile,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    InSync,
    OutOfSync,
}

/// What the backend currently reports for one resource identity. Real
/// backends read ownership tokens back (D16); until that lands (PR 3),
/// callers build this from [`crate::state::LocalState`], which is the
/// declared fallback in the same discovery order.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The pane id of the invoking terminal, if any (`Backend::caller_pane_id`).
    pub caller_pane_id: Option<String>,
    pub resources: BTreeMap<String, Observed>,
}

#[derive(Debug, Clone)]
pub struct Observed {
    /// IR resource kind (`workspace`, `tab`, `pane`, `agent`, `task`),
    /// needed only to order a `Detach` action for a resource that is no
    /// longer declared at all.
    pub kind: String,
    pub backend_id: String,
    /// The identity of this resource's current structural parent, used to
    /// detect a topology change (a pane moved to a different tab).
    pub parent: Option<String>,
    pub owner: Option<Owner>,
}

/// The ownership tokens `drove_name` (the map key), `drove_profile` and
/// `drove_digest` (D21), as read back from the backend or the local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub profile: String,
    pub digest: String,
}

impl Snapshot {
    pub fn with_caller(mut self, pane_id: &str) -> Self {
        self.caller_pane_id = Some(pane_id.to_owned());
        self
    }

    pub fn owned(
        mut self,
        kind: &str,
        identity: &str,
        backend_id: &str,
        parent: Option<&str>,
        profile: &str,
        digest: &str,
    ) -> Self {
        self.resources.insert(
            identity.to_owned(),
            Observed {
                kind: kind.to_owned(),
                backend_id: backend_id.to_owned(),
                parent: parent.map(str::to_owned),
                owner: Some(Owner {
                    profile: profile.to_owned(),
                    digest: digest.to_owned(),
                }),
            },
        );
        self
    }

    pub fn unmanaged(
        mut self,
        kind: &str,
        identity: &str,
        backend_id: &str,
        parent: Option<&str>,
    ) -> Self {
        self.resources.insert(
            identity.to_owned(),
            Observed {
                kind: kind.to_owned(),
                backend_id: backend_id.to_owned(),
                parent: parent.map(str::to_owned),
                owner: None,
            },
        );
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub profile: String,
    pub desired_digest: String,
    pub status: SyncStatus,
    /// Whether each `adopt = "caller"` pane (by pane name) was actually
    /// adopted, or created normally because no caller pane was found (D24).
    #[serde(default)]
    pub adopted: BTreeMap<String, bool>,
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn has_destructive_actions(&self) -> bool {
        self.actions.iter().any(|action| action.destructive)
    }

    /// The stable text rendering used by `drove plan`/`drove status`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.actions.is_empty() {
            out.push_str(&format!("in sync: profile `{}`\n", self.profile));
            return out;
        }
        out.push_str(&format!(
            "out of sync: profile `{}` ({} action{})\n",
            self.profile,
            self.actions.len(),
            if self.actions.len() == 1 { "" } else { "s" }
        ));
        for action in &self.actions {
            let warning = if action.destructive {
                " [destructive]"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {:?} {}{} — {}\n",
                action.kind, action.address, warning, action.reason
            ));
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    /// The target resource's IR identity (D5): a plain name, except a tab's
    /// `workspace/tab`.
    pub address: String,
    /// The backend id, when the resource (or, for a fresh create, its
    /// neighbour) is already known to the backend.
    pub backend_id: Option<String>,
    pub destructive: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    CreateWorkspace,
    RenameWorkspace,
    CreateTab,
    RenameTab,
    SplitPane,
    ClosePane,
    SetRatio,
    RenamePane,
    RestartCommand,
    AdoptPane,
    StartAgent,
    PromptAgent,
    RunTask,
    Detach,
    Conflict,
}

const RANK_WORKSPACE: u8 = 0;
const RANK_TAB: u8 = 1;
const RANK_PANE: u8 = 2;
const RANK_AGENT: u8 = 3;
const RANK_TASK: u8 = 4;
const RANK_UNKNOWN: u8 = 5;

// Destructive closes are ordered first so a topology-change pair (close the
// old placement, then split the new one) never runs its create half first;
// otherwise creates precede renames precede content updates (item 6).
const PHASE_CLOSE: u8 = 0;
const PHASE_CREATE: u8 = 1;
const PHASE_RENAME: u8 = 2;
const PHASE_UPDATE: u8 = 3;
const PHASE_DETACH: u8 = 4;
const PHASE_CONFLICT: u8 = 5;

type RankedAction = (u8, u8, String, Action);

/// A resource's identity for ownership purposes (D5): its own name, except
/// a tab, whose identity is scoped to its workspace.
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

fn effective_owner<'a>(observed: &'a Observed, profile: &str) -> Option<&'a Owner> {
    observed
        .owner
        .as_ref()
        .filter(|owner| owner.profile == profile)
}

pub fn build_plan(profile: &Profile, snapshot: &Snapshot) -> Result<Plan> {
    let ir = profile.to_ir();
    let mut ranked: Vec<RankedAction> = Vec::new();
    let mut adopted: BTreeMap<String, bool> = BTreeMap::new();
    let declared: BTreeSet<String> = ir.resources.iter().map(identity).collect();

    let mut tabs_with_adopt: BTreeSet<String> = BTreeSet::new();
    for resource in &ir.resources {
        if resource.kind == "pane"
            && resource.fields.get("adopt").and_then(Value::as_str) == Some("caller")
            && let Some(tab_id) = &resource.parent
        {
            tabs_with_adopt.insert(tab_id.clone());
        }
    }

    let mut tab_fresh: BTreeMap<String, bool> = BTreeMap::new();
    for resource in &ir.resources {
        match resource.kind.as_str() {
            "workspace" => plan_workspace(resource, profile, snapshot, &mut ranked),
            "tab" => {
                let id = identity(resource);
                let has_adopt_caller =
                    tabs_with_adopt.contains(&id) && snapshot.caller_pane_id.is_some();
                let fresh = plan_tab(resource, profile, snapshot, has_adopt_caller, &mut ranked);
                tab_fresh.insert(id, fresh);
            }
            "pane" => {
                let tab_id = resource.parent.clone().unwrap_or_default();
                let fresh = *tab_fresh.get(&tab_id).unwrap_or(&false);
                plan_pane(
                    resource,
                    profile,
                    snapshot,
                    fresh,
                    &mut ranked,
                    &mut adopted,
                );
            }
            "agent" => plan_agent(resource, profile, snapshot, &mut ranked),
            "task" => {}
            _ => {}
        }
    }

    plan_tasks(&ir, profile, snapshot, &mut ranked);
    plan_detach(&declared, profile, snapshot, &mut ranked);

    ranked.sort_by(|left, right| {
        (left.0, left.1, left.2.as_str()).cmp(&(right.0, right.1, right.2.as_str()))
    });
    let actions: Vec<Action> = ranked.into_iter().map(|(_, _, _, action)| action).collect();
    let status = if actions.is_empty() {
        SyncStatus::InSync
    } else {
        SyncStatus::OutOfSync
    };

    Ok(Plan {
        profile: profile.name.clone(),
        desired_digest: profile.digest()?,
        status,
        adopted,
        actions,
    })
}

fn plan_workspace(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    ranked: &mut Vec<RankedAction>,
) {
    let id = identity(resource);
    match snapshot.resources.get(&id) {
        None => ranked.push((
            RANK_WORKSPACE,
            PHASE_CREATE,
            id.clone(),
            Action {
                kind: ActionKind::CreateWorkspace,
                address: id,
                backend_id: None,
                destructive: false,
                reason: "workspace declared but not observed".into(),
            },
        )),
        Some(observed) => {
            if let Some(owner) = effective_owner(observed, &profile.name)
                && owner.digest != resource.digest
            {
                ranked.push((
                    RANK_WORKSPACE,
                    PHASE_RENAME,
                    id.clone(),
                    Action {
                        kind: ActionKind::RenameWorkspace,
                        address: id,
                        backend_id: Some(observed.backend_id.clone()),
                        destructive: false,
                        reason: "workspace label, cwd, or env changed".into(),
                    },
                ));
            }
        }
    }
}

/// Plans the tab itself. Returns whether the tab is freshly created this
/// plan, in which case its panes and agents are subsumed into the one
/// `CreateTab` layout application (item 4) rather than planned individually.
fn plan_tab(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    has_adopt_caller: bool,
    ranked: &mut Vec<RankedAction>,
) -> bool {
    let id = identity(resource);
    let observed = snapshot.resources.get(&id);

    // A tab holding an `adopt = "caller"` pane with a live caller already
    // exists in the backend around that pane, even on a first run where the
    // planner has no snapshot entry for it yet.
    let is_fresh = observed.is_none() && !has_adopt_caller;
    if is_fresh {
        ranked.push((
            RANK_TAB,
            PHASE_CREATE,
            id.clone(),
            Action {
                kind: ActionKind::CreateTab,
                address: id,
                backend_id: None,
                destructive: false,
                reason: "tab declared but not observed; applying as a new layout".into(),
            },
        ));
        return true;
    }

    if let Some(observed) = observed
        && let Some(owner) = effective_owner(observed, &profile.name)
        && owner.digest != resource.digest
    {
        let backend_id = Some(observed.backend_id.clone());
        ranked.push((
            RANK_TAB,
            PHASE_RENAME,
            id.clone(),
            Action {
                kind: ActionKind::RenameTab,
                address: id.clone(),
                backend_id: backend_id.clone(),
                destructive: false,
                reason: "tab label changed".into(),
            },
        ));
        ranked.push((
            RANK_TAB,
            PHASE_UPDATE,
            id.clone(),
            Action {
                kind: ActionKind::SetRatio,
                address: id,
                backend_id,
                destructive: false,
                reason: "tab layout ratios changed".into(),
            },
        ));
    }

    false
}

fn pane_serves(resource: &Resource) -> bool {
    resource
        .fields
        .get("serve")
        .and_then(Value::as_array)
        .map(|candidates| !candidates.is_empty())
        .unwrap_or(false)
}

fn plan_pane(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    tab_fresh: bool,
    ranked: &mut Vec<RankedAction>,
    adopted: &mut BTreeMap<String, bool>,
) {
    let id = identity(resource);
    let is_adopt = resource.fields.get("adopt").and_then(Value::as_str) == Some("caller");
    let serves = pane_serves(resource);
    let observed = snapshot.resources.get(&id);
    let owner = observed.and_then(|observed| effective_owner(observed, &profile.name));

    if is_adopt {
        match (&snapshot.caller_pane_id, owner) {
            (Some(caller_id), None) => {
                adopted.insert(resource.name.clone(), true);
                ranked.push((
                    RANK_PANE,
                    PHASE_CREATE,
                    id.clone(),
                    Action {
                        kind: ActionKind::AdoptPane,
                        address: id,
                        backend_id: Some(caller_id.clone()),
                        destructive: false,
                        reason: "adopting the invoking pane".into(),
                    },
                ));
            }
            (Some(_), Some(owner)) => {
                adopted.insert(resource.name.clone(), true);
                if owner.digest != resource.digest {
                    push_pane_content_change(resource, &id, observed, serves, ranked);
                }
            }
            (None, _) => {
                adopted.insert(resource.name.clone(), false);
                if !tab_fresh {
                    plan_normal_pane(resource, &id, profile, snapshot, serves, ranked);
                }
            }
        }
        return;
    }

    if tab_fresh {
        return;
    }
    plan_normal_pane(resource, &id, profile, snapshot, serves, ranked);
}

fn plan_normal_pane(
    resource: &Resource,
    id: &str,
    profile: &Profile,
    snapshot: &Snapshot,
    serves: bool,
    ranked: &mut Vec<RankedAction>,
) {
    let Some(observed) = snapshot.resources.get(id) else {
        ranked.push((
            RANK_PANE,
            PHASE_CREATE,
            id.to_owned(),
            Action {
                kind: ActionKind::SplitPane,
                address: id.to_owned(),
                backend_id: None,
                destructive: false,
                reason: "pane declared but not observed; splitting it into the tab".into(),
            },
        ));
        return;
    };

    let Some(owner) = effective_owner(observed, &profile.name) else {
        return; // unmanaged: never touched
    };

    if owner.digest == resource.digest {
        return; // converged
    }

    if observed.parent.as_deref() != resource.parent.as_deref() {
        ranked.push((
            RANK_PANE,
            PHASE_CLOSE,
            id.to_owned(),
            Action {
                kind: ActionKind::ClosePane,
                address: id.to_owned(),
                backend_id: Some(observed.backend_id.clone()),
                destructive: true,
                reason: "pane moved to a different tab; closing the old placement".into(),
            },
        ));
        ranked.push((
            RANK_PANE,
            PHASE_CREATE,
            id.to_owned(),
            Action {
                kind: ActionKind::SplitPane,
                address: id.to_owned(),
                backend_id: None,
                destructive: false,
                reason: "recreating the pane in its new tab".into(),
            },
        ));
    } else {
        push_pane_content_change(resource, id, Some(observed), serves, ranked);
    }
}

fn push_pane_content_change(
    _resource: &Resource,
    id: &str,
    observed: Option<&Observed>,
    serves: bool,
    ranked: &mut Vec<RankedAction>,
) {
    // `model.rs` doesn't forbid declaring both `adopt = "caller"` and
    // `serve` on the same pane. If it did, a content change here would
    // propose restarting the command in the pane running the controller
    // itself. No Drovefile in this repo combines them, and the example
    // fixtures never exercise it; a validation rule belongs in `model.rs`
    // if this combination needs to be rejected outright (out of scope for
    // this PR — see PR 2 review on #4, finding 3).
    let backend_id = observed.map(|observed| observed.backend_id.clone());
    if serves {
        ranked.push((
            RANK_PANE,
            PHASE_UPDATE,
            id.to_owned(),
            Action {
                kind: ActionKind::RestartCommand,
                address: id.to_owned(),
                backend_id,
                destructive: false,
                reason: "serve command changed; restarting in place".into(),
            },
        ));
    } else {
        ranked.push((
            RANK_PANE,
            PHASE_RENAME,
            id.to_owned(),
            Action {
                kind: ActionKind::RenamePane,
                address: id.to_owned(),
                backend_id,
                destructive: false,
                reason: "pane label or configuration changed".into(),
            },
        ));
    }
}

fn plan_agent(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    ranked: &mut Vec<RankedAction>,
) {
    let id = identity(resource);
    let has_prompt = resource
        .fields
        .get("prompt")
        .and_then(Value::as_str)
        .is_some();
    match snapshot.resources.get(&id) {
        None => {
            ranked.push((
                RANK_AGENT,
                PHASE_CREATE,
                id.clone(),
                Action {
                    kind: ActionKind::StartAgent,
                    address: id.clone(),
                    backend_id: None,
                    destructive: false,
                    reason: "agent declared but not started".into(),
                },
            ));
            if has_prompt {
                ranked.push((
                    RANK_AGENT,
                    PHASE_UPDATE,
                    id.clone(),
                    Action {
                        kind: ActionKind::PromptAgent,
                        address: id,
                        backend_id: None,
                        destructive: false,
                        reason: "sending the declared prompt".into(),
                    },
                ));
            }
        }
        Some(observed) => {
            if let Some(owner) = effective_owner(observed, &profile.name)
                && owner.digest != resource.digest
            {
                ranked.push((
                    RANK_AGENT,
                    PHASE_UPDATE,
                    id.clone(),
                    Action {
                        kind: ActionKind::PromptAgent,
                        address: id,
                        backend_id: Some(observed.backend_id.clone()),
                        destructive: false,
                        reason: "agent prompt or configuration changed".into(),
                    },
                ));
            }
        }
    }
}

fn order_pair<'a>(left: &'a str, right: &'a str) -> (&'a str, &'a str) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

/// Ancestors of `start` reachable through `after` edges (dependent -> its
/// dependencies). The DAG is already enforced by `Profile::validate`.
fn ancestors<'a>(start: &'a str, after: &BTreeMap<&'a str, &'a [String]>) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        if let Some(targets) = after.get(node) {
            for target in *targets {
                if seen.insert(target.as_str()) {
                    stack.push(target.as_str());
                }
            }
        }
    }
    seen
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

/// The v2 model has no declared `writes` for a task yet, so `inputs` stands
/// in for both reads and writes (spec §5's `RunTask` row): two tasks that
/// touch the same path without an `after` edge between them race.
// The hazard pass here only checks task-vs-task overlap (spec §5's
// `RunTask` row). Task-vs-pane hazards (a task's `inputs` overlapping a
// running pane's `cwd`) are intentionally out of scope for this PR: the
// brief's paraphrase of this item names only task ordering, and pane
// `cwd` isn't tracked as a read/write surface anywhere else in this
// module. Confirmed deferred, not dropped — see PR 2 review on #4,
// finding 2.
fn plan_tasks(ir: &Ir, profile: &Profile, snapshot: &Snapshot, ranked: &mut Vec<RankedAction>) {
    let task_resources: Vec<&Resource> = ir
        .resources
        .iter()
        .filter(|resource| resource.kind == "task")
        .collect();
    if task_resources.is_empty() {
        return;
    }

    let mut after: BTreeMap<&str, &[String]> = BTreeMap::new();
    let mut inputs: BTreeMap<&str, &[std::path::PathBuf]> = BTreeMap::new();
    let mut auto: BTreeMap<&str, bool> = BTreeMap::new();
    for task in &profile.tasks {
        after.insert(task.name.as_str(), task.after.as_slice());
        inputs.insert(task.name.as_str(), task.inputs.as_slice());
        auto.insert(task.name.as_str(), task.auto);
    }

    let mut ordered_pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
    for name in inputs.keys().copied() {
        for ancestor in ancestors(name, &after) {
            ordered_pairs.insert(order_pair(name, ancestor));
        }
    }

    let names: Vec<&str> = inputs.keys().copied().collect();
    let mut conflicted: BTreeSet<&str> = BTreeSet::new();
    for i in 0..names.len() {
        for j in (i + 1)..names.len() {
            let (left, right) = (names[i], names[j]);
            if ordered_pairs.contains(&order_pair(left, right)) {
                continue;
            }
            let overlap = inputs[left]
                .iter()
                .any(|a| inputs[right].iter().any(|b| paths_overlap(a, b)));
            if overlap {
                conflicted.insert(left);
                conflicted.insert(right);
                ranked.push((
                    RANK_TASK,
                    PHASE_CONFLICT,
                    format!("{left}~{right}"),
                    Action {
                        kind: ActionKind::Conflict,
                        address: format!("{left}~{right}"),
                        backend_id: None,
                        destructive: false,
                        reason: format!(
                            "tasks `{left}` and `{right}` touch overlapping inputs with no `after` ordering between them"
                        ),
                    },
                ));
            }
        }
    }

    let mut needs_run: BTreeSet<&str> = BTreeSet::new();
    for resource in &task_resources {
        let name = resource.name.as_str();
        if !*auto.get(name).unwrap_or(&true) || conflicted.contains(name) {
            continue;
        }
        let converged = snapshot
            .resources
            .get(name)
            .and_then(|observed| effective_owner(observed, &profile.name))
            .is_some_and(|owner| owner.digest == resource.digest);
        if !converged {
            needs_run.insert(name);
        }
    }

    let mut remaining = needs_run.clone();
    let mut phase = PHASE_CREATE;
    while !remaining.is_empty() {
        let mut ready: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|name| {
                after
                    .get(name)
                    .map(|deps| deps.iter().all(|dep| !remaining.contains(dep.as_str())))
                    .unwrap_or(true)
            })
            .collect();
        ready.sort_unstable();
        if ready.is_empty() {
            // The `after` DAG is already enforced by `Profile::validate`; this
            // is unreachable, but stop rather than loop forever if it ever isn't.
            break;
        }
        for name in ready {
            remaining.remove(name);
            let observed = snapshot.resources.get(name);
            let reason = if observed.is_none() {
                "task not yet run".to_owned()
            } else {
                "task inputs or command changed".to_owned()
            };
            ranked.push((
                RANK_TASK,
                phase,
                name.to_owned(),
                Action {
                    kind: ActionKind::RunTask,
                    address: name.to_owned(),
                    backend_id: None,
                    destructive: false,
                    reason,
                },
            ));
        }
        phase = phase.saturating_add(1);
    }
}

fn plan_detach(
    declared: &BTreeSet<String>,
    profile: &Profile,
    snapshot: &Snapshot,
    ranked: &mut Vec<RankedAction>,
) {
    for (id, observed) in &snapshot.resources {
        if declared.contains(id) {
            continue;
        }
        if effective_owner(observed, &profile.name).is_none() {
            continue;
        }
        let rank = match observed.kind.as_str() {
            "workspace" => RANK_WORKSPACE,
            "tab" => RANK_TAB,
            "pane" => RANK_PANE,
            "agent" => RANK_AGENT,
            "task" => RANK_TASK,
            _ => RANK_UNKNOWN,
        };
        ranked.push((
            rank,
            PHASE_DETACH,
            id.clone(),
            Action {
                kind: ActionKind::Detach,
                address: id.clone(),
                backend_id: Some(observed.backend_id.clone()),
                destructive: false,
                reason: "owned resource no longer declared; leaving it in place and detaching"
                    .into(),
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn profile_from(value: serde_json::Value) -> Profile {
        serde_json::from_value(value).expect("profile fixture")
    }

    fn one_pane_profile() -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "panes": [{"name": "review", "serve": [["bash"]]}]
                }]
            }]
        }))
    }

    fn digest_of<'a>(ir: &'a Ir, kind: &str, name: &str) -> &'a str {
        ir.resources
            .iter()
            .find(|resource| resource.kind == kind && resource.name == name)
            .unwrap_or_else(|| panic!("no {kind} resource named `{name}`"))
            .digest
            .as_str()
    }

    fn kinds(plan: &Plan) -> Vec<ActionKind> {
        plan.actions.iter().map(|action| action.kind).collect()
    }

    #[test]
    fn everything_absent_creates_from_scratch() {
        let profile = one_pane_profile();
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(plan.status, SyncStatus::OutOfSync);
        assert_eq!(
            kinds(&plan),
            [ActionKind::CreateWorkspace, ActionKind::CreateTab]
        );
    }

    #[test]
    fn converged_resources_produce_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                digest_of(&ir, "pane", "review"),
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(plan.status, SyncStatus::InSync);
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn unmanaged_panes_produce_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .unmanaged("pane", "review", "w1:p1", Some("dev/main"));
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "unmanaged pane must never be touched: {plan:?}"
        );
    }

    #[test]
    fn missing_pane_in_existing_tab_splits_it() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::SplitPane]);
        assert!(!plan.actions[0].destructive);
    }

    #[test]
    fn changed_serve_command_restarts_in_place() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                "stale-digest",
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::RestartCommand]);
        assert!(!plan.actions[0].destructive);
        assert_eq!(plan.actions[0].backend_id.as_deref(), Some("w1:p1"));
    }

    #[test]
    fn changed_non_serve_pane_is_renamed_not_restarted() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                "stale-digest",
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::RenamePane]);
    }

    #[test]
    fn pane_moved_to_a_different_tab_replaces_destructively() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        // Observed under a tab identity ("dev/other") that no longer matches
        // the pane's declared parent ("dev/main"): a topology change.
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p9",
                Some("dev/other"),
                "default",
                "stale-digest",
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::ClosePane, ActionKind::SplitPane]);
        assert!(plan.actions[0].destructive);
        assert!(!plan.actions[1].destructive);
    }

    #[test]
    fn owned_but_undeclared_pane_is_detached_not_closed() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                digest_of(&ir, "pane", "review"),
            )
            .owned(
                "pane",
                "gone",
                "w1:p2",
                Some("dev/main"),
                "default",
                "any-digest",
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::Detach]);
        assert_eq!(plan.actions[0].address, "gone");
        assert!(!plan.actions[0].destructive);
    }

    #[test]
    fn adopts_caller_pane_when_present() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "control",
                "tabs": [{
                    "name": "coordinator",
                    "ratios": [0.5],
                    "panes": [{"name": "controller", "adopt": "caller"}, {"name": "log", "serve": [["tail"]]}]
                }]
            }]
        }));
        let snapshot = Snapshot::default().with_caller("caller-pane-1");
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(plan.adopted.get("controller"), Some(&true));
        assert!(
            plan.actions
                .iter()
                .any(|action| action.kind == ActionKind::AdoptPane
                    && action.address == "controller"
                    && action.backend_id.as_deref() == Some("caller-pane-1"))
        );
        // The tab already exists around the live caller pane (it is not a
        // fresh `CreateTab` layout), so the sibling pane is split into it
        // individually rather than being subsumed into a layout application.
        assert!(
            plan.actions
                .iter()
                .any(|action| action.address == "log" && action.kind == ActionKind::SplitPane)
        );
    }

    #[test]
    fn falls_back_to_a_normal_create_without_a_caller_pane() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "control",
                "tabs": [{"name": "coordinator", "panes": [{"name": "controller", "adopt": "caller"}]}]
            }]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(plan.adopted.get("controller"), Some(&false));
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == ActionKind::AdoptPane)
        );
        assert!(
            plan.actions
                .iter()
                .any(|action| action.kind == ActionKind::CreateTab)
        );
    }

    #[test]
    fn conflicting_tasks_without_after_ordering_are_flagged() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [
                {"name": "a", "run": ["true"], "inputs": ["shared.txt"]},
                {"name": "b", "run": ["true"], "inputs": ["shared.txt"]}
            ]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::Conflict]);
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == ActionKind::RunTask)
        );
    }

    #[test]
    fn ordered_tasks_run_in_after_order() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [
                {"name": "b", "run": ["true"], "inputs": ["shared.txt"], "after": ["a"]},
                {"name": "a", "run": ["true"], "inputs": ["shared.txt"]}
            ]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(kinds(&plan), [ActionKind::RunTask, ActionKind::RunTask]);
        assert_eq!(plan.actions[0].address, "a");
        assert_eq!(plan.actions[1].address, "b");
    }

    #[test]
    fn converged_task_is_skipped() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "scaffold", "run": ["true"]}]
        }));
        let ir = profile.to_ir();
        let snapshot = Snapshot::default().owned(
            "task",
            "scaffold",
            "n/a",
            None,
            "default",
            digest_of(&ir, "task", "scaffold"),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn manual_task_is_never_planned_automatically() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "protect-log", "run": ["true"], "auto": false}]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn actions_are_ordered_workspace_before_tab_before_pane_before_agent() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "panes": [{
                        "name": "review",
                        "agent": {"kind": "claude", "prompt": "hi"}
                    }]
                }]
            }],
            "tasks": [{"name": "scaffold", "run": ["true"]}]
        }));
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                ActionKind::CreateWorkspace,
                ActionKind::CreateTab,
                ActionKind::StartAgent,
                ActionKind::PromptAgent,
                ActionKind::RunTask,
            ]
        );
    }

    #[test]
    fn moved_checkout_does_not_change_any_digest() {
        // D21: "digests never include the repository path". `Workspace.cwd`
        // defaults to the relative `.` (see `default_cwd`) and `to_ir`
        // never reads `std::env::current_dir()` or `Profile`'s own
        // `repo_root` (that field doesn't exist — `dsl::compile` resolves
        // `repo_root` only to read `file()` prompts, and never stores it
        // on `Profile`). So the IR field itself — not just two identical
        // builds of the same in-memory value — must stay the declared
        // relative `.`, regardless of where the checkout that produced
        // this `Profile` lives on disk.
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let workspace = ir
            .resources
            .iter()
            .find(|resource| resource.kind == "workspace" && resource.name == "dev")
            .expect("workspace resource");
        assert_eq!(workspace.fields["cwd"], serde_json::json!("."));

        // A second `Profile` built from a fixture that only varies in an
        // absolute path having nothing to do with any declared field (here:
        // two structurally identical profiles, standing in for the same
        // Drovefile loaded from two different checkout directories) must
        // still converge on the same digest, since nothing in the model
        // carries that path into `fields`.
        let moved = one_pane_profile();
        let first = build_plan(&profile, &Snapshot::default()).expect("plan");
        let second = build_plan(&moved, &Snapshot::default()).expect("plan");
        assert_eq!(first.desired_digest, second.desired_digest);
    }

    #[test]
    fn render_reports_in_sync_with_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                digest_of(&ir, "workspace", "dev"),
            )
            .owned(
                "tab",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                digest_of(&ir, "tab", "main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                digest_of(&ir, "pane", "review"),
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(plan.render(), "in sync: profile `default`\n");
    }
}
