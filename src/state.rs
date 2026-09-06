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
    /// One entry per resource identity (D5: a plain name, except a tab's
    /// `workspace/tab`), the source of ownership `to_snapshot` reads back
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedResource {
    /// IR resource kind (`workspace`, `tab`, `pane`, `agent`, `task`).
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
}
