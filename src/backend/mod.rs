//! The `Backend` trait: the operation surface the planner (PR 2) drives
//! against a workspace host. Herdr is the first implementation
//! ([`herdr::HerdrClient`]); Radiator's hub is the second (PR 5).

pub mod herdr;
pub mod radiator;

use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use self::herdr::{ExportedLayout, SessionSnapshot};
use crate::model::SplitDirection;

/// What a backend can do, declared at connect time and checked against the
/// IR at plan time (spec §3). A backend that lacks a capability degrades
/// (flattens tabs, ignores ratios, ...) rather than failing outright.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub tabs: bool,
    pub splits_and_ratios: bool,
    pub workspace_env: bool,
    pub pane_command_at_create: bool,
    pub agent_start: bool,
    pub agent_prompt: bool,
    pub adopt_caller: bool,
    pub metadata_tokens: bool,
    pub process_info: bool,
    pub events: bool,
    /// Whether `Backend::output` can read pane text for an `output()`
    /// readiness probe (D23). Radiator sets this false.
    pub readiness_output: bool,
}

/// Backend-observed information about a running pane's process, used for
/// drift detection when a digest-based ownership token is unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub command: Vec<String>,
    pub pid: Option<u32>,
}

/// The operation surface a backend must implement. PR 2's planner drives
/// these; PR 3 fills in Herdr's incremental convergence; PR 5 adds Radiator.
pub trait Backend {
    fn capabilities(&self) -> Capabilities;

    /// The pane id of the invoking terminal, if Drove is running inside one
    /// the backend recognizes (`HERDR_PANE_ID`, `RADIATOR_PANE_ID`, ...).
    fn caller_pane_id(&self) -> Option<String>;

    fn snapshot(&self) -> Result<SessionSnapshot>;

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String>;

    fn create_tab(
        &self,
        workspace_id: &str,
        tab_label: &str,
        root: Value,
    ) -> Result<ExportedLayout>;

    fn split_pane(
        &self,
        pane_id: &str,
        direction: SplitDirection,
        ratio: f64,
        command: Option<&[String]>,
        cwd: Option<&Path>,
    ) -> Result<String>;

    fn close_pane(&self, pane_id: &str) -> Result<()>;

    fn set_ratio(&self, tab_id: &str, ratios: &[f64]) -> Result<()>;

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()>;

    fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()>;

    fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()>;

    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()>;

    fn prompt_agent(&self, pane_id: &str, prompt: &str) -> Result<()>;

    fn process_info(&self, pane_id: &str) -> Result<Option<ProcessInfo>>;

    fn report_tokens(&self, address: &str, tokens: &BTreeMap<String, String>) -> Result<()>;

    /// Recent pane text, host-side input to an `output()` readiness probe
    /// (D23). Only meaningful when `capabilities().readiness_output`.
    fn output(&self, pane_id: &str) -> Result<String>;
}
