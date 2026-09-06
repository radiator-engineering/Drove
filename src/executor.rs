//! Safe execution of an already-reviewed Drove plan.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::{
    bootstrap,
    herdr::{ExportedLayout, HerdrClient},
    model::{Profile, TabSpec, WorkspaceSpec, canonical_digest},
    planner::{ActionKind, Plan},
    state::{LocalState, ManagedTab, ManagedWorkspace},
};

pub struct ExecuteOptions {
    pub allow_replace: bool,
    pub approve_bootstrap: bool,
}

pub fn execute(
    client: &HerdrClient,
    profile: &Profile,
    source_digest: &str,
    plan: &Plan,
    state: &mut LocalState,
    options: &ExecuteOptions,
) -> Result<()> {
    if plan.has_destructive_actions() && !options.allow_replace {
        bail!("plan replaces live managed tabs; rerun with --allow-replace after reviewing it");
    }

    for action in &plan.actions {
        if action.kind == ActionKind::Conflict {
            bail!(
                "cannot apply unresolved ownership conflict `{}`: {}",
                action.address,
                action.reason
            );
        }
        state.begin_action(
            &format!("{:?} {}", action.kind, action.address),
            &action.digest,
        )?;
        match action.kind {
            ActionKind::Conflict => unreachable!("handled before journaling"),
            ActionKind::RunBootstrap => {
                let task = profile
                    .bootstrap
                    .iter()
                    .find(|task| task.id == action.address)
                    .with_context(|| format!("unknown bootstrap task `{}`", action.address))?;
                bootstrap::execute_task(
                    task,
                    &state.repo_root.clone(),
                    source_digest,
                    state,
                    options.approve_bootstrap,
                )?;
            }
            ActionKind::CreateWorkspace => {
                let workspace = find_workspace(profile, &action.address)?;
                create_workspace(client, profile, workspace, state)?;
            }
            ActionKind::RenameWorkspace => {
                let workspace = find_workspace(profile, &action.address)?;
                let managed = managed_workspace(state, profile, &workspace.id)?;
                client.rename_workspace(&managed.workspace_id, &workspace.label)?;
                managed.desired_digest = canonical_digest(workspace)?;
            }
            ActionKind::CreateTab => {
                let (workspace, tab) = find_tab(profile, &action.address)?;
                let workspace_id = managed_workspace(state, profile, &workspace.id)?
                    .workspace_id
                    .clone();
                let layout = apply_tab(client, state, workspace, tab, &workspace_id, None)?;
                store_tab(state, profile, workspace, tab, &layout)?;
            }
            ActionKind::RenameTab => {
                let (workspace, tab) = find_tab(profile, &action.address)?;
                let managed = managed_tab(state, profile, &workspace.id, &tab.id)?;
                client.rename_tab(&managed.tab_id, &tab.label)?;
                managed.desired_digest = canonical_digest(tab)?;
            }
            ActionKind::ReplaceTab => {
                let (workspace, tab) = find_tab(profile, &action.address)?;
                let workspace_id = managed_workspace(state, profile, &workspace.id)?
                    .workspace_id
                    .clone();
                let old_tab_id = managed_tab(state, profile, &workspace.id, &tab.id)?
                    .tab_id
                    .clone();
                let layout = apply_tab(
                    client,
                    state,
                    workspace,
                    tab,
                    &workspace_id,
                    Some(&old_tab_id),
                )?;
                store_tab(state, profile, workspace, tab, &layout)?;
            }
            ActionKind::StartAgent => {
                let agent = profile
                    .agents
                    .iter()
                    .find(|agent| agent.id == action.address)
                    .with_context(|| format!("unknown agent `{}`", action.address))?;
                let pane_id = state
                    .profile(&profile.name)
                    .and_then(|managed| {
                        managed
                            .workspaces
                            .values()
                            .flat_map(|workspace| workspace.tabs.values())
                            .find_map(|tab| tab.panes.get(&agent.pane))
                    })
                    .with_context(|| format!("pane `{}` has not been created", agent.pane))?;
                client.start_agent(
                    pane_id,
                    agent.name.as_deref().unwrap_or(&agent.id),
                    &agent.kind,
                    &agent.args,
                )?;
            }
            ActionKind::Detach => detach(state, profile, &action.address),
        }
        state.finish_action(&action.digest)?;
    }

    let desired_digest = profile.digest()?;
    state.profile_mut(&profile.name).desired_digest = desired_digest;
    state.save()?;
    for workspace in state
        .profile(&profile.name)
        .into_iter()
        .flat_map(|profile| profile.workspaces.values())
    {
        let _ = client.report_workspace_status(&workspace.workspace_id, "in sync");
    }
    Ok(())
}

fn create_workspace(
    client: &HerdrClient,
    profile: &Profile,
    workspace: &WorkspaceSpec,
    state: &mut LocalState,
) -> Result<()> {
    let cwd = state.repo_root.join(&workspace.cwd);
    let workspace_id = client.create_workspace(&workspace.label, &cwd)?;
    let initial_tab_id = client
        .snapshot()?
        .tabs
        .into_iter()
        .find(|tab| tab.workspace_id == workspace_id)
        .map(|tab| tab.tab_id);

    state.profile_mut(&profile.name).workspaces.insert(
        workspace.id.clone(),
        ManagedWorkspace {
            workspace_id: workspace_id.clone(),
            desired_digest: canonical_digest(workspace)?,
            tabs: BTreeMap::new(),
        },
    );
    state.save()?;

    for (index, tab) in workspace.tabs.iter().enumerate() {
        let replace = (index == 0).then_some(initial_tab_id.as_deref()).flatten();
        let layout = apply_tab(client, state, workspace, tab, &workspace_id, replace)?;
        store_tab(state, profile, workspace, tab, &layout)?;
    }
    Ok(())
}

fn apply_tab(
    client: &HerdrClient,
    state: &LocalState,
    workspace: &WorkspaceSpec,
    tab: &TabSpec,
    workspace_id: &str,
    tab_id: Option<&str>,
) -> Result<ExportedLayout> {
    client.apply_layout(
        workspace_id,
        tab_id,
        &tab.label,
        tab.layout.to_herdr_json(&state.repo_root, &workspace.cwd),
    )
}

fn store_tab(
    state: &mut LocalState,
    profile: &Profile,
    workspace: &WorkspaceSpec,
    tab: &TabSpec,
    layout: &ExportedLayout,
) -> Result<()> {
    let mut desired_panes = Vec::new();
    tab.layout.pane_ids(&mut desired_panes);
    let actual_panes = layout.pane_ids_preorder();
    if desired_panes.len() != actual_panes.len() {
        bail!(
            "Herdr returned {} panes for {} declared panes",
            actual_panes.len(),
            desired_panes.len()
        );
    }
    let panes = desired_panes.into_iter().zip(actual_panes).collect();
    managed_workspace(state, profile, &workspace.id)?
        .tabs
        .insert(
            tab.id.clone(),
            ManagedTab {
                tab_id: layout.tab_id.clone(),
                desired_digest: canonical_digest(tab)?,
                panes,
            },
        );
    state.save()
}

fn detach(state: &mut LocalState, profile: &Profile, address: &str) {
    if let Some((workspace, tab)) = address.split_once('/') {
        if let Some(workspace) = state
            .profile_mut(&profile.name)
            .workspaces
            .get_mut(workspace)
        {
            workspace.tabs.remove(tab);
        }
    } else {
        state.profile_mut(&profile.name).workspaces.remove(address);
    }
}

fn find_workspace<'a>(profile: &'a Profile, id: &str) -> Result<&'a WorkspaceSpec> {
    profile
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .with_context(|| format!("unknown workspace `{id}`"))
}

fn find_tab<'a>(profile: &'a Profile, address: &str) -> Result<(&'a WorkspaceSpec, &'a TabSpec)> {
    let (workspace_id, tab_id) = address
        .split_once('/')
        .with_context(|| format!("invalid tab address `{address}`"))?;
    let workspace = find_workspace(profile, workspace_id)?;
    let tab = workspace
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .with_context(|| format!("unknown tab `{address}`"))?;
    Ok((workspace, tab))
}

fn managed_workspace<'a>(
    state: &'a mut LocalState,
    profile: &Profile,
    id: &str,
) -> Result<&'a mut ManagedWorkspace> {
    state
        .profile_mut(&profile.name)
        .workspaces
        .get_mut(id)
        .with_context(|| format!("workspace `{id}` is not managed"))
}

fn managed_tab<'a>(
    state: &'a mut LocalState,
    profile: &Profile,
    workspace_id: &str,
    tab_id: &str,
) -> Result<&'a mut ManagedTab> {
    managed_workspace(state, profile, workspace_id)?
        .tabs
        .get_mut(tab_id)
        .with_context(|| format!("tab `{workspace_id}/{tab_id}` is not managed"))
}
