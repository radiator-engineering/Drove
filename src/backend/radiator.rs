//! Typed client for the Radiator hub's NDJSON socket protocol, and the
//! `Backend` impl that degrades where the hub falls short of what the model
//! wants (spec §3, D3, D23; `.context/handoffs/recon-radiator-report.md` and
//! `recon-radiator-gaps-report.md`).
//!
//! Radiator's hub is workspace → flat panes with no tab or split layer
//! (`crates/proto/src/types.rs` in `radiator-cli`), no `agent.start`, and no
//! metadata storage today. This module flattens tabs into one hub pane list
//! per workspace, runs agents as a `serve` command plus a follow-up
//! `pane.send_text`, and keeps Drove's ownership tokens in a local journal
//! keyed by hub pane id whenever the hub can't store them itself — degrading
//! to `Ownership::Unknown` if the journal and the hub ever disagree (spec
//! §9, D16).
//!
//! The hub additions the gaps report proposes (`pane.set_metadata`,
//! `PaneInfo.process`, `pane.tail`, `workspace.rename`, `hub.capabilities`)
//! are a parallel hub PR, not landed yet. Every call against them goes
//! through `RadiatorClient::request_optional`, which turns the hub's
//! `unknown_method` error into `Ok(None)` instead of a failure, so this
//! backend runs unchanged against a hub with or without them.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    Backend, Capabilities, ProcessInfo,
    herdr::{AgentInfo, ExportedLayout, PaneInfo, SessionSnapshot, TabInfo, WorkspaceInfo},
};
use crate::model::SplitDirection;

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// The hub name Radiator itself defaults to (`radiator-cli`'s `--hub-name`).
pub const DEFAULT_HUB_NAME: &str = "main";

#[derive(Debug, Clone)]
pub struct RadiatorClient {
    socket_path: PathBuf,
    journal_root: PathBuf,
}

/// What a `report_tokens`/journal lookup found for one backend resource id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    /// The hub, the journal, or both agree on this resource's tokens.
    Known(BTreeMap<String, String>),
    /// The hub and the journal disagree, or neither has anything — the
    /// planner must treat this pane as drifted rather than guess.
    Unknown,
}

impl RadiatorClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            journal_root: journal_root(),
        }
    }

    /// Like [`Self::new`], but the token journal lives under `journal_root`
    /// instead of the real Drove state directory — for tests that must not
    /// race other tests over process-wide environment variables.
    #[cfg(test)]
    fn with_journal_root(socket_path: PathBuf, journal_root: PathBuf) -> Self {
        Self {
            socket_path,
            journal_root,
        }
    }

    pub fn discover(explicit_socket: Option<&Path>, hub_name: Option<&str>) -> Self {
        Self::new(resolve_socket_path(explicit_socket, hub_name))
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Send one NDJSON request and return its `result`, or an error built
    /// from the hub's `{code, message}` when it answers `error` instead.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        match self.request_raw(method, params)? {
            Ok(result) => Ok(result),
            Err(error) => bail!("Radiator API error {}: {}", error.code, error.message),
        }
    }

    /// Like [`Self::request`], but a `code: "unknown_method"` error — the
    /// hub not having this method yet — comes back as `Ok(None)` instead of
    /// failing, so a caller can degrade gracefully. Any other error still
    /// fails outright.
    fn request_optional(&self, method: &str, params: Value) -> Result<Option<Value>> {
        match self.request_raw(method, params)? {
            Ok(result) => Ok(Some(result)),
            Err(error) if error.code == "unknown_method" => Ok(None),
            Err(error) => bail!("Radiator API error {}: {}", error.code, error.message),
        }
    }

    fn request_raw(
        &self,
        method: &str,
        params: Value,
    ) -> Result<std::result::Result<Value, RpcError>> {
        let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let request = json!({"id": id, "method": method, "params": params});
        let mut stream = BufReader::new(connect(&self.socket_path).with_context(|| {
            format!(
                "cannot connect to Radiator hub at {}",
                self.socket_path.display()
            )
        })?);
        serde_json::to_writer(stream.get_mut(), &request)
            .context("cannot encode Radiator hub request")?;
        stream
            .get_mut()
            .write_all(b"\n")
            .context("cannot send Radiator hub request")?;
        stream
            .get_mut()
            .flush()
            .context("cannot flush Radiator hub request")?;

        let mut line = String::new();
        let read = stream
            .read_line(&mut line)
            .context("cannot read Radiator hub response")?;
        if read == 0 {
            bail!("Radiator hub closed the socket without a response");
        }
        if !line.ends_with('\n') {
            bail!("Radiator hub returned a truncated response without an NDJSON newline");
        }
        let response: ApiResponse =
            serde_json::from_str(&line).context("Radiator hub returned invalid JSON")?;
        if response.id != id {
            bail!("Radiator hub response id did not match the request");
        }
        match (response.result, response.error) {
            (_, Some(error)) => Ok(Err(error)),
            (Some(result), None) => Ok(Ok(result)),
            (None, None) => bail!("Radiator hub response carried neither result nor error"),
        }
    }

    pub fn ping(&self) -> Result<Value> {
        self.request("hub.ping", json!({}))
    }

    /// The hub's raw `hub.snapshot` value, kept as `Value` because the gaps
    /// report's proposed `PaneInfo.metadata`/`PaneInfo.process` fields may or
    /// may not be present depending on whether the parallel hub PR has
    /// landed; callers that want those look them up with
    /// [`find_pane_field`] rather than a fixed struct.
    fn raw_snapshot(&self) -> Result<Value> {
        self.request("hub.snapshot", json!({}))
    }

    pub fn open_workspace(&self, name: &str) -> Result<String> {
        let result = self.request("workspace.open", json!({"name": name}))?;
        string_field(&result, "id")
            .map(ToOwned::to_owned)
            .context("workspace.open response omitted id")
    }

    pub fn close_workspace(&self, id: &str) -> Result<()> {
        self.request("workspace.close", json!({"id": id}))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_pane(
        &self,
        workspace_id: &str,
        kind: &str,
        title: &str,
        command: Option<&str>,
        args: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<String> {
        // The hub's `OpenPaneParams.env` is `Vec<(String, String)>`, not a
        // JSON object (`crates/hub/src/server.rs`), so pairs are sent as
        // `[[key, value], ...]`.
        let env_pairs: Vec<[&str; 2]> = env.iter().map(|(k, v)| [k.as_str(), v.as_str()]).collect();
        let mut params = json!({
            "workspace": workspace_id,
            "kind": kind,
            "title": title,
            "args": args,
            "env": env_pairs,
        });
        if let Some(command) = command {
            params["command"] = json!(command);
        }
        if let Some(cwd) = cwd {
            params["cwd"] = json!(cwd.to_string_lossy());
        }
        let result = self.request("pane.open", params)?;
        string_field(&result, "id")
            .map(ToOwned::to_owned)
            .context("pane.open response omitted id")
    }

    pub fn close_pane(&self, id: &str) -> Result<()> {
        self.request("pane.close", json!({"id": id}))?;
        Ok(())
    }

    pub fn rename_pane(&self, id: &str, title: &str) -> Result<()> {
        self.request("pane.rename", json!({"id": id, "title": title}))?;
        Ok(())
    }

    pub fn send_text(&self, id: &str, text: &str) -> Result<()> {
        self.request("pane.send_text", json!({"id": id, "text": text}))?;
        Ok(())
    }

    pub fn send_keys(&self, id: &str, keys: &[&str]) -> Result<()> {
        self.request("pane.send_keys", json!({"id": id, "keys": keys}))?;
        Ok(())
    }

    /// `workspace.rename` is a proposed hub addition (gaps report item 4).
    /// Returns whether the hub actually applied it, so a caller can warn
    /// instead of failing when it hasn't landed yet.
    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<bool> {
        let outcome = self.request_optional("workspace.rename", json!({"id": id, "name": name}))?;
        Ok(outcome.is_some())
    }

    /// `pane.set_metadata` is a proposed hub addition (gaps report item 1).
    /// On a hub that has it, tokens are written through to the hub. On one
    /// that doesn't, they land in the local journal keyed by `id` instead.
    pub fn report_tokens(&self, id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        let outcome =
            self.request_optional("pane.set_metadata", json!({"id": id, "set": tokens}))?;
        if outcome.is_none() {
            self.journal_merge(id, tokens)?;
        }
        Ok(())
    }

    /// Resolve what this backend believes `id`'s ownership tokens are,
    /// consulting the hub's reported metadata (if the hub supports it) and
    /// the local journal, and returning `Unknown` when they disagree (spec
    /// §9, brief item 3).
    pub fn resolve_ownership(&self, id: &str) -> Result<Ownership> {
        let hub_tokens = self.hub_reported_metadata(id)?;
        let journal_tokens = self.load_journal()?.panes.get(id).cloned();
        Ok(match (hub_tokens, journal_tokens) {
            (Some(hub), Some(journal)) if hub == journal => Ownership::Known(hub),
            (Some(_), Some(_)) => Ownership::Unknown,
            (Some(hub), None) => Ownership::Known(hub),
            (None, Some(journal)) => Ownership::Known(journal),
            (None, None) => Ownership::Unknown,
        })
    }

    fn hub_reported_metadata(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
        let snapshot = self.raw_snapshot()?;
        Ok(find_pane_field(&snapshot, id, "metadata")
            .and_then(|value| serde_json::from_value(value.clone()).ok()))
    }

    /// Process info from the gaps report's proposed `PaneInfo.process`
    /// field. `None` means the hub hasn't reported it (whether because it
    /// lacks the field or the pane truly has nothing to report), not an
    /// error — capability `process_info` stays `false` until this is real.
    pub fn process_info(&self, id: &str) -> Result<Option<ProcessInfo>> {
        let snapshot = self.raw_snapshot()?;
        let Some(process) = find_pane_field(&snapshot, id, "process") else {
            return Ok(None);
        };
        let argv = process
            .get("argv")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let pid = process
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok());
        Ok(Some(ProcessInfo { command: argv, pid }))
    }

    fn journal_path(&self) -> PathBuf {
        let digest = hex::encode(Sha256::digest(
            self.socket_path.to_string_lossy().as_bytes(),
        ));
        self.journal_root.join(format!("{digest}.json"))
    }

    fn load_journal(&self) -> Result<Journal> {
        match fs::read(self.journal_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid Radiator token journal"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Journal::default()),
            Err(error) => Err(error).context("cannot read Radiator token journal"),
        }
    }

    fn journal_merge(&self, id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        let mut journal = self.load_journal()?;
        journal
            .panes
            .entry(id.to_owned())
            .or_default()
            .extend(tokens.iter().map(|(k, v)| (k.clone(), v.clone())));
        let path = self.journal_path();
        let parent = path.parent().context("journal path has no parent")?;
        fs::create_dir_all(parent).context("cannot create Radiator journal directory")?;
        fs::write(&path, serde_json::to_vec_pretty(&journal)?)
            .context("cannot write Radiator token journal")?;
        Ok(())
    }
}

impl Backend for RadiatorClient {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tabs: false,
            splits_and_ratios: false,
            // `workspace.open` takes only `name`; no `cwd`/`env` param
            // exists to verify against (recon-radiator-report §6).
            workspace_env: false,
            pane_command_at_create: true,
            // No `agent.start`; an agent is run as a `serve` command instead
            // (spec §3 capability table).
            agent_start: false,
            agent_prompt: true,
            adopt_caller: true,
            // `pane.set_metadata` is a proposed hub addition (gaps report
            // item 1); tokens live in the local journal until it lands.
            metadata_tokens: false,
            // `PaneInfo.process` is a proposed hub addition (gaps report
            // item 3).
            process_info: false,
            events: true,
        }
    }

    fn caller_pane_id(&self) -> Option<String> {
        env::var("RADIATOR_PANE_ID")
            .ok()
            .filter(|id| !id.is_empty())
    }

    fn snapshot(&self) -> Result<SessionSnapshot> {
        let raw = self.raw_snapshot()?;
        let workspaces = raw
            .get("workspaces")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut snapshot = SessionSnapshot::default();
        for workspace in &workspaces {
            let Some(workspace_id) = workspace.get("id").and_then(Value::as_str) else {
                continue;
            };
            let label = workspace
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(workspace_id)
                .to_owned();
            snapshot.workspaces.push(WorkspaceInfo {
                workspace_id: workspace_id.to_owned(),
                label,
            });

            // Radiator has no tab layer (recon-radiator-report §2): every
            // pane in a workspace is folded into one synthetic tab so the
            // shared `SessionSnapshot` shape still has somewhere to put it.
            let tab_id = flat_tab_id(workspace_id);
            snapshot.tabs.push(TabInfo {
                tab_id: tab_id.clone(),
                workspace_id: workspace_id.to_owned(),
                label: String::new(),
            });

            for pane in workspace
                .get("panes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(pane_id) = pane.get("id").and_then(Value::as_str) else {
                    continue;
                };
                snapshot.panes.push(PaneInfo {
                    pane_id: pane_id.to_owned(),
                    tab_id: tab_id.clone(),
                    workspace_id: workspace_id.to_owned(),
                    cwd: None,
                });
                if pane.get("kind").and_then(Value::as_str) == Some("chat") {
                    snapshot.agents.push(AgentInfo {
                        pane_id: pane_id.to_owned(),
                        agent: String::new(),
                        agent_status: String::new(),
                    });
                }
            }
        }
        Ok(snapshot)
    }

    fn create_workspace(&self, label: &str, _cwd: &Path) -> Result<String> {
        // `workspace.open` has no `cwd` param; the caller's `cwd` becomes
        // each pane's `cwd` at `pane.open` time instead (create_tab below).
        self.open_workspace(label)
    }

    fn create_tab(
        &self,
        workspace_id: &str,
        tab_label: &str,
        root: Value,
    ) -> Result<ExportedLayout> {
        let mut specs = Vec::new();
        collect_pane_specs(&root, &mut specs);
        if specs.is_empty() {
            specs.push(PaneSpec::default());
        }
        if specs.len() > 1 || has_split(&root) {
            eprintln!(
                "warning: Radiator hub has no tabs or splits; flattening tab `{tab_label}` in workspace `{workspace_id}` into {} pane(s)",
                specs.len()
            );
        }

        let mut pane_ids = Vec::new();
        for spec in &specs {
            let title = spec.title.as_deref().unwrap_or(tab_label);
            let command = spec.command.first().map(String::as_str);
            let args = spec.command.get(1..).unwrap_or(&[]);
            let pane_id = self.open_pane(
                workspace_id,
                "term",
                title,
                command,
                args,
                spec.cwd.as_deref(),
                &spec.env,
            )?;
            pane_ids.push(pane_id);
        }

        let root = pane_ids
            .into_iter()
            .rev()
            .fold(None, |acc, pane_id| {
                let node = json!({"type": "pane", "pane_id": pane_id});
                Some(match acc {
                    None => node,
                    Some(rest) => json!({"type": "split", "first": node, "second": rest}),
                })
            })
            .unwrap_or_else(|| json!({"type": "pane", "pane_id": Value::Null}));

        Ok(ExportedLayout {
            workspace_id: workspace_id.to_owned(),
            tab_id: flat_tab_id(workspace_id),
            root,
        })
    }

    fn split_pane(
        &self,
        _pane_id: &str,
        _direction: SplitDirection,
        _ratio: f64,
        _command: Option<&[String]>,
        _cwd: Option<&Path>,
    ) -> Result<String> {
        bail!(
            "Radiator hub has no pane splits (capabilities().splits_and_ratios is false); open a new pane in the workspace instead"
        )
    }

    fn close_pane(&self, pane_id: &str) -> Result<()> {
        RadiatorClient::close_pane(self, pane_id)
    }

    fn set_ratio(&self, _tab_id: &str, _ratios: &[f64]) -> Result<()> {
        bail!("Radiator hub has no split ratios (capabilities().splits_and_ratios is false)")
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        if !RadiatorClient::rename_workspace(self, workspace_id, label)? {
            eprintln!(
                "warning: Radiator hub at {} has no workspace.rename yet; workspace `{workspace_id}` keeps its hub-assigned name",
                self.socket_path.display()
            );
        }
        Ok(())
    }

    fn rename_tab(&self, _tab_id: &str, _label: &str) -> Result<()> {
        // Radiator has no tab layer to rename; the tab's label is carried
        // only on the panes Drove opens under it (create_tab's `title`).
        Ok(())
    }

    fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()> {
        RadiatorClient::rename_pane(self, pane_id, label)
    }

    fn start_agent(
        &self,
        _pane_id: &str,
        _name: &str,
        _kind: &str,
        _args: &[String],
    ) -> Result<()> {
        bail!(
            "Radiator hub has no agent.start (capabilities().agent_start is false); declare the agent's argv as the pane's `serve` command instead"
        )
    }

    fn prompt_agent(&self, pane_id: &str, prompt: &str) -> Result<()> {
        self.send_text(pane_id, prompt)?;
        self.send_keys(pane_id, &["enter"])
    }

    fn process_info(&self, pane_id: &str) -> Result<Option<ProcessInfo>> {
        RadiatorClient::process_info(self, pane_id)
    }

    fn report_tokens(&self, address: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        RadiatorClient::report_tokens(self, address, tokens)
    }
}

#[derive(Debug, Default, Clone)]
struct PaneSpec {
    title: Option<String>,
    command: Vec<String>,
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
}

/// Walk an IR tab's `root` layout tree, collecting one [`PaneSpec`] per leaf
/// pane in left-to-right order. The exact shape of `root` is the planner's
/// (PR 2); this accepts the same `{"type": "split"|"pane", ...}` shape
/// [`ExportedLayout::pane_ids_preorder`] already reads, plus a `panes` list
/// for a flat tab with no split at all, and falls back to treating an
/// unrecognized leaf as one pane rather than dropping it.
fn collect_pane_specs(node: &Value, out: &mut Vec<PaneSpec>) {
    if let Some(panes) = node.get("panes").and_then(Value::as_array) {
        for pane in panes {
            collect_pane_specs(pane, out);
        }
        return;
    }
    if let (Some(first), Some(second)) = (node.get("first"), node.get("second")) {
        collect_pane_specs(first, out);
        collect_pane_specs(second, out);
        return;
    }
    out.push(pane_spec_from(node));
}

fn has_split(node: &Value) -> bool {
    node.get("type").and_then(Value::as_str) == Some("split")
        || (node.get("first").is_some() && node.get("second").is_some())
}

fn pane_spec_from(node: &Value) -> PaneSpec {
    let title = node
        .get("title")
        .or_else(|| node.get("label"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let command = node
        .get("command")
        .or_else(|| node.get("serve"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let cwd = node.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    let env = node
        .get("env")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    PaneSpec {
        title,
        command,
        cwd,
        env,
    }
}

fn flat_tab_id(workspace_id: &str) -> String {
    format!("{workspace_id}:panes")
}

fn find_pane_field<'a>(snapshot: &'a Value, pane_id: &str, field: &str) -> Option<&'a Value> {
    snapshot
        .get("workspaces")?
        .as_array()?
        .iter()
        .find_map(|workspace| {
            workspace.get("panes")?.as_array()?.iter().find_map(|pane| {
                if pane.get("id").and_then(Value::as_str) == Some(pane_id) {
                    pane.get(field)
                } else {
                    None
                }
            })
        })
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Deserialize)]
struct RpcError {
    code: String,
    message: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Journal {
    #[serde(default)]
    panes: BTreeMap<String, BTreeMap<String, String>>,
}

/// Resolve the hub's socket path: an explicit override, then the named-hub
/// default, then `RADIATOR_HUB_SOCKET`, then the `main` hub's default —
/// mirroring `radiator-cli`'s own `radiator_hub::paths::socket_path`
/// (`$XDG_RUNTIME_DIR/radiator/hub-{name}.sock`, falling back to
/// `~/.local/state/radiator/hub-{name}.sock`).
pub fn resolve_socket_path(explicit_socket: Option<&Path>, hub_name: Option<&str>) -> PathBuf {
    resolve_socket_path_with(
        explicit_socket,
        hub_name,
        env::var("RADIATOR_HUB_SOCKET").ok(),
    )
}

/// Testable core of [`resolve_socket_path`]: takes the environment variable
/// as a plain value instead of reading it, so tests never race each other
/// over process-wide state (mirrors `radiator-cli`'s own `socket_path_with`).
fn resolve_socket_path_with(
    explicit_socket: Option<&Path>,
    hub_name: Option<&str>,
    radiator_hub_socket: Option<String>,
) -> PathBuf {
    if let Some(path) = explicit_socket {
        return path.to_owned();
    }
    if let Some(hub_name) = hub_name {
        return hub_socket_path(hub_name);
    }
    if let Some(path) = radiator_hub_socket {
        return PathBuf::from(path);
    }
    hub_socket_path(DEFAULT_HUB_NAME)
}

fn hub_socket_path(hub_name: &str) -> PathBuf {
    radiator_runtime_dir().join(format!("hub-{hub_name}.sock"))
}

fn radiator_runtime_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("radiator");
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("radiator");
    }
    env::temp_dir().join("radiator")
}

fn journal_root() -> PathBuf {
    if let Ok(root) = env::var("DROVE_STATE_HOME") {
        return PathBuf::from(root).join("radiator-journal");
    }
    if let Ok(root) = env::var("XDG_STATE_HOME") {
        return PathBuf::from(root).join("drove").join("radiator-journal");
    }
    #[cfg(windows)]
    {
        if let Ok(root) = env::var("LOCALAPPDATA") {
            return PathBuf::from(root).join("drove").join("radiator-journal");
        }
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("drove")
            .join("radiator-journal");
    }
    env::temp_dir().join("drove-radiator-journal")
}

/// Whether Drove should default to the Radiator backend when no explicit
/// `--backend` flag is given: `RADIATOR_HUB_SOCKET` is set and `HERDR_ENV`
/// (Herdr's own pane-adoption marker) is not, so a Herdr pane never gets
/// silently redirected to a Radiator hub it happens to also have a socket
/// for (spec §6; brief item 4). CLI wiring for `--backend radiator` itself
/// lives in `src/cli.rs`, owned by the PR that touches it.
pub fn selected_by_environment() -> bool {
    selected_by_environment_with(
        env::var_os("HERDR_ENV").is_some(),
        env::var_os("RADIATOR_HUB_SOCKET").is_some(),
    )
}

fn selected_by_environment_with(herdr_env_is_set: bool, radiator_hub_socket_is_set: bool) -> bool {
    !herdr_env_is_set && radiator_hub_socket_is_set
}

#[cfg(unix)]
fn connect(path: &Path) -> std::io::Result<Stream> {
    use interprocess::local_socket::{GenericFilePath, prelude::*};

    Stream::connect(path.to_fs_name::<GenericFilePath>()?)
}

#[cfg(windows)]
fn connect(path: &Path) -> std::io::Result<Stream> {
    use interprocess::local_socket::{GenericNamespaced, prelude::*};

    Stream::connect(
        path.to_string_lossy()
            .to_string()
            .to_ns_name::<GenericNamespaced>()?,
    )
}

#[cfg(test)]
mod tests {
    use std::thread;

    use interprocess::local_socket::{Listener, ListenerOptions, traits::Listener as _};

    use super::*;

    fn fake_hub(path: PathBuf, handle: impl FnOnce(Value) -> Value + Send + 'static) {
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = handle(request);
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });
    }

    #[test]
    fn explicit_socket_wins_over_hub_name_and_environment() {
        let path = resolve_socket_path(Some(Path::new("/tmp/custom.sock")), Some("dev"));
        assert_eq!(path, PathBuf::from("/tmp/custom.sock"));
    }

    #[test]
    fn hub_name_produces_hub_prefixed_socket_file_name() {
        let path = resolve_socket_path(None, Some("dev"));
        assert_eq!(path.file_name().expect("file name"), "hub-dev.sock");
    }

    #[test]
    fn environment_socket_wins_when_no_explicit_hub_name() {
        let path = resolve_socket_path_with(None, None, Some("/tmp/from-env.sock".to_owned()));
        assert_eq!(path, PathBuf::from("/tmp/from-env.sock"));
    }

    #[test]
    fn default_hub_name_is_used_when_nothing_else_is_given() {
        let path = resolve_socket_path_with(None, None, None);
        assert_eq!(path.file_name().expect("file name"), "hub-main.sock");
    }

    #[test]
    fn selected_by_environment_requires_radiator_socket_and_no_herdr_env() {
        assert!(!selected_by_environment_with(false, false));
        assert!(selected_by_environment_with(false, true));
        assert!(!selected_by_environment_with(true, true));
        assert!(!selected_by_environment_with(true, false));
    }

    #[test]
    fn exchanges_one_ndjson_request_with_fake_hub() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-main.sock");
        fake_hub(
            path.clone(),
            |request| json!({"id": request["id"], "result": {"pong": true}}),
        );

        let result = RadiatorClient::new(path).ping().expect("ping");
        assert_eq!(result["pong"], true);
    }

    #[test]
    fn rejects_truncated_ndjson_response() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-truncated.sock");
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"pong": true}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
        });

        let error = RadiatorClient::new(path).ping().expect_err("truncated");
        assert!(error.to_string().contains("truncated"));
    }

    #[test]
    fn open_pane_maps_serve_argv_to_command_and_args() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-open-pane.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "pane.open");
            assert_eq!(request["params"]["command"], "agentmon");
            assert_eq!(request["params"]["args"][0], "--since");
            json!({"id": request["id"], "result": {"id": "w0:p1", "kind": "term", "title": "agentmon", "runner": "idle"}})
        });

        let client = RadiatorClient::new(path);
        let pane_id = client
            .open_pane(
                "w0",
                "term",
                "agentmon",
                Some("agentmon"),
                &["--since".to_owned()],
                None,
                &BTreeMap::new(),
            )
            .expect("open pane");
        assert_eq!(pane_id, "w0:p1");
    }

    #[test]
    fn create_tab_flattens_multiple_panes_into_preorder_ids() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-flatten.sock");
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            for expected_id in ["w0:p1", "w0:p2"] {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                assert_eq!(request["method"], "pane.open");
                let response = json!({
                    "id": request["id"],
                    "result": {"id": expected_id, "kind": "term", "title": "p", "runner": "idle"}
                });
                serde_json::to_writer(stream.get_mut(), &response).expect("write");
                stream.get_mut().write_all(b"\n").expect("newline");
            }
        });

        let client = RadiatorClient::new(path);
        let root = json!({
            "type": "split",
            "first": {"type": "pane", "command": ["bash"]},
            "second": {"type": "pane", "command": ["lazygit"]},
        });
        let layout = client
            .create_tab("w0", "lazygit", root)
            .expect("create tab");
        assert_eq!(layout.pane_ids_preorder(), ["w0:p1", "w0:p2"]);
        assert_eq!(layout.tab_id, "w0:panes");
    }

    #[test]
    fn rename_workspace_reports_unsupported_without_failing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-rename.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "workspace.rename");
            json!({"id": request["id"], "error": {"code": "unknown_method", "message": "no such method: workspace.rename"}})
        });

        let applied = RadiatorClient::new(path)
            .rename_workspace("w0", "renamed")
            .expect("rename call succeeds even when unsupported");
        assert!(!applied);
    }

    #[test]
    fn report_tokens_falls_back_to_local_journal_when_metadata_unsupported() {
        let state_home = tempfile::tempdir().expect("tempdir");
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-metadata.sock");
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            // First: `pane.set_metadata`, unsupported.
            {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                assert_eq!(request["method"], "pane.set_metadata");
                let response = json!({"id": request["id"], "error": {"code": "unknown_method", "message": "no such method: pane.set_metadata"}});
                serde_json::to_writer(stream.get_mut(), &response).expect("write");
                stream.get_mut().write_all(b"\n").expect("newline");
            }
            // Second: `hub.snapshot`, from `resolve_ownership`, with no
            // `metadata` field anywhere in it.
            {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                assert_eq!(request["method"], "hub.snapshot");
                let response = json!({
                    "id": request["id"],
                    "result": {"workspaces": [], "seq": 0}
                });
                serde_json::to_writer(stream.get_mut(), &response).expect("write");
                stream.get_mut().write_all(b"\n").expect("newline");
            }
        });

        let client = RadiatorClient::with_journal_root(path, state_home.path().to_owned());
        let mut tokens = BTreeMap::new();
        tokens.insert("drove_name".to_owned(), "gitlog".to_owned());
        client
            .report_tokens("w0:p1", &tokens)
            .expect("report tokens falls back to the journal");

        let ownership = client
            .resolve_ownership("w0:p1")
            .expect("resolve ownership");
        assert_eq!(ownership, Ownership::Known(tokens));
    }

    #[test]
    fn resolve_ownership_is_unknown_when_hub_and_journal_disagree() {
        let state_home = tempfile::tempdir().expect("tempdir");
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-disagree.sock");
        let client = RadiatorClient::with_journal_root(path.clone(), state_home.path().to_owned());

        let mut journal_tokens = BTreeMap::new();
        journal_tokens.insert("drove_digest".to_owned(), "old".to_owned());
        client
            .journal_merge("w0:p1", &journal_tokens)
            .expect("seed journal");

        fake_hub(path, |request| {
            assert_eq!(request["method"], "hub.snapshot");
            json!({
                "id": request["id"],
                "result": {
                    "workspaces": [{
                        "id": "w0",
                        "name": "dev",
                        "runner": "idle",
                        "panes": [{
                            "id": "w0:p1",
                            "kind": "term",
                            "title": "p",
                            "runner": "idle",
                            "metadata": {"drove_digest": "new"},
                        }],
                    }],
                    "seq": 1,
                }
            })
        });

        let ownership = client
            .resolve_ownership("w0:p1")
            .expect("resolve ownership");
        assert_eq!(ownership, Ownership::Unknown);
    }

    #[test]
    fn process_info_reads_the_proposed_process_field_when_present() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-process.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "hub.snapshot");
            json!({
                "id": request["id"],
                "result": {
                    "workspaces": [{
                        "id": "w0",
                        "name": "dev",
                        "runner": "idle",
                        "panes": [{
                            "id": "w0:p1",
                            "kind": "term",
                            "title": "p",
                            "runner": "working",
                            "process": {"pid": 4242, "argv": ["cargo", "watch"], "status": "running"},
                        }],
                    }],
                    "seq": 1,
                }
            })
        });

        let info = RadiatorClient::new(path)
            .process_info("w0:p1")
            .expect("process info")
            .expect("process field present");
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.command, vec!["cargo".to_owned(), "watch".to_owned()]);
    }

    #[test]
    fn process_info_is_none_when_the_hub_does_not_report_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-process.sock");
        fake_hub(path.clone(), |request| {
            json!({
                "id": request["id"],
                "result": {
                    "workspaces": [{
                        "id": "w0",
                        "name": "dev",
                        "runner": "idle",
                        "panes": [{"id": "w0:p1", "kind": "term", "title": "p", "runner": "idle"}],
                    }],
                    "seq": 1,
                }
            })
        });

        let info = RadiatorClient::new(path)
            .process_info("w0:p1")
            .expect("process info");
        assert!(info.is_none());
    }

    #[test]
    fn snapshot_flattens_every_pane_into_one_synthetic_tab_per_workspace() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-snapshot.sock");
        fake_hub(path.clone(), |request| {
            json!({
                "id": request["id"],
                "result": {
                    "workspaces": [{
                        "id": "w0",
                        "name": "dev",
                        "runner": "idle",
                        "panes": [
                            {"id": "w0:p1", "kind": "term", "title": "shell", "runner": "idle"},
                            {"id": "w0:p2", "kind": "chat", "title": "aide", "runner": "idle"},
                        ],
                    }],
                    "seq": 3,
                }
            })
        });

        let snapshot = RadiatorClient::new(path).snapshot().expect("snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.panes.len(), 2);
        assert!(snapshot.panes.iter().all(|pane| pane.tab_id == "w0:panes"));
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].pane_id, "w0:p2");
    }

    #[test]
    fn split_pane_and_set_ratio_report_the_missing_capability() {
        let client = RadiatorClient::new(PathBuf::from("/nonexistent.sock"));
        let split_error =
            Backend::split_pane(&client, "w0:p1", SplitDirection::Right, 0.5, None, None)
                .expect_err("no splits");
        assert!(split_error.to_string().contains("splits"));
        let ratio_error = Backend::set_ratio(&client, "w0:panes", &[0.5]).expect_err("no ratios");
        assert!(ratio_error.to_string().contains("ratios"));
    }

    #[test]
    fn start_agent_reports_the_missing_capability() {
        let client = RadiatorClient::new(PathBuf::from("/nonexistent.sock"));
        let error = Backend::start_agent(&client, "w0:p1", "review", "claude", &[])
            .expect_err("no agent.start");
        assert!(error.to_string().contains("agent.start"));
    }

    #[cfg(unix)]
    fn bind(path: &Path) -> std::io::Result<Listener> {
        use interprocess::local_socket::{GenericFilePath, prelude::*};

        ListenerOptions::new()
            .name(path.to_fs_name::<GenericFilePath>()?)
            .create_sync()
    }

    #[cfg(windows)]
    fn bind(path: &Path) -> std::io::Result<Listener> {
        use interprocess::local_socket::{GenericNamespaced, prelude::*};

        ListenerOptions::new()
            .name(
                path.to_string_lossy()
                    .to_string()
                    .to_ns_name::<GenericNamespaced>()?,
            )
            .create_sync()
    }
}
