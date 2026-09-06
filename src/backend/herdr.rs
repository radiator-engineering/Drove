//! Minimal typed client for Herdr's public NDJSON socket API.

use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Backend, Capabilities, HerdrExt, PaneSpec, ProcessInfo, Split};
use crate::model::SplitDirection;

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct HerdrClient {
    socket_path: PathBuf,
}

impl HerdrClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn discover(explicit_socket: Option<&Path>, session: Option<&str>) -> Self {
        Self::new(resolve_socket_path(explicit_socket, session))
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, None)
    }

    /// Sends a request, bounding how long the response read may block. Used
    /// by `output()` so an `output()` readiness probe (D23) cannot hang
    /// forever on a Herdr response that never arrives.
    pub fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let id = format!("drove:{}", REQUEST_ID.fetch_add(1, Ordering::Relaxed));
        let request = json!({"id": id, "method": method, "params": params});
        let stream = connect(&self.socket_path).with_context(|| {
            format!("cannot connect to Herdr at {}", self.socket_path.display())
        })?;
        // Best-effort: Windows named pipes (interprocess 2.4.4) don't
        // support socket-level receive timeouts and return `Unsupported`
        // for this call regardless of `timeout`, so a failure here must not
        // be fatal. The timeout is enforced independently below, on every
        // platform, via a bounded read on a helper thread.
        let _ = stream.set_recv_timeout(timeout);
        let mut stream = BufReader::new(stream);
        serde_json::to_writer(stream.get_mut(), &request).context("cannot encode Herdr request")?;
        stream
            .get_mut()
            .write_all(b"\n")
            .context("cannot send Herdr request")?;
        stream
            .get_mut()
            .flush()
            .context("cannot flush Herdr request")?;

        let line = read_line_with_timeout(stream, timeout, "Herdr")?;
        let response: ApiResponse =
            serde_json::from_str(&line).context("Herdr returned invalid JSON")?;
        if response.id != request["id"] {
            bail!("Herdr response id did not match the request");
        }
        if let Some(error) = response.error {
            bail!("Herdr API error {}: {}", error.code, error.message);
        }
        response.result.context("Herdr response omitted result")
    }

    pub fn ping(&self) -> Result<Value> {
        self.request("ping", json!({}))
    }

    pub fn snapshot(&self) -> Result<SessionSnapshot> {
        let result = self.request("session.snapshot", json!({}))?;
        let snapshot = result
            .get("snapshot")
            .cloned()
            .context("session.snapshot response omitted snapshot")?;
        let mut snapshot: SessionSnapshot =
            serde_json::from_value(snapshot).context("invalid Herdr session snapshot")?;
        for pane in &mut snapshot.panes {
            pane.process_info = self.pane_process_info(&pane.pane_id)?;
        }
        snapshot.caller_pane_id = caller_pane_id_from_env();
        Ok(snapshot)
    }

    pub fn export_layout(&self, tab_id: &str) -> Result<ExportedLayout> {
        let result = self.request("layout.export", json!({"tab_id": tab_id}))?;
        let layout = result
            .get("layout")
            .cloned()
            .context("layout.export response omitted layout")?;
        serde_json::from_value(layout).context("invalid Herdr layout export")
    }

    pub fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String> {
        let result = self.request(
            "workspace.create",
            json!({"label": label, "cwd": cwd, "focus": false}),
        )?;
        find_string(&result, "workspace_id")
            .map(ToOwned::to_owned)
            .context("workspace.create response omitted workspace_id")
    }

    pub fn apply_layout(
        &self,
        workspace_id: &str,
        tab_id: Option<&str>,
        tab_label: &str,
        root: Value,
    ) -> Result<ExportedLayout> {
        let mut params = json!({
            "tab_label": tab_label,
            "focus": false,
            "root": root,
        });
        if let Some(tab_id) = tab_id {
            params["tab_id"] = json!(tab_id);
        } else {
            params["workspace_id"] = json!(workspace_id);
        }
        let result = self.request("layout.apply", params)?;
        let layout = result
            .get("layout")
            .cloned()
            .or_else(|| result.get("created_layout").cloned())
            .context("layout.apply response omitted layout")?;
        serde_json::from_value(layout).context("invalid applied Herdr layout")
    }

    pub fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        self.request(
            "workspace.rename",
            json!({"workspace_id": workspace_id, "label": label}),
        )?;
        Ok(())
    }

    pub fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()> {
        self.request("tab.rename", json!({"tab_id": tab_id, "label": label}))?;
        Ok(())
    }

    pub fn start_agent(
        &self,
        pane_id: &str,
        name: &str,
        kind: &str,
        args: &[String],
    ) -> Result<()> {
        self.request(
            "agent.start",
            json!({
                "pane_id": pane_id,
                "name": name,
                "kind": kind,
                "args": args,
            }),
        )?;
        Ok(())
    }

    pub fn report_workspace_status(&self, workspace_id: &str, status: &str) -> Result<()> {
        self.request(
            "workspace.report_metadata",
            json!({
                "workspace_id": workspace_id,
                "source": "drove",
                "tokens": {"drove_status": status},
            }),
        )?;
        Ok(())
    }

    pub fn report_metadata(
        &self,
        address: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        if is_pane_id(address) {
            self.report_pane_metadata(address, tokens)
        } else {
            self.report_workspace_metadata(address, tokens)
        }
    }

    pub fn report_workspace_metadata(
        &self,
        workspace_id: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        self.request(
            "workspace.report_metadata",
            json!({
                "workspace_id": workspace_id,
                "source": "drove",
                "tokens": tokens,
            }),
        )?;
        Ok(())
    }

    pub fn report_pane_metadata(
        &self,
        pane_id: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        self.request(
            "pane.report_metadata",
            json!({
                "pane_id": pane_id,
                "source": "drove",
                "tokens": tokens,
            }),
        )?;
        Ok(())
    }

    pub fn split_pane(
        &self,
        target_pane_id: &str,
        direction: SplitDirection,
        ratio: f64,
        command: Option<&[String]>,
        cwd: Option<&Path>,
    ) -> Result<String> {
        let mut params = json!({
            "target_pane_id": target_pane_id,
            "direction": direction,
            "ratio": ratio,
            "focus": false,
        });
        if let Some(cwd) = cwd {
            params["cwd"] = json!(cwd);
        }
        let result = self.request("pane.split", params)?;
        let pane = result
            .get("pane")
            .context("pane.split response omitted pane")?;
        let pane_id = pane
            .get("pane_id")
            .and_then(Value::as_str)
            .context("pane.split response omitted pane_id")?
            .to_owned();
        if let Some(command) = command {
            self.run_command(&pane_id, command)?;
        }
        Ok(pane_id)
    }

    /// Types a command into an empty shell pane and submits it. `pane.split`
    /// has no `command` param (API report §"Command in pane vs starting a
    /// pane with a command"); Herdr only accepts argv at pane creation via
    /// `layout.apply`.
    pub fn run_command(&self, pane_id: &str, command: &[String]) -> Result<()> {
        if command.is_empty() {
            return Ok(());
        }
        self.request(
            "pane.send_text",
            json!({"pane_id": pane_id, "text": shell_join(command)}),
        )?;
        self.request(
            "pane.send_keys",
            json!({"pane_id": pane_id, "keys": ["Enter"]}),
        )?;
        Ok(())
    }

    pub fn close_pane(&self, pane_id: &str) -> Result<()> {
        self.request("pane.close", json!({"pane_id": pane_id}))?;
        Ok(())
    }

    /// Closes a whole workspace (`workspace.close`), for callers (test
    /// cleanup, tear-down) that own a workspace outright rather than an
    /// individual pane.
    pub fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        self.request("workspace.close", json!({"workspace_id": workspace_id}))?;
        Ok(())
    }

    /// Sets every split ratio in a tab from a flat, left-to-right ratio
    /// list (spec D6: a tab is a pane list with a shared `split` direction
    /// and one ratio per gap, not a binary tree). Herdr addresses one split
    /// node per call by a boolean path from the tab root: `false` follows
    /// `first`, `true` follows `second`. A flat list nests right-leaning
    /// (`pane0 | (pane1 | (pane2 | pane3))`), so ratio `i` lives at the
    /// split reached by taking `second` `i` times.
    pub fn set_ratio(&self, tab_id: &str, ratios: &[f64]) -> Result<()> {
        for (index, ratio) in ratios.iter().enumerate() {
            let path: Vec<bool> = std::iter::repeat_n(true, index).collect();
            self.request(
                "layout.set_split_ratio",
                json!({"tab_id": tab_id, "path": path, "ratio": ratio}),
            )?;
        }
        Ok(())
    }

    pub fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()> {
        self.request("pane.rename", json!({"pane_id": pane_id, "label": label}))?;
        Ok(())
    }

    pub fn prompt_agent(&self, target: &str, text: &str) -> Result<()> {
        self.request("agent.prompt", json!({"target": target, "text": text}))?;
        Ok(())
    }

    pub fn pane_process_info(&self, pane_id: &str) -> Result<Option<ProcessInfo>> {
        let result = self.request("pane.process_info", json!({"pane_id": pane_id}))?;
        let info = result
            .get("process_info")
            .cloned()
            .context("pane.process_info response omitted process_info")?;
        let info: RawPaneProcessInfo =
            serde_json::from_value(info).context("invalid Herdr process info")?;
        Ok(info.into_process_info())
    }

    /// Recent pane output (`pane.read`, `source: "recent"`), the host-side
    /// input to an `output()` readiness probe (D23). `timeout` bounds the
    /// response read so a withheld response cannot hang the probe forever.
    pub fn output(&self, pane_id: &str, timeout: Duration) -> Result<String> {
        let result = self.request_with_timeout(
            "pane.read",
            json!({
                "pane_id": pane_id,
                "source": "recent",
                "format": "text",
                "strip_ansi": true,
            }),
            Some(timeout),
        )?;
        result
            .get("read")
            .and_then(|read| read.get("text"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .context("pane.read response omitted text")
    }
}

/// Herdr pane ids are `<workspace>:p<n>`; tab ids are `<workspace>:t<n>`;
/// workspace ids carry no `:` (API report §"Caller's pane").
fn is_pane_id(address: &str) -> bool {
    address
        .rsplit_once(':')
        .is_some_and(|(_, segment)| segment.starts_with('p'))
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Deserialize)]
struct RawPaneProcessInfo {
    #[serde(default)]
    shell_pid: Option<u32>,
    #[serde(default)]
    foreground_processes: Vec<RawPaneProcess>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPaneProcess {
    pid: u32,
    #[serde(default)]
    argv: Option<Vec<String>>,
    name: String,
}

impl RawPaneProcessInfo {
    fn into_process_info(self) -> Option<ProcessInfo> {
        if let Some(process) = self.foreground_processes.into_iter().next() {
            let command = process.argv.unwrap_or_else(|| vec![process.name]);
            return Some(ProcessInfo {
                command,
                pid: Some(process.pid),
            });
        }
        self.shell_pid.map(|pid| ProcessInfo {
            command: Vec::new(),
            pid: Some(pid),
        })
    }
}

impl Backend for HerdrClient {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            workspace_env: true,
            pane_command_at_create: true,
            metadata_tokens: true,
            process_info: true,
            events: true,
            readiness_output: true,
        }
    }

    fn caller_pane_id(&self) -> Option<String> {
        caller_pane_id_from_env()
    }

    fn snapshot(&self) -> Result<SessionSnapshot> {
        HerdrClient::snapshot(self)
    }

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String> {
        HerdrClient::create_workspace(self, label, cwd)
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        HerdrClient::rename_workspace(self, workspace_id, label)
    }

    /// Opens a pane with no placement by applying a single-pane layout to
    /// `workspace_id`, which Herdr places in the workspace's first tab
    /// (spec §3). A Herdr placement is applied afterwards through
    /// [`HerdrExt`].
    fn create_pane(&self, workspace_id: &str, spec: &PaneSpec) -> Result<String> {
        let label = spec.label.as_deref().unwrap_or("pane");
        let mut leaf = json!({"type": "pane", "label": label});
        if let Some(command) = &spec.command {
            leaf["command"] = json!(command);
        }
        if let Some(cwd) = &spec.cwd {
            leaf["cwd"] = json!(cwd);
        }
        let layout = HerdrClient::apply_layout(self, workspace_id, None, label, leaf)?;
        layout
            .pane_ids_preorder()
            .into_iter()
            .next()
            .context("layout.apply for create_pane returned no pane")
    }

    fn close_pane(&self, pane_id: &str) -> Result<()> {
        HerdrClient::close_pane(self, pane_id)
    }

    fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()> {
        HerdrClient::rename_pane(self, pane_id, label)
    }

    fn restart_command(&self, pane_id: &str, argv: &[String]) -> Result<()> {
        HerdrClient::run_command(self, pane_id, argv)
    }

    fn prompt_agent(&self, pane_id: &str, prompt: &str) -> Result<()> {
        HerdrClient::prompt_agent(self, pane_id, prompt)
    }

    fn process_info(&self, pane_id: &str) -> Result<Option<ProcessInfo>> {
        HerdrClient::pane_process_info(self, pane_id)
    }

    fn report_tokens(
        &self,
        address: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        HerdrClient::report_metadata(self, address, tokens)
    }

    fn output(&self, pane_id: &str, timeout: Duration) -> Result<String> {
        HerdrClient::output(self, pane_id, timeout)
    }

    fn herdr(&self) -> Option<&dyn HerdrExt> {
        Some(self)
    }
}

impl HerdrExt for HerdrClient {
    /// Creates a tab holding one initial pane (Herdr has no empty tab) and
    /// returns its tab id; `HerdrExt::split_pane` adds the rest. `split` and
    /// `ratios` describe the finished layout, so `ratios` is applied here and
    /// `split` is honored per split when panes are added.
    fn create_tab(
        &self,
        workspace_id: &str,
        label: &str,
        _split: Split,
        ratios: &[f64],
    ) -> Result<String> {
        let leaf = json!({"type": "pane", "label": label});
        let layout = HerdrClient::apply_layout(self, workspace_id, None, label, leaf)?;
        if !ratios.is_empty() {
            HerdrClient::set_ratio(self, &layout.tab_id, ratios)?;
        }
        Ok(layout.tab_id)
    }

    /// Splits the tab's current last pane in `split` direction, opening a new
    /// pane from `spec`. Ratios across the tab are set separately with
    /// [`HerdrExt::set_ratio`].
    fn split_pane(&self, tab_id: &str, spec: &PaneSpec, split: Split) -> Result<String> {
        let target = HerdrClient::export_layout(self, tab_id)?
            .pane_ids_preorder()
            .into_iter()
            .next_back()
            .with_context(|| format!("tab `{tab_id}` has no pane to split"))?;
        let command = spec.command.as_deref();
        let pane_id =
            HerdrClient::split_pane(self, &target, split, 0.5, command, spec.cwd.as_deref())?;
        if let Some(label) = &spec.label {
            HerdrClient::rename_pane(self, &pane_id, label)?;
        }
        Ok(pane_id)
    }

    fn set_ratio(&self, tab_id: &str, ratios: &[f64]) -> Result<()> {
        HerdrClient::set_ratio(self, tab_id, ratios)
    }

    fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()> {
        HerdrClient::rename_tab(self, tab_id, label)
    }

    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()> {
        HerdrClient::start_agent(self, pane_id, name, kind, args)
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
    /// The invoking pane, from `HERDR_PANE_ID` when Drove runs inside a
    /// Herdr-managed pane (D8, D24). Not part of the wire payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}

impl SessionSnapshot {
    pub fn workspace(&self, id: &str) -> Option<&WorkspaceInfo> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == id)
    }

    pub fn tab(&self, id: &str) -> Option<&TabInfo> {
        self.tabs.iter().find(|tab| tab.tab_id == id)
    }

    pub fn pane(&self, id: &str) -> Option<&PaneInfo> {
        self.panes.iter().find(|pane| pane.pane_id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub tokens: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub tab_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub tokens: std::collections::BTreeMap<String, String>,
    /// Filled in by `HerdrClient::snapshot` with one `pane.process_info`
    /// call per pane; absent from the raw `session.snapshot` payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_info: Option<ProcessInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub agent_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportedLayout {
    pub workspace_id: String,
    pub tab_id: String,
    #[serde(default)]
    pub root: Value,
}

impl ExportedLayout {
    pub fn pane_ids_preorder(&self) -> Vec<String> {
        fn visit(node: &Value, ids: &mut Vec<String>) {
            if node.get("type").and_then(Value::as_str) == Some("pane") {
                if let Some(id) = node.get("pane_id").and_then(Value::as_str) {
                    ids.push(id.to_owned());
                }
                return;
            }
            if let Some(first) = node.get("first") {
                visit(first, ids);
            }
            if let Some(second) = node.get("second") {
                visit(second, ids);
            }
        }
        let mut ids = Vec::new();
        visit(&self.root, &mut ids);
        ids
    }
}

fn caller_pane_id_from_env() -> Option<String> {
    env::var("HERDR_PANE_ID").ok().filter(|id| !id.is_empty())
}

fn find_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str).or_else(|| {
        value
            .as_object()
            .and_then(|object| object.values().find_map(|child| find_string(child, key)))
    })
}

pub fn resolve_socket_path(explicit_socket: Option<&Path>, session: Option<&str>) -> PathBuf {
    if let Some(path) = explicit_socket {
        return path.to_owned();
    }
    if let Some(session) = session {
        return herdr_config_dir()
            .join("sessions")
            .join(session)
            .join("herdr.sock");
    }
    if let Ok(path) = env::var("HERDR_SOCKET_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(session) = env::var("HERDR_SESSION")
        && !session.is_empty()
        && session != "default"
    {
        return herdr_config_dir()
            .join("sessions")
            .join(session)
            .join("herdr.sock");
    }
    herdr_config_dir().join("herdr.sock")
}

fn herdr_config_dir() -> PathBuf {
    if let Ok(root) = env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(root).join("herdr");
    }
    #[cfg(windows)]
    {
        if let Ok(root) = env::var("APPDATA") {
            return PathBuf::from(root).join("herdr");
        }
        if let Ok(root) = env::var("USERPROFILE") {
            return PathBuf::from(root)
                .join("AppData")
                .join("Roaming")
                .join("herdr");
        }
    }
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".config").join("herdr");
    }
    env::temp_dir().join("herdr")
}

/// Reads one NDJSON line, bounded by `timeout` regardless of whether the
/// platform's local-socket backend honors a socket-level receive timeout
/// (Windows named pipes, as of interprocess 2.4.4, do not). The blocking
/// read runs on a helper thread; the caller waits on a channel instead of
/// the read itself, so the bound applies on every OS. If the timeout
/// elapses, the helper thread is left to finish (or leak) on its own —
/// the socket has no way to be pulled out from under a blocking read.
fn read_line_with_timeout(
    mut stream: BufReader<Stream>,
    timeout: Option<Duration>,
    label: &str,
) -> Result<String> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let outcome = stream.read_line(&mut line).map(|read| (read, line));
        let _ = sender.send(outcome);
    });
    let (read, line) = match timeout {
        Some(duration) => receiver
            .recv_timeout(duration)
            .map_err(|_| anyhow::anyhow!("{label} response timed out after {duration:?}"))?
            .with_context(|| format!("cannot read {label} response"))?,
        None => receiver
            .recv()
            .context("response reader thread disconnected without a result")?
            .with_context(|| format!("cannot read {label} response"))?,
    };
    if read == 0 {
        bail!("{label} closed the socket without a response");
    }
    if !line.ends_with('\n') {
        bail!("{label} returned a truncated response without an NDJSON newline");
    }
    Ok(line)
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

    #[test]
    fn resolves_named_session_before_environment_override() {
        let path = resolve_socket_path(Some(Path::new("/tmp/custom.sock")), Some("work"));
        assert_eq!(path, PathBuf::from("/tmp/custom.sock"));
    }

    #[test]
    fn extracts_layout_panes_in_preorder() {
        let layout = ExportedLayout {
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            root: json!({
                "type": "split",
                "first": {"type": "pane", "pane_id": "w1:p1"},
                "second": {"type": "pane", "pane_id": "w1:p2"}
            }),
        };
        assert_eq!(layout.pane_ids_preorder(), ["w1:p1", "w1:p2"]);
    }

    #[test]
    fn shell_join_quotes_every_argument() {
        let joined = shell_join(&[
            "echo".into(),
            "$(rm -rf /)".into(),
            "a;b".into(),
            "`whoami`".into(),
            "*.rs".into(),
            "a>b".into(),
            "it's".into(),
        ]);
        assert_eq!(
            joined,
            r"'echo' '$(rm -rf /)' 'a;b' '`whoami`' '*.rs' 'a>b' 'it'\''s'"
        );
    }

    #[test]
    fn exchanges_one_ndjson_request_with_fake_server() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {"type": "pong"}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let result = HerdrClient::new(path).ping().expect("ping");
        assert_eq!(result["type"], "pong");
        server.join().expect("server thread");
    }

    #[test]
    fn agent_start_targets_the_resolved_pane() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-agent.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(request["method"], "agent.start");
            assert_eq!(request["params"]["pane_id"], "w1:p2");
            assert_eq!(request["params"]["kind"], "cursor");
            let response = json!({
                "id": request["id"],
                "result": {"type": "agent_started"}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        HerdrClient::new(path)
            .start_agent("w1:p2", "review", "cursor", &["--model".into()])
            .expect("start agent");
        server.join().expect("server thread");
    }

    #[test]
    fn split_pane_types_and_submits_the_command() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-split.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..3 {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let result = match request["method"].as_str().expect("method") {
                    "pane.split" => json!({"pane": {"pane_id": "w1:p3"}}),
                    "pane.send_text" | "pane.send_keys" => json!({"type": "ok"}),
                    other => panic!("unexpected method {other}"),
                };
                let response = json!({"id": request["id"], "result": result});
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
                requests.push(request);
            }
            requests
        });

        let pane_id = HerdrClient::new(path)
            .split_pane(
                "w1:p1",
                SplitDirection::Down,
                0.5,
                Some(&["echo".into(), "hi there".into()]),
                None,
            )
            .expect("split pane");
        assert_eq!(pane_id, "w1:p3");

        let requests = server.join().expect("server thread");
        assert_eq!(requests[0]["method"], "pane.split");
        assert_eq!(requests[0]["params"]["target_pane_id"], "w1:p1");
        assert_eq!(requests[0]["params"]["direction"], "down");
        assert_eq!(requests[1]["method"], "pane.send_text");
        assert_eq!(requests[1]["params"]["pane_id"], "w1:p3");
        assert_eq!(requests[1]["params"]["text"], "'echo' 'hi there'");
        assert_eq!(requests[2]["method"], "pane.send_keys");
        assert_eq!(requests[2]["params"]["keys"], json!(["Enter"]));
    }

    #[test]
    fn set_ratio_sends_one_right_leaning_path_per_gap() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-ratio.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let response = json!({
                    "id": request["id"],
                    "result": {"type": "layout_split_ratio_set"}
                });
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
                requests.push(request);
            }
            requests
        });

        HerdrClient::new(path)
            .set_ratio("w1:t1", &[0.5, 0.3])
            .expect("set ratio");

        let requests = server.join().expect("server thread");
        assert_eq!(requests[0]["params"]["path"], json!([]));
        assert_eq!(requests[0]["params"]["ratio"], 0.5);
        assert_eq!(requests[1]["params"]["path"], json!([true]));
        assert_eq!(requests[1]["params"]["ratio"], 0.3);
    }

    #[test]
    fn report_metadata_addresses_a_pane_id_at_pane_report_metadata() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-tokens-pane.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "ok"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        let mut tokens = std::collections::BTreeMap::new();
        tokens.insert("drove_digest".to_owned(), "abc123".to_owned());
        HerdrClient::new(path)
            .report_metadata("w1:p2", &tokens)
            .expect("report metadata");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "pane.report_metadata");
        assert_eq!(request["params"]["pane_id"], "w1:p2");
    }

    #[test]
    fn report_metadata_addresses_a_workspace_id_at_workspace_report_metadata() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-tokens-workspace.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "ok"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        let mut tokens = std::collections::BTreeMap::new();
        tokens.insert("drove_digest".to_owned(), "abc123".to_owned());
        HerdrClient::new(path)
            .report_metadata("w1", &tokens)
            .expect("report metadata");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "workspace.report_metadata");
        assert_eq!(request["params"]["workspace_id"], "w1");
    }

    #[test]
    fn process_info_prefers_the_foreground_process_over_the_shell() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-process-info.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {"process_info": {
                    "pane_id": "w1:p1",
                    "shell_pid": 100,
                    "foreground_processes": [
                        {"pid": 200, "name": "cargo", "argv": ["cargo", "test"]}
                    ]
                }}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let info = HerdrClient::new(path)
            .pane_process_info("w1:p1")
            .expect("process info")
            .expect("some process");
        assert_eq!(info.command, vec!["cargo".to_owned(), "test".to_owned()]);
        assert_eq!(info.pid, Some(200));
        server.join().expect("server thread");
    }

    #[test]
    fn process_info_falls_back_to_the_shell_when_nothing_is_foreground() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-process-info-shell.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {"process_info": {
                    "pane_id": "w1:p1",
                    "shell_pid": 100,
                    "foreground_processes": []
                }}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let info = HerdrClient::new(path)
            .pane_process_info("w1:p1")
            .expect("process info")
            .expect("some process");
        assert!(info.command.is_empty());
        assert_eq!(info.pid, Some(100));
        server.join().expect("server thread");
    }

    #[test]
    fn output_reads_recent_pane_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-output.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(request["method"], "pane.read");
            assert_eq!(request["params"]["source"], "recent");
            let response = json!({
                "id": request["id"],
                "result": {"read": {"text": "scaffold: watching for changes"}}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let text = HerdrClient::new(path)
            .output("w1:p1", Duration::from_secs(1))
            .expect("pane output");
        assert_eq!(text, "scaffold: watching for changes");
        server.join().expect("server thread");
    }

    #[test]
    fn output_times_out_on_a_withheld_response() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-output-timeout.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            // Accept the request but never respond, holding the connection
            // open past the client's timeout.
            stream.read_line(&mut line).expect("read");
            thread::sleep(Duration::from_millis(500));
        });

        let error = HerdrClient::new(path)
            .output("w1:p1", Duration::from_millis(100))
            .expect_err("withheld response times out");
        assert!(error.to_string().contains("Herdr response"));
        server.join().expect("server thread");
    }

    #[test]
    fn close_pane_sends_the_target_pane_id() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-close.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "ok"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        HerdrClient::new(path)
            .close_pane("w1:p2")
            .expect("close pane");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "pane.close");
        assert_eq!(request["params"]["pane_id"], "w1:p2");
    }

    #[test]
    fn close_workspace_sends_the_target_workspace_id() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-close-workspace.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "ok"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        HerdrClient::new(path)
            .close_workspace("w1")
            .expect("close workspace");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "workspace.close");
        assert_eq!(request["params"]["workspace_id"], "w1");
    }

    #[test]
    fn rename_pane_sends_the_pane_id_and_label() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-rename-pane.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "ok"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        HerdrClient::new(path)
            .rename_pane("w1:p2", "renamed")
            .expect("rename pane");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "pane.rename");
        assert_eq!(request["params"]["pane_id"], "w1:p2");
        assert_eq!(request["params"]["label"], "renamed");
    }

    #[test]
    fn prompt_agent_sends_the_target_and_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-prompt.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "agent_prompted"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        HerdrClient::new(path)
            .prompt_agent("review", "fix the bug")
            .expect("prompt agent");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "agent.prompt");
        assert_eq!(request["params"]["target"], "review");
        assert_eq!(request["params"]["text"], "fix the bug");
    }

    #[test]
    fn rejects_truncated_ndjson_response() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-truncated.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {"type": "pong"}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
        });

        let error = HerdrClient::new(path)
            .ping()
            .expect_err("truncated response");
        assert!(error.to_string().contains("truncated response"));
        server.join().expect("server thread");
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
