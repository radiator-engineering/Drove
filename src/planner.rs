//! Pure desired-versus-observed planning.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    herdr::{ExportedLayout, HerdrClient, SessionSnapshot},
    model::{Profile, canonical_digest},
    state::LocalState,
};

#[derive(Debug, Clone)]
pub struct ObservedState {
    pub snapshot: SessionSnapshot,
    pub layouts: BTreeMap<String, ExportedLayout>,
}

impl ObservedState {
    pub fn gather(client: &HerdrClient, local: &LocalState, profile: &str) -> Result<Self> {
        let snapshot = client.snapshot()?;
        let mut layouts = BTreeMap::new();
        if let Some(managed) = local.profile(profile) {
            for workspace in managed.workspaces.values() {
                for tab in workspace.tabs.values() {
                    if snapshot.tab(&tab.tab_id).is_some()
                        && let Ok(layout) = client.export_layout(&tab.tab_id)
                    {
                        layouts.insert(tab.tab_id.clone(), layout);
                    }
                }
            }
        }
        Ok(Self { snapshot, layouts })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    InSync,
    OutOfSync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub profile: String,
    pub desired_digest: String,
    pub status: SyncStatus,
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn has_destructive_actions(&self) -> bool {
        self.actions.iter().any(|action| action.destructive)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub address: String,
    pub reason: String,
    pub destructive: bool,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Conflict,
    CreateWorkspace,
    RenameWorkspace,
    CreateTab,
    RenameTab,
    ReplaceTab,
    StartAgent,
    RunBootstrap,
    Detach,
}

pub fn build_plan(
    profile: &Profile,
    local: &LocalState,
    observed: &ObservedState,
    bootstrap_actions: impl IntoIterator<Item = Action>,
) -> Result<Plan> {
    let desired_digest = profile.digest()?;
    let managed = local.profile(&profile.name);
    let mut actions = Vec::new();
    for entry in local.journal.iter().filter(|entry| !entry.completed) {
        let unresolved_create = entry
            .action
            .strip_prefix("CreateWorkspace ")
            .is_some_and(|id| {
                managed
                    .and_then(|profile| profile.workspaces.get(id))
                    .and_then(|workspace| observed.snapshot.workspace(&workspace.workspace_id))
                    .is_none()
            })
            || entry
                .action
                .strip_prefix("CreateTab ")
                .is_some_and(|address| {
                    let Some((workspace_id, tab_id)) = address.split_once('/') else {
                        return true;
                    };
                    managed
                        .and_then(|profile| profile.workspaces.get(workspace_id))
                        .and_then(|workspace| workspace.tabs.get(tab_id))
                        .and_then(|tab| observed.snapshot.tab(&tab.tab_id))
                        .is_none()
                });
        if unresolved_create {
            actions.push(action(
                ActionKind::Conflict,
                entry.action.clone(),
                "a previous create may have reached Herdr before local ownership was recorded",
                false,
                &entry.digest,
            )?);
        }
    }
    let desired_workspace_ids = profile
        .workspaces
        .iter()
        .map(|workspace| workspace.id.as_str())
        .collect::<BTreeSet<_>>();

    for workspace in &profile.workspaces {
        let managed_workspace = managed.and_then(|state| state.workspaces.get(&workspace.id));
        let actual_workspace =
            managed_workspace.and_then(|state| observed.snapshot.workspace(&state.workspace_id));

        if actual_workspace.is_none() {
            actions.push(action(
                ActionKind::CreateWorkspace,
                workspace.id.clone(),
                "managed workspace is missing",
                false,
                &workspace,
            )?);
            continue;
        }
        let Some(actual_workspace) = actual_workspace else {
            continue;
        };
        if actual_workspace.label != workspace.label {
            actions.push(action(
                ActionKind::RenameWorkspace,
                workspace.id.clone(),
                format!(
                    "workspace label is `{}`, expected `{}`",
                    actual_workspace.label, workspace.label
                ),
                false,
                &workspace.label,
            )?);
        }

        let Some(managed_workspace) = managed_workspace else {
            continue;
        };
        let desired_tab_ids = workspace
            .tabs
            .iter()
            .map(|tab| tab.id.as_str())
            .collect::<BTreeSet<_>>();
        for tab in &workspace.tabs {
            let address = format!("{}/{}", workspace.id, tab.id);
            let managed_tab = managed_workspace.tabs.get(&tab.id);
            let actual_tab = managed_tab.and_then(|state| observed.snapshot.tab(&state.tab_id));
            if actual_tab.is_none() {
                actions.push(action(
                    ActionKind::CreateTab,
                    address,
                    "managed tab is missing",
                    false,
                    tab,
                )?);
                continue;
            }
            let Some(actual_tab) = actual_tab else {
                continue;
            };
            if actual_tab.label != tab.label {
                actions.push(action(
                    ActionKind::RenameTab,
                    address.clone(),
                    format!(
                        "tab label is `{}`, expected `{}`",
                        actual_tab.label, tab.label
                    ),
                    false,
                    &tab.label,
                )?);
            }

            let expected = tab.layout.to_herdr_json(&local.repo_root, &workspace.cwd);
            match observed.layouts.get(&actual_tab.tab_id) {
                Some(layout) if layouts_match(&expected, &layout.root) => {}
                Some(_) => actions.push(action(
                    ActionKind::ReplaceTab,
                    address,
                    "managed tab layout differs; replacement discards live PTYs",
                    true,
                    tab,
                )?),
                None => actions.push(action(
                    ActionKind::ReplaceTab,
                    address,
                    "managed tab layout could not be observed",
                    true,
                    tab,
                )?),
            }
        }

        for removed in managed_workspace
            .tabs
            .keys()
            .filter(|id| !desired_tab_ids.contains(id.as_str()))
        {
            actions.push(action(
                ActionKind::Detach,
                format!("{}/{}", workspace.id, removed),
                "declaration was removed; live tab will be preserved",
                false,
                removed,
            )?);
        }
    }

    if let Some(managed) = managed {
        for removed in managed
            .workspaces
            .keys()
            .filter(|id| !desired_workspace_ids.contains(id.as_str()))
        {
            actions.push(action(
                ActionKind::Detach,
                (*removed).clone(),
                "declaration was removed; live workspace will be preserved",
                false,
                removed,
            )?);
        }
    }

    for agent in &profile.agents {
        let pane_id = managed.and_then(|profile_state| {
            profile_state
                .workspaces
                .values()
                .flat_map(|workspace| workspace.tabs.values())
                .find_map(|tab| tab.panes.get(&agent.pane))
        });
        let running = pane_id.is_some_and(|pane_id| {
            observed
                .snapshot
                .agents
                .iter()
                .any(|actual| actual.pane_id == *pane_id && actual.agent == agent.kind)
        });
        if !running {
            actions.push(action(
                ActionKind::StartAgent,
                agent.id.clone(),
                "declared agent is not running in its managed pane",
                false,
                agent,
            )?);
        }
    }

    actions.extend(bootstrap_actions);
    actions.sort_by(|left, right| {
        action_priority(left)
            .cmp(&action_priority(right))
            .then_with(|| {
                if left.kind == ActionKind::RunBootstrap && right.kind == ActionKind::RunBootstrap {
                    std::cmp::Ordering::Equal
                } else {
                    left.address.cmp(&right.address)
                }
            })
    });
    Ok(Plan {
        profile: profile.name.clone(),
        desired_digest,
        status: if actions.is_empty() {
            SyncStatus::InSync
        } else {
            SyncStatus::OutOfSync
        },
        actions,
    })
}

fn action(
    kind: ActionKind,
    address: String,
    reason: impl Into<String>,
    destructive: bool,
    desired: &impl Serialize,
) -> Result<Action> {
    let reason = reason.into();
    let digest = canonical_digest(&(kind, &address, &reason, desired))?;
    Ok(Action {
        kind,
        address,
        reason,
        destructive,
        digest,
    })
}

pub fn bootstrap_action(task: &crate::model::BootstrapTask, reason: &str) -> Result<Action> {
    action(
        ActionKind::RunBootstrap,
        task.id.clone(),
        reason,
        false,
        task,
    )
}

fn action_priority(action: &Action) -> u8 {
    match action.kind {
        ActionKind::RunBootstrap => 0,
        ActionKind::Conflict => 1,
        ActionKind::CreateWorkspace => 2,
        ActionKind::RenameWorkspace => 3,
        ActionKind::CreateTab => 4,
        ActionKind::RenameTab => 5,
        ActionKind::ReplaceTab => 6,
        ActionKind::StartAgent => 7,
        ActionKind::Detach => 8,
    }
}

fn layouts_match(expected: &Value, actual: &Value) -> bool {
    normalize_layout(expected) == normalize_layout(actual)
}

fn normalize_layout(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| {
                    !matches!(
                        key.as_str(),
                        "pane_id" | "workspace_id" | "tab_id" | "focused"
                    )
                })
                .map(|(key, value)| (key.clone(), normalize_layout(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(normalize_layout).collect()),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::{
        herdr::{AgentInfo, PaneInfo, TabInfo, WorkspaceInfo},
        model::DroveConfig,
        state::{JournalEntry, ManagedProfile, ManagedTab, ManagedWorkspace},
    };

    #[test]
    fn unmanaged_resources_do_not_create_actions() {
        let profile: Profile = serde_json::from_value(json!({
            "name": "default",
            "workspaces": []
        }))
        .expect("profile");
        let config = DroveConfig::new(vec![profile.clone()]).expect("config");
        assert!(config.profile("default").is_ok());
        let local = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        let observed = ObservedState {
            snapshot: SessionSnapshot {
                workspaces: vec![WorkspaceInfo {
                    workspace_id: "unmanaged".into(),
                    label: "keep me".into(),
                }],
                tabs: vec![],
                panes: vec![],
                agents: vec![],
                ..SessionSnapshot::default()
            },
            layouts: BTreeMap::new(),
        };
        let plan = build_plan(&profile, &local, &observed, []).expect("plan");
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn detects_layout_drift_without_counting_runtime_ids() {
        let profile: Profile = serde_json::from_value(json!({
            "name": "default",
            "workspaces": [{
                "id": "dev",
                "label": "dev",
                "tabs": [{
                    "id": "main",
                    "label": "main",
                    "layout": {"type": "pane", "id": "shell", "label": "shell"}
                }]
            }]
        }))
        .expect("profile");
        let mut local = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: Vec::new(),
            path: PathBuf::new(),
        };
        local.profiles.insert(
            "default".into(),
            ManagedProfile {
                desired_digest: String::new(),
                workspaces: BTreeMap::from([(
                    "dev".into(),
                    ManagedWorkspace {
                        workspace_id: "w1".into(),
                        desired_digest: String::new(),
                        tabs: BTreeMap::from([(
                            "main".into(),
                            ManagedTab {
                                tab_id: "w1:t1".into(),
                                desired_digest: String::new(),
                                panes: BTreeMap::from([("shell".into(), "w1:p1".into())]),
                            },
                        )]),
                    },
                )]),
            },
        );
        let observed = ObservedState {
            snapshot: SessionSnapshot {
                workspaces: vec![WorkspaceInfo {
                    workspace_id: "w1".into(),
                    label: "dev".into(),
                }],
                tabs: vec![TabInfo {
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    label: "main".into(),
                }],
                panes: vec![PaneInfo {
                    pane_id: "w1:p1".into(),
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    cwd: Some(PathBuf::from("/repo")),
                }],
                agents: Vec::<AgentInfo>::new(),
                ..SessionSnapshot::default()
            },
            layouts: BTreeMap::from([(
                "w1:t1".into(),
                ExportedLayout {
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                    root: json!({
                        "type": "pane",
                        "pane_id": "w1:p1",
                        "label": "shell",
                        "cwd": "/repo"
                    }),
                },
            )]),
        };
        let plan = build_plan(&profile, &local, &observed, []).expect("plan");
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    #[test]
    fn interrupted_unrecorded_create_becomes_conflict() {
        let profile: Profile = serde_json::from_value(json!({
            "name": "default",
            "workspaces": [{"id": "dev", "label": "dev", "tabs": []}]
        }))
        .expect("profile");
        let local = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![JournalEntry {
                action: "CreateWorkspace dev".into(),
                digest: "pending".into(),
                completed: false,
            }],
            path: PathBuf::new(),
        };
        let observed = ObservedState {
            snapshot: SessionSnapshot::default(),
            layouts: BTreeMap::new(),
        };
        let plan = build_plan(&profile, &local, &observed, []).expect("plan");
        assert_eq!(plan.actions[0].kind, ActionKind::Conflict);
        assert!(plan.actions.iter().any(|action| {
            action.kind == ActionKind::CreateWorkspace && action.address == "dev"
        }));
    }

    #[test]
    fn running_agent_in_mapped_pane_is_in_sync() {
        let profile: Profile = serde_json::from_value(json!({
            "name": "default",
            "workspaces": [{
                "id": "dev",
                "label": "dev",
                "tabs": [{
                    "id": "main",
                    "label": "main",
                    "layout": {"type": "pane", "id": "agent-pane"}
                }]
            }],
            "agents": [{
                "id": "review",
                "pane": "agent-pane",
                "kind": "cursor"
            }]
        }))
        .expect("profile");
        let mut local = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![],
            path: PathBuf::new(),
        };
        local.profiles.insert(
            "default".into(),
            ManagedProfile {
                desired_digest: profile.digest().expect("digest"),
                workspaces: BTreeMap::from([(
                    "dev".into(),
                    ManagedWorkspace {
                        workspace_id: "w1".into(),
                        desired_digest: String::new(),
                        tabs: BTreeMap::from([(
                            "main".into(),
                            ManagedTab {
                                tab_id: "w1:t1".into(),
                                desired_digest: String::new(),
                                panes: BTreeMap::from([("agent-pane".into(), "w1:p1".into())]),
                            },
                        )]),
                    },
                )]),
            },
        );
        let observed = ObservedState {
            snapshot: SessionSnapshot {
                workspaces: vec![WorkspaceInfo {
                    workspace_id: "w1".into(),
                    label: "dev".into(),
                }],
                tabs: vec![TabInfo {
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    label: "main".into(),
                }],
                panes: vec![PaneInfo {
                    pane_id: "w1:p1".into(),
                    tab_id: "w1:t1".into(),
                    workspace_id: "w1".into(),
                    cwd: Some(PathBuf::from("/repo")),
                }],
                agents: vec![AgentInfo {
                    pane_id: "w1:p1".into(),
                    agent: "cursor".into(),
                    agent_status: "working".into(),
                }],
                ..SessionSnapshot::default()
            },
            layouts: BTreeMap::from([(
                "w1:t1".into(),
                ExportedLayout {
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                    root: json!({
                        "type": "pane",
                        "pane_id": "w1:p1",
                        "cwd": "/repo"
                    }),
                },
            )]),
        };
        let plan = build_plan(&profile, &local, &observed, []).expect("plan");
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
    }

    #[test]
    fn preserves_topological_bootstrap_order() {
        let profile = Profile {
            name: "default".into(),
            workspaces: vec![],
            agents: vec![],
            bootstrap: vec![],
        };
        let local = LocalState {
            schema_version: 1,
            repo_root: PathBuf::from("/repo"),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![],
            path: PathBuf::new(),
        };
        let observed = ObservedState {
            snapshot: SessionSnapshot::default(),
            layouts: BTreeMap::new(),
        };
        let bootstrap = ["z-foundation", "a-dependent"].map(|address| Action {
            kind: ActionKind::RunBootstrap,
            address: address.into(),
            reason: "pending".into(),
            destructive: false,
            digest: address.into(),
        });
        let plan = build_plan(&profile, &local, &observed, bootstrap).expect("plan");
        assert_eq!(plan.actions[0].address, "z-foundation");
        assert_eq!(plan.actions[1].address, "a-dependent");
    }
}
