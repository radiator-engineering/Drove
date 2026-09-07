//! Plans the reconciliation actions needed to converge a `Profile`'s
//! compiled IR against a backend-observed [`Snapshot`] (spec §5, D5, D9,
//! D16, D17, D21, D22, D29, D30).
//!
//! Ownership is decided by comparing each resource's declared identity
//! (`drove_name`/`drove_profile`) and content digest (`drove_digest`, D21)
//! against what the snapshot reports for that identity:
//!
//! - no observed entry: the resource is declared but absent -> create it.
//! - observed, no owner token: unmanaged -> never touched.
//! - observed, owned by this profile, same digest: converged -> no action.
//! - observed, owned by this profile, different digest: a content change
//!   (`RestartCommand`/`RenamePane`/`RenameWorkspace`/...) unless the pane's
//!   placement group moved, which is a topology change handled as a
//!   destructive `ClosePane` followed by a fresh `SplitPane` (D22).
//! - owned by this profile, no longer declared: `Detach` (leave it running,
//!   stop tracking it; only `drove down` closes owned resources).
//!
//! A resource's identity is its own declared name (D5). A Herdr placement
//! group's identity is `workspace/<name>`, the scope its panes share; the
//! group is derived from the panes' placements (D29), not a resource of its
//! own, so the IR carries it in [`crate::ir::Ir::placements`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    backend::ProcessInfo,
    ir::{Ir, PlacementGroup, Resource},
    model::{Pane, Profile, Workspace},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    InSync,
    OutOfSync,
}

/// What the backend currently reports for one resource identity. Real
/// backends read ownership tokens back (D16); until that lands, callers build
/// this from [`crate::state::LocalState`], the declared fallback in the same
/// discovery order.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// The pane id of the invoking terminal, if any (`Backend::caller_pane_id`).
    pub caller_pane_id: Option<String>,
    pub resources: BTreeMap<String, Observed>,
}

#[derive(Debug, Clone)]
pub struct Observed {
    /// IR resource kind (`workspace`, `pane`, `agent`, `task`) or `placement`
    /// for a Herdr group, needed only to order a `Detach` for a resource no
    /// longer declared.
    pub kind: String,
    pub backend_id: String,
    /// The identity of this resource's current placement group, used to
    /// detect a topology change (a pane moved to a different group).
    pub parent: Option<String>,
    pub owner: Option<Owner>,
    /// What the backend currently reports running in this pane (D51 point
    /// 2, D54), when a caller actually asked one. The outer `Option` is
    /// whether live data was fetched at all — `None` (the default) means
    /// this `Snapshot` was built from local state alone (D16) with no
    /// backend consulted, so drift can't be judged either way and D54 stays
    /// silent. Once fetched, the inner `Option` is the backend's own answer:
    /// `Some(info)` for a running command, `None` for an idle shell or a
    /// pane the backend lost.
    pub process_info: Option<Option<ProcessInfo>>,
    /// Whether Drove successfully started this pane's declared command after
    /// creating the physical pane. `Some(false)` means a previous apply saved
    /// the pane id after a post-create launch failure, so matching content
    /// digest alone is not enough to consider it converged.
    pub command_started: Option<bool>,
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
                process_info: None,
                command_started: None,
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
                process_info: None,
                command_started: None,
            },
        );
        self
    }

    /// Attaches what the backend reports running in an already-inserted
    /// pane (D54). A no-op when `identity` isn't in the snapshot yet, so
    /// callers can merge live process info in without caring about
    /// insertion order.
    pub fn with_process_info(mut self, identity: &str, process_info: Option<ProcessInfo>) -> Self {
        if let Some(observed) = self.resources.get_mut(identity) {
            observed.process_info = Some(process_info);
        }
        self
    }

    pub fn with_command_started(mut self, identity: &str, started: bool) -> Self {
        if let Some(observed) = self.resources.get_mut(identity) {
            observed.command_started = Some(started);
        }
        self
    }

    /// Merges every pane's live `process_info` from a Herdr snapshot in by
    /// matching backend ids (D54) — the smallest way to get drift detection
    /// the declared-fallback `to_snapshot` (D16) has no live backend to ask.
    pub fn merge_process_info(mut self, live: &crate::backend::herdr::SessionSnapshot) -> Self {
        for observed in self.resources.values_mut() {
            if observed.kind == "pane"
                && let Some(pane) = live.pane(&observed.backend_id)
            {
                observed.process_info = Some(pane.process_info.clone());
            }
        }
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
    pub actions: Vec<PlannedAction>,
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

/// One planned reconciliation action. `kind` nests by flavor (D29): a core
/// verb every backend honors, or a flavor verb that yields `Unsupported` on a
/// backend without that flavor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedAction {
    pub kind: Action,
    /// The target resource's IR identity (D5).
    pub address: String,
    /// The backend id, when the resource (or, for a fresh create, its
    /// neighbour) is already known to the backend.
    pub backend_id: Option<String>,
    pub destructive: bool,
    pub reason: String,
}

/// An action nested by flavor (spec §4, D29).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Core(CoreAction),
    Herdr(HerdrAction),
    Radiator(RadiatorAction),
}

/// Verbs every backend implements fully (spec §4).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoreAction {
    CreateWorkspace,
    RenameWorkspace,
    CreatePane,
    ClosePane,
    RenamePane,
    RestartCommand,
    AdoptPane,
    PromptAgent,
    RunTask,
    Detach,
    Conflict,
}

/// Verbs only the Herdr flavor implements (spec §4).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HerdrAction {
    CreateTab,
    RenameTab,
    SplitPane,
    SetRatio,
    StartAgent,
}

/// Verbs only the Radiator flavor implements. None yet (spec §8, D37).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RadiatorAction {}

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

type RankedAction = (u8, u8, String, PlannedAction);

fn effective_owner<'a>(observed: &'a Observed, profile: &str) -> Option<&'a Owner> {
    observed
        .owner
        .as_ref()
        .filter(|owner| owner.profile == profile)
}

/// Whether `digest` predates the composite per-category format (D53:
/// [`crate::ir`]'s `composite_digest`) — a pre-D53 single hash, or any other
/// value that doesn't parse as a JSON object, such as a hand-built digest in
/// a test. A resource still recorded this way carries no field breakdown to
/// diff against, so every call site here treats it as converged (a one-time
/// migration amnesty) rather than guessing what changed from a format that
/// never said. `cli::up_command` re-stamps the fresh composite digest for a
/// resource still in this state once it has confirmed the resource is still
/// live, ending the amnesty without the planner ever reporting an edit for
/// it; D54's drift check is unaffected either way, since it never reads the
/// digest at all.
pub(crate) fn is_legacy_digest(digest: &str) -> bool {
    !serde_json::from_str::<Value>(digest).is_ok_and(|value| value.is_object())
}

/// Compares two composite digests (D53: [`crate::ir`]'s `composite_digest`)
/// category by category, returning the categories whose value differs. Only
/// meaningful once both sides have already been confirmed to parse as that
/// shape ([`is_legacy_digest`]) — `None` when either doesn't, so a defensive
/// caller can still fall back to treating the resource as unclassifiable
/// rather than panicking.
fn diff_categories(old: &str, new: &str) -> Option<BTreeSet<String>> {
    let old = serde_json::from_str::<Value>(old).ok()?;
    let new = serde_json::from_str::<Value>(new).ok()?;
    let old = old.as_object()?;
    let new = new.as_object()?;
    Some(
        new.keys()
            .filter(|key| old.get(key.as_str()) != new.get(key.as_str()))
            .cloned()
            .collect(),
    )
}

pub fn build_plan(profile: &Profile, snapshot: &Snapshot) -> Result<Plan> {
    let ir = profile.to_ir();
    let mut ranked: Vec<RankedAction> = Vec::new();
    let mut adopted: BTreeMap<String, bool> = BTreeMap::new();

    let mut declared: BTreeSet<String> = ir.resources.iter().map(|r| r.name.clone()).collect();
    for group in &ir.placements {
        declared.insert(group.id.clone());
    }

    // A placement group that holds an `adopt = "caller"` pane already exists
    // in the backend around that live pane, even on a first run.
    let mut groups_with_adopt: BTreeSet<String> = BTreeSet::new();
    for resource in &ir.resources {
        if resource.kind == "pane"
            && resource.fields.get("adopt").and_then(Value::as_str) == Some("caller")
            && let Some(group_id) = &resource.parent
        {
            groups_with_adopt.insert(group_id.clone());
        }
    }

    // D34: identities a `was =` rename claims from the backend this plan.
    // Excluded from `declared` below's complement so `plan_detach` never
    // proposes detaching an identity a rename just migrated away from.
    let mut consumed_by_rename: BTreeSet<String> = BTreeSet::new();

    // Panes the workspace `cwd`/`env` cascade below already closed and
    // re-split: `plan_pane`/`plan_normal_pane` skip these, since the
    // cascade's recreate already reflects each pane's current declaration
    // (see the comment at the `cascaded.insert` call site).
    let mut cascaded: BTreeSet<String> = BTreeSet::new();

    for resource in &ir.resources {
        if resource.kind == "workspace" {
            plan_workspace(
                resource,
                profile,
                snapshot,
                &mut consumed_by_rename,
                &mut cascaded,
                &mut ranked,
            );
        }
    }

    let mut group_fresh: BTreeMap<String, bool> = BTreeMap::new();
    for group in &ir.placements {
        let has_adopt_caller =
            groups_with_adopt.contains(&group.id) && snapshot.caller_pane_id.is_some();
        let fresh = plan_group(group, profile, snapshot, has_adopt_caller, &mut ranked);
        group_fresh.insert(group.id.clone(), fresh);
    }

    for resource in &ir.resources {
        match resource.kind.as_str() {
            "pane" => {
                let group_id = resource.parent.clone().unwrap_or_default();
                let fresh = *group_fresh.get(&group_id).unwrap_or(&false);
                let mut routing = PaneRouting {
                    consumed_by_rename: &mut consumed_by_rename,
                    cascaded: &cascaded,
                };
                plan_pane(
                    resource,
                    profile,
                    snapshot,
                    fresh,
                    &mut routing,
                    &mut ranked,
                    &mut adopted,
                );
            }
            "agent" => plan_agent(resource, profile, snapshot, &mut ranked),
            _ => {}
        }
    }

    plan_tasks(&ir, profile, snapshot, &mut ranked);
    declared.extend(consumed_by_rename);
    plan_detach(&declared, profile, snapshot, &mut ranked);

    ranked.sort_by(|left, right| {
        (left.0, left.1, left.2.as_str()).cmp(&(right.0, right.1, right.2.as_str()))
    });
    let actions: Vec<PlannedAction> = ranked.into_iter().map(|(_, _, _, action)| action).collect();
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

fn workspace_by_name<'a>(profile: &'a Profile, name: &str) -> Option<&'a Workspace> {
    profile.workspaces.iter().find(|w| w.name == name)
}

fn pane_by_name<'a>(profile: &'a Profile, name: &str) -> Option<&'a Pane> {
    profile
        .workspaces
        .iter()
        .flat_map(|w| &w.tabs)
        .flat_map(|t| &t.panes)
        .find(|p| p.name == name)
}

/// D34: both the `was`-declared old identity and the new one are live —
/// ambiguous, so the planner refuses to guess and flags it instead of
/// silently picking a side.
fn identity_conflict(rank: u8, new_name: &str, old_name: &str, kind: &str) -> RankedAction {
    (
        rank,
        PHASE_CONFLICT,
        new_name.to_owned(),
        PlannedAction {
            kind: Action::Core(CoreAction::Conflict),
            address: new_name.to_owned(),
            backend_id: None,
            destructive: false,
            reason: format!(
                "{kind} `{new_name}` declares `was = \"{old_name}\"` but both identities are live"
            ),
        },
    )
}

fn plan_workspace(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    consumed_by_rename: &mut BTreeSet<String>,
    cascaded: &mut BTreeSet<String>,
    ranked: &mut Vec<RankedAction>,
) {
    let id = resource.name.clone();

    if let Some(was) = workspace_by_name(profile, &id).and_then(|w| w.was.as_deref()) {
        let has_new = snapshot.resources.contains_key(&id);
        let old_owned = snapshot
            .resources
            .get(was)
            .and_then(|observed| effective_owner(observed, &profile.name))
            .is_some();
        if old_owned {
            if has_new {
                ranked.push(identity_conflict(RANK_WORKSPACE, &id, was, "workspace"));
                // Both identities are live and ambiguous: leave the old one
                // alone rather than detaching it out from under the conflict.
                consumed_by_rename.insert(was.to_owned());
                return;
            }
            let backend_id = snapshot.resources[was].backend_id.clone();
            ranked.push((
                RANK_WORKSPACE,
                PHASE_RENAME,
                id.clone(),
                PlannedAction {
                    kind: Action::Core(CoreAction::RenameWorkspace),
                    address: id,
                    backend_id: Some(backend_id),
                    destructive: false,
                    reason: format!(
                        "`was = \"{was}\"` matched a live workspace; migrating identity instead of creating"
                    ),
                },
            ));
            consumed_by_rename.insert(was.to_owned());
            return;
        }
    }

    match snapshot.resources.get(&id) {
        None => ranked.push((
            RANK_WORKSPACE,
            PHASE_CREATE,
            id.clone(),
            PlannedAction {
                kind: Action::Core(CoreAction::CreateWorkspace),
                address: id,
                backend_id: None,
                destructive: false,
                reason: "workspace declared but not observed".into(),
            },
        )),
        Some(observed) => {
            if let Some(owner) = effective_owner(observed, &profile.name)
                && owner.digest != resource.digest
                && !is_legacy_digest(&owner.digest)
            {
                let changed = diff_categories(&owner.digest, &resource.digest);
                // D53 point 2: only the workspace's own `label`/`cwd`/`env`
                // warrant a `RenameWorkspace` here — a pane-only change
                // (the `children` category alone) is already planned by
                // that pane's own rule and would otherwise fight it with a
                // spurious rename every run.
                let own_field_changed = changed
                    .as_ref()
                    .map(|changed| changed.iter().any(|key| key != "children"))
                    .unwrap_or(true);
                if own_field_changed {
                    ranked.push((
                        RANK_WORKSPACE,
                        PHASE_RENAME,
                        id.clone(),
                        PlannedAction {
                            kind: Action::Core(CoreAction::RenameWorkspace),
                            address: id.clone(),
                            backend_id: Some(observed.backend_id.clone()),
                            destructive: false,
                            reason: "workspace label, cwd, or env changed".into(),
                        },
                    ));
                }
                // D53 point 2: `cwd`/`env` changing cascades the pane rule
                // (point 1) to every pane that inherits the changed value —
                // a pane with no `cwd` of its own inherits the workspace
                // `cwd`; a pane with no `env` entries of its own inherits
                // the workspace `env`. Checked per category, not together:
                // a pane can own one and inherit the other, so a workspace
                // `cwd`-only change must not cascade to a pane that only
                // inherits `env` (and vice versa). Only fires when the
                // category breakdown says so (a legacy digest never reaches
                // here at all, see the `is_legacy_digest` guard above).
                let ws_cwd_changed = changed
                    .as_ref()
                    .is_some_and(|changed| changed.contains("cwd"));
                let ws_env_changed = changed
                    .as_ref()
                    .is_some_and(|changed| changed.contains("env"));
                if (ws_cwd_changed || ws_env_changed)
                    && let Some(workspace) = workspace_by_name(profile, &id)
                {
                    for pane in workspace.tabs.iter().flat_map(|group| &group.panes) {
                        let inherits_cwd = ws_cwd_changed && pane.cwd.is_none();
                        let inherits_env = ws_env_changed && pane.env.is_empty();
                        if !inherits_cwd && !inherits_env {
                            continue;
                        }
                        let Some(pane_observed) = snapshot.resources.get(&pane.name) else {
                            continue; // not created yet; the create picks up the new value
                        };
                        if effective_owner(pane_observed, &profile.name).is_none() {
                            continue;
                        }
                        let field = match (ws_cwd_changed, ws_env_changed) {
                            (true, true) => "cwd/env",
                            (true, false) => "cwd",
                            (false, true) => "env",
                            (false, false) => unreachable!(),
                        };
                        push_close_and_split(
                            &pane.name,
                            pane_observed.backend_id.clone(),
                            format!(
                                "workspace `{id}` {field} changed; a pane cannot change directory or environment in place"
                            ),
                            ranked,
                        );
                        // The per-pane rule (`plan_normal_pane`) recreates a
                        // pane whose own digest changed or that moved
                        // placement group; without this, a pane whose own
                        // fields *also* changed (or that also moved) would
                        // be recreated twice, or get a `RestartCommand`
                        // against the backend id this cascade just closed.
                        // The cascade's recreate already reflects the pane's
                        // full current declaration, so the per-pane rule has
                        // nothing left to add.
                        cascaded.insert(pane.name.clone());
                    }
                }
            }
        }
    }
}

/// Plans a Herdr placement group. Returns whether the group is freshly
/// created this plan, in which case its panes are subsumed into the one
/// `CreateTab` layout application (item 4) rather than planned individually.
fn plan_group(
    group: &PlacementGroup,
    profile: &Profile,
    snapshot: &Snapshot,
    has_adopt_caller: bool,
    ranked: &mut Vec<RankedAction>,
) -> bool {
    let id = group.id.clone();
    let observed = snapshot.resources.get(&id);

    let is_fresh = observed.is_none() && !has_adopt_caller;
    if is_fresh {
        ranked.push((
            RANK_TAB,
            PHASE_CREATE,
            id.clone(),
            PlannedAction {
                kind: Action::Herdr(HerdrAction::CreateTab),
                address: id,
                backend_id: None,
                destructive: false,
                reason: "placement group declared but not observed; applying as a new layout"
                    .into(),
            },
        ));
        return true;
    }

    if let Some(observed) = observed
        && let Some(owner) = effective_owner(observed, &profile.name)
        && owner.digest != group.topology_digest
        && !is_legacy_digest(&owner.digest)
    {
        let backend_id = Some(observed.backend_id.clone());
        let changed = diff_categories(&owner.digest, &group.topology_digest);
        // D53 point 4: reordering panes or flipping a group's split direction
        // (the same set of panes) has no backend verb — `SetRatio` would
        // silently apply the new ratio list to the old physical order. A
        // pane added or removed keeps the legacy `RenameTab` + `SetRatio`
        // pair (right per the audit); a legacy digest never reaches here at
        // all (see the `is_legacy_digest` guard above).
        let reordered = changed.as_ref().is_some_and(|changed| {
            changed.contains("order_split") && !changed.contains("pane_set")
        });
        if reordered {
            ranked.push((
                RANK_TAB,
                PHASE_CONFLICT,
                id.clone(),
                PlannedAction {
                    kind: Action::Core(CoreAction::Conflict),
                    address: id,
                    backend_id,
                    destructive: false,
                    reason:
                        "cannot reorder panes or change the split in place; remove the group and re-add it"
                            .into(),
                },
            ));
        } else {
            ranked.push((
                RANK_TAB,
                PHASE_RENAME,
                id.clone(),
                PlannedAction {
                    kind: Action::Herdr(HerdrAction::RenameTab),
                    address: id.clone(),
                    backend_id: backend_id.clone(),
                    destructive: false,
                    reason: "placement group label changed".into(),
                },
            ));
            ranked.push((
                RANK_TAB,
                PHASE_UPDATE,
                id.clone(),
                PlannedAction {
                    kind: Action::Herdr(HerdrAction::SetRatio),
                    address: id,
                    backend_id,
                    destructive: false,
                    reason: "placement group ratios changed".into(),
                },
            ));
        }
    }

    false
}

/// Bundles the two per-pane routing sets `plan_pane` threads through, so the
/// function stays under clippy's argument-count limit.
struct PaneRouting<'a> {
    consumed_by_rename: &'a mut BTreeSet<String>,
    /// Panes the workspace `cwd`/`env` cascade already closed and re-split
    /// (see the `cascaded.insert` call site in `plan_workspace`).
    cascaded: &'a BTreeSet<String>,
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
    group_fresh: bool,
    routing: &mut PaneRouting,
    ranked: &mut Vec<RankedAction>,
    adopted: &mut BTreeMap<String, bool>,
) {
    let id = resource.name.clone();
    if routing.cascaded.contains(&id) {
        return; // already closed and re-split by the workspace cascade
    }
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
                    PlannedAction {
                        kind: Action::Core(CoreAction::AdoptPane),
                        address: id,
                        backend_id: Some(caller_id.clone()),
                        destructive: false,
                        reason: "adopting the invoking pane".into(),
                    },
                ));
            }
            (Some(_), Some(owner)) => {
                adopted.insert(resource.name.clone(), true);
                let converged_by_digest =
                    owner.digest == resource.digest || is_legacy_digest(&owner.digest);
                if converged_by_digest {
                    if serves
                        && let Some(observed) = observed
                        && observed.command_started == Some(false)
                    {
                        push_restart_command(
                            &id,
                            Some(observed.backend_id.clone()),
                            "command start pending".to_owned(),
                            ranked,
                        );
                        return;
                    }
                    // Converged by digest: still worth a look at what's
                    // actually running, for a serve pane (D54) — same as
                    // the non-adopted path in `plan_normal_pane`, which an
                    // adopted pane otherwise never reaches.
                    if serves && let Some(observed) = observed {
                        check_drift(&id, resource, observed, ranked);
                    }
                } else if let Some(observed) = observed {
                    push_pane_content_change(&id, observed, owner, resource, ranked);
                }
            }
            (None, _) => {
                adopted.insert(resource.name.clone(), false);
                if !group_fresh {
                    plan_normal_pane(
                        resource,
                        &id,
                        profile,
                        snapshot,
                        serves,
                        routing.consumed_by_rename,
                        ranked,
                    );
                }
            }
        }
        return;
    }

    if group_fresh {
        return;
    }
    plan_normal_pane(
        resource,
        &id,
        profile,
        snapshot,
        serves,
        routing.consumed_by_rename,
        ranked,
    );
}

fn plan_normal_pane(
    resource: &Resource,
    id: &str,
    profile: &Profile,
    snapshot: &Snapshot,
    serves: bool,
    consumed_by_rename: &mut BTreeSet<String>,
    ranked: &mut Vec<RankedAction>,
) {
    if let Some(was) = pane_by_name(profile, id).and_then(|p| p.was.as_deref()) {
        let has_new = snapshot.resources.contains_key(id);
        let old_owned = snapshot
            .resources
            .get(was)
            .and_then(|observed| effective_owner(observed, &profile.name))
            .is_some();
        if old_owned {
            if has_new {
                ranked.push(identity_conflict(RANK_PANE, id, was, "pane"));
                // Both identities are live and ambiguous: leave the old one
                // alone rather than detaching it out from under the conflict.
                consumed_by_rename.insert(was.to_owned());
                return;
            }
            let backend_id = snapshot.resources[was].backend_id.clone();
            ranked.push((
                RANK_PANE,
                PHASE_RENAME,
                id.to_owned(),
                PlannedAction {
                    kind: Action::Core(CoreAction::RenamePane),
                    address: id.to_owned(),
                    backend_id: Some(backend_id),
                    destructive: false,
                    reason: format!(
                        "`was = \"{was}\"` matched a live pane; migrating identity instead of creating"
                    ),
                },
            ));
            consumed_by_rename.insert(was.to_owned());
            return;
        }
    }

    let Some(observed) = snapshot.resources.get(id) else {
        ranked.push((
            RANK_PANE,
            PHASE_CREATE,
            id.to_owned(),
            PlannedAction {
                kind: Action::Herdr(HerdrAction::SplitPane),
                address: id.to_owned(),
                backend_id: None,
                destructive: false,
                reason: "pane declared but not observed; splitting it into the group".into(),
            },
        ));
        return;
    };

    let Some(owner) = effective_owner(observed, &profile.name) else {
        return; // unmanaged: never touched
    };

    let converged_by_digest = owner.digest == resource.digest || is_legacy_digest(&owner.digest);
    if converged_by_digest && observed.parent.as_deref() == resource.parent.as_deref() {
        if serves && observed.command_started == Some(false) {
            push_restart_command(
                id,
                Some(observed.backend_id.clone()),
                "command start pending".to_owned(),
                ranked,
            );
            return;
        }
        // Converged by digest (or recorded under a pre-D53 digest this
        // planner can't diff by category, so it's given the benefit of the
        // doubt — see `is_legacy_digest`): still worth a look at what's
        // actually running, for a serve pane (D54) — a digest only ever
        // compares two declared states, so it's blind to a change made by
        // hand.
        if serves {
            check_drift(id, resource, observed, ranked);
        }
        return;
    }

    if observed.parent.as_deref() != resource.parent.as_deref() {
        ranked.push((
            RANK_PANE,
            PHASE_CLOSE,
            id.to_owned(),
            PlannedAction {
                kind: Action::Core(CoreAction::ClosePane),
                address: id.to_owned(),
                backend_id: Some(observed.backend_id.clone()),
                destructive: true,
                reason: "pane moved to a different placement group; closing the old placement"
                    .into(),
            },
        ));
        ranked.push((
            RANK_PANE,
            PHASE_CREATE,
            id.to_owned(),
            PlannedAction {
                kind: Action::Herdr(HerdrAction::SplitPane),
                address: id.to_owned(),
                backend_id: None,
                destructive: false,
                reason: "recreating the pane in its new placement group".into(),
            },
        ));
    } else {
        push_pane_content_change(id, observed, owner, resource, ranked);
    }
}

fn push_rename_pane(
    id: &str,
    backend_id: Option<String>,
    reason: String,
    ranked: &mut Vec<RankedAction>,
) {
    ranked.push((
        RANK_PANE,
        PHASE_RENAME,
        id.to_owned(),
        PlannedAction {
            kind: Action::Core(CoreAction::RenamePane),
            address: id.to_owned(),
            backend_id,
            destructive: false,
            reason,
        },
    ));
}

fn push_restart_command(
    id: &str,
    backend_id: Option<String>,
    reason: String,
    ranked: &mut Vec<RankedAction>,
) {
    ranked.push((
        RANK_PANE,
        PHASE_UPDATE,
        id.to_owned(),
        PlannedAction {
            kind: Action::Core(CoreAction::RestartCommand),
            address: id.to_owned(),
            backend_id,
            destructive: false,
            reason,
        },
    ));
}

/// A pane can't change its own working directory or environment in place
/// (D53 point 1: no Herdr verb re-`cd`s a live shell), so this closes and
/// re-splits it with the new declaration — the same destructive pattern as
/// a placement-group move, gated the same way (D22).
fn push_close_and_split(
    id: &str,
    backend_id: String,
    reason: String,
    ranked: &mut Vec<RankedAction>,
) {
    ranked.push((
        RANK_PANE,
        PHASE_CLOSE,
        id.to_owned(),
        PlannedAction {
            kind: Action::Core(CoreAction::ClosePane),
            address: id.to_owned(),
            backend_id: Some(backend_id),
            destructive: true,
            reason: reason.clone(),
        },
    ));
    ranked.push((
        RANK_PANE,
        PHASE_CREATE,
        id.to_owned(),
        PlannedAction {
            kind: Action::Herdr(HerdrAction::SplitPane),
            address: id.to_owned(),
            backend_id: None,
            destructive: false,
            reason,
        },
    ));
}

/// Routes a pane content change to a verb (D53), now that the composite
/// digest (D53, [`crate::ir`]) can say *which* category changed instead of
/// just *that* something did:
///
/// - `cwd`/`env`: destructive recreate (point 1) — Herdr's `run_command`
///   composite (`pane.send_text` + `pane.send_keys`; there is no native verb
///   that takes a working directory, checked against `herdr api schema
///   --json` for Herdr 0.8.2) can't `cd` an existing shell, so the
///   `serve`-pane exception in point 1 never applies today.
/// - `serve`: restart in place (unchanged from before D53).
/// - `label` and/or `on_start` alone: `RenamePane`. `on_start` has no verb
///   of its own (point 3, "no backend action") — it still rides `RenamePane`
///   so the plan updates the recorded digest and doesn't repeat the message
///   forever; that call is a same-label no-op on the backend when `label`
///   itself didn't change.
/// - anything else (`ready`, `after`, `adopt`, `on_stop`): `Conflict` (point
///   5) — the planner has no verb to map it to.
/// - no category breakdown recorded yet (a digest from before this
///   feature): the pre-D53 fallback, for one transition apply.
fn push_pane_content_change(
    id: &str,
    observed: &Observed,
    owner: &Owner,
    resource: &Resource,
    ranked: &mut Vec<RankedAction>,
) {
    let backend_id = Some(observed.backend_id.clone());
    // Both call sites already guard `!is_legacy_digest(&owner.digest)`
    // before reaching here, so this only defends against a future caller
    // that doesn't: never plan an edit against a digest this planner can't
    // actually diff by category.
    let Some(changed) = diff_categories(&owner.digest, &resource.digest) else {
        return;
    };

    if changed.contains("cwd") || changed.contains("env") {
        let field = match (changed.contains("cwd"), changed.contains("env")) {
            (true, true) => "cwd and env",
            (true, false) => "cwd",
            _ => "env",
        };
        push_close_and_split(
            id,
            observed.backend_id.clone(),
            format!("{field} changed; a pane cannot change directory in place"),
            ranked,
        );
        return;
    }

    if changed.contains("serve") {
        push_restart_command(
            id,
            backend_id,
            "serve command changed; restarting in place".into(),
            ranked,
        );
        return;
    }

    let only_label_or_on_start = !changed.is_empty()
        && changed
            .iter()
            .all(|key| key == "label" || key == "on_start");
    if only_label_or_on_start {
        let reason = if changed.contains("on_start") {
            format!("on_start changed for `{id}`; runs on next create")
        } else {
            "pane label or configuration changed".into()
        };
        push_rename_pane(id, backend_id, reason, ranked);
        return;
    }

    ranked.push((
        RANK_PANE,
        PHASE_CONFLICT,
        id.to_owned(),
        PlannedAction {
            kind: Action::Core(CoreAction::Conflict),
            address: id.to_owned(),
            backend_id,
            destructive: false,
            reason: format!(
                "unsupported change: {}",
                changed.into_iter().collect::<Vec<_>>().join(", ")
            ),
        },
    ));
}

/// D54: for a pane declaring `serve`, compares what the backend reports
/// running (`process_info`, merged in by [`Snapshot::merge_process_info`])
/// against the declared argv, independent of the recorded digest — a user
/// running a different command by hand, or a crashed process replaced by an
/// idle shell, never shows up in a digest that only ever compares declared
/// states to each other.
fn check_drift(id: &str, resource: &Resource, observed: &Observed, ranked: &mut Vec<RankedAction>) {
    // No live data was fetched for this plan (a `Snapshot` built from local
    // state alone, D16) — nothing to compare against, so stay silent rather
    // than treat "unknown" as "nothing running".
    let Some(live) = &observed.process_info else {
        return;
    };
    let declared = declared_serve_argv(resource);
    if declared.is_empty() {
        return;
    }
    let matches = live
        .as_ref()
        .is_some_and(|info| normalize_argv(&info.command) == normalize_argv(&declared));
    if matches {
        return;
    }
    let reason = match live {
        Some(info) => format!("drifted: running {}", info.command.join(" ")),
        None => "drifted: nothing running".to_owned(),
    };
    push_restart_command(id, Some(observed.backend_id.clone()), reason, ranked);
}

fn declared_serve_argv(resource: &Resource) -> Vec<String> {
    resource
        .fields
        .get("serve")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(Value::as_array)
        .map(|argv| {
            argv.iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

const SHELL_WRAPPERS: [&str; 3] = ["sh", "bash", "zsh"];

/// D54 point 1: trims trailing whitespace and, when the whole argv is a
/// `sh -c "<command>"`-shaped wrapper, unwraps it to the words of the
/// wrapped command before comparing.
fn normalize_argv(argv: &[String]) -> Vec<String> {
    let trimmed: Vec<String> = argv.iter().map(|arg| arg.trim().to_owned()).collect();
    if let [program, flag, command] = trimmed.as_slice()
        && flag == "-c"
    {
        let program_name = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program.as_str());
        if SHELL_WRAPPERS.contains(&program_name) {
            return command.split_whitespace().map(str::to_owned).collect();
        }
    }
    trimmed
}

fn plan_agent(
    resource: &Resource,
    profile: &Profile,
    snapshot: &Snapshot,
    ranked: &mut Vec<RankedAction>,
) {
    let id = resource.name.clone();
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
                PlannedAction {
                    kind: Action::Herdr(HerdrAction::StartAgent),
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
                    PlannedAction {
                        kind: Action::Core(CoreAction::PromptAgent),
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
                    PlannedAction {
                        kind: Action::Core(CoreAction::PromptAgent),
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
                    PlannedAction {
                        kind: Action::Core(CoreAction::Conflict),
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
                PlannedAction {
                    kind: Action::Core(CoreAction::RunTask),
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
            "placement" => RANK_TAB,
            "pane" => RANK_PANE,
            "agent" => RANK_AGENT,
            "task" => RANK_TASK,
            _ => RANK_UNKNOWN,
        };
        ranked.push((
            rank,
            PHASE_DETACH,
            id.clone(),
            PlannedAction {
                kind: Action::Core(CoreAction::Detach),
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

    fn pane_digest<'a>(ir: &'a Ir, name: &str) -> &'a str {
        ir.resources
            .iter()
            .find(|resource| resource.kind == "pane" && resource.name == name)
            .unwrap_or_else(|| panic!("no pane resource named `{name}`"))
            .digest
            .as_str()
    }

    fn resource_digest<'a>(ir: &'a Ir, kind: &str, name: &str) -> &'a str {
        ir.resources
            .iter()
            .find(|resource| resource.kind == kind && resource.name == name)
            .unwrap_or_else(|| panic!("no {kind} resource named `{name}`"))
            .digest
            .as_str()
    }

    fn group_digest<'a>(ir: &'a Ir, id: &str) -> &'a str {
        ir.placements
            .iter()
            .find(|group| group.id == id)
            .unwrap_or_else(|| panic!("no placement group `{id}`"))
            .topology_digest
            .as_str()
    }

    fn kinds(plan: &Plan) -> Vec<Action> {
        plan.actions.iter().map(|action| action.kind).collect()
    }

    /// A snapshot that already owns the workspace and the group, converged,
    /// so only pane-level differences remain to plan.
    fn converged_shell(ir: &Ir) -> Snapshot {
        Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(ir, "dev/main"),
            )
    }

    #[test]
    fn everything_absent_creates_from_scratch() {
        let profile = one_pane_profile();
        let plan = build_plan(&profile, &Snapshot::default()).expect("plan");
        assert_eq!(plan.status, SyncStatus::OutOfSync);
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::CreateWorkspace),
                Action::Herdr(HerdrAction::CreateTab)
            ]
        );
    }

    #[test]
    fn converged_resources_produce_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p1",
            Some("dev/main"),
            "default",
            pane_digest(&ir, "review"),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(plan.status, SyncStatus::InSync);
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn unmanaged_panes_produce_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).unmanaged("pane", "review", "w1:p1", Some("dev/main"));
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "unmanaged pane must never be touched: {plan:?}"
        );
    }

    #[test]
    fn missing_pane_in_existing_group_splits_it() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir);
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Herdr(HerdrAction::SplitPane)]);
        assert!(!plan.actions[0].destructive);
    }

    #[test]
    fn changed_serve_command_restarts_in_place() {
        let old = pane_profile(json!({"name": "review", "serve": [["bash"]]}));
        let new = pane_profile(json!({"name": "review", "serve": [["bash", "-x"]]}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RestartCommand)]);
        assert!(!plan.actions[0].destructive);
        assert_eq!(plan.actions[0].backend_id.as_deref(), Some("w1:p1"));
        assert_eq!(
            plan.actions[0].reason,
            "serve command changed; restarting in place"
        );
    }

    #[test]
    fn changed_non_serve_pane_label_is_renamed_not_restarted() {
        let old = pane_profile(json!({"name": "review", "label": "old"}));
        let new = pane_profile(json!({"name": "review", "label": "new"}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RenamePane)]);
    }

    // D53 migration: a pane, workspace, or group still recorded under a
    // pre-D53 digest (a single hash, or any other string that doesn't parse
    // as the composite category-hash object) can't be diffed by category, so
    // it must not be misread as "everything changed" and recreated or
    // restarted — see `is_legacy_digest`. The controller flagged this after
    // review: every existing state file records the old format, and the
    // first `up`/`plan`/`status` after upgrading must not treat that alone
    // as an edit.

    #[test]
    fn legacy_pane_digest_is_treated_as_converged_not_changed() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p1",
            Some("dev/main"),
            "default",
            "stale-digest",
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "a legacy pane digest must not be read as a change: {plan:?}"
        );
    }

    #[test]
    fn legacy_workspace_digest_is_treated_as_converged_not_changed() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "a",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned("workspace", "dev", "w1", None, "default", "stale-digest")
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(&ir, "dev/main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&ir, "review"),
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "a legacy workspace digest must not be read as a change: {plan:?}"
        );
    }

    #[test]
    fn legacy_group_digest_is_treated_as_converged_not_changed() {
        let profile = two_pane_group("a", "b");
        let ir = profile.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(&ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                "stale-digest",
            )
            .owned(
                "pane",
                "a",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&ir, "a"),
            )
            .owned(
                "pane",
                "b",
                "w1:p2",
                Some("dev/main"),
                "default",
                pane_digest(&ir, "b"),
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "a legacy group topology digest must not be read as a change: {plan:?}"
        );
    }

    #[test]
    fn pane_moved_to_a_different_group_replaces_destructively() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        // Observed under a group identity ("dev/other") that no longer
        // matches the pane's declared placement ("dev/main"): a topology
        // change.
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p9",
            Some("dev/other"),
            "default",
            pane_digest(&ir, "review"),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::ClosePane),
                Action::Herdr(HerdrAction::SplitPane)
            ]
        );
        assert!(plan.actions[0].destructive);
        assert!(!plan.actions[1].destructive);
    }

    #[test]
    fn owned_but_undeclared_pane_is_detached_not_closed() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir)
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&ir, "review"),
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
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Detach)]);
        assert_eq!(plan.actions[0].address, "gone");
        assert!(!plan.actions[0].destructive);
    }

    #[test]
    fn pane_was_matches_live_identity_renames_instead_of_creating() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "review", "was": "shell"}]}]
            }]
        }));
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "shell",
            "w1:p1",
            Some("dev/main"),
            "default",
            "old-digest",
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RenamePane)]);
        assert_eq!(plan.actions[0].address, "review");
        assert_eq!(plan.actions[0].backend_id.as_deref(), Some("w1:p1"));
    }

    #[test]
    fn workspace_was_matches_live_identity_renames_instead_of_creating() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "was": "legacy",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let snapshot =
            Snapshot::default().owned("workspace", "legacy", "w1", None, "default", "old-digest");
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(plan.actions.iter().any(|action| action.kind
            == Action::Core(CoreAction::RenameWorkspace)
            && action.address == "dev"
            && action.backend_id.as_deref() == Some("w1")));
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::Detach))
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::CreateWorkspace))
        );
    }

    #[test]
    fn was_matching_a_pane_already_live_under_the_new_name_is_a_conflict() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "review", "was": "shell"}]}]
            }]
        }));
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir)
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&ir, "review"),
            )
            .owned(
                "pane",
                "shell",
                "w1:p2",
                Some("dev/main"),
                "default",
                "any-digest",
            );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Conflict)]);
    }

    #[test]
    fn was_matching_a_workspace_already_live_under_the_new_name_is_a_conflict() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "was": "legacy",
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
                resource_digest(&ir, "workspace", "dev"),
            )
            .owned("workspace", "legacy", "w2", None, "default", "any-digest");
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::Conflict)
                    && action.address == "dev")
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::RenameWorkspace))
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::Detach))
        );
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
                .any(|action| action.kind == Action::Core(CoreAction::AdoptPane)
                    && action.address == "controller"
                    && action.backend_id.as_deref() == Some("caller-pane-1"))
        );
        // The group already exists around the live caller pane (it is not a
        // fresh `CreateTab` layout), so the sibling pane is split into it
        // individually rather than being subsumed into a layout application.
        assert!(plan.actions.iter().any(|action| action.address == "log"
            && action.kind == Action::Herdr(HerdrAction::SplitPane)));
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
                .any(|action| action.kind == Action::Core(CoreAction::AdoptPane))
        );
        assert!(
            plan.actions
                .iter()
                .any(|action| action.kind == Action::Herdr(HerdrAction::CreateTab))
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
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Conflict)]);
        assert!(
            !plan
                .actions
                .iter()
                .any(|action| action.kind == Action::Core(CoreAction::RunTask))
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
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::RunTask),
                Action::Core(CoreAction::RunTask)
            ]
        );
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
            resource_digest(&ir, "task", "scaffold"),
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
    fn actions_are_ordered_workspace_before_group_before_pane_before_agent() {
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
                Action::Core(CoreAction::CreateWorkspace),
                Action::Herdr(HerdrAction::CreateTab),
                Action::Herdr(HerdrAction::StartAgent),
                Action::Core(CoreAction::PromptAgent),
                Action::Core(CoreAction::RunTask),
            ]
        );
    }

    #[test]
    fn moved_checkout_does_not_change_any_digest() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let workspace = ir
            .resources
            .iter()
            .find(|resource| resource.kind == "workspace" && resource.name == "dev")
            .expect("workspace resource");
        assert_eq!(workspace.fields["cwd"], serde_json::json!("."));

        let moved = one_pane_profile();
        let first = build_plan(&profile, &Snapshot::default()).expect("plan");
        let second = build_plan(&moved, &Snapshot::default()).expect("plan");
        assert_eq!(first.desired_digest, second.desired_digest);
    }

    #[test]
    fn render_reports_in_sync_with_no_actions() {
        let profile = one_pane_profile();
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p1",
            Some("dev/main"),
            "default",
            pane_digest(&ir, "review"),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(plan.render(), "in sync: profile `default`\n");
    }

    // --- D53: the planner never reports success for an edit it cannot apply ---

    fn pane_profile(pane: serde_json::Value) -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [pane]}]
            }]
        }))
    }

    /// A converged snapshot recorded from `old`, so a plan against `new`
    /// (built from an otherwise-identical profile) exercises the real
    /// composite-digest category comparison rather than the pre-D53
    /// fallback a synthetic `"stale-digest"` string takes.
    fn converged_from(old: &Profile) -> Snapshot {
        let ir = old.to_ir();
        converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p1",
            Some("dev/main"),
            "default",
            pane_digest(&ir, "review"),
        )
    }

    #[test]
    fn cwd_change_recreates_a_non_serve_pane_destructively() {
        let old = pane_profile(json!({"name": "review", "cwd": "a"}));
        let new = pane_profile(json!({"name": "review", "cwd": "b"}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::ClosePane),
                Action::Herdr(HerdrAction::SplitPane)
            ]
        );
        assert!(plan.actions[0].destructive);
        assert_eq!(
            plan.actions[0].reason,
            "cwd changed; a pane cannot change directory in place"
        );
    }

    #[test]
    fn env_change_recreates_a_pane_destructively() {
        let old = pane_profile(json!({"name": "review", "env": {"K": "1"}}));
        let new = pane_profile(json!({"name": "review", "env": {"K": "2"}}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::ClosePane),
                Action::Herdr(HerdrAction::SplitPane)
            ]
        );
        assert_eq!(
            plan.actions[0].reason,
            "env changed; a pane cannot change directory in place"
        );
    }

    /// D53 point 1's exception (a `serve` pane whose `cwd` change can go
    /// through `RestartCommand` instead) never applies: Drove's
    /// `run_command` types text into an existing shell (`pane.send_text` +
    /// `pane.send_keys`), it does not take a `cwd`, and Herdr 0.8.2's own
    /// `pane.split`/`pane.send_*` verbs have no working-directory parameter
    /// either (`herdr api schema --json`). A `serve` pane's `cwd` change
    /// still recreates the pane.
    #[test]
    fn cwd_change_on_a_serve_pane_still_recreates_destructively() {
        let old = pane_profile(json!({"name": "review", "cwd": "a", "serve": [["bash"]]}));
        let new = pane_profile(json!({"name": "review", "cwd": "b", "serve": [["bash"]]}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                Action::Core(CoreAction::ClosePane),
                Action::Herdr(HerdrAction::SplitPane)
            ]
        );
    }

    #[test]
    fn on_start_change_alone_has_no_backend_action_but_records_the_new_digest() {
        let old = pane_profile(json!({"name": "review", "on_start": ["echo", "old"]}));
        let new = pane_profile(json!({"name": "review", "on_start": ["echo", "new"]}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RenamePane)]);
        assert_eq!(
            plan.actions[0].reason,
            "on_start changed for `review`; runs on next create"
        );
    }

    #[test]
    fn unmapped_pane_change_is_a_conflict() {
        let old = pane_profile(json!({"name": "review", "on_stop": ["echo", "old"]}));
        let new = pane_profile(json!({"name": "review", "on_stop": ["echo", "new"]}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Conflict)]);
        assert_eq!(plan.actions[0].reason, "unsupported change: other");
    }

    #[test]
    fn workspace_cwd_change_renames_and_cascades_to_an_inheriting_pane() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "a",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "b",
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(&old_ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(&old_ir, "dev/main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "review"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::RenameWorkspace) && a.address == "dev")
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::ClosePane)
                    && a.address == "review"
                    && a.destructive
                    && a.reason
                        == "workspace `dev` cwd changed; a pane cannot change directory or environment in place")
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Herdr(HerdrAction::SplitPane) && a.address == "review")
        );
    }

    #[test]
    fn workspace_env_change_cascades_to_an_inheriting_pane() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "env": {"FOO": "a"},
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "env": {"FOO": "b"},
                "tabs": [{"name": "main", "panes": [{"name": "review"}]}]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(&old_ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(&old_ir, "dev/main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "review"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::RenameWorkspace) && a.address == "dev")
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::ClosePane)
                    && a.address == "review"
                    && a.destructive
                    && a.reason
                        == "workspace `dev` env changed; a pane cannot change directory or environment in place")
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Herdr(HerdrAction::SplitPane) && a.address == "review")
        );
    }

    #[test]
    fn workspace_is_not_renamed_when_only_a_pane_changed() {
        let old = pane_profile(json!({"name": "review", "serve": [["bash"]]}));
        let new = pane_profile(json!({"name": "review", "serve": [["bash", "-x"]]}));
        let snapshot = converged_from(&old);
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::RenameWorkspace)),
            "a pane-only change must not also rename the workspace: {plan:?}"
        );
    }

    /// Compound case: a workspace `cwd` change cascades to an inheriting
    /// pane at the same time that pane also moves to a different placement
    /// group. Both `plan_workspace`'s cascade and `plan_normal_pane`'s own
    /// parent-mismatch rule would otherwise close and re-split the same
    /// pane independently — two `ClosePane`s racing to close a backend id
    /// only the first one still finds live. The cascade must win outright,
    /// not just first in rank order.
    #[test]
    fn cascade_and_a_placement_group_move_do_not_both_recreate_the_pane() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "a",
                "tabs": [
                    {"name": "main", "panes": [{"name": "review"}]},
                    {"name": "second", "panes": [{"name": "other"}]}
                ]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "b",
                "tabs": [
                    {"name": "main", "panes": [{"name": "stub"}]},
                    {"name": "second", "panes": [{"name": "other"}, {"name": "review"}]}
                ]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(&old_ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(&old_ir, "dev/main"),
            )
            .owned(
                "placement",
                "dev/second",
                "w1:t2",
                Some("dev"),
                "default",
                group_digest(&old_ir, "dev/second"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "review"),
            )
            .owned(
                "pane",
                "other",
                "w1:p2",
                Some("dev/second"),
                "default",
                pane_digest(&old_ir, "other"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        let review_closes = plan
            .actions
            .iter()
            .filter(|a| a.kind == Action::Core(CoreAction::ClosePane) && a.address == "review")
            .count();
        let review_splits = plan
            .actions
            .iter()
            .filter(|a| a.kind == Action::Herdr(HerdrAction::SplitPane) && a.address == "review")
            .count();
        assert_eq!(
            review_closes, 1,
            "the cascade and the placement-group move must not both close `review`: {plan:?}"
        );
        assert_eq!(
            review_splits, 1,
            "the cascade and the placement-group move must not both re-split `review`: {plan:?}"
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::ClosePane)
                    && a.address == "review"
                    && a.reason.contains("cwd changed")),
            "the surviving close should be the cascade's, not the placement-move's own: {plan:?}"
        );
    }

    /// Compound case: a workspace `cwd` change cascades to an inheriting
    /// serve pane whose live process has also drifted from what it
    /// declares. The pane's own digest doesn't reflect an inherited `cwd`
    /// (only the workspace's digest does), so before the cascade skip fix
    /// `plan_normal_pane` would treat it as converged-by-digest and run
    /// `check_drift` too, issuing a `RestartCommand` against the backend id
    /// the cascade had just closed.
    #[test]
    fn cascade_and_command_drift_do_not_both_act_on_the_same_pane() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "a",
                "tabs": [{"name": "main", "panes": [{"name": "review", "serve": [["bash"]]}]}]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "cwd": "b",
                "tabs": [{"name": "main", "panes": [{"name": "review", "serve": [["bash"]]}]}]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = Snapshot::default()
            .owned(
                "workspace",
                "dev",
                "w1",
                None,
                "default",
                resource_digest(&old_ir, "workspace", "dev"),
            )
            .owned(
                "placement",
                "dev/main",
                "w1:t1",
                Some("dev"),
                "default",
                group_digest(&old_ir, "dev/main"),
            )
            .owned(
                "pane",
                "review",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "review"),
            )
            .with_process_info(
                "review",
                Some(crate::backend::ProcessInfo {
                    command: vec!["vim".into()],
                    pid: Some(1),
                }),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::RestartCommand)),
            "the cascade already recreates `review`; drift must not also restart it: {plan:?}"
        );
        assert_eq!(
            plan.actions
                .iter()
                .filter(|a| a.address == "review")
                .count(),
            2,
            "exactly the cascade's ClosePane and SplitPane, nothing else: {plan:?}"
        );
    }

    fn two_pane_group(first: &str, second: &str) -> Profile {
        profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "ratios": [0.5],
                    "panes": [{"name": first}, {"name": second}]
                }]
            }]
        }))
    }

    #[test]
    fn reordering_panes_in_a_tab_is_a_conflict_not_a_silent_ratio_change() {
        let old = two_pane_group("a", "b");
        let new = two_pane_group("b", "a");
        let old_ir = old.to_ir();
        let snapshot = converged_shell(&old_ir)
            .owned(
                "pane",
                "a",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "a"),
            )
            .owned(
                "pane",
                "b",
                "w1:p2",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "b"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Conflict)]);
        assert_eq!(
            plan.actions[0].reason,
            "cannot reorder panes or change the split in place; remove the group and re-add it"
        );
    }

    #[test]
    fn changing_split_direction_with_the_same_panes_is_a_conflict() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "split": "right",
                    "panes": [{"name": "a"}, {"name": "b"}]
                }]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "split": "down",
                    "panes": [{"name": "a"}, {"name": "b"}]
                }]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = converged_shell(&old_ir)
            .owned(
                "pane",
                "a",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "a"),
            )
            .owned(
                "pane",
                "b",
                "w1:p2",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "b"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::Conflict)]);
    }

    #[test]
    fn ratio_only_change_still_renames_tab_and_sets_ratio() {
        let old = two_pane_group("a", "b");
        let mut new = old.clone();
        new.workspaces[0].tabs[0].ratios = vec![0.75];
        let old_ir = old.to_ir();
        let snapshot = converged_shell(&old_ir)
            .owned(
                "pane",
                "a",
                "w1:p1",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "a"),
            )
            .owned(
                "pane",
                "b",
                "w1:p2",
                Some("dev/main"),
                "default",
                pane_digest(&old_ir, "b"),
            );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert_eq!(
            kinds(&plan),
            [
                Action::Herdr(HerdrAction::RenameTab),
                Action::Herdr(HerdrAction::SetRatio)
            ]
        );
    }

    #[test]
    fn adding_a_pane_still_uses_rename_tab_and_set_ratio() {
        let old = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "a"}]}]
            }]
        }));
        let new = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "ratios": [0.5], "panes": [{"name": "a"}, {"name": "b"}]}]
            }]
        }));
        let old_ir = old.to_ir();
        let snapshot = Snapshot::default().owned(
            "placement",
            "dev/main",
            "w1:t1",
            Some("dev"),
            "default",
            group_digest(&old_ir, "dev/main"),
        );
        let plan = build_plan(&new, &snapshot).expect("plan");
        assert!(
            plan.actions
                .iter()
                .any(|a| a.kind == Action::Herdr(HerdrAction::RenameTab))
        );
        assert!(
            !plan
                .actions
                .iter()
                .any(|a| a.kind == Action::Core(CoreAction::Conflict))
        );
    }

    // --- D54: process drift ---

    fn serving_pane_snapshot(argv: &[&str]) -> (Profile, Ir, Snapshot) {
        let profile = pane_profile(json!({"name": "review", "serve": [argv]}));
        let ir = profile.to_ir();
        let snapshot = converged_shell(&ir).owned(
            "pane",
            "review",
            "w1:p1",
            Some("dev/main"),
            "default",
            pane_digest(&ir, "review"),
        );
        (profile, ir, snapshot)
    }

    #[test]
    fn drift_restarts_a_pane_running_a_different_command() {
        let (profile, _, snapshot) = serving_pane_snapshot(&["bash"]);
        let snapshot = snapshot.with_process_info(
            "review",
            Some(crate::backend::ProcessInfo {
                command: vec!["vim".into()],
                pid: Some(1),
            }),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RestartCommand)]);
        assert_eq!(plan.actions[0].reason, "drifted: running vim");
    }

    #[test]
    fn drift_restarts_a_pane_running_nothing() {
        let (profile, _, snapshot) = serving_pane_snapshot(&["bash"]);
        let snapshot = snapshot.with_process_info("review", None);
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert_eq!(kinds(&plan), [Action::Core(CoreAction::RestartCommand)]);
        assert_eq!(plan.actions[0].reason, "drifted: nothing running");
    }

    #[test]
    fn no_drift_when_the_running_command_matches() {
        let (profile, _, snapshot) = serving_pane_snapshot(&["bash"]);
        let snapshot = snapshot.with_process_info(
            "review",
            Some(crate::backend::ProcessInfo {
                command: vec!["bash".into()],
                pid: Some(1),
            }),
        );
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "matching command must not drift: {plan:?}"
        );
    }

    #[test]
    fn no_drift_without_live_process_info() {
        // No `with_process_info`/`merge_process_info` call at all: a
        // declared-fallback `Snapshot` (D16) with no backend consulted.
        let (profile, _, snapshot) = serving_pane_snapshot(&["bash"]);
        let plan = build_plan(&profile, &snapshot).expect("plan");
        assert!(
            plan.actions.is_empty(),
            "no live data means no drift verdict either way: {plan:?}"
        );
    }
}
