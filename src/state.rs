//! Local ownership, approvals, and apply journal.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalState {
    /// Absent from an older or hand-trimmed state file: tolerated as `0`,
    /// the same as every other `#[serde(default)]` field here (D51 point 5).
    #[serde(default)]
    pub schema_version: u32,
    pub repo_root: PathBuf,
    #[serde(default)]
    pub profiles: BTreeMap<String, ManagedProfile>,
    #[serde(default)]
    pub approvals: BTreeSet<String>,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
    #[serde(skip, default)]
    pub(crate) path: PathBuf,
}

impl LocalState {
    /// Loads local state from `repo_root`'s state file. A file that exists
    /// but does not deserialize (a partial write from a crash, hand-editing,
    /// or an older/newer schema shape) is not fatal (D51 point 5): it is
    /// renamed aside to `<path>.corrupt-<timestamp>` with a printed warning,
    /// and loading continues from empty, the same as a missing file. A
    /// missing `schema_version` is tolerated the same way today's
    /// `#[serde(default)]` fields are — deserialization only fails on a
    /// field with the wrong shape, not a merely-absent optional one.
    pub fn load(repo_root: &Path) -> Result<Self> {
        Self::load_from(state_path(repo_root), repo_root)
    }

    /// The body of [`LocalState::load`], taking the state file path
    /// explicitly rather than deriving it from `state_path` (which reads a
    /// process-global env var), so tests can point it at a fixture without
    /// mutating the environment.
    fn load_from(path: PathBuf, repo_root: &Path) -> Result<Self> {
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(mut state) => {
                    state.path = path;
                    Ok(state)
                }
                Err(error) => {
                    let corrupt_path =
                        path.with_extension(format!("json.corrupt-{}", filename_safe_timestamp()));
                    fs::rename(&path, &corrupt_path).with_context(|| {
                        format!(
                            "cannot move invalid Drove local state {} aside to {}",
                            path.display(),
                            corrupt_path.display()
                        )
                    })?;
                    eprintln!(
                        "warning: Drove local state at {} was invalid ({error}); moved aside to {} and starting from empty",
                        path.display(),
                        corrupt_path.display()
                    );
                    Ok(Self {
                        schema_version: 1,
                        repo_root: repo_root.to_owned(),
                        profiles: BTreeMap::new(),
                        approvals: BTreeSet::new(),
                        journal: Vec::new(),
                        path,
                    })
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                schema_version: 1,
                repo_root: repo_root.to_owned(),
                profiles: BTreeMap::new(),
                approvals: BTreeSet::new(),
                journal: Vec::new(),
                path,
            }),
            Err(error) => Err(error).context("cannot read Drove local state"),
        }
    }

    pub fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("state path has no parent")?;
        fs::create_dir_all(parent).context("cannot create Drove state directory")?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)
            .context("cannot write temporary Drove state")?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).context("cannot replace old Drove state")?;
        }
        fs::rename(&temporary, &self.path).context("cannot replace Drove state")?;
        Ok(())
    }

    pub fn profile(&self, name: &str) -> Option<&ManagedProfile> {
        self.profiles.get(name)
    }

    pub fn profile_mut(&mut self, name: &str) -> &mut ManagedProfile {
        self.profiles.entry(name.to_owned()).or_default()
    }

    /// D55 point 1: stamps `profile`'s [`ManagedProfile::target`] with the
    /// resolved backend/session for this run, resetting the profile first if
    /// it was last saved against a different one. A profile with no stamp
    /// yet (a legacy file, or the profile's first run) is stamped without
    /// touching its resources. Returns the `state recorded for <old>;
    /// starting fresh for <new>` line to print when a reset happened.
    pub fn reset_stale_target(
        &mut self,
        profile: &str,
        backend_id: &str,
        session: Option<&str>,
    ) -> Option<String> {
        let new_target = StoredTarget {
            backend: backend_id.to_owned(),
            session: session.map(str::to_owned),
        };
        let managed = self.profile_mut(profile);
        match managed.target.clone() {
            None => {
                managed.target = Some(new_target);
                None
            }
            Some(old) if old == new_target => None,
            Some(old) => {
                let message = format!(
                    "state recorded for {}; starting fresh for {}",
                    old.describe(),
                    new_target.describe()
                );
                *managed = ManagedProfile {
                    target: Some(new_target),
                    ..Default::default()
                };
                Some(message)
            }
        }
    }

    /// Records a `was =` rename (D34): moves a managed resource's entry from
    /// its old identity to its new one, keeping the same backend id, parent
    /// and digest. After this, the next `drove up`/`plan`/`status` sees the
    /// resource under `new_identity` and the `was` declaration goes inert —
    /// there is no longer an old identity live for it to match.
    pub fn rename_resource(&mut self, profile: &str, old_identity: &str, new_identity: &str) {
        let managed = self.profile_mut(profile);
        if let Some(resource) = managed.resources.remove(old_identity) {
            managed.resources.insert(new_identity.to_owned(), resource);
        }
    }

    pub fn is_approved(&self, digest: &str) -> bool {
        self.approvals.contains(digest)
    }

    pub fn approve(&mut self, digest: String) {
        self.approvals.insert(digest);
    }

    pub fn begin_action(&mut self, action: &str, digest: &str) -> Result<()> {
        // A prior attempt at this same action that was killed before
        // `finish_action` ran would otherwise sit `completed: false` forever
        // once this attempt succeeds, leaving `status`/`run` reporting an
        // interruption that's actually resolved.
        for entry in self.journal.iter_mut() {
            if entry.action == action && !entry.completed {
                entry.completed = true;
                entry.success = None;
            }
        }
        self.journal.push(JournalEntry {
            action: action.to_owned(),
            digest: digest.to_owned(),
            completed: false,
            success: None,
        });
        self.trim_journal();
        self.save()
    }

    pub fn finish_action(&mut self, digest: &str, success: bool) -> Result<()> {
        if let Some(entry) = self
            .journal
            .iter_mut()
            .rev()
            .find(|entry| entry.digest == digest && !entry.completed)
        {
            entry.completed = true;
            entry.success = Some(success);
        }
        self.save()
    }

    /// Journal entries an apply began but never finished (D52 point 4): a
    /// `run`/hook killed mid-execution, surfaced instead of silently
    /// retried. `(action, digest)` pairs, in journal order.
    pub fn interrupted(&self) -> Vec<(&str, &str)> {
        self.journal
            .iter()
            .filter(|entry| !entry.completed)
            .map(|entry| (entry.action.as_str(), entry.digest.as_str()))
            .collect()
    }

    /// Whether `action` (e.g. `task:scaffold`) has an unfinished journal
    /// entry: the previous run started it but never recorded completion
    /// (D52 point 4).
    pub fn has_interrupted(&self, action: &str) -> bool {
        self.journal
            .iter()
            .any(|entry| entry.action == action && !entry.completed)
    }

    fn trim_journal(&mut self) {
        const MAX_ENTRIES: usize = 100;
        if self.journal.len() > MAX_ENTRIES {
            self.journal.drain(..self.journal.len() - MAX_ENTRIES);
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManagedProfile {
    #[serde(default)]
    pub desired_digest: String,
    /// The backend/session this profile's `resources` were last saved
    /// against (D55 point 1). Herdr's ids are small and session-scoped, so
    /// the same repo used against two sessions can easily see the same id
    /// string mean two different resources; recording the target lets a
    /// mismatched load reset the profile instead of trusting stale ids.
    #[serde(default)]
    pub target: Option<StoredTarget>,
    /// One entry per resource identity (D5: a plain name; a Herdr placement
    /// group's identity is `workspace/<name>` and its digest is the group's
    /// topology digest, D30), the source of ownership `to_snapshot` reads back
    /// (spec §5: "observed digest at apply time, runtime ids").
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResource>,
}

/// The backend id and session name a [`ManagedProfile`] was last saved
/// against (D55 point 1). `session: None` is an unnamed/default session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTarget {
    pub backend: String,
    #[serde(default)]
    pub session: Option<String>,
}

impl StoredTarget {
    fn describe(&self) -> String {
        match &self.session {
            Some(session) => format!("{}:{session}", self.backend),
            None => self.backend.clone(),
        }
    }
}

impl ManagedProfile {
    /// Builds the [`crate::planner::Snapshot`] the planner diffs the IR
    /// against, from what was recorded the last time this profile was
    /// applied (D16: tokens first, then local state, as the fallback).
    pub fn to_snapshot(
        &self,
        profile: &str,
        caller_pane_id: Option<String>,
    ) -> crate::planner::Snapshot {
        let mut snapshot = crate::planner::Snapshot {
            caller_pane_id,
            ..Default::default()
        };
        for (identity, resource) in &self.resources {
            snapshot = snapshot.owned(
                &resource.kind,
                identity,
                &resource.backend_id,
                resource.parent.as_deref(),
                profile,
                &resource.digest,
            );
        }
        snapshot
    }
}

/// Why [`prune_missing`] dropped a recorded resource (D55 point 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneReason {
    /// The recorded `backend_id` is not in the live snapshot at all — the
    /// resource was closed, or its whole session was wiped and restarted
    /// (D48).
    NotInSession,
    /// The recorded `backend_id` is live, but what's there no longer
    /// matches what Drove itself last recorded (a workspace/tab label, or a
    /// pane's `cwd`) — Herdr reused the id for a different resource.
    IdReused,
}

impl PruneReason {
    /// The word printed in `pruned <id> (<detail>)` (`down`) and `recreate
    /// <identity>: <detail>; recreating` (`status`/`plan`, where
    /// `NotInSession` keeps its original longer wording for backward
    /// compatibility).
    pub fn detail(&self) -> &'static str {
        match self {
            PruneReason::NotInSession => "not in session",
            PruneReason::IdReused => "id reused",
        }
    }

    /// The longer wording `status`/`plan` used before this reason existed,
    /// kept for `NotInSession` so existing output is unchanged.
    pub fn recreate_detail(&self) -> &'static str {
        match self {
            PruneReason::NotInSession => "backend id no longer exists",
            PruneReason::IdReused => "id reused",
        }
    }
}

/// Prunes a managed profile against the live backend snapshot (D48, D55
/// point 2): a `workspace`, `placement` or `pane` resource whose recorded
/// `backend_id` is absent from the snapshot's matching id set is gone, and
/// is dropped from the returned copy together with what it carried. A
/// workspace or placement whose `backend_id` is live but whose recorded
/// label (D55 point 2: `Some` only once this build has recorded one) no
/// longer matches what's live is dropped too, and so is a pane whose
/// recorded `cwd` no longer matches — Herdr reused the id for an unrelated
/// resource. A record with no `label`/`cwd` yet (saved before this field
/// existed) is not compared, since there is nothing to compare against; see
/// `docs/drovefile.md` for the reasoning. A missing or reused workspace
/// also drops every placement and pane whose `backend_id` is namespaced
/// under it (Herdr ids nest `<workspace>:t1`, `<workspace>:p1` directly off
/// the workspace id, not off the tab); a missing or reused placement drops
/// the panes recorded under it by identity (`ManagedResource::parent` holds
/// the placement's identity, not its backend id, so cascading here cannot
/// use the same prefix trick). `agent` and `task` resources are left alone:
/// their own `backend_id`/`parent` are not part of this workspace tree (a
/// task has no `backend_id` at all), so they are out of scope for this
/// prune. Returns the pruned profile and the dropped identities with their
/// reason, sorted by identity.
pub fn prune_missing(
    managed: &ManagedProfile,
    snapshot: &crate::backend::herdr::SessionSnapshot,
) -> (ManagedProfile, Vec<(String, PruneReason)>) {
    let workspace_labels: BTreeMap<&str, &str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace.label.as_str()))
        .collect();
    let tab_labels: BTreeMap<&str, &str> = snapshot
        .tabs
        .iter()
        .map(|tab| (tab.tab_id.as_str(), tab.label.as_str()))
        .collect();
    let pane_cwds: BTreeMap<&str, Option<&str>> = snapshot
        .panes
        .iter()
        .map(|pane| {
            (
                pane.pane_id.as_str(),
                pane.cwd.as_deref().and_then(|cwd| cwd.to_str()),
            )
        })
        .collect();

    let mut dropped: BTreeMap<String, PruneReason> = BTreeMap::new();
    let mut missing_workspace_backend_ids: BTreeMap<String, PruneReason> = BTreeMap::new();
    let mut missing_placement_identities: BTreeMap<String, PruneReason> = BTreeMap::new();

    for (identity, resource) in &managed.resources {
        match resource.kind.as_str() {
            "workspace" => match workspace_labels.get(resource.backend_id.as_str()) {
                None => {
                    missing_workspace_backend_ids
                        .insert(resource.backend_id.clone(), PruneReason::NotInSession);
                    dropped.insert(identity.clone(), PruneReason::NotInSession);
                }
                Some(live_label) => {
                    if let Some(recorded_label) = &resource.label
                        && *live_label != recorded_label.as_str()
                    {
                        missing_workspace_backend_ids
                            .insert(resource.backend_id.clone(), PruneReason::IdReused);
                        dropped.insert(identity.clone(), PruneReason::IdReused);
                    }
                }
            },
            "placement" => match tab_labels.get(resource.backend_id.as_str()) {
                None => {
                    missing_placement_identities
                        .insert(identity.clone(), PruneReason::NotInSession);
                    dropped.insert(identity.clone(), PruneReason::NotInSession);
                }
                Some(live_label) => {
                    if let Some(recorded_label) = &resource.label
                        && *live_label != recorded_label.as_str()
                    {
                        missing_placement_identities
                            .insert(identity.clone(), PruneReason::IdReused);
                        dropped.insert(identity.clone(), PruneReason::IdReused);
                    }
                }
            },
            "pane" => match pane_cwds.get(resource.backend_id.as_str()) {
                None => {
                    dropped.insert(identity.clone(), PruneReason::NotInSession);
                }
                Some(&live_cwd) => {
                    if let Some(recorded_cwd) = &resource.cwd
                        && live_cwd.is_some_and(|cwd| cwd != recorded_cwd.as_str())
                    {
                        dropped.insert(identity.clone(), PruneReason::IdReused);
                    }
                }
            },
            _ => {}
        }
    }

    for (identity, resource) in &managed.resources {
        if dropped.contains_key(identity) {
            continue;
        }
        if matches!(resource.kind.as_str(), "placement" | "pane")
            && let Some((_, reason)) = missing_workspace_backend_ids.iter().find(|(ws, _)| {
                resource
                    .backend_id
                    .strip_prefix(ws.as_str())
                    .is_some_and(|rest| rest.starts_with(':'))
            })
        {
            dropped.insert(identity.clone(), *reason);
            continue;
        }
        if resource.kind == "pane"
            && let Some(parent) = &resource.parent
            && let Some(reason) = missing_placement_identities.get(parent)
        {
            dropped.insert(identity.clone(), *reason);
        }
    }

    let mut pruned = managed.clone();
    pruned
        .resources
        .retain(|identity, _| !dropped.contains_key(identity));

    (pruned, dropped.into_iter().collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedResource {
    /// IR resource kind (`workspace`, `pane`, `agent`, `task`) or
    /// `placement` for a Herdr group (D29).
    pub kind: String,
    pub backend_id: String,
    #[serde(default)]
    pub parent: Option<String>,
    pub digest: String,
    /// The workspace/tab label Drove sent at apply time (D55 point 2): a
    /// workspace or placement only. `None` for a pane/agent/task, and for a
    /// record saved before this field existed.
    #[serde(default)]
    pub label: Option<String>,
    /// The pane's declared `cwd` at apply time (D55 point 2): a pane only.
    /// `None` for every other kind, and for a record saved before this field
    /// existed.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Set only for a pane declaring `adopt = "caller"` (D24).
    #[serde(default)]
    pub adopted: Option<bool>,
    /// The outcome of the most recent task run (`"ok"`, `"failed"`,
    /// `"skipped"`), reported by `drove run` with no task name. Unused for
    /// non-task resources.
    #[serde(default)]
    pub last_outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub action: String,
    pub digest: String,
    pub completed: bool,
    #[serde(default)]
    pub success: Option<bool>,
}

/// An RFC 3339 UTC timestamp with `:` replaced by `-` so it is safe in a
/// filename on every platform this project supports (Windows rejects `:` in
/// a path component), for the corrupt-state rename in [`LocalState::load`].
/// No datetime crate is a dependency, so this converts `SystemTime` by hand
/// (Howard Hinnant's `civil_from_days`, the standard days-since-epoch to
/// Gregorian-date algorithm).
fn filename_safe_timestamp() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    let second = time_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}Z")
}

/// Days-since-1970-01-01 to a Gregorian `(year, month, day)`, per Howard
/// Hinnant's public-domain `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

fn state_path(repo_root: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(repo_root.to_string_lossy().as_bytes()));
    state_root().join("projects").join(format!("{digest}.json"))
}

fn state_root() -> PathBuf {
    if let Ok(root) = env::var("DROVE_STATE_HOME") {
        return PathBuf::from(root);
    }
    if let Ok(root) = env::var("XDG_STATE_HOME") {
        return PathBuf::from(root).join("drove");
    }
    #[cfg(windows)]
    {
        if let Ok(root) = env::var("LOCALAPPDATA") {
            return PathBuf::from(root).join("drove");
        }
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("drove");
    }
    env::temp_dir().join("drove-state")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::herdr::{PaneInfo, SessionSnapshot, TabInfo, WorkspaceInfo};

    fn resource(kind: &str, backend_id: &str, parent: Option<&str>) -> ManagedResource {
        ManagedResource {
            kind: kind.into(),
            backend_id: backend_id.into(),
            parent: parent.map(str::to_owned),
            digest: "digest".into(),
            label: None,
            cwd: None,
            adopted: None,
            last_outcome: None,
        }
    }

    fn full_managed() -> ManagedProfile {
        let mut managed = ManagedProfile::default();
        managed
            .resources
            .insert("core".into(), resource("workspace", "w1", None));
        managed.resources.insert(
            "core/main".into(),
            resource("placement", "w1:t1", Some("core")),
        );
        managed.resources.insert(
            "review".into(),
            resource("pane", "w1:p1", Some("core/main")),
        );
        managed
            .resources
            .insert("maintenance".into(), resource("workspace", "w2", None));
        managed.resources.insert(
            "maintenance/main".into(),
            resource("placement", "w2:t1", Some("maintenance")),
        );
        managed.resources.insert(
            "logs".into(),
            resource("pane", "w2:p1", Some("maintenance/main")),
        );
        managed
            .resources
            .insert("build".into(), resource("task", "", None));
        managed
    }

    fn full_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            workspaces: vec![
                WorkspaceInfo {
                    workspace_id: "w1".into(),
                    label: String::new(),
                    tokens: Default::default(),
                },
                WorkspaceInfo {
                    workspace_id: "w2".into(),
                    label: String::new(),
                    tokens: Default::default(),
                },
            ],
            tabs: vec![
                TabInfo {
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    label: String::new(),
                },
                TabInfo {
                    tab_id: "w2:t1".into(),
                    workspace_id: "w2".into(),
                    label: String::new(),
                },
            ],
            panes: vec![
                PaneInfo {
                    pane_id: "w1:p1".into(),
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    cwd: None,
                    tokens: Default::default(),
                    process_info: None,
                },
                PaneInfo {
                    pane_id: "w2:p1".into(),
                    tab_id: "w2:t1".into(),
                    workspace_id: "w2".into(),
                    cwd: None,
                    tokens: Default::default(),
                    process_info: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn prune_missing_drops_nothing_against_a_full_snapshot() {
        let managed = full_managed();
        let (pruned, dropped) = prune_missing(&managed, &full_snapshot());
        assert!(dropped.is_empty());
        assert_eq!(pruned.resources.len(), managed.resources.len());
    }

    #[test]
    fn prune_missing_workspace_drops_its_placement_and_panes() {
        let managed = full_managed();
        let mut snapshot = full_snapshot();
        snapshot.workspaces.retain(|w| w.workspace_id != "w1");
        snapshot.tabs.retain(|t| t.workspace_id != "w1");
        snapshot.panes.retain(|p| p.workspace_id != "w1");

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert_eq!(
            dropped,
            vec![
                ("core".to_owned(), PruneReason::NotInSession),
                ("core/main".to_owned(), PruneReason::NotInSession),
                ("review".to_owned(), PruneReason::NotInSession),
            ]
        );
        assert!(!pruned.resources.contains_key("core"));
        assert!(!pruned.resources.contains_key("core/main"));
        assert!(!pruned.resources.contains_key("review"));
        assert!(pruned.resources.contains_key("maintenance"));
        assert!(pruned.resources.contains_key("maintenance/main"));
        assert!(pruned.resources.contains_key("logs"));
        assert!(pruned.resources.contains_key("build"));
    }

    #[test]
    fn prune_missing_placement_drops_only_its_panes() {
        let managed = full_managed();
        let mut snapshot = full_snapshot();
        snapshot.tabs.retain(|t| t.tab_id != "w1:t1");

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert_eq!(
            dropped,
            vec![
                ("core/main".to_owned(), PruneReason::NotInSession),
                ("review".to_owned(), PruneReason::NotInSession),
            ]
        );
        assert!(pruned.resources.contains_key("core"));
        assert!(!pruned.resources.contains_key("core/main"));
        assert!(!pruned.resources.contains_key("review"));
        assert!(pruned.resources.contains_key("maintenance"));
    }

    #[test]
    fn prune_missing_pane_drops_only_that_pane() {
        let managed = full_managed();
        let mut snapshot = full_snapshot();
        snapshot.panes.retain(|p| p.pane_id != "w1:p1");

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert_eq!(
            dropped,
            vec![("review".to_owned(), PruneReason::NotInSession)]
        );
        assert!(pruned.resources.contains_key("core"));
        assert!(pruned.resources.contains_key("core/main"));
        assert!(!pruned.resources.contains_key("review"));
    }

    // D55 point 2: a live id whose label/cwd no longer matches what Drove
    // recorded is a reused id, not the resource Drove created.

    #[test]
    fn prune_missing_drops_a_same_id_workspace_whose_live_label_changed() {
        let mut managed = full_managed();
        managed.resources.insert(
            "core".into(),
            ManagedResource {
                label: Some("control".into()),
                ..resource("workspace", "w1", None)
            },
        );
        let mut snapshot = full_snapshot();
        snapshot.workspaces[0].label = "other".into();

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert_eq!(
            dropped,
            vec![
                ("core".to_owned(), PruneReason::IdReused),
                ("core/main".to_owned(), PruneReason::IdReused),
                ("review".to_owned(), PruneReason::IdReused),
            ]
        );
        assert!(!pruned.resources.contains_key("core"));
        // The workspace's own tree is dropped with it, same as a missing id.
        assert!(!pruned.resources.contains_key("core/main"));
        assert!(!pruned.resources.contains_key("review"));
    }

    #[test]
    fn prune_missing_keeps_a_same_id_workspace_whose_live_label_still_matches() {
        let mut managed = full_managed();
        managed.resources.insert(
            "core".into(),
            ManagedResource {
                label: Some("control".into()),
                ..resource("workspace", "w1", None)
            },
        );
        let mut snapshot = full_snapshot();
        snapshot.workspaces[0].label = "control".into();

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert!(dropped.is_empty());
        assert!(pruned.resources.contains_key("core"));
    }

    #[test]
    fn prune_missing_ignores_label_when_none_was_ever_recorded() {
        // A record saved before D55 (label: None) has nothing to compare a
        // live label against, so it is not pruned just because they differ.
        let managed = full_managed();
        let mut snapshot = full_snapshot();
        snapshot.workspaces[0].label = "renamed-outside-drove".into();

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert!(dropped.is_empty());
        assert!(pruned.resources.contains_key("core"));
    }

    #[test]
    fn prune_missing_drops_a_pane_whose_live_cwd_changed() {
        let mut managed = full_managed();
        managed.resources.insert(
            "review".into(),
            ManagedResource {
                cwd: Some("/repo".into()),
                ..resource("pane", "w1:p1", Some("core/main"))
            },
        );
        let mut snapshot = full_snapshot();
        snapshot.panes[0].cwd = Some("/elsewhere".into());

        let (pruned, dropped) = prune_missing(&managed, &snapshot);

        assert_eq!(dropped, vec![("review".to_owned(), PruneReason::IdReused)]);
        assert!(!pruned.resources.contains_key("review"));
        assert!(pruned.resources.contains_key("core"));
    }

    #[test]
    fn reset_stale_target_stamps_a_profile_with_no_prior_target() {
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        state
            .profile_mut("default")
            .resources
            .insert("core".into(), resource("workspace", "w1", None));

        let message = state.reset_stale_target("default", "herdr", Some("a"));

        assert_eq!(message, None);
        assert!(
            state
                .profile("default")
                .expect("profile")
                .resources
                .contains_key("core"),
            "a legacy profile's resources are untouched on first stamp"
        );
        assert_eq!(
            state.profile("default").expect("profile").target,
            Some(StoredTarget {
                backend: "herdr".into(),
                session: Some("a".into()),
            })
        );
    }

    #[test]
    fn reset_stale_target_resets_when_the_stamped_target_differs() {
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        state
            .profile_mut("default")
            .resources
            .insert("core".into(), resource("workspace", "w1", None));
        state.reset_stale_target("default", "herdr", Some("a"));

        let message = state.reset_stale_target("default", "herdr", Some("b"));

        assert_eq!(
            message,
            Some("state recorded for herdr:a; starting fresh for herdr:b".to_owned())
        );
        assert!(
            !state
                .profile("default")
                .expect("profile")
                .resources
                .contains_key("core"),
            "a profile stamped for a different target is treated as empty"
        );
        assert_eq!(
            state.profile("default").expect("profile").target,
            Some(StoredTarget {
                backend: "herdr".into(),
                session: Some("b".into()),
            })
        );
    }

    #[test]
    fn reset_stale_target_is_a_no_op_when_the_target_is_unchanged() {
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        state
            .profile_mut("default")
            .resources
            .insert("core".into(), resource("workspace", "w1", None));
        state.reset_stale_target("default", "herdr", None);

        let message = state.reset_stale_target("default", "herdr", None);

        assert_eq!(message, None);
        assert!(
            state
                .profile("default")
                .expect("profile")
                .resources
                .contains_key("core")
        );
    }

    #[test]
    fn interrupted_reports_only_unfinished_journal_entries() {
        let state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![
                JournalEntry {
                    action: "task:scaffold".into(),
                    digest: "digest-1".into(),
                    completed: false,
                    success: None,
                },
                JournalEntry {
                    action: "task:build".into(),
                    digest: "digest-2".into(),
                    completed: true,
                    success: Some(true),
                },
            ],
            path: PathBuf::new(),
        };

        assert_eq!(state.interrupted(), vec![("task:scaffold", "digest-1")]);
        assert!(state.has_interrupted("task:scaffold"));
        assert!(!state.has_interrupted("task:build"));
        assert!(!state.has_interrupted("task:unknown"));
    }

    #[test]
    fn begin_action_supersedes_a_stale_incomplete_entry_for_the_same_action() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: dir.path().join("state.json"),
        };

        // A first run of `task:scaffold` was killed before `finish_action`
        // ran, leaving a stale `completed: false` entry.
        state
            .begin_action("task:scaffold", "digest-1")
            .expect("begin first attempt");
        assert!(state.has_interrupted("task:scaffold"));

        // A retry begins and this time completes; the stale entry from the
        // killed attempt must stop being reported once the retry starts,
        // not linger forever (CodeRabbit finding on PR #31).
        state
            .begin_action("task:scaffold", "digest-1")
            .expect("begin retry");
        state.finish_action("digest-1", true).expect("finish retry");

        assert!(
            !state.has_interrupted("task:scaffold"),
            "a completed retry must clear the earlier killed attempt too"
        );
        assert!(state.interrupted().is_empty());
    }

    #[test]
    fn local_state_serializes_approvals() {
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        state.approve("abc".to_owned());
        let encoded = serde_json::to_vec(&state).expect("encode");
        let loaded: LocalState = serde_json::from_slice(&encoded).expect("decode");
        assert!(loaded.is_approved("abc"));
    }

    #[test]
    fn rename_resource_migrates_identity_and_round_trips() {
        let mut state = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        state.profile_mut("default").resources.insert(
            "old".to_owned(),
            ManagedResource {
                kind: "pane".into(),
                backend_id: "w1:p1".into(),
                parent: Some("dev/main".into()),
                digest: "digest-1".into(),
                label: None,
                cwd: None,
                adopted: None,
                last_outcome: None,
            },
        );

        state.rename_resource("default", "old", "new");

        assert!(
            !state
                .profile("default")
                .expect("default profile")
                .resources
                .contains_key("old")
        );
        let migrated = state
            .profile("default")
            .expect("default profile")
            .resources
            .get("new")
            .expect("resource under new identity");
        assert_eq!(migrated.backend_id, "w1:p1");
        assert_eq!(migrated.digest, "digest-1");

        let encoded = serde_json::to_vec(&state).expect("encode");
        let loaded: LocalState = serde_json::from_slice(&encoded).expect("decode");
        assert!(
            loaded
                .profile("default")
                .expect("default profile")
                .resources
                .contains_key("new")
        );
        assert!(
            !loaded
                .profile("default")
                .expect("default profile")
                .resources
                .contains_key("old")
        );
    }

    #[test]
    fn load_from_a_missing_file_starts_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let state = LocalState::load_from(path.clone(), Path::new("/repo")).expect("load");
        assert!(state.profiles.is_empty());
        assert_eq!(state.path, path);
    }

    #[test]
    fn load_from_a_corrupt_file_moves_it_aside_and_starts_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        fs::write(&path, b"{not valid json").expect("write corrupt state");

        let state = LocalState::load_from(path.clone(), Path::new("/repo")).expect("load");

        assert!(state.profiles.is_empty());
        assert!(!path.exists(), "corrupt file should have been moved aside");
        let siblings: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            siblings
                .iter()
                .any(|name| name.starts_with("state.json.corrupt-")),
            "expected a state.json.corrupt-* sibling, found {siblings:?}"
        );
    }

    #[test]
    fn load_from_a_file_missing_schema_version_still_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        fs::write(&path, br#"{"repo_root": "/repo"}"#).expect("write state");

        let state = LocalState::load_from(path, Path::new("/repo")).expect("load");

        assert_eq!(state.schema_version, 0);
        assert!(state.profiles.is_empty());
    }
}
