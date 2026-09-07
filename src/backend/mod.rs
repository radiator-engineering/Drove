//! The `Backend` trait: the operation surface the planner drives against a
//! workspace host, split into a **core** every backend fully honors and
//! per-backend **flavors** reached through typed accessors (spec §3, D27,
//! D28). Herdr is the first implementation ([`herdr::HerdrClient`]); the
//! Radiator hub is the second ([`radiator::RadiatorClient`]).

pub mod herdr;
pub mod radiator;
pub mod select;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use self::herdr::SessionSnapshot;

/// A pane split direction. Named `Split` in the flavor surface (spec §3);
/// the model layer calls the same type [`crate::model::SplitDirection`].
pub use crate::model::SplitDirection as Split;

/// The graded core features a backend may support only partially — the same
/// verb, honored to different degrees (spec §3, D27). Verbs a backend either
/// implements fully or not at all (tabs, splits, agent start) are not flags
/// here: they are flavors, and the presence of the flavor's `impl` is the
/// capability.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub workspace_env: bool,
    pub pane_command_at_create: bool,
    pub metadata_tokens: bool,
    pub process_info: bool,
    pub events: bool,
    /// Whether [`Backend::output`] can read pane text for an `output()`
    /// readiness probe (D23).
    pub readiness_output: bool,
}

/// Backend-observed information about a running pane's process, used for
/// drift detection when a digest-based ownership token is unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessInfo {
    pub command: Vec<String>,
    pub pid: Option<u32>,
}

/// The core inputs for creating one pane: what every backend needs to open a
/// pane, with no placement (spec §3). Placement — which tab, split and
/// ratios — is a Herdr flavor concern carried on [`HerdrExt::split_pane`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneSpec {
    pub label: Option<String>,
    pub cwd: Option<PathBuf>,
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
}

/// The intersection every backend must fully honor (spec §3, D27). Verbs one
/// backend alone understands live in an extension trait ([`HerdrExt`],
/// [`RadiatorExt`]) reached through the [`Backend::herdr`]/
/// [`Backend::radiator`] accessors, which default to `None`.
pub trait Backend {
    fn snapshot(&self) -> Result<SessionSnapshot>;

    /// The pane id of the invoking terminal, if Drove is running inside one
    /// the backend recognizes (`HERDR_PANE_ID`, `RADIATOR_PANE_ID`, ...).
    fn caller_pane_id(&self) -> Option<String>;

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String>;

    fn rename_workspace(&self, id: &str, label: &str) -> Result<()>;

    /// Creates a pane with no placement. On Herdr this opens the pane in the
    /// workspace's first Herdr tab; placement is applied separately through
    /// [`HerdrExt`] (spec §3).
    fn create_pane(&self, workspace_id: &str, spec: &PaneSpec) -> Result<String>;

    fn close_pane(&self, id: &str) -> Result<()>;

    fn rename_pane(&self, id: &str, label: &str) -> Result<()>;

    /// Restarts the command running in an existing pane in place (D22).
    fn restart_command(&self, id: &str, argv: &[String]) -> Result<()>;

    fn prompt_agent(&self, id: &str, prompt: &str) -> Result<()>;

    fn process_info(&self, id: &str) -> Result<Option<ProcessInfo>>;

    fn report_tokens(&self, address: &str, tokens: &BTreeMap<String, String>) -> Result<()>;

    /// Recent pane text, host-side input to an `output()` readiness probe
    /// (D23). Only meaningful when `capabilities().readiness_output`.
    /// `timeout` bounds the backend round trip so a withheld response cannot
    /// block a readiness check indefinitely.
    fn output(&self, id: &str, timeout: Duration) -> Result<String>;

    fn capabilities(&self) -> Capabilities;

    /// The Herdr flavor, if this backend implements it. The presence of the
    /// impl is the capability; there is no bool for "has tabs" (D28).
    fn herdr(&self) -> Option<&dyn HerdrExt> {
        None
    }

    /// The Radiator flavor, if this backend implements it (D28).
    fn radiator(&self) -> Option<&dyn RadiatorExt> {
        None
    }
}

/// The state of a named Herdr session's server after [`HerdrExt::ensure_session`]
/// (D44): already up, just started headlessly, or unstartable without a TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// The session's socket already answered `ping`; nothing was started.
    Running,
    /// The session's server was not up, and this call started it headlessly
    /// and saw its socket come up.
    Started,
    /// Nothing could start the session's server without a TUI. `hint` is the
    /// exact command for the user to run by hand (`herdr --session NAME`).
    CannotStart { hint: String },
}

/// The result of [`HerdrExt::stop_session`] (D47): whether the stop itself
/// found the session running (`stopped`) versus already stopped, and whether
/// the delete that always follows it succeeded (`deleted`; a failed delete
/// is an error, so a returned `Ok` always carries `deleted: true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStop {
    pub stopped: bool,
    pub deleted: bool,
}

/// The result of building a fresh Herdr tab ([`HerdrExt::create_tab`]): the
/// new Herdr tab's backend id, and the backend ids of the panes it holds in
/// the declared order they were created. The caller records these so a later
/// run sees the Herdr tab and its panes as owned rather than remaking them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLayout {
    pub tab_id: String,
    pub pane_ids: Vec<String>,
}

/// The Herdr flavor: tabs, splits, ratios, agent start, plus the session and
/// focus verbs `drove up` drives — verbs only Herdr implements (spec §3, D28,
/// D44). Reached through [`Backend::herdr`].
pub trait HerdrExt {
    /// Builds a fresh Herdr tab holding `panes` in declared order, split in
    /// `split`, then applies `ratios`. Herdr has no empty tab, so the first
    /// pane opens the Herdr tab and the rest are split into it; `ratios` are
    /// applied only after every split gap exists (a fresh group plans one
    /// `CreateTab`, never per-pane splits). Returns the Herdr tab and pane
    /// backend ids (D29).
    ///
    /// `existing_tab`, when set, is the id of a Herdr tab to apply this
    /// layout onto instead of opening a new Herdr tab. It is the root
    /// Herdr tab that Herdr handed back from `create_workspace`, for the
    /// first Herdr tab of a workspace this same apply created (D49),
    /// renamed afterward to `label`.
    fn create_tab(
        &self,
        workspace_id: &str,
        label: &str,
        split: Split,
        ratios: &[f64],
        panes: &[PaneSpec],
        existing_tab: Option<&str>,
    ) -> Result<TabLayout>;

    /// Takes (and clears) the root Herdr tab id captured for `workspace_id`
    /// by a `create_workspace` call earlier in this same apply, if any
    /// (D49). A workspace this apply adopted or found already present
    /// never has an entry, so this returns `None` for it. Consuming the id
    /// on the first call means only the first `create_tab` for a freshly
    /// created workspace ever sees it.
    fn take_root_tab(&self, workspace_id: &str) -> Option<String> {
        let _ = workspace_id;
        None
    }

    fn split_pane(&self, tab_id: &str, spec: &PaneSpec, split: Split) -> Result<String>;

    fn set_ratio(&self, tab_id: &str, ratios: &[f64]) -> Result<()>;

    fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()>;

    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()>;

    /// Brings workspace `id` to the front through `workspace.focus` (D43 step
    /// 4, D44).
    fn focus_workspace(&self, id: &str) -> Result<()>;

    /// Ensures the named session's server is running (D43 step 2, D44):
    /// [`SessionState::Running`] when its socket already answers `ping`,
    /// otherwise starts the server headlessly by shelling out to the `herdr`
    /// binary and returns [`SessionState::Started`] once the socket comes up,
    /// or [`SessionState::CannotStart`] with the command to run by hand.
    fn ensure_session(&self, name: &str) -> Result<SessionState>;

    /// Stops then deletes the named session (D47), the teardown mirror of
    /// [`Self::ensure_session`]: shells out to `herdr session stop NAME`, then
    /// `herdr session delete NAME`. A stop that fails because the session was
    /// not running is not an error — delete still runs and the returned
    /// [`SessionStop::stopped`] is `false`. A missing `herdr` binary or a
    /// failed delete is an error.
    fn stop_session(&self, name: &str) -> Result<SessionStop>;
}

/// The Radiator flavor. Empty until the hub protocol for chat panes, runner
/// state and the attention queue is stable (spec §8, D37).
pub trait RadiatorExt {}
