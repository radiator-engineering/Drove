use std::process::Command;

use serde_json::Value;

#[test]
fn installed_herdr_schema_contains_drove_methods() {
    let output = match Command::new("herdr")
        .args(["api", "schema", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let schema: Value = serde_json::from_slice(&output.stdout).expect("Herdr schema JSON");
    let text = serde_json::to_string(&schema).expect("schema text");
    for method in [
        "session.snapshot",
        "workspace.create",
        "workspace.rename",
        "tab.rename",
        "layout.export",
        "layout.apply",
        "agent.start",
        "workspace.report_metadata",
    ] {
        assert!(text.contains(method), "Herdr schema omitted {method}");
    }
}

/// D55 point 3: checks the exact fields Drove reads off Herdr's own schema,
/// not just that a method name is mentioned somewhere (harness gap 2 in
/// `audit-herdr.md`) — a Herdr release renaming `root_pane`/`tab`/`cwd`
/// would pass `installed_herdr_schema_contains_drove_methods` above and
/// still silently break `create_workspace`'s duck-typing (`herdr.rs:153`
/// onward) or D55's own label/cwd comparison.
#[test]
fn installed_herdr_schema_carries_the_fields_drove_reads() {
    let output = match Command::new("herdr")
        .args(["api", "schema", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let schema: Value = serde_json::from_slice(&output.stdout).expect("Herdr schema JSON");

    let success = &schema["schemas"]["success_response"]["$defs"];
    let pane_info = &success["PaneInfo"]["properties"];
    assert!(
        pane_info.get("tab_id").is_some(),
        "PaneInfo dropped tab_id: {pane_info}"
    );
    assert!(
        pane_info.get("cwd").is_some(),
        "PaneInfo dropped cwd, which SessionSnapshot.panes[] and D55's pruning read: {pane_info}"
    );

    let response_result = &success["ResponseResult"]["oneOf"];
    let variants = response_result
        .as_array()
        .expect("ResponseResult is a oneOf array");
    let workspace_create_result = variants
        .iter()
        .find(|variant| variant["properties"].get("root_pane").is_some())
        .unwrap_or_else(|| {
            panic!("no ResponseResult variant carries root_pane: {response_result:?}")
        });
    assert!(
        workspace_create_result["properties"]["root_pane"]["\u{24}ref"]
            .as_str()
            .is_some_and(|reference| reference.ends_with("/PaneInfo")),
        "root_pane no longer refers to PaneInfo (so root_pane.tab_id would stop meaning what \
         `create_workspace` assumes): {workspace_create_result}"
    );
    assert!(
        workspace_create_result["properties"].get("tab").is_some(),
        "workspace.create's result dropped `tab`, the fallback `create_workspace` reads \
         tab_id from when root_pane is absent: {workspace_create_result}"
    );

    let error_body = &schema["schemas"]["error_response"]["$defs"]["ErrorBody"]["properties"];
    assert!(
        error_body.get("code").is_some(),
        "ErrorBody dropped `code`, which `stop_failed_because_not_running` reads looking for \
         session_stop_failed: {error_body}"
    );
}

/// D55 point 3: `session_stop_failed` is not an enumerated schema value (the
/// schema types `code` as a free string), so the only way to check Drove's
/// assumption about it is to actually provoke it: stopping a session name
/// that is not running. Confirms `stop_failed_because_not_running`
/// (`src/backend/herdr.rs`) is still reading the code Herdr actually sends.
#[test]
fn installed_herdr_reports_session_stop_failed_for_a_session_that_is_not_running() {
    let name = format!("drove-contract-test-not-running-{}", std::process::id());
    let output = match Command::new("herdr")
        .args(["session", "stop", &name, "--json"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return,
    };
    assert!(
        !output.status.success(),
        "stopping a session that was never started must fail"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let carries_code = [stdout.as_ref(), stderr.as_ref()].iter().any(|stream| {
        stream.lines().any(|line| {
            serde_json::from_str::<Value>(line.trim())
                .ok()
                .and_then(|value| {
                    value
                        .get("code")
                        .or_else(|| value.get("error").and_then(|error| error.get("code")))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some("session_stop_failed")
        })
    });
    assert!(
        carries_code,
        "expected a session_stop_failed code in stdout/stderr: stdout={stdout:?} stderr={stderr:?}"
    );
}

/// D55 point 3: `resolve_socket_path` against the socket column of `herdr
/// session list` for whatever Herdr session this test process is actually
/// ambient in (harness gap 2b — the finding-2 `"default"` socket mismatch
/// would have been caught by exactly this comparison). Skipped, like the
/// tests above, wherever `herdr` is not on `PATH`, and also when
/// `HERDR_SOCKET_PATH` is set ambiently: that variable takes over
/// `resolve_socket_path` entirely (by design), so there is nothing session-
/// derived left to compare against `session list`.
#[test]
fn resolve_socket_path_matches_the_ambient_session_in_herdr_session_list() {
    if std::env::var("HERDR_SOCKET_PATH").is_ok() {
        return;
    }
    let output = match Command::new("herdr")
        .args(["session", "list", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let listing: Value = serde_json::from_slice(&output.stdout).expect("herdr session list JSON");
    let Some(sessions) = listing["sessions"].as_array() else {
        return;
    };

    let ambient_session = std::env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.is_empty());
    let row = match &ambient_session {
        Some(name) if name != "default" => sessions
            .iter()
            .find(|row| row["name"].as_str() == Some(name.as_str())),
        _ => sessions
            .iter()
            .find(|row| row["default"].as_bool() == Some(true)),
    };
    let Some(row) = row else {
        return;
    };
    if row["running"].as_bool() != Some(true) {
        return;
    }
    let live_socket = row["socket_path"]
        .as_str()
        .expect("socket_path is a string");

    // D51 point 6: the CLI normalises a resolved session name of exactly
    // `default` to `None` before this point (`select::resolve`) — mimicked
    // here since this test calls `resolve_socket_path` directly.
    let session_argument = ambient_session.as_deref().filter(|name| *name != "default");
    let resolved = drove::backend::herdr::resolve_socket_path(None, session_argument);
    assert_eq!(
        resolved.to_str().expect("resolved socket path is UTF-8"),
        live_socket,
        "resolve_socket_path disagreed with the live `herdr session list` socket for {ambient_session:?}"
    );
}
