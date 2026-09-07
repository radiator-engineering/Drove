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
    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = state_path(repo_root);
        match fs::read(&path) {
            Ok(bytes) => {
                let mut state: Self =
                    serde_json::from_slice(&bytes).context("invalid Drove local state")?;
                state.path = path;
                Ok(state)
            }
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
    /// One entry per resource identity (D5: a plain name; a Herdr placement
    /// group's identity is `workspace/<name>` and its digest is the group's
    /// topology digest, D30), the source of ownership `to_snapshot` reads back
    /// (spec §5: "observed digest at apply time, runtime ids").
    #[serde(default)]
    pub resources: BTreeMap<String, ManagedResource>,
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

/// Prunes a managed profile against the live backend snapshot (D48): a
/// `workspace`, `placement` or `pane` resource whose recorded `backend_id`
/// is absent from the snapshot's matching id set is gone, and is dropped
/// from the returned copy together with what it carried. A missing
/// workspace also drops every placement and pane whose `backend_id` is
/// namespaced under it (Herdr ids nest `<workspace>:t1`, `<workspace>:p1`
/// directly off the workspace id, not off the tab); a missing placement
/// drops the panes recorded under it by identity (`ManagedResource::parent`
/// holds the placement's identity, not its backend id, so cascading here
/// cannot use the same prefix trick). `agent` and `task` resources are left
/// alone: their own `backend_id`/`parent` are not part of this workspace
/// tree (a task has no `backend_id` at all), so they are out of scope for
/// this prune. Returns the pruned profile and the dropped identities,
/// sorted.
pub fn prune_missing(
    managed: &ManagedProfile,
    snapshot: &crate::backend::herdr::SessionSnapshot,
) -> (ManagedProfile, Vec<String>) {
    let workspace_ids: BTreeSet<&str> = snapshot
        .workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.as_str())
        .collect();
    let tab_ids: BTreeSet<&str> = snapshot
        .tabs
        .iter()
        .map(|tab| tab.tab_id.as_str())
        .collect();
    let pane_ids: BTreeSet<&str> = snapshot
        .panes
        .iter()
        .map(|pane| pane.pane_id.as_str())
        .collect();

    let mut dropped = BTreeSet::new();
    let mut missing_workspace_backend_ids = BTreeSet::new();
    let mut missing_placement_identities = BTreeSet::new();

    for (identity, resource) in &managed.resources {
        match resource.kind.as_str() {
            "workspace" if !workspace_ids.contains(resource.backend_id.as_str()) => {
                missing_workspace_backend_ids.insert(resource.backend_id.clone());
                dropped.insert(identity.clone());
            }
            "placement" if !tab_ids.contains(resource.backend_id.as_str()) => {
                missing_placement_identities.insert(identity.clone());
                dropped.insert(identity.clone());
            }
            "pane" if !pane_ids.contains(resource.backend_id.as_str()) => {
                dropped.insert(identity.clone());
            }
            _ => {}
        }
    }

    for (identity, resource) in &managed.resources {
        match resource.kind.as_str() {
            "placement" | "pane" => {
                let under_missing_workspace = missing_workspace_backend_ids.iter().any(|ws| {
                    resource
                        .backend_id
                        .strip_prefix(ws.as_str())
                        .is_some_and(|rest| rest.starts_with(':'))
                });
                if under_missing_workspace {
                    dropped.insert(identity.clone());
                }
            }
            _ => {}
        }
        if resource.kind == "pane"
            && let Some(parent) = &resource.parent
            && missing_placement_identities.contains(parent)
        {
            dropped.insert(identity.clone());
        }
    }

    let mut pruned = managed.clone();
    pruned
        .resources
        .retain(|identity, _| !dropped.contains(identity));

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

        assert_eq!(dropped, vec!["core", "core/main", "review"]);
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

        assert_eq!(dropped, vec!["core/main", "review"]);
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

        assert_eq!(dropped, vec!["review"]);
        assert!(pruned.resources.contains_key("core"));
        assert!(pruned.resources.contains_key("core/main"));
        assert!(!pruned.resources.contains_key("review"));
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
}
