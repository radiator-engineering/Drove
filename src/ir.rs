//! Canonical intermediate representation (schema version 2): a flat, ordered
//! list of typed resources. Starlark compiles to it; backends consume only
//! it; `drove render` prints it verbatim.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{Profile, canonical_digest};

pub const IR_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ir {
    pub schema_version: u32,
    pub profile: String,
    pub resources: Vec<Resource>,
}

impl Ir {
    pub fn from_json(text: &str) -> Result<Self> {
        serde_json::from_str(text).context("invalid Drove IR document")
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// One resource in the flat IR. `digest` is the SHA-256 of the resource's own
/// canonical JSON content plus its children's digests (D21): backend ids and
/// the repository path are never part of it, and `name`/`parent` (identity,
/// not content) are excluded too so a rename does not look like a content
/// change. Every declared field, and the digests of any children, do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    pub kind: String,
    pub name: String,
    pub parent: Option<String>,
    pub digest: String,
    pub fields: Value,
}

fn content_digest(fields: &Value, children: &[&str]) -> Result<String> {
    canonical_digest(&json!({"fields": fields, "children": children}))
}

pub fn to_ir(profile: &Profile) -> Ir {
    to_ir_inner(profile).expect("resource content is always serializable JSON")
}

fn to_ir_inner(profile: &Profile) -> Result<Ir> {
    let mut resources = Vec::new();

    for workspace in &profile.workspaces {
        let mut tab_digests = Vec::new();
        let mut workspace_children_resources = Vec::new();

        for tab in &workspace.tabs {
            let tab_address = format!("{}/{}", workspace.name, tab.name);
            let mut pane_digests = Vec::new();
            let mut tab_children_resources = Vec::new();

            for pane in &tab.panes {
                let mut agent_digest = None;
                if let Some(agent) = &pane.agent {
                    let agent_fields = json!({
                        "kind": agent.kind,
                        "args": agent.args,
                        "prompt": agent.prompt,
                    });
                    let digest = content_digest(&agent_fields, &[])?;
                    let name = agent
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{}-agent", pane.name));
                    agent_digest = Some(digest.clone());
                    tab_children_resources.push(Resource {
                        kind: "agent".into(),
                        name,
                        parent: Some(pane.name.clone()),
                        digest,
                        fields: agent_fields,
                    });
                }

                let pane_fields = json!({
                    "label": pane.label,
                    "cwd": pane.cwd,
                    "env": pane.env,
                    "serve": pane.serve,
                    "ready": pane.ready,
                    "after": pane.after,
                    "adopt": pane.adopt,
                    "on_start": pane.on_start,
                    "on_stop": pane.on_stop,
                });
                let agent_children: Vec<&str> = agent_digest.as_deref().into_iter().collect();
                let pane_digest = content_digest(&pane_fields, &agent_children)?;
                pane_digests.push(pane_digest.clone());
                tab_children_resources.push(Resource {
                    kind: "pane".into(),
                    name: pane.name.clone(),
                    parent: Some(tab_address.clone()),
                    digest: pane_digest,
                    fields: pane_fields,
                });
            }

            let tab_fields = json!({
                "label": tab.label,
                "split": tab.split,
                "ratios": tab.ratios,
                "panes": tab.panes.iter().map(|pane| pane.name.clone()).collect::<Vec<_>>(),
            });
            let pane_digest_refs: Vec<&str> = pane_digests.iter().map(String::as_str).collect();
            let tab_digest = content_digest(&tab_fields, &pane_digest_refs)?;
            tab_digests.push(tab_digest.clone());

            workspace_children_resources.push(Resource {
                kind: "tab".into(),
                name: tab.name.clone(),
                parent: Some(workspace.name.clone()),
                digest: tab_digest,
                fields: tab_fields,
            });
            workspace_children_resources.extend(tab_children_resources);
        }

        let workspace_fields = json!({
            "label": workspace.label,
            "cwd": workspace.cwd,
            "env": workspace.env,
        });
        let tab_digest_refs: Vec<&str> = tab_digests.iter().map(String::as_str).collect();
        let workspace_digest = content_digest(&workspace_fields, &tab_digest_refs)?;

        resources.push(Resource {
            kind: "workspace".into(),
            name: workspace.name.clone(),
            parent: None,
            digest: workspace_digest,
            fields: workspace_fields,
        });
        resources.extend(workspace_children_resources);
    }

    for task in &profile.tasks {
        let fields = json!({
            "run": task.run,
            "check": task.check,
            "inputs": task.inputs,
            "after": task.after,
            "auto": task.auto,
            "on_start": task.on_start,
            "on_stop": task.on_stop,
        });
        let digest = content_digest(&fields, &[])?;
        resources.push(Resource {
            kind: "task".into(),
            name: task.name.clone(),
            parent: None,
            digest,
            fields,
        });
    }

    resources.sort_by(|left, right| {
        kind_rank(&left.kind)
            .cmp(&kind_rank(&right.kind))
            .then_with(|| address(left).cmp(&address(right)))
    });

    Ok(Ir {
        schema_version: IR_SCHEMA_VERSION,
        profile: profile.name.clone(),
        resources,
    })
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "workspace" => 0,
        "tab" => 1,
        "pane" => 2,
        "agent" => 3,
        "task" => 4,
        _ => 5,
    }
}

fn address(resource: &Resource) -> String {
    match &resource.parent {
        Some(parent) => format!("{parent}/{}", resource.name),
        None => resource.name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{Agent, Pane, Profile, Tab, Workspace};

    fn sample_profile() -> Profile {
        Profile {
            name: "default".into(),
            workspaces: vec![Workspace {
                name: "dev".into(),
                label: None,
                cwd: ".".into(),
                env: Default::default(),
                tabs: vec![Tab {
                    name: "main".into(),
                    label: None,
                    split: Default::default(),
                    ratios: vec![],
                    panes: vec![Pane {
                        name: "review".into(),
                        label: None,
                        cwd: None,
                        env: Default::default(),
                        serve: vec![],
                        ready: None,
                        after: vec![],
                        adopt: None,
                        agent: Some(Agent {
                            name: None,
                            kind: "claude".into(),
                            args: vec![],
                            prompt: Some("hello".into()),
                        }),
                        on_start: None,
                        on_stop: None,
                    }],
                }],
            }],
            tasks: vec![],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let ir = sample_profile().to_ir();
        let text = ir.to_json_pretty().expect("serialize");
        let parsed = Ir::from_json(&text).expect("parse");
        assert_eq!(ir, parsed);
    }

    #[test]
    fn ordering_is_deterministic() {
        let ir = sample_profile().to_ir();
        let kinds: Vec<&str> = ir.resources.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["workspace", "tab", "pane", "agent"]);
    }

    #[test]
    fn digest_is_stable_under_key_order() {
        let a = json!({"a": 1, "b": 2});
        let b = json!({"b": 2, "a": 1});
        assert_eq!(
            content_digest(&a, &["child"]).expect("digest"),
            content_digest(&b, &["child"]).expect("digest")
        );
    }

    #[test]
    fn digest_ignores_identity_but_not_content() {
        let profile = sample_profile();
        let ir = profile.to_ir();
        let pane = ir
            .resources
            .iter()
            .find(|r| r.kind == "pane")
            .expect("pane resource");

        let mut renamed = profile.clone();
        renamed.workspaces[0].tabs[0].panes[0].name = "renamed".into();
        let renamed_ir = renamed.to_ir();
        let renamed_pane = renamed_ir
            .resources
            .iter()
            .find(|r| r.kind == "pane")
            .expect("pane resource");
        assert_eq!(
            pane.digest, renamed_pane.digest,
            "renaming a resource must not change its content digest"
        );

        let mut changed = profile;
        changed.workspaces[0].tabs[0].panes[0]
            .env
            .insert("K".into(), "V".into());
        let changed_ir = changed.to_ir();
        let changed_pane = changed_ir
            .resources
            .iter()
            .find(|r| r.kind == "pane")
            .expect("pane resource");
        assert_ne!(
            pane.digest, changed_pane.digest,
            "a declared field change must change the content digest"
        );
    }

    #[test]
    fn parent_digest_changes_when_a_child_changes() {
        let profile = sample_profile();
        let ir = profile.to_ir();
        let tab = ir
            .resources
            .iter()
            .find(|r| r.kind == "tab")
            .expect("tab resource");
        let workspace = ir
            .resources
            .iter()
            .find(|r| r.kind == "workspace")
            .expect("workspace resource");

        let mut changed = profile;
        changed.workspaces[0].tabs[0].panes[0]
            .agent
            .as_mut()
            .expect("agent")
            .prompt = Some("bye".into());
        let changed_ir = changed.to_ir();
        let changed_tab = changed_ir
            .resources
            .iter()
            .find(|r| r.kind == "tab")
            .expect("tab resource");
        let changed_workspace = changed_ir
            .resources
            .iter()
            .find(|r| r.kind == "workspace")
            .expect("workspace resource");

        assert_ne!(tab.digest, changed_tab.digest);
        assert_ne!(workspace.digest, changed_workspace.digest);
    }
}
