//! Typed client for the Radiator hub's NDJSON socket protocol, and the
//! `Backend` impl that degrades where the hub falls short of what the model
//! wants (spec §3, D3, D23, D35; `.context/handoffs/recon-radiator-report.md`
//! and `recon-radiator-gaps-report.md`).
//!
//! Radiator's hub is workspace → flat panes with no tab or split layer
//! (`crates/proto/src/types.rs` in `radiator-cli`), no `agent.start`, and no
//! metadata storage on every hub build. This module flattens tabs into one
//! hub pane list per workspace, runs agents as a `serve` command plus a
//! follow-up `pane.send_text`, and keeps Drove's ownership tokens in a local
//! journal keyed by hub pane id whenever the hub can't store them itself —
//! degrading to `Ownership::Unknown` if the journal and the hub ever
//! disagree (spec §9, D16).
//!
//! Hub commit `80c0f1d` (`radiator-cli`) landed `pane.set_metadata`,
//! `PaneInfo.metadata`, `PaneInfo.process`, `pane.tail`, `workspace.rename`
//! and `hub.capabilities`. `hub.capabilities` is queried once per client
//! (`RadiatorClient::hub_capabilities`, cached) and gates whether metadata
//! tokens and process info are trusted from the hub at all: a hub that
//! reports metadata support is authoritative for tokens and the local
//! journal is never consulted; a hub that doesn't is served from the
//! journal alone (D35). An older hub that lacks `hub.capabilities` itself
//! answers `unknown_method`, which is treated the same as a hub that
//! answers with every capability `false`.

use std::{
    collections::BTreeMap,
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    Backend, Capabilities, PaneSpec, ProcessInfo,
    herdr::{AgentInfo, PaneInfo, SessionSnapshot, TabInfo, WorkspaceInfo},
};

static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// The hub name Radiator itself defaults to (`radiator-cli`'s `--hub-name`).
pub const DEFAULT_HUB_NAME: &str = "main";

#[derive(Debug)]
pub struct RadiatorClient {
    socket_path: PathBuf,
    journal_root: PathBuf,
    /// `hub.capabilities`, queried once and cached for the life of this
    /// client (D35) — the hub's answer is static per build, so there is no
    /// reason to ask again on every `snapshot()`/`report_tokens` call.
    capabilities: Mutex<Option<HubCapabilities>>,
}

/// The subset of `hub.capabilities`'s reply this backend gates behavior on.
/// Unrecognized/missing fields default to `false`, so an older hub that
/// predates one of these flags (or the whole method) never fails to parse.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct HubCapabilities {
    #[serde(default)]
    metadata: bool,
    #[serde(default)]
    process: bool,
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
            capabilities: Mutex::new(None),
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
            capabilities: Mutex::new(None),
        }
    }

    /// `hub.capabilities`, queried once and cached (D35). An older hub with
    /// no `hub.capabilities` at all answers `unknown_method`, folded into
    /// the same all-`false` default as a hub that explicitly reports no
    /// optional features (or one whose reply doesn't parse) — either way
    /// that is a stable fact about this hub build, worth caching. A
    /// transport failure is not: it is reported as an all-`false` default
    /// for this call only, without being written to the cache, so a
    /// transient blip doesn't permanently strand this client on the
    /// journal-only path once the hub is reachable again. The lock is held
    /// across the whole check-request-fill sequence so concurrent callers
    /// on a cache miss share one `hub.capabilities` round trip rather than
    /// each firing their own.
    fn hub_capabilities(&self) -> HubCapabilities {
        let mut cache = self.capabilities.lock().expect("capabilities cache lock");
        if let Some(cached) = *cache {
            return cached;
        }
        let queried = match self.request_optional("hub.capabilities", json!({})) {
            Ok(reply) => reply
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            Err(_) => return HubCapabilities::default(),
        };
        *cache = Some(queried);
        queried
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
        self.request_raw_with_timeout(method, params, None)
    }

    /// Like [`Self::request_raw`], but bounds how long the response read may
    /// block. Used by `tail()` so an `output()` readiness probe (D23)
    /// cannot hang forever on a hub response that never arrives.
    fn request_raw_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<std::result::Result<Value, RpcError>> {
        let id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let request = json!({"id": id, "method": method, "params": params});
        let stream = connect(&self.socket_path).with_context(|| {
            format!(
                "cannot connect to Radiator hub at {}",
                self.socket_path.display()
            )
        })?;
        stream
            .set_recv_timeout(timeout)
            .context("cannot set Radiator hub response timeout")?;
        let mut stream = BufReader::new(stream);
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

    /// The hub's raw `hub.snapshot` value, kept as `Value` because
    /// `PaneInfo.metadata`/`PaneInfo.process` (D35) are still absent from an
    /// older hub's payload; callers that want those look them up with
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

    /// Renames a workspace via `workspace.rename` (D35). Fails outright when
    /// the hub refuses — including an older hub that doesn't have the
    /// method at all — rather than warning and leaving the hub's name in
    /// place, since a caller that asked for a rename needs to know it
    /// didn't happen.
    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<()> {
        self.request("workspace.rename", json!({"id": id, "name": name}))?;
        Ok(())
    }

    /// Writes `id`'s ownership tokens through to the hub via
    /// `pane.set_metadata` when it reports metadata support, clearing any
    /// journal entry left over from before the hub could store metadata (an
    /// older hub, or one caught mid rolling-upgrade) — the hub is now
    /// authoritative for `id`, so a stale journal entry must not linger to
    /// be read back as a conflict. On a hub that reports no metadata
    /// support, tokens land in the local journal instead and the hub is
    /// never called (D35).
    pub fn report_tokens(&self, id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        if self.hub_capabilities().metadata {
            self.request("pane.set_metadata", json!({"id": id, "set": tokens}))?;
            self.journal_clear(id)?;
        } else {
            self.journal_merge(id, tokens)?;
        }
        Ok(())
    }

    /// Resolve what this backend believes `id`'s ownership tokens are. The
    /// hub is authoritative whenever it reports metadata support at all —
    /// the journal only fills in for a hub that can't store metadata, and
    /// once a hub gains that support its answer must supersede whatever the
    /// journal was tracking beforehand, not merely agree with it (a stale
    /// journal entry from before the hub could report metadata is not the
    /// same as a live disagreement, spec §9, D35). `Unknown` is reserved for
    /// a metadata-capable hub with nothing recorded for `id`, and for a
    /// non-capable hub whose journal has nothing either.
    pub fn resolve_ownership(&self, id: &str) -> Result<Ownership> {
        if self.hub_capabilities().metadata {
            // Read the hub's answer before clearing the journal: if
            // `hub.snapshot` fails, `?` returns early and the (possibly
            // still-needed) journal entry is left in place rather than
            // deleted ahead of a read that never completed.
            let hub_tokens = self.hub_reported_metadata(id)?;
            self.journal_clear(id)?;
            return Ok(match hub_tokens {
                Some(hub) => Ownership::Known(hub),
                None => Ownership::Unknown,
            });
        }
        let journal_tokens = self.load_journal()?.panes.get(id).cloned();
        Ok(match journal_tokens {
            Some(journal) => Ownership::Known(journal),
            None => Ownership::Unknown,
        })
    }

    fn hub_reported_metadata(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
        let snapshot = self.raw_snapshot()?;
        Ok(find_pane_field(&snapshot, id, "metadata")
            .and_then(|value| serde_json::from_value(value.clone()).ok()))
    }

    /// Process info from `PaneInfo.process` (D35). `None` means the hub
    /// hasn't reported it for this pane (whether because it predates the
    /// field or the pane truly has nothing to report — a chat pane, or a
    /// term pane restored before the hub tracked spawn specs), not an
    /// error. `capabilities().process_info` reflects `hub.capabilities`'s
    /// `process` flag, which callers use to decide whether to trust this at
    /// all.
    pub fn process_info(&self, id: &str) -> Result<Option<ProcessInfo>> {
        let snapshot = self.raw_snapshot()?;
        let Some(process) = find_pane_field(&snapshot, id, "process") else {
            return Ok(None);
        };
        Ok(Some(process_info_from_value(process)))
    }

    /// Recent pane text via the hub's `pane.tail` (landed in the hub
    /// protocol PR, `radiator-hub-protocol`, event seq 127/169), the
    /// host-side input to an `output()` readiness probe (D23). `timeout`
    /// bounds the hub round trip so a withheld response cannot hang the
    /// probe forever.
    pub fn tail(&self, id: &str, timeout: Duration) -> Result<String> {
        let result =
            match self.request_raw_with_timeout("pane.tail", json!({"id": id}), Some(timeout))? {
                Ok(result) => result,
                Err(error) => bail!("Radiator API error {}: {}", error.code, error.message),
            };
        let lines = result
            .get("lines")
            .and_then(Value::as_array)
            .context("pane.tail response omitted lines")?;
        Ok(lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// `address`'s tokens as reported by the hub's `metadata` field on the
    /// already-fetched `raw` snapshot, falling back to the local journal
    /// only when the hub reports no metadata support at all (D35) — mirrors
    /// [`Self::resolve_ownership`]'s hub-wins rule without a second
    /// `hub.snapshot` round trip per resource.
    fn tokens_from_snapshot_or_journal(
        &self,
        address: &str,
        reported: Option<&Value>,
    ) -> BTreeMap<String, String> {
        if self.hub_capabilities().metadata {
            return reported
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();
        }
        self.load_journal()
            .ok()
            .and_then(|journal| journal.panes.get(address).cloned())
            .unwrap_or_default()
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

    /// Drop `id`'s journal entry, if any. Called once the hub itself becomes
    /// authoritative for `id` (a successful `pane.set_metadata` write, or
    /// `resolve_ownership` seeing the hub report anything at all), so a
    /// journal entry written before the hub could store metadata never
    /// outlives its purpose and gets read back as a false conflict.
    fn journal_clear(&self, id: &str) -> Result<()> {
        let mut journal = self.load_journal()?;
        if journal.panes.remove(id).is_none() {
            return Ok(());
        }
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
        let hub = self.hub_capabilities();
        Capabilities {
            // `workspace.open` takes only `name`; no `cwd`/`env` param
            // exists to verify against (recon-radiator-report §6).
            workspace_env: false,
            pane_command_at_create: true,
            // From `hub.capabilities` (D35): tokens live in the local
            // journal only when the hub reports no metadata support.
            metadata_tokens: hub.metadata,
            // From `hub.capabilities` (D35).
            process_info: hub.process,
            events: true,
            // `pane.tail` landed in the hub protocol PR (radiator-hub-
            // protocol, event seq 127/169), so `tail()` calls it directly.
            readiness_output: true,
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
            let workspace_tokens =
                self.tokens_from_snapshot_or_journal(workspace_id, workspace.get("metadata"));
            snapshot.workspaces.push(WorkspaceInfo {
                workspace_id: workspace_id.to_owned(),
                label,
                tokens: workspace_tokens,
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
                let pane_tokens =
                    self.tokens_from_snapshot_or_journal(pane_id, pane.get("metadata"));
                let process_info = pane.get("process").map(process_info_from_value);
                snapshot.panes.push(PaneInfo {
                    pane_id: pane_id.to_owned(),
                    tab_id: tab_id.clone(),
                    workspace_id: workspace_id.to_owned(),
                    cwd: None,
                    tokens: pane_tokens,
                    process_info,
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

    /// Opens one pane in the workspace from a placement-free [`PaneSpec`].
    /// The hub has no tab or split layer, so every Drove pane is a flat hub
    /// pane; the argv's head is the command and its tail the args.
    fn create_pane(&self, workspace_id: &str, spec: &PaneSpec) -> Result<String> {
        let argv = spec.command.clone().unwrap_or_default();
        let command = argv.first().map(String::as_str);
        let args = argv.get(1..).unwrap_or(&[]);
        let title = spec.label.as_deref().unwrap_or("pane");
        self.open_pane(
            workspace_id,
            "term",
            title,
            command,
            args,
            spec.cwd.as_deref(),
            &spec.env,
        )
    }

    fn close_pane(&self, pane_id: &str) -> Result<()> {
        RadiatorClient::close_pane(self, pane_id)
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        RadiatorClient::rename_workspace(self, workspace_id, label)
    }

    fn rename_pane(&self, pane_id: &str, label: &str) -> Result<()> {
        RadiatorClient::rename_pane(self, pane_id, label)
    }

    /// Re-runs the command in an existing pane by typing the argv and
    /// submitting it; the hub has no in-place restart verb.
    fn restart_command(&self, pane_id: &str, argv: &[String]) -> Result<()> {
        if argv.is_empty() {
            return Ok(());
        }
        self.send_text(pane_id, &shell_join(argv))?;
        self.send_keys(pane_id, &["enter"])
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

    fn output(&self, pane_id: &str, timeout: Duration) -> Result<String> {
        RadiatorClient::tail(self, pane_id, timeout)
    }
}

/// Quotes each argument for a POSIX shell so a typed `restart_command` argv
/// survives word splitting when the hub submits it as a line of text.
fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| format!("'{}'", arg.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn flat_tab_id(workspace_id: &str) -> String {
    format!("{workspace_id}:panes")
}

fn process_info_from_value(process: &Value) -> ProcessInfo {
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
    ProcessInfo { command: argv, pid }
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

    /// Like [`fake_hub`], but answers a fixed sequence of requests (one
    /// accept per request, since [`RadiatorClient`] opens a fresh connection
    /// per RPC) — for tests where a method call triggers more than one
    /// request, such as a cached `hub.capabilities` lookup ahead of the
    /// call under test.
    fn fake_hub_sequence(path: PathBuf, handlers: Vec<Box<dyn FnOnce(Value) -> Value + Send>>) {
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            for handler in handlers {
                let stream = listener.accept().expect("accept");
                let mut stream = BufReader::new(stream);
                let mut line = String::new();
                stream.read_line(&mut line).expect("read");
                let request: Value = serde_json::from_str(&line).expect("request JSON");
                let response = handler(request);
                serde_json::to_writer(stream.get_mut(), &response).expect("write JSON");
                stream.get_mut().write_all(b"\n").expect("newline");
            }
        });
    }

    fn capabilities_response(
        metadata: bool,
        process: bool,
    ) -> Box<dyn FnOnce(Value) -> Value + Send> {
        Box::new(move |request| {
            assert_eq!(request["method"], "hub.capabilities");
            json!({
                "id": request["id"],
                "result": {
                    "metadata": metadata,
                    "process": process,
                    "readiness_output": true,
                    "workspace_rename": true,
                }
            })
        })
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
    fn create_pane_opens_one_flat_hub_pane_from_the_spec() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-create-pane.sock");
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            assert_eq!(request["method"], "pane.open");
            assert_eq!(request["params"]["command"], "lazygit");
            assert_eq!(request["params"]["args"], json!(["--all"]));
            let response = json!({
                "id": request["id"],
                "result": {"id": "w0:p1", "kind": "term", "title": "gitlog", "runner": "idle"}
            });
            serde_json::to_writer(stream.get_mut(), &response).expect("write");
            stream.get_mut().write_all(b"\n").expect("newline");
        });

        let client = RadiatorClient::new(path);
        let spec = PaneSpec {
            label: Some("gitlog".into()),
            command: Some(vec!["lazygit".into(), "--all".into()]),
            ..PaneSpec::default()
        };
        let pane_id = Backend::create_pane(&client, "w0", &spec).expect("create pane");
        assert_eq!(pane_id, "w0:p1");
    }

    #[test]
    fn rename_workspace_errors_when_the_hub_refuses() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-rename.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "workspace.rename");
            json!({"id": request["id"], "error": {"code": "unknown_method", "message": "no such method: workspace.rename"}})
        });

        let error = RadiatorClient::new(path)
            .rename_workspace("w0", "renamed")
            .expect_err("hub refusal must surface as an error");
        assert!(error.to_string().contains("unknown_method"));
    }

    #[test]
    fn capabilities_reflect_a_hub_that_supports_metadata_and_process() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-full-capabilities.sock");
        fake_hub(path.clone(), capabilities_response(true, true));

        let capabilities = Backend::capabilities(&RadiatorClient::new(path));
        assert!(capabilities.metadata_tokens);
        assert!(capabilities.process_info);
    }

    #[test]
    fn capabilities_reflect_a_hub_that_supports_neither() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-capabilities.sock");
        fake_hub(path.clone(), capabilities_response(false, false));

        let capabilities = Backend::capabilities(&RadiatorClient::new(path));
        assert!(!capabilities.metadata_tokens);
        assert!(!capabilities.process_info);
    }

    #[test]
    fn capabilities_treat_a_pre_d35_hub_as_supporting_neither() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-pre-d35.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "hub.capabilities");
            json!({"id": request["id"], "error": {"code": "unknown_method", "message": "no such method: hub.capabilities"}})
        });

        let capabilities = Backend::capabilities(&RadiatorClient::new(path));
        assert!(!capabilities.metadata_tokens);
        assert!(!capabilities.process_info);
    }

    #[test]
    fn hub_capabilities_is_queried_once_and_cached_across_calls() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-capabilities-cached.sock");
        // Only one `hub.capabilities` exchange is served; a second call
        // that hit the network again would hang waiting for a connection
        // nothing is listening for.
        fake_hub(path.clone(), capabilities_response(false, false));

        let client = RadiatorClient::new(path);
        let first = Backend::capabilities(&client);
        let second = Backend::capabilities(&client);
        assert_eq!(first, second);
    }

    #[test]
    fn hub_capabilities_does_not_cache_a_transport_failure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join("hub-capabilities-transport-failure.sock");
        let client = RadiatorClient::new(path.clone());

        // Nothing is listening yet, so the request fails to connect at all
        // — not with `unknown_method`. That failure must not be cached as
        // "no optional features", or the client would be stuck on the
        // journal-only path forever even once the hub comes up.
        let before = Backend::capabilities(&client);
        assert!(!before.metadata_tokens);
        assert!(!before.process_info);

        fake_hub(path, capabilities_response(true, true));
        let after = Backend::capabilities(&client);
        assert!(after.metadata_tokens);
        assert!(after.process_info);
    }

    #[test]
    fn report_tokens_falls_back_to_local_journal_when_metadata_unsupported() {
        let state_home = tempfile::tempdir().expect("tempdir");
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-no-metadata.sock");
        // Only `hub.capabilities` is ever called: `report_tokens` and
        // `resolve_ownership` both consult (and cache) the same capability
        // flag first, see it is `false`, and never touch `pane.set_metadata`
        // or `hub.snapshot` at all — the journal alone answers both calls.
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "hub.capabilities");
            json!({
                "id": request["id"],
                "result": {"metadata": false, "process": false}
            })
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

    /// Regression for the reviewer's rolling-upgrade finding on PR 5: tokens
    /// get journaled while the hub lacks `pane.set_metadata`, the hub then
    /// gains it (a hub upgrade) and reports its own (possibly different)
    /// current tokens for the same pane. The hub must win outright — not
    /// read as a conflict against the now-superseded journal entry — and
    /// the stale journal entry must be cleared so it can't resurface later.
    #[test]
    fn resolve_ownership_prefers_hub_over_a_stale_journal_entry_after_rolling_upgrade() {
        let state_home = tempfile::tempdir().expect("tempdir");
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-upgrade.sock");
        let client = RadiatorClient::with_journal_root(path.clone(), state_home.path().to_owned());

        // Written back when this hub (or an earlier build of it) had no
        // `pane.set_metadata`.
        let mut journal_tokens = BTreeMap::new();
        journal_tokens.insert("drove_digest".to_owned(), "old".to_owned());
        client
            .journal_merge("w0:p1", &journal_tokens)
            .expect("seed journal");

        let mut hub_tokens = BTreeMap::new();
        hub_tokens.insert("drove_digest".to_owned(), "new".to_owned());
        let hub_tokens_for_response = hub_tokens.clone();
        fake_hub_sequence(
            path,
            vec![
                capabilities_response(true, false),
                Box::new(move |request| {
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
                                    "metadata": hub_tokens_for_response,
                                }],
                            }],
                            "seq": 1,
                        }
                    })
                }),
            ],
        );

        let ownership = client
            .resolve_ownership("w0:p1")
            .expect("resolve ownership");
        assert_eq!(ownership, Ownership::Known(hub_tokens));
        assert!(
            !client
                .load_journal()
                .expect("load journal")
                .panes
                .contains_key("w0:p1"),
            "stale journal entry must be cleared once the hub reports for this pane"
        );
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
        fake_hub_sequence(
            path.clone(),
            vec![
                // `snapshot()` reads `hub.snapshot` first, then queries
                // (and caches) `hub.capabilities` while resolving the first
                // resource's tokens.
                Box::new(|request| {
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
                }),
                capabilities_response(false, false),
            ],
        );

        let snapshot = RadiatorClient::new(path).snapshot().expect("snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.panes.len(), 2);
        assert!(snapshot.panes.iter().all(|pane| pane.tab_id == "w0:panes"));
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].pane_id, "w0:p2");
    }

    #[test]
    fn radiator_offers_no_herdr_flavor() {
        // Tabs, splits, ratios and agent start are the Herdr flavor (D28).
        // Radiator does not implement it, so the accessor is `None` and the
        // executor answers `Unsupported` for any `Herdr(..)` action rather
        // than the backend faking a degraded no-op.
        let client = RadiatorClient::new(PathBuf::from("/nonexistent.sock"));
        assert!(client.herdr().is_none());
    }

    #[test]
    fn tail_joins_the_returned_lines_with_newlines() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-tail.sock");
        fake_hub(path.clone(), |request| {
            assert_eq!(request["method"], "pane.tail");
            assert_eq!(request["params"]["id"], "w0:p1");
            json!({
                "id": request["id"],
                "result": {"lines": ["scaffold: watching for changes", "ready"], "matched": false}
            })
        });

        let text = RadiatorClient::new(path)
            .tail("w0:p1", Duration::from_secs(1))
            .expect("tail");
        assert_eq!(text, "scaffold: watching for changes\nready");
    }

    #[test]
    fn tail_times_out_on_a_withheld_response() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-tail-timeout.sock");
        let listener = bind(&path).expect("bind fake hub");
        thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read");
            thread::sleep(Duration::from_millis(500));
        });

        let error = RadiatorClient::new(path)
            .tail("w0:p1", Duration::from_millis(100))
            .expect_err("withheld response times out");
        assert!(error.to_string().contains("Radiator hub response"));
    }

    #[test]
    fn snapshot_reads_pane_and_workspace_tokens_from_hub_metadata() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("hub-snapshot-tokens.sock");
        fake_hub_sequence(
            path.clone(),
            vec![
                // `snapshot()` reads `hub.snapshot` first, then queries
                // (and caches) `hub.capabilities` while resolving the first
                // resource's tokens.
                Box::new(|request| {
                    assert_eq!(request["method"], "hub.snapshot");
                    json!({
                        "id": request["id"],
                        "result": {
                            "workspaces": [{
                                "id": "w0",
                                "name": "dev",
                                "runner": "idle",
                                "metadata": {"drove_name": "default"},
                                "panes": [{
                                    "id": "w0:p1",
                                    "kind": "term",
                                    "title": "shell",
                                    "runner": "idle",
                                    "metadata": {"drove_digest": "abc123"},
                                    "process": {"pid": 99, "argv": ["bash"]},
                                }],
                            }],
                            "seq": 1,
                        }
                    })
                }),
                capabilities_response(true, true),
            ],
        );

        let snapshot = RadiatorClient::new(path).snapshot().expect("snapshot");
        assert_eq!(
            snapshot.workspaces[0].tokens.get("drove_name"),
            Some(&"default".to_owned())
        );
        assert_eq!(
            snapshot.panes[0].tokens.get("drove_digest"),
            Some(&"abc123".to_owned())
        );
        let process_info = snapshot.panes[0]
            .process_info
            .as_ref()
            .expect("process info present");
        assert_eq!(process_info.pid, Some(99));
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
