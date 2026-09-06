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
        });
        self.trim_journal();
        self.save()
    }

    pub fn finish_action(&mut self, digest: &str) -> Result<()> {
        if let Some(entry) = self
            .journal
            .iter_mut()
            .rev()
            .find(|entry| entry.digest == digest && !entry.completed)
        {
            entry.completed = true;
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
    #[serde(default)]
    pub workspaces: BTreeMap<String, ManagedWorkspace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedWorkspace {
    pub workspace_id: String,
    pub desired_digest: String,
    #[serde(default)]
    pub tabs: BTreeMap<String, ManagedTab>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedTab {
    pub tab_id: String,
    pub desired_digest: String,
    #[serde(default)]
    pub panes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub action: String,
    pub digest: String,
    pub completed: bool,
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
