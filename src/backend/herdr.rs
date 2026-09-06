//! Minimal typed client for Herdr's public NDJSON socket API.

use std::{
    env,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result, bail};
use interprocess::local_socket::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Backend, Capabilities, ProcessInfo};
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
        let id = format!("drove:{}", REQUEST_ID.fetch_add(1, Ordering::Relaxed));
        let request = json!({"id": id, "method": method, "params": params});
        let mut stream = BufReader::new(connect(&self.socket_path).with_context(|| {
            format!("cannot connect to Herdr at {}", self.socket_path.display())
        })?);
        serde_json::to_writer(stream.get_mut(), &request).context("cannot encode Herdr request")?;
        stream
            .get_mut()
            .write_all(b"\n")
            .context("cannot send Herdr request")?;
        stream
            .get_mut()
            .flush()
            .context("cannot flush Herdr request")?;

        let mut line = String::new();
        let read = stream
            .read_line(&mut line)
            .context("cannot read Herdr response")?;
        if read == 0 {
            bail!("Herdr closed the socket without a response");
        }
        if !line.ends_with('\n') {
            bail!("Herdr returned a truncated response without an NDJSON newline");
        }
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
        serde_json::from_value(snapshot).context("invalid Herdr session snapshot")
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
}

impl Backend for HerdrClient {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tabs: true,
            splits_and_ratios: true,
            workspace_env: true,
            pane_command_at_create: true,
            agent_start: true,
            agent_prompt: true,
            adopt_caller: true,
            metadata_tokens: true,
            process_info: true,
            events: true,
        }
    }

    fn caller_pane_id(&self) -> Option<String> {
        env::var("HERDR_PANE_ID").ok().filter(|id| !id.is_empty())
    }

    fn snapshot(&self) -> Result<SessionSnapshot> {
        HerdrClient::snapshot(self)
    }

    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String> {
        HerdrClient::create_workspace(self, label, cwd)
    }

    fn create_tab(
        &self,
        workspace_id: &str,
        tab_label: &str,
        root: Value,
    ) -> Result<ExportedLayout> {
        HerdrClient::apply_layout(self, workspace_id, None, tab_label, root)
    }

    fn split_pane(
        &self,
        _pane_id: &str,
        _direction: SplitDirection,
        _ratio: f64,
        _command: Option<&[String]>,
        _cwd: Option<&Path>,
    ) -> Result<String> {
        unimplemented!("// PR 3: incremental pane.split")
    }

    fn close_pane(&self, _pane_id: &str) -> Result<()> {
        unimplemented!("// PR 3: pane.close")
    }

    fn set_ratio(&self, _tab_id: &str, _ratios: &[f64]) -> Result<()> {
        unimplemented!("// PR 3: layout.set_split_ratio")
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        HerdrClient::rename_workspace(self, workspace_id, label)
    }

    fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()> {
        HerdrClient::rename_tab(self, tab_id, label)
    }

    fn rename_pane(&self, _pane_id: &str, _label: &str) -> Result<()> {
        unimplemented!("// PR 3: pane.rename")
    }

    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()> {
        HerdrClient::start_agent(self, pane_id, name, kind, args)
    }

    fn prompt_agent(&self, _pane_id: &str, _prompt: &str) -> Result<()> {
        unimplemented!("// PR 3: agent.prompt")
    }

    fn process_info(&self, _pane_id: &str) -> Result<Option<ProcessInfo>> {
        unimplemented!("// PR 3: process info via pane inspection")
    }

    fn report_tokens(
        &self,
        address: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        HerdrClient::report_metadata(self, address, tokens)
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
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        thread,
    };

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
