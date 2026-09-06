//! Canonical desired-state model (schema version 2) produced by a `Drovefile`.
//!
//! Five resource kinds — workspace, tab, pane, agent, task — share one
//! profile-scoped namespace for `after`, `on_*` and adoption references.
//! See `docs/superpowers/specs/2026-09-06-drove-v2-design.md` §3-4.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DroveConfig {
    pub schema_version: u32,
    pub profiles: BTreeMap<String, Profile>,
}

impl DroveConfig {
    pub fn new(profiles: Vec<Profile>) -> Result<Self> {
        let mut by_name = BTreeMap::new();
        for profile in profiles {
            validate_name("profile", &profile.name)?;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        let mut workspace_names = BTreeSet::new();
        let mut pane_names = BTreeSet::new();
        let mut agent_names = BTreeSet::new();
        let mut adopt_count = 0usize;

        for workspace in &self.workspaces {
            validate_name("workspace", &workspace.name)?;
            if !workspace_names.insert(workspace.name.as_str()) {
                bail!("duplicate workspace name `{}`", workspace.name);
            }

            let mut tab_names = BTreeSet::new();
            for tab in &workspace.tabs {
                // Tabs are a Herdr placement hint, not part of the shared
                // `after`/adoption namespace (spec §3-4), so their name is
                // a free-form label rather than the strict identifier used
                // for workspace/pane/agent/task names.
                if tab.name.is_empty() || tab.name.len() > 64 {
                    bail!("tab name `{}` must be 1-64 characters", tab.name);
                }
                if !tab_names.insert(tab.name.as_str()) {
                    bail!(
                        "duplicate tab name `{}` in workspace `{}`",
                        tab.name,
                        workspace.name
                    );
                }
                tab.validate()?;
                for pane in &tab.panes {
                    validate_name("pane", &pane.name)?;
                    if !pane_names.insert(pane.name.as_str()) {
                        bail!("duplicate pane name `{}` in profile", pane.name);
                    }
                    if let Some(agent) = &pane.agent {
                        agent.validate()?;
                        if let Some(agent_name) = &agent.name
                            && (pane_names.contains(agent_name.as_str())
                                || !agent_names.insert(agent_name.as_str()))
                        {
                            bail!(
                                "agent `{agent_name}` collides with another resource of the same name in the shared `after` namespace"
                            );
                        }
                    }
                    if let Some(adopt) = &pane.adopt {
                        if adopt != "caller" {
                            bail!(
                                "pane `{}` declares adopt = `{adopt}`; only `caller` is supported",
                                pane.name
                            );
                        }
                        adopt_count += 1;
                    }
                }
            }
        }
        if adopt_count > 1 {
            bail!("only one pane per profile may declare `adopt = \"caller\"`");
        }

        let mut task_names = BTreeSet::new();
        for task in &self.tasks {
            validate_name("task", &task.name)?;
            if !task_names.insert(task.name.as_str()) {
                bail!("duplicate task name `{}` in profile", task.name);
            }
            if pane_names.contains(task.name.as_str()) || agent_names.contains(task.name.as_str()) {
                bail!(
                    "task `{}` collides with a pane or agent of the same name in the shared `after` namespace",
                    task.name
                );
            }
            if task.run.is_empty() {
                bail!("task `{}` requires a non-empty `run` argv", task.name);
            }
        }

        self.validate_after(&pane_names, &agent_names, &task_names)
    }

    fn validate_after(
        &self,
        pane_names: &BTreeSet<&str>,
        agent_names: &BTreeSet<&str>,
        task_names: &BTreeSet<&str>,
    ) -> Result<()> {
        let mut edges: BTreeMap<&str, &[String]> = BTreeMap::new();
        for workspace in &self.workspaces {
            for tab in &workspace.tabs {
                for pane in &tab.panes {
                    edges.insert(pane.name.as_str(), pane.after.as_slice());
                }
            }
        }
        for task in &self.tasks {
            edges.insert(task.name.as_str(), task.after.as_slice());
        }

        let known = || {
            pane_names
                .iter()
                .copied()
                .chain(agent_names.iter().copied())
                .chain(task_names.iter().copied())
        };
        for (id, targets) in &edges {
            for target in *targets {
                if !known().any(|name| name == target) {
                    bail!("`{id}` declares `after = [\"{target}\"]` for an unknown resource");
                }
            }
        }

        fn visit<'a>(
            id: &'a str,
            edges: &BTreeMap<&'a str, &'a [String]>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                bail!("`after` dependency cycle includes `{id}`");
            }
            if let Some(targets) = edges.get(id) {
                for target in *targets {
                    visit(target, edges, visiting, visited)?;
                }
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }

        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in edges.keys().copied() {
            visit(id, &edges, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        canonical_digest(self)
    }

    pub fn to_ir(&self) -> crate::ir::Ir {
        crate::ir::to_ir(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

impl Workspace {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tab {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub split: SplitDirection,
    #[serde(default)]
    pub ratios: Vec<f64>,
    #[serde(default)]
    pub panes: Vec<Pane>,
}

impl Tab {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    fn validate(&self) -> Result<()> {
        let expected = self.panes.len().saturating_sub(1);
        if self.ratios.len() != expected {
            bail!(
                "tab `{}` declares {} ratio(s) for {} pane(s); expected {expected}",
                self.name,
                self.ratios.len(),
                self.panes.len()
            );
        }
        for ratio in &self.ratios {
            if !(0.05..=0.95).contains(ratio) {
                bail!(
                    "tab `{}` ratio {ratio} must be between 0.05 and 0.95",
                    self.name
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    #[default]
    Right,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pane {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `any_of` candidates, tried in order; a plain `serve = [...]` argv is
    /// normalized to a single-candidate list at compile time.
    #[serde(default)]
    pub serve: Vec<Vec<String>>,
    #[serde(default)]
    pub ready: Option<Readiness>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub adopt: Option<String>,
    #[serde(default)]
    pub agent: Option<Agent>,
    #[serde(default)]
    pub on_start: Option<Vec<String>>,
    #[serde(default)]
    pub on_stop: Option<Vec<String>>,
}

impl Pane {
    pub fn label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Readiness {
    Output { value: String },
    Port { value: u16 },
    Cmd { value: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    #[serde(default)]
    pub name: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Resolved at compile time: either the literal inline string, or the
    /// contents of a `file("repo/relative/path")` reference (D25).
    #[serde(default)]
    pub prompt: Option<String>,
}

const PROMPT_MAX_BYTES: usize = 2 * 1024;

impl Agent {
    fn validate(&self) -> Result<()> {
        if self.kind.is_empty() {
            bail!("agent declares an empty `kind`");
        }
        if let Some(name) = &self.name {
            validate_name("agent", name)?;
        }
        if let Some(prompt) = &self.prompt
            && prompt.len() > PROMPT_MAX_BYTES
        {
            bail!(
                "agent `{}` prompt exceeds {PROMPT_MAX_BYTES} bytes",
                self.kind
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub name: String,
    pub run: Vec<String>,
    #[serde(default)]
    pub check: Option<Vec<String>>,
    #[serde(default)]
    pub inputs: Vec<PathBuf>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default = "default_true")]
    pub auto: bool,
    #[serde(default)]
    pub on_start: Option<Vec<String>>,
    #[serde(default)]
    pub on_stop: Option<Vec<String>>,
}

pub fn canonical_digest<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    let mut bytes = name.bytes();
    let starts_ok = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
    let rest_ok = name.len() <= 32
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
    if !starts_ok || !rest_ok {
        bail!("{kind} name `{name}` must match [a-z][a-z0-9_-]{{0,31}}");
    }
    Ok(())
}

fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn profile_from(value: serde_json::Value) -> Profile {
        serde_json::from_value(value).expect("profile fixture")
    }

    #[test]
    fn rejects_invalid_names() {
        // Name syntax is checked by `DroveConfig::new`, which validates the
        // profile name itself before delegating into `Profile::validate`.
        let profile = profile_from(json!({
            "name": "Default",
            "workspaces": []
        }));
        let error = DroveConfig::new(vec![profile]).expect_err("invalid name");
        assert!(error.to_string().contains("must match"));
    }

    #[test]
    fn rejects_duplicate_panes_across_profile() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [
                    {"name": "one", "panes": [{"name": "same"}]},
                    {"name": "two", "panes": [{"name": "same"}]}
                ]
            }]
        }));
        let error = profile.validate().expect_err("duplicate should fail");
        assert!(error.to_string().contains("duplicate pane name"));
    }

    #[test]
    fn rejects_duplicate_workspace_names() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [
                {"name": "dev", "tabs": []},
                {"name": "dev", "tabs": []}
            ]
        }));
        let error = profile.validate().expect_err("duplicate workspace");
        assert!(error.to_string().contains("duplicate workspace name"));
    }

    #[test]
    fn rejects_second_adopt_in_profile() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "ratios": [0.5],
                    "panes": [{"name": "one", "adopt": "caller"}, {"name": "two"}]
                }, {
                    "name": "second",
                    "panes": [{"name": "three", "adopt": "caller"}]
                }]
            }]
        }));
        let error = profile.validate().expect_err("second adopt should fail");
        assert!(error.to_string().contains("only one pane per profile"));
    }

    #[test]
    fn rejects_unsupported_adopt_value() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "one", "adopt": "someone-else"}]}]
            }]
        }));
        let error = profile.validate().expect_err("bad adopt value");
        assert!(error.to_string().contains("only `caller` is supported"));
    }

    #[test]
    fn rejects_wrong_ratio_count() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "ratios": [0.5, 0.5],
                    "panes": [{"name": "one"}, {"name": "two"}]
                }]
            }]
        }));
        let error = profile.validate().expect_err("wrong ratio count");
        assert!(error.to_string().contains("expected 1"));
    }

    #[test]
    fn rejects_ratio_out_of_range() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "ratios": [0.99],
                    "panes": [{"name": "one"}, {"name": "two"}]
                }]
            }]
        }));
        let error = profile.validate().expect_err("out of range ratio");
        assert!(error.to_string().contains("between 0.05 and 0.95"));
    }

    #[test]
    fn rejects_after_targeting_unknown_resource() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "one", "after": ["missing"]}]}]
            }]
        }));
        let error = profile.validate().expect_err("unknown after target");
        assert!(error.to_string().contains("unknown resource"));
    }

    #[test]
    fn rejects_after_cycle() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [
                {"name": "a", "run": ["true"], "after": ["b"]},
                {"name": "b", "run": ["true"], "after": ["a"]}
            ]
        }));
        let error = profile.validate().expect_err("cycle");
        assert!(error.to_string().contains("dependency cycle"));
    }

    #[test]
    fn accepts_valid_after_dag_across_panes_and_tasks() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "one", "after": ["scaffold"]}]}]
            }],
            "tasks": [{"name": "scaffold", "run": ["true"]}]
        }));
        profile.validate().expect("valid DAG");
    }

    #[test]
    fn rejects_task_missing_run() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [{"name": "empty", "run": []}]
        }));
        let error = profile.validate().expect_err("empty run");
        assert!(error.to_string().contains("non-empty `run`"));
    }

    #[test]
    fn rejects_duplicate_task_name() {
        let profile = profile_from(json!({
            "name": "default",
            "tasks": [
                {"name": "scaffold", "run": ["true"]},
                {"name": "scaffold", "run": ["true"]}
            ]
        }));
        let error = profile.validate().expect_err("duplicate task name");
        assert!(error.to_string().contains("duplicate task name"));
    }

    #[test]
    fn rejects_task_name_colliding_with_pane_name() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "panes": [{"name": "scaffold"}]}]
            }],
            "tasks": [{"name": "scaffold", "run": ["true"]}]
        }));
        let error = profile.validate().expect_err("name collision");
        assert!(error.to_string().contains("collides with a pane"));
    }

    #[test]
    fn rejects_agent_name_colliding_with_pane_name() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "ratios": [0.5], "panes": [
                    {"name": "review"},
                    {"name": "worker", "agent": {"name": "review", "kind": "claude"}}
                ]}]
            }]
        }));
        let error = profile.validate().expect_err("agent/pane name collision");
        assert!(error.to_string().contains("collides with another resource"));
    }

    #[test]
    fn agent_name_is_a_valid_after_target() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{"name": "main", "ratios": [0.5], "panes": [
                    {"name": "worker", "agent": {"name": "review", "kind": "claude"}},
                    {"name": "follower", "after": ["review"]}
                ]}]
            }]
        }));
        profile
            .validate()
            .expect("agent name is a known after target");
    }

    #[test]
    fn rejects_duplicate_profile_name() {
        let profile = Profile {
            name: "default".into(),
            workspaces: vec![],
            tasks: vec![],
        };
        let error =
            DroveConfig::new(vec![profile.clone(), profile]).expect_err("duplicate profile name");
        assert!(error.to_string().contains("duplicate profile name"));
    }

    #[test]
    fn requires_a_default_profile() {
        let profile = Profile {
            name: "other".into(),
            workspaces: vec![],
            tasks: vec![],
        };
        let error = DroveConfig::new(vec![profile]).expect_err("missing default profile");
        assert!(
            error
                .to_string()
                .contains("must declare a `default` profile")
        );
    }

    #[test]
    fn rejects_oversized_inline_prompt() {
        let profile = profile_from(json!({
            "name": "default",
            "workspaces": [{
                "name": "dev",
                "tabs": [{
                    "name": "main",
                    "panes": [{
                        "name": "review",
                        "agent": {"kind": "claude", "prompt": "x".repeat(3000)}
                    }]
                }]
            }]
        }));
        let error = profile.validate().expect_err("oversized prompt");
        assert!(error.to_string().contains("exceeds"));
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
}
