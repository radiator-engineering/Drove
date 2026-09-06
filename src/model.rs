//! Canonical desired-state model produced by a `Drovefile`.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DroveConfig {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, Profile>,
}

impl DroveConfig {
    pub fn new(profiles: Vec<Profile>) -> Result<Self> {
        let mut by_name = BTreeMap::new();
        for profile in profiles {
            validate_id("profile", &profile.name)?;
            if by_name.insert(profile.name.clone(), profile).is_some() {
                bail!("duplicate profile name");
            }
        }
        if !by_name.contains_key("default") {
            bail!("Drovefile must declare a `default` profile");
        }
        let config = Self {
            schema_version: SCHEMA_VERSION,
            profiles: by_name,
        };
        for profile in config.profiles.values() {
            profile.validate()?;
        }
        Ok(config)
    }

    pub fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles
            .get(name)
            .with_context(|| format!("profile `{name}` is not declared"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSpec>,
    #[serde(default)]
    pub agents: Vec<AgentSpec>,
    #[serde(default)]
    pub bootstrap: Vec<BootstrapTask>,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        let mut workspace_ids = BTreeSet::new();
        let mut pane_ids = BTreeSet::new();

        for workspace in &self.workspaces {
            validate_id("workspace", &workspace.id)?;
            validate_relative_path("workspace cwd", &workspace.cwd)?;
            if !workspace_ids.insert(workspace.id.as_str()) {
                bail!("duplicate workspace id `{}`", workspace.id);
            }

            let mut tab_ids = BTreeSet::new();
            for tab in &workspace.tabs {
                validate_id("tab", &tab.id)?;
                if !tab_ids.insert(tab.id.as_str()) {
                    bail!(
                        "duplicate tab id `{}` in workspace `{}`",
                        tab.id,
                        workspace.id
                    );
                }
                tab.layout.validate(&mut pane_ids)?;
            }
        }

        let mut agent_ids = BTreeSet::new();
        for agent in &self.agents {
            validate_id("agent", &agent.id)?;
            if !agent_ids.insert(agent.id.as_str()) {
                bail!("duplicate agent id `{}`", agent.id);
            }
            if !pane_ids.contains(agent.pane.as_str()) {
                bail!(
                    "agent `{}` references unknown pane `{}`",
                    agent.id,
                    agent.pane
                );
            }
        }

        validate_bootstrap(&self.bootstrap)
    }

    pub fn digest(&self) -> Result<String> {
        canonical_digest(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSpec {
    pub id: String,
    pub label: String,
    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,
    #[serde(default)]
    pub tabs: Vec<TabSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TabSpec {
    pub id: String,
    pub label: String,
    pub layout: LayoutNode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LayoutNode {
    Pane {
        id: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        command: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Split {
        direction: SplitDirection,
        ratio: f64,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    fn validate<'a>(&'a self, pane_ids: &mut BTreeSet<&'a str>) -> Result<()> {
        match self {
            Self::Pane {
                id, cwd, command, ..
            } => {
                validate_id("pane", id)?;
                if !pane_ids.insert(id) {
                    bail!("duplicate pane id `{id}` in profile");
                }
                if let Some(cwd) = cwd {
                    validate_relative_path("pane cwd", cwd)?;
                }
                if command.first().is_some_and(String::is_empty) {
                    bail!("pane `{id}` command executable cannot be empty");
                }
            }
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !(0.05..=0.95).contains(ratio) {
                    bail!("split ratio must be between 0.05 and 0.95");
                }
                first.validate(pane_ids)?;
                second.validate(pane_ids)?;
            }
        }
        Ok(())
    }

    pub fn to_herdr_json(&self, repo_root: &Path, workspace_cwd: &Path) -> Value {
        match self {
            Self::Pane {
                label,
                cwd,
                command,
                env,
                ..
            } => {
                let cwd = normalize_path(
                    &repo_root
                        .join(workspace_cwd)
                        .join(cwd.as_deref().unwrap_or_else(|| Path::new("."))),
                );
                let mut pane = json!({
                    "type": "pane",
                    "cwd": cwd,
                });
                if let Some(label) = label {
                    pane["label"] = json!(label);
                }
                if !command.is_empty() {
                    pane["command"] = json!(command);
                }
                if !env.is_empty() {
                    pane["env"] = json!(env);
                }
                pane
            }
            Self::Split {
                direction,
                ratio,
                first,
                second,
            } => json!({
                "type": "split",
                "direction": direction,
                "ratio": ratio,
                "first": first.to_herdr_json(repo_root, workspace_cwd),
                "second": second.to_herdr_json(repo_root, workspace_cwd),
            }),
        }
    }

    pub fn pane_ids(&self, result: &mut Vec<String>) {
        match self {
            Self::Pane { id, .. } => result.push(id.clone()),
            Self::Split { first, second, .. } => {
                first.pane_ids(result);
                second.pane_ids(result);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub id: String,
    pub pane: String,
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BootstrapTask {
    pub id: String,
    pub check: Vec<String>,
    pub run: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl BootstrapTask {
    pub fn digest(&self, repo_root: &Path, source_digest: &str) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(SCHEMA_VERSION.to_le_bytes());
        hasher.update(repo_root.to_string_lossy().as_bytes());
        hasher.update(source_digest.as_bytes());
        hasher.update(serde_json::to_vec(self)?);
        for input in &self.inputs {
            validate_relative_path("bootstrap input", input)?;
            let path = repo_root.join(input);
            let canonical = path
                .canonicalize()
                .with_context(|| format!("failed to resolve bootstrap input {}", path.display()))?;
            if !canonical.starts_with(repo_root) {
                bail!(
                    "bootstrap input `{}` escapes the repository",
                    input.display()
                );
            }
            hasher.update(input.to_string_lossy().as_bytes());
            hasher.update(std::fs::read(&canonical).with_context(|| {
                format!("failed to read bootstrap input {}", canonical.display())
            })?);
        }
        Ok(hex::encode(hasher.finalize()))
    }
}

fn validate_bootstrap(tasks: &[BootstrapTask]) -> Result<()> {
    let by_id: BTreeMap<&str, &BootstrapTask> =
        tasks.iter().map(|task| (task.id.as_str(), task)).collect();
    if by_id.len() != tasks.len() {
        bail!("duplicate bootstrap task id");
    }
    for task in tasks {
        validate_id("bootstrap task", &task.id)?;
        if task.check.is_empty() || task.run.is_empty() {
            bail!(
                "bootstrap task `{}` requires non-empty check and run argv",
                task.id
            );
        }
        for dependency in &task.depends_on {
            if !by_id.contains_key(dependency.as_str()) {
                bail!(
                    "bootstrap task `{}` depends on unknown task `{dependency}`",
                    task.id
                );
            }
        }
        for input in &task.inputs {
            validate_relative_path("bootstrap input", input)?;
        }
    }

    fn visit<'a>(
        id: &'a str,
        tasks: &BTreeMap<&'a str, &'a BootstrapTask>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            bail!("bootstrap dependency cycle includes `{id}`");
        }
        for dependency in &tasks[id].depends_on {
            visit(dependency, tasks, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in by_id.keys().copied() {
        visit(id, &by_id, &mut visiting, &mut visited)?;
    }
    Ok(())
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_id(kind: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{kind} id `{id}` must match [A-Za-z0-9_-]{{1,64}}");
    }
    Ok(())
}

fn validate_relative_path(kind: &str, path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!(
            "{kind} `{}` must stay inside the repository",
            path.display()
        );
    }
    Ok(())
}

fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

fn normalize_path(path: &Path) -> PathBuf {
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_panes_across_profile() {
        let profile: Profile = serde_json::from_value(json!({
            "name": "default",
            "workspaces": [{
                "id": "dev",
                "label": "dev",
                "tabs": [
                    {"id": "one", "label": "one", "layout": {"type": "pane", "id": "same"}},
                    {"id": "two", "label": "two", "layout": {"type": "pane", "id": "same"}}
                ]
            }]
        }))
        .expect("profile fixture");
        let error = profile.validate().expect_err("duplicate should fail");
        assert!(error.to_string().contains("duplicate pane id"));
    }

    #[test]
    fn canonical_digest_is_stable() {
        let one = BTreeMap::from([("a", 1), ("b", 2)]);
        let two = BTreeMap::from([("b", 2), ("a", 1)]);
        assert_eq!(
            canonical_digest(&one).expect("digest"),
            canonical_digest(&two).expect("digest")
        );
    }

    #[test]
    fn bootstrap_digest_tracks_input_bytes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().canonicalize().expect("canonical root");
        std::fs::write(root.join("setup.sh"), "one").expect("write input");
        let task = BootstrapTask {
            id: "setup".into(),
            check: vec!["tool".into(), "check".into()],
            run: vec!["tool".into(), "apply".into()],
            inputs: vec![PathBuf::from("setup.sh")],
            depends_on: vec![],
        };
        let first = task.digest(&root, "source").expect("digest");
        std::fs::write(root.join("setup.sh"), "two").expect("change input");
        let second = task.digest(&root, "source").expect("digest");
        assert_ne!(first, second);
    }
}
