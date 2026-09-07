//! Minimal typed client for Herdr's public NDJSON socket API.

use std::{
    collections::HashMap,
    env,
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Backend, Capabilities, HerdrExt, PaneSpec, ProcessInfo, SessionState, SessionStop, Split,
    TabLayout,
};
use crate::model::SplitDirection;

/// How long [`HerdrExt::ensure_session`] waits for a just-started session's
/// socket to answer `ping` before giving up (D43 step 2).
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(10);

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct HerdrClient {
    socket_path: PathBuf,
    /// Root tab ids captured from `workspace.create` responses, keyed by
    /// workspace id, for workspaces this client created (D49). Taken (and
    /// cleared) by [`HerdrClient::take_root_tab`] the first time the first
    /// declared tab of that workspace is applied.
    created_root_tabs: Mutex<HashMap<String, String>>,
}

impl HerdrClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            created_root_tabs: Mutex::new(HashMap::new()),
        }
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

    /// `session.snapshot` decides reachability on its own (D51 point 2): a
    /// failing `pane.process_info` call for one pane — a race with the pane
    /// closing between the two round trips, or a transient socket hiccup —
    /// leaves that pane's `process_info` as `None` and is recorded in
    /// `process_info_unavailable` instead of failing the whole snapshot.
    pub fn snapshot(&self) -> Result<SessionSnapshot> {
        let result = self.request("session.snapshot", json!({}))?;
        let snapshot = result
            .get("snapshot")
            .cloned()
            .context("session.snapshot response omitted snapshot")?;
        let mut snapshot: SessionSnapshot =
            serde_json::from_value(snapshot).context("invalid Herdr session snapshot")?;
        for pane in &mut snapshot.panes {
            match self.pane_process_info(&pane.pane_id) {
                Ok(info) => pane.process_info = info,
                Err(_) => {
                    pane.process_info = None;
                    snapshot.process_info_unavailable.push(pane.pane_id.clone());
                }
            }
        }
        // D51 point 3: only trust `HERDR_PANE_ID` when it names a pane the
        // snapshot actually lists; a stale value (the pane was closed and
        // the id reused, or the var leaked into an unrelated shell) is
        // dropped so `adopt = "caller"` plans as a normal create instead of
        // adopting a phantom pane.
        snapshot.caller_pane_id = live_caller_pane_id(caller_pane_id_from_env(), &snapshot.panes);
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

    /// Creates a workspace and captures the root tab Herdr always returns
    /// with it (`root_pane.tab_id`, or `tab.tab_id`), so the first declared
    /// tab of this workspace can reuse it instead of opening a stray extra
    /// tab (D49, issue 25). Retrieve it with
    /// [`HerdrClient::take_root_tab`].
    pub fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String> {
        let result = self.request(
            "workspace.create",
            json!({"label": label, "cwd": cwd, "focus": false}),
        )?;
        let workspace_id = find_string(&result, "workspace_id")
            .map(ToOwned::to_owned)
            .context("workspace.create response omitted workspace_id")?;
        if let Some(tab_id) = find_string(&result, "tab_id") {
            self.created_root_tabs
                .lock()
                .expect("root tab lock poisoned")
                .insert(workspace_id.clone(), tab_id.to_owned());
        }
        Ok(workspace_id)
    }

    /// Takes (and clears) the root tab id captured for `workspace_id` by
    /// [`HerdrClient::create_workspace`], if this client created it earlier
    /// in the current process (D49).
    pub fn take_root_tab(&self, workspace_id: &str) -> Option<String> {
        self.created_root_tabs
            .lock()
            .expect("root tab lock poisoned")
            .remove(workspace_id)
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

    /// Not cross-checked against a live snapshot here — that would cost a
    /// second `session.snapshot` request on every call, when the caller
    /// almost always has a freshly fetched one already in hand. D51 point 3
    /// is enforced instead at each call site in `src/cli.rs` that already
    /// holds a live `SessionSnapshot`: it filters this method's result
    /// against that snapshot's `panes`, so a stale or leaked env value that
    /// names a pane the snapshot doesn't list is dropped.
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
    /// Builds a fresh tab holding every pane in `panes`. Herdr has no empty
    /// tab, so the first pane opens the tab (through `layout.apply`, which
    /// honors the leaf's command and cwd) and each remaining pane is split in
    /// with [`HerdrExt::split_pane`]. Ratios address split gaps, so they are
    /// applied only after every gap exists — never against the one-pane tab,
    /// which has no gap to address (D6, D29).
    fn create_tab(
        &self,
        workspace_id: &str,
        label: &str,
        split: Split,
        ratios: &[f64],
        panes: &[PaneSpec],
        existing_tab: Option<&str>,
    ) -> Result<TabLayout> {
        let first = panes.first();
        let first_label = first
            .and_then(|spec| spec.label.clone())
            .unwrap_or_else(|| label.to_owned());
        let mut leaf = json!({"type": "pane", "label": first_label});
        if let Some(command) = first.and_then(|spec| spec.command.as_ref()) {
            leaf["command"] = json!(command);
        }
        if let Some(cwd) = first.and_then(|spec| spec.cwd.as_ref()) {
            leaf["cwd"] = json!(cwd);
        }
        let layout = HerdrClient::apply_layout(self, workspace_id, existing_tab, label, leaf)?;
        let mut pane_ids = layout.pane_ids_preorder();
        let tab_id = layout.tab_id;

        if existing_tab.is_some() {
            HerdrClient::rename_tab(self, &tab_id, label)?;
        }

        for spec in panes.iter().skip(1) {
            let pane_id = <Self as HerdrExt>::split_pane(self, &tab_id, spec, split)?;
            pane_ids.push(pane_id);
        }
        if !ratios.is_empty() {
            HerdrClient::set_ratio(self, &tab_id, ratios)?;
        }
        Ok(TabLayout { tab_id, pane_ids })
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

    fn take_root_tab(&self, workspace_id: &str) -> Option<String> {
        HerdrClient::take_root_tab(self, workspace_id)
    }

    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()> {
        HerdrClient::start_agent(self, pane_id, name, kind, args)
    }

    fn focus_workspace(&self, id: &str) -> Result<()> {
        self.request("workspace.focus", json!({"workspace_id": id}))?;
        Ok(())
    }

    /// Pings this client's socket first: an answer means the session is
    /// already up ([`SessionState::Running`]) and nothing is started. When it
    /// does not answer, this shells out to the `herdr` binary to run its
    /// headless server for the named session (`herdr server --session NAME`,
    /// verified against Herdr 0.8.2), then waits up to
    /// `SESSION_START_TIMEOUT` for the socket to answer. If the binary is
    /// missing, the spawn fails, or the socket never comes up, it returns
    /// [`SessionState::CannotStart`] with the exact `herdr --session NAME`
    /// command for the user to run in a terminal (D43 step 2, D44).
    fn ensure_session(&self, name: &str) -> Result<SessionState> {
        if self.ping().is_ok() {
            return Ok(SessionState::Running);
        }
        let hint = format!("herdr --session {name}");
        if start_session_server(name).is_err() {
            return Ok(SessionState::CannotStart { hint });
        }
        if self.wait_for_ping(SESSION_START_TIMEOUT) {
            Ok(SessionState::Started)
        } else {
            Ok(SessionState::CannotStart { hint })
        }
    }

    fn stop_session(&self, name: &str) -> Result<SessionStop> {
        run_stop_session(&herdr_bin_path(), name)
    }
}

impl HerdrClient {
    /// Polls `ping` until the socket answers or `timeout` elapses.
    fn wait_for_ping(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.ping().is_ok() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// The `herdr` binary to shell out to: `HERDR_BIN_PATH` when set, else `herdr`
/// resolved on `PATH` (D44).
fn herdr_bin_path() -> OsString {
    env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| OsString::from("herdr"))
}

/// Starts the named session's headless server (`herdr server --session NAME`).
/// The child is left running detached from Drove's own stdio; Drove does not
/// wait on it. Errors only when the process cannot be spawned at all (a
/// missing binary); a server that starts but never opens its socket is caught
/// by the caller's `wait_for_ping`.
fn start_session_server(name: &str) -> Result<()> {
    Command::new(herdr_bin_path())
        .arg("server")
        .arg("--session")
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot start the Herdr session server")?;
    Ok(())
}

/// Stops then deletes the named session by shelling out to `bin` (D47):
/// `herdr session stop NAME --json`, then `herdr session delete NAME
/// --json`. Herdr reports a stop against a session that is not running as a
/// failed `session.stop` call carrying the `session_stop_failed` code
/// (checked by [`stop_failed_because_not_running`]); only that documented
/// failure is read as "already stopped" rather than an error, so delete
/// still runs for it. Any other stop failure, spawning either command
/// failing (a missing binary), or the delete exiting non-zero is an error —
/// a stop failure for an undocumented reason never reaches delete. Takes
/// `bin` explicitly, rather than reading `HERDR_BIN_PATH` itself, so tests
/// can point it at a fake script without mutating process-global
/// environment.
fn run_stop_session(bin: &OsStr, name: &str) -> Result<SessionStop> {
    let stop = Command::new(bin)
        .args(["session", "stop", name, "--json"])
        .output()
        .context("cannot run the herdr binary")?;
    let stopped = if stop.status.success() {
        true
    } else if stop_failed_because_not_running(&stop.stdout, &stop.stderr) {
        false
    } else {
        bail!(
            "herdr session stop {name} failed: {}",
            String::from_utf8_lossy(&stop.stderr).trim()
        );
    };

    let delete = Command::new(bin)
        .args(["session", "delete", name, "--json"])
        .output()
        .context("cannot run the herdr binary")?;
    if !delete.status.success() {
        bail!(
            "herdr session delete {name} failed: {}",
            String::from_utf8_lossy(&delete.stderr).trim()
        );
    }

    Ok(SessionStop {
        stopped,
        deleted: true,
    })
}

/// Whether a failed `herdr session stop --json` reports the documented
/// `session_stop_failed` code (D47) — the only stop failure read as
/// "already stopped" rather than propagated as an error. Herdr's exact wire
/// shape for a `--json` CLI failure isn't pinned by the spec, so this scans
/// each stream line by line (D51 point 7) for the first line that parses as
/// a JSON object naming that code either at the top level
/// (`{"code": "session_stop_failed", ...}`) or nested under `error`
/// (Herdr's socket API error shape, `{"error": {"code": ...}}`) — a leading
/// non-JSON line (a deprecation notice, a channel-update nudge) no longer
/// hides the JSON payload that follows it. A line that fails to parse as
/// JSON, or names any other code, does not count.
fn stop_failed_because_not_running(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].into_iter().any(|bytes| {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return false;
        };
        text.lines().any(|line| {
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                return false;
            };
            let code = value
                .get("code")
                .or_else(|| value.get("error").and_then(|error| error.get("code")));
            code.and_then(Value::as_str) == Some("session_stop_failed")
        })
    })
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
    /// Pane ids whose `pane.process_info` call failed during this snapshot
    /// (D51 point 2) — the pane is still listed under `panes`, just with
    /// `process_info: None`. Not part of the wire payload; `status --json`
    /// reports it under `"process_info_unavailable"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process_info_unavailable: Vec<String>,
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

/// D51 point 3: only trusts `candidate` (from `HERDR_PANE_ID`) when it names
/// a pane `panes` actually lists — a stale value (a closed pane whose id was
/// reused, or the var leaking into an unrelated shell) is dropped rather
/// than reported as the caller's pane.
fn live_caller_pane_id(candidate: Option<String>, panes: &[PaneInfo]) -> Option<String> {
    candidate.filter(|id| panes.iter().any(|pane| &pane.pane_id == id))
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
    use std::{fs, thread};

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
    fn create_tab_opens_the_first_pane_splits_the_rest_then_sets_ratios_last() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-createtab.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            // layout.apply, layout.export, pane.split, pane.rename,
            // layout.set_split_ratio.
            for _ in 0..5 {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let result = match request["method"].as_str().expect("method") {
                    "layout.apply" => json!({
                        "layout": {
                            "workspace_id": "w1",
                            "tab_id": "w1:t2",
                            "root": {"type": "pane", "pane_id": "w1:p2"},
                        }
                    }),
                    "layout.export" => json!({
                        "layout": {
                            "workspace_id": "w1",
                            "tab_id": "w1:t2",
                            "root": {"type": "pane", "pane_id": "w1:p2"},
                        }
                    }),
                    "pane.split" => json!({"pane": {"pane_id": "w1:p3"}}),
                    "pane.rename" => json!({"type": "ok"}),
                    "layout.set_split_ratio" => json!({"type": "layout_split_ratio_set"}),
                    other => panic!("unexpected method {other}"),
                };
                let response = json!({"id": request["id"], "result": result});
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
                requests.push(request);
            }
            requests
        });

        let panes = [
            PaneSpec {
                label: Some("editor".into()),
                ..PaneSpec::default()
            },
            PaneSpec {
                label: Some("tests".into()),
                ..PaneSpec::default()
            },
        ];
        let layout = HerdrExt::create_tab(
            &HerdrClient::new(path),
            "w1",
            "main",
            SplitDirection::Right,
            &[0.67],
            &panes,
            None,
        )
        .expect("create tab");

        assert_eq!(layout.tab_id, "w1:t2");
        assert_eq!(layout.pane_ids, ["w1:p2", "w1:p3"]);

        let requests = server.join().expect("server thread");
        let methods: Vec<&str> = requests
            .iter()
            .map(|request| request["method"].as_str().expect("method"))
            .collect();
        // The first pane opens the tab; its label is the leaf label.
        assert_eq!(methods[0], "layout.apply");
        assert_eq!(requests[0]["params"]["root"]["label"], "editor");
        // The regression guard: the ratio is set only after the split that
        // creates the gap exists, never against the one-pane tab.
        let split_at = methods
            .iter()
            .position(|method| *method == "pane.split")
            .expect("a split happened");
        let ratio_at = methods
            .iter()
            .position(|method| *method == "layout.set_split_ratio")
            .expect("a ratio was set");
        assert!(
            ratio_at > split_at,
            "ratios must be applied after the split exists: {methods:?}"
        );
    }

    #[test]
    fn create_workspace_captures_the_root_tab_id_from_root_pane() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join("herdr-create-workspace-root-pane.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {
                    "workspace_id": "w9",
                    "root_pane": {"pane_id": "w9:p1", "tab_id": "w9:t1"},
                }
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let client = HerdrClient::new(path);
        let workspace_id = client
            .create_workspace("dev", Path::new("."))
            .expect("create workspace");
        assert_eq!(workspace_id, "w9");
        assert_eq!(client.take_root_tab("w9"), Some("w9:t1".to_owned()));
        server.join().expect("server thread");
    }

    #[test]
    fn create_workspace_captures_the_root_tab_id_from_tab() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-create-workspace-tab.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {
                    "workspace_id": "w9",
                    "tab": {"tab_id": "w9:t1", "label": "1"},
                }
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let client = HerdrClient::new(path);
        let workspace_id = client
            .create_workspace("dev", Path::new("."))
            .expect("create workspace");
        assert_eq!(
            client.take_root_tab(&workspace_id),
            Some("w9:t1".to_owned())
        );
        server.join().expect("server thread");
    }

    #[test]
    fn take_root_tab_returns_it_only_once() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-take-root-tab-once.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({
                "id": request["id"],
                "result": {
                    "workspace_id": "w9",
                    "root_pane": {"tab_id": "w9:t1"},
                }
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let client = HerdrClient::new(path);
        client
            .create_workspace("dev", Path::new("."))
            .expect("create workspace");
        assert_eq!(client.take_root_tab("w9"), Some("w9:t1".to_owned()));
        assert_eq!(client.take_root_tab("w9"), None);
        server.join().expect("server thread");
    }

    #[test]
    fn create_tab_applies_onto_an_existing_tab_and_renames_it_instead_of_opening_a_new_one() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-createtab-existing.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            // layout.apply, tab.rename.
            for _ in 0..2 {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let result = match request["method"].as_str().expect("method") {
                    "layout.apply" => json!({
                        "layout": {
                            "workspace_id": "w1",
                            "tab_id": "w1:t1",
                            "root": {"type": "pane", "pane_id": "w1:p1"},
                        }
                    }),
                    "tab.rename" => json!({"type": "ok"}),
                    other => panic!("unexpected method {other}"),
                };
                let response = json!({"id": request["id"], "result": result});
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
                requests.push(request);
            }
            requests
        });

        let panes = [PaneSpec {
            label: Some("editor".into()),
            ..PaneSpec::default()
        }];
        let layout = HerdrExt::create_tab(
            &HerdrClient::new(path),
            "w1",
            "main",
            SplitDirection::Right,
            &[],
            &panes,
            Some("w1:t1"),
        )
        .expect("create tab onto existing tab");

        assert_eq!(layout.tab_id, "w1:t1");
        assert_eq!(layout.pane_ids, ["w1:p1"]);

        let requests = server.join().expect("server thread");
        assert_eq!(requests[0]["method"], "layout.apply");
        assert_eq!(requests[0]["params"]["tab_id"], "w1:t1");
        assert!(
            requests[0]["params"].get("workspace_id").is_none(),
            "reusing an existing tab must not also address the workspace: {:?}",
            requests[0]
        );
        assert_eq!(requests[1]["method"], "tab.rename");
        assert_eq!(requests[1]["params"]["tab_id"], "w1:t1");
        assert_eq!(requests[1]["params"]["label"], "main");
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
    fn focus_workspace_sends_the_workspace_id() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-focus.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = json!({"id": request["id"], "result": {"type": "workspace_focused"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
            request
        });

        HerdrExt::focus_workspace(&HerdrClient::new(path), "w1").expect("focus workspace");

        let request = server.join().expect("server thread");
        assert_eq!(request["method"], "workspace.focus");
        assert_eq!(request["params"]["workspace_id"], "w1");
    }

    #[test]
    fn ensure_session_returns_running_on_a_reachable_socket_without_starting_anything() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-ensure.sock");
        let listener = bind(&path).expect("bind fake Herdr");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(
                request["method"], "ping",
                "a reachable socket is detected by ping alone, never by shelling out"
            );
            let response = json!({"id": request["id"], "result": {"type": "pong"}});
            serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let state =
            HerdrExt::ensure_session(&HerdrClient::new(path), "unused").expect("ensure session");
        assert_eq!(state, SessionState::Running);
        server.join().expect("server thread");
    }

    #[cfg(unix)]
    #[test]
    fn stop_session_stops_then_deletes_in_order() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("argv.log");
        let bin = write_fake_herdr(&directory, &log, "exit 0");

        let stop = run_stop_session(bin.as_os_str(), "x").expect("stop session");
        assert_eq!(
            stop,
            SessionStop {
                stopped: true,
                deleted: true
            }
        );

        let calls = fs::read_to_string(&log).expect("argv log");
        let mut lines = calls.lines();
        assert_eq!(lines.next(), Some("session stop x --json"));
        assert_eq!(lines.next(), Some("session delete x --json"));
        assert_eq!(lines.next(), None);
    }

    #[cfg(unix)]
    #[test]
    fn stop_session_still_deletes_a_session_that_was_already_stopped() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("argv.log");
        let bin = write_fake_herdr(
            &directory,
            &log,
            r#"if [ "$2" = "stop" ]; then echo '{"code":"session_stop_failed","message":"not running"}' >&2; exit 1; fi
exit 0"#,
        );

        let stop = run_stop_session(bin.as_os_str(), "x").expect("stop session");
        assert_eq!(
            stop,
            SessionStop {
                stopped: false,
                deleted: true
            }
        );

        let calls = fs::read_to_string(&log).expect("argv log");
        let mut lines = calls.lines();
        assert_eq!(lines.next(), Some("session stop x --json"));
        assert_eq!(lines.next(), Some("session delete x --json"));
        assert_eq!(lines.next(), None);
    }

    #[cfg(unix)]
    #[test]
    fn stop_session_with_an_undocumented_stop_failure_is_an_error_and_never_deletes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("argv.log");
        let bin = write_fake_herdr(
            &directory,
            &log,
            r#"if [ "$2" = "stop" ]; then echo '{"code":"internal_error","message":"disk full"}' >&2; exit 1; fi
exit 0"#,
        );

        let error = run_stop_session(bin.as_os_str(), "x").expect_err("undocumented stop failure");
        assert!(error.to_string().contains("disk full"));

        let calls = fs::read_to_string(&log).expect("argv log");
        let mut lines = calls.lines();
        assert_eq!(lines.next(), Some("session stop x --json"));
        assert_eq!(
            lines.next(),
            None,
            "a stop failure for an undocumented reason must never reach delete"
        );
    }

    #[test]
    fn stop_session_with_a_missing_binary_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let bin = directory.path().join("no-such-herdr-binary");

        let error = run_stop_session(bin.as_os_str(), "x").expect_err("missing binary");
        assert!(error.to_string().contains("cannot run the herdr binary"));
    }

    #[cfg(unix)]
    #[test]
    fn stop_session_with_a_failed_delete_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("argv.log");
        let bin = write_fake_herdr(
            &directory,
            &log,
            r#"if [ "$2" = "delete" ]; then echo "boom" >&2; exit 1; fi
exit 0"#,
        );

        let error = run_stop_session(bin.as_os_str(), "x").expect_err("delete failure");
        assert!(error.to_string().contains("boom"));

        let calls = fs::read_to_string(&log).expect("argv log");
        let mut lines = calls.lines();
        assert_eq!(lines.next(), Some("session stop x --json"));
        assert_eq!(lines.next(), Some("session delete x --json"));
        assert_eq!(lines.next(), None);
    }

    /// Writes an executable shell script at `<directory>/herdr` that appends
    /// its arguments (space-joined) to `log` before running `body`, so a
    /// test can assert both the recorded argv and the simulated exit
    /// behavior of `stop_session`'s two shelled-out calls.
    #[cfg(unix)]
    fn write_fake_herdr(directory: &tempfile::TempDir, log: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = directory.path().join("herdr");
        fs::write(
            &script,
            format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", log.display()),
        )
        .expect("write fake herdr script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("make fake herdr script executable");
        warm_up(&script);
        // The warm-up run above may have appended to `log`; the caller's
        // assertions expect it to start empty.
        let _ = fs::write(log, "");
        script
    }

    /// A parallel `cargo test` run occasionally hits Linux's `ETXTBSY`
    /// ("text file busy", os error 26) execing a script immediately after
    /// writing and chmod'ing it — a known kernel race between another
    /// thread's fork() and this file's write-fd closing. Retrying a
    /// throwaway invocation until it succeeds settles the race before the
    /// real test calls into `run_stop_session`.
    #[cfg(unix)]
    fn warm_up(script: &Path) {
        for _ in 0..50 {
            match Command::new(script).arg("--warmup").output() {
                Ok(_) => return,
                Err(error) if error.raw_os_error() == Some(26) => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("warm up fake herdr script: {error}"),
            }
        }
        panic!("fake herdr script stayed text-busy after 50 retries");
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

    /// One scripted answer for [`serve_scripted`] (D51 point 8): either a
    /// `result` value, or `{"error": {"code", "message"}}` — enough to
    /// exercise a real Herdr API error (a transient `pane.process_info`
    /// failure, a documented `session_stop_failed`) without a live Herdr.
    enum FakeAnswer {
        Result(Value),
        Error {
            code: &'static str,
            message: &'static str,
        },
    }

    /// A fake Herdr socket that answers each accepted connection with the
    /// next entry of `script`, in order, regardless of which method was
    /// requested — enough to drive a fixed request sequence, since every
    /// `HerdrClient` request connects fresh per call (D51 point 8). Mirrors
    /// `tests/cli.rs`'s harness of the same name and shape; the two cannot
    /// share code across the integration-test/unit-test boundary.
    fn serve_scripted(path: PathBuf, script: Vec<FakeAnswer>) -> thread::JoinHandle<()> {
        let listener = bind(&path).expect("bind fake Herdr");
        thread::spawn(move || {
            for answer in script {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let response = match answer {
                    FakeAnswer::Result(result) => json!({"id": request["id"], "result": result}),
                    FakeAnswer::Error { code, message } => {
                        json!({"id": request["id"], "error": {"code": code, "message": message}})
                    }
                };
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
            }
        })
    }

    #[test]
    fn snapshot_reports_a_failed_pane_instead_of_failing_the_whole_snapshot() {
        // D51 point 2: `pane.process_info` can fail for one pane (a race
        // with the pane closing between the snapshot and the follow-up
        // call) without that failure sinking the whole `session.snapshot`.
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("herdr-partial-process-info.sock");
        let snapshot = json!({
            "version": "0.8.2",
            "protocol": 1,
            "workspaces": [],
            "tabs": [],
            "panes": [
                {"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1"},
                {"pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1"},
            ],
            "agents": [],
        });
        let server = serve_scripted(
            path.clone(),
            vec![
                FakeAnswer::Result(json!({"snapshot": snapshot})),
                FakeAnswer::Result(json!({"process_info": {}})),
                FakeAnswer::Error {
                    code: "pane_not_found",
                    message: "pane w1:p2 does not exist",
                },
            ],
        );

        let snapshot = HerdrClient::new(path).snapshot().expect("snapshot");
        server.join().expect("server thread");

        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.panes[0].process_info, None);
        assert_eq!(snapshot.panes[1].process_info, None);
        assert_eq!(snapshot.process_info_unavailable, ["w1:p2"]);
    }

    fn fake_pane(pane_id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.to_owned(),
            tab_id: "w1:t1".to_owned(),
            workspace_id: "w1".to_owned(),
            cwd: None,
            tokens: std::collections::BTreeMap::new(),
            process_info: None,
        }
    }

    #[test]
    fn live_caller_pane_id_drops_a_stale_or_absent_candidate() {
        // D51 point 3: a stale or leaked `HERDR_PANE_ID` (a closed pane
        // whose id was reused, or the var escaping into an unrelated shell)
        // must not be reported as the caller's pane when the live snapshot
        // doesn't actually list it.
        let panes = [fake_pane("w1:p1")];
        assert_eq!(
            live_caller_pane_id(Some("w1:p9".to_owned()), &panes),
            None,
            "a pane id absent from the live snapshot must be dropped"
        );
        assert_eq!(live_caller_pane_id(None, &panes), None);
    }

    #[test]
    fn live_caller_pane_id_keeps_a_candidate_the_snapshot_lists() {
        let panes = [fake_pane("w1:p1"), fake_pane("w1:p2")];
        assert_eq!(
            live_caller_pane_id(Some("w1:p1".to_owned()), &panes),
            Some("w1:p1".to_owned())
        );
    }

    #[test]
    fn stop_failed_because_not_running_scans_past_a_leading_non_json_line() {
        // D51 point 7: Herdr sometimes writes a plain warning line to
        // stdout/stderr ahead of its JSON result; the check must scan every
        // line rather than require the whole trimmed output to parse as one
        // JSON value.
        let stdout =
            b"warning: legacy session format detected\n{\"code\": \"session_stop_failed\"}\n";
        assert!(stop_failed_because_not_running(stdout, b""));
    }

    #[test]
    fn stop_failed_because_not_running_rejects_an_unrelated_error_code() {
        let stdout = b"{\"code\": \"pane_not_found\"}\n";
        assert!(!stop_failed_because_not_running(stdout, b""));
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
