//! Canonical intermediate representation (schema version 3): a flat, ordered
//! list of typed resources — workspace, pane, agent, task — plus the derived
//! Herdr placement groups. Starlark compiles to it; backends consume only
//! it; `drove render` prints it verbatim.
//!
//! A pane carries an optional [`Placement`] (D29). The core resource list has
//! no group resource kind: a Herdr group is derived by the Herdr flavor from
//! the placements of the panes that name it (D29), and each group carries a
//! *topology* digest over its shape, kept separate from every pane's
//! *content* digest so a pane can move between groups without its content
//! looking changed (D30).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backend::Split;
use crate::model::{Profile, canonical_digest};

pub const IR_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ir {
    pub schema_version: u32,
    pub profile: String,
    pub resources: Vec<Resource>,
    /// Herdr placement groups, derived from the placements of the panes that
    /// name them (D29). Empty for a profile whose panes declare no placement.
    #[serde(default)]
    pub placements: Vec<PlacementGroup>,
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
/// change. A pane's placement is excluded as well (D30): it lives in the
/// group's topology digest, not the pane's content digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resource {
    pub kind: String,
    pub name: String,
    pub parent: Option<String>,
    pub digest: String,
    pub fields: Value,
}

/// Where a pane sits, in one backend's own terms (D29). The `flavor` tag
/// discriminates; only Herdr has a placement today.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "flavor", rename_all = "snake_case")]
pub enum Placement {
    Herdr {
        tab: String,
        split: Split,
        ratios: Vec<f64>,
    },
}

/// A derived Herdr placement group: the set of panes that name one Herdr tab,
/// with the shape they asked for and a topology digest over it (D29, D30). Not
/// a core resource — the Herdr flavor reconstructs it from pane placements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementGroup {
    /// Identity for ownership: `workspace/<Herdr tab name>`.
    pub id: String,
    pub workspace: String,
    /// The Herdr tab name the panes named.
    pub name: String,
    pub label: String,
    pub split: Split,
    pub ratios: Vec<f64>,
    /// Pane names, in declared order.
    pub panes: Vec<String>,
    /// Digest over `(name, split, ratios, ordered pane names)` — the group's
    /// shape, independent of any pane's content (D30).
    pub topology_digest: String,
}

fn content_digest(fields: &Value, children: &[&str]) -> Result<String> {
    canonical_digest(&json!({"fields": fields, "children": children}))
}

fn topology_digest(name: &str, split: Split, ratios: &[f64], panes: &[String]) -> Result<String> {
    canonical_digest(&json!({
        "name": name,
        "split": split,
        "ratios": ratios,
        "panes": panes,
    }))
}

pub fn to_ir(profile: &Profile) -> Ir {
    to_ir_inner(profile).expect("resource content is always serializable JSON")
}

fn to_ir_inner(profile: &Profile) -> Result<Ir> {
    let mut resources = Vec::new();
    let mut placements = Vec::new();

    for workspace in &profile.workspaces {
        let mut workspace_children_resources = Vec::new();
        let mut group_digests = Vec::new();

        for group in &workspace.tabs {
            let group_address = format!("{}/{}", workspace.name, group.name);
            let mut pane_digests = Vec::new();
            let mut ordered_panes = Vec::new();
            let mut group_children_resources = Vec::new();

            for pane in &group.panes {
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
                    group_children_resources.push(Resource {
                        kind: "agent".into(),
                        name,
                        parent: Some(pane.name.clone()),
                        digest,
                        fields: agent_fields,
                    });
                }

                // Content digest covers core fields only and, per D30,
                // excludes placement. These are exactly the v2 pane fields,
                // so a v2 state file upgrades to v3 with every content digest
                // unchanged.
                let content_fields = json!({
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
                let pane_digest = content_digest(&content_fields, &agent_children)?;
                pane_digests.push(pane_digest.clone());
                ordered_panes.push(pane.name.clone());

                let placement = Placement::Herdr {
                    tab: group.name.clone(),
                    split: group.split,
                    ratios: group.ratios.clone(),
                };
                let mut fields = content_fields;
                fields["placement"] = serde_json::to_value(&placement)?;

                group_children_resources.push(Resource {
                    kind: "pane".into(),
                    name: pane.name.clone(),
                    parent: Some(group_address.clone()),
                    digest: pane_digest,
                    fields,
                });
            }

            let group_topology =
                topology_digest(&group.name, group.split, &group.ratios, &ordered_panes)?;
            group_digests.push((group_topology.clone(), pane_digests));

            placements.push(PlacementGroup {
                id: group_address,
                workspace: workspace.name.clone(),
                name: group.name.clone(),
                label: group.label().to_owned(),
                split: group.split,
                ratios: group.ratios.clone(),
                panes: ordered_panes,
                topology_digest: group_topology,
            });
            workspace_children_resources.extend(group_children_resources);
        }

        let workspace_fields = json!({
            "label": workspace.label,
            "cwd": workspace.cwd,
            "env": workspace.env,
        });
        // The workspace digest still folds in each group's shape and its
        // panes' content, so a change anywhere under the workspace changes it.
        let mut child_digests: Vec<&str> = Vec::new();
        for (group_topology, pane_digests) in &group_digests {
            child_digests.push(group_topology.as_str());
            child_digests.extend(pane_digests.iter().map(String::as_str));
        }
        let workspace_digest = content_digest(&workspace_fields, &child_digests)?;

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
        placements,
    })
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "workspace" => 0,
        "pane" => 1,
        "agent" => 2,
        "task" => 3,
        _ => 4,
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

    fn pane<'a>(ir: &'a Ir, name: &str) -> &'a Resource {
        ir.resources
            .iter()
            .find(|r| r.kind == "pane" && r.name == name)
            .expect("pane resource")
    }

    #[test]
    fn round_trips_through_json() {
        let ir = sample_profile().to_ir();
        let text = ir.to_json_pretty().expect("serialize");
        let parsed = Ir::from_json(&text).expect("parse");
        assert_eq!(ir, parsed);
    }

    #[test]
    fn ordering_is_deterministic_and_has_no_group_resource() {
        let ir = sample_profile().to_ir();
        let kinds: Vec<&str> = ir.resources.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, ["workspace", "pane", "agent"]);
    }

    #[test]
    fn schema_version_is_three() {
        assert_eq!(sample_profile().to_ir().schema_version, 3);
    }

    #[test]
    fn a_pane_carries_a_herdr_placement() {
        let ir = sample_profile().to_ir();
        let placement: Placement =
            serde_json::from_value(pane(&ir, "review").fields["placement"].clone())
                .expect("placement");
        assert_eq!(
            placement,
            Placement::Herdr {
                tab: "main".into(),
                split: Split::Right,
                ratios: vec![],
            }
        );
    }

    #[test]
    fn one_group_per_declared_placement() {
        let ir = sample_profile().to_ir();
        assert_eq!(ir.placements.len(), 1);
        assert_eq!(ir.placements[0].id, "dev/main");
        assert_eq!(ir.placements[0].panes, ["review"]);
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
    fn content_digest_ignores_identity_but_not_content() {
        let profile = sample_profile();
        let ir = profile.to_ir();
        let original = pane(&ir, "review").digest.clone();

        let mut renamed = profile.clone();
        renamed.workspaces[0].tabs[0].panes[0].name = "renamed".into();
        let renamed_ir = renamed.to_ir();
        assert_eq!(
            original,
            pane(&renamed_ir, "renamed").digest,
            "renaming a resource must not change its content digest"
        );

        let mut changed = profile;
        changed.workspaces[0].tabs[0].panes[0]
            .env
            .insert("K".into(), "V".into());
        let changed_ir = changed.to_ir();
        assert_ne!(
            original,
            pane(&changed_ir, "review").digest,
            "a declared field change must change the content digest"
        );
    }

    #[test]
    fn content_digest_excludes_placement_but_topology_reflects_it() {
        // D30: moving a pane between groups leaves its content digest equal
        // while both groups' topology digests change.
        let mut profile = sample_profile();
        profile.workspaces[0].tabs.push(Tab {
            name: "side".into(),
            label: None,
            split: Default::default(),
            ratios: vec![],
            panes: vec![],
        });
        let before = profile.to_ir();
        let before_content = pane(&before, "review").digest.clone();
        let before_main = before
            .placements
            .iter()
            .find(|g| g.name == "main")
            .expect("main group")
            .topology_digest
            .clone();

        // Move `review` from `main` to `side`.
        let moved_pane = profile.workspaces[0].tabs[0].panes.remove(0);
        profile.workspaces[0].tabs[1].panes.push(moved_pane);
        let after = profile.to_ir();

        assert_eq!(
            before_content,
            pane(&after, "review").digest,
            "a pane's content digest must not change when only its placement does"
        );
        let after_side = after
            .placements
            .iter()
            .find(|g| g.name == "side")
            .expect("side group")
            .topology_digest
            .clone();
        assert_ne!(
            before_main, after_side,
            "the group's topology digest must reflect which panes it holds"
        );
    }

    #[test]
    fn workspace_digest_changes_when_a_child_changes() {
        let profile = sample_profile();
        let workspace_digest = |p: &Profile| {
            p.to_ir()
                .resources
                .iter()
                .find(|r| r.kind == "workspace")
                .expect("workspace resource")
                .digest
                .clone()
        };
        let before = workspace_digest(&profile);

        let mut changed = profile;
        changed.workspaces[0].tabs[0].panes[0]
            .agent
            .as_mut()
            .expect("agent")
            .prompt = Some("bye".into());
        assert_ne!(before, workspace_digest(&changed));
    }
}
