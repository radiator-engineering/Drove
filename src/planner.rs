//! Pure desired-versus-observed planning.
//!
//! PR 1 keeps `Plan`/`Action` compiling against the v2 model with
//! deliberately reduced behavior: `build_plan` does not yet diff against
//! backend-observed state, so every profile reports in sync. PR 2 replaces
//! this with the reads/writes, hazard-checked planner from spec §5 (actions
//! carrying `reads`/`writes` over resource addresses, observed digests,
//! early cutoff, `after` ordering).

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::model::Profile;

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
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Placeholder kind so the enum is non-empty ahead of PR 2's actions
    /// table (CreateWorkspace, SplitPane, StartAgent, RunTask, ...).
    NotImplemented,
}

/// PR 2 stub: reports every profile as in sync against the v2 model.
pub fn build_plan(profile: &Profile) -> Result<Plan> {
    Ok(Plan {
        profile: profile.name.clone(),
        desired_digest: profile.digest()?,
        status: SyncStatus::InSync,
        actions: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DroveConfig;

    #[test]
    fn empty_profile_is_in_sync() {
        let profile = Profile {
            name: "default".into(),
            workspaces: vec![],
            tasks: vec![],
        };
        DroveConfig::new(vec![profile.clone()]).expect("config");
        let plan = build_plan(&profile).expect("plan");
        assert_eq!(plan.status, SyncStatus::InSync);
        assert!(plan.actions.is_empty());
    }
}
