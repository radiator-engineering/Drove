//! Live-Herdr smoke test for PR 3's incremental convergence surface. This
//! test creates and closes real workspaces, so it must never touch an
//! ambient Herdr session someone happens to have open: it only runs when
//! `HERDR_SOCKET_PATH` or `HERDR_SESSION` is set explicitly, and otherwise
//! skips itself so a plain `cargo test` stays both green and side-effect
//! free. `scripts/smoke-herdr.sh` sets `HERDR_SOCKET_PATH` to an ephemeral,
//! isolated `--session` it starts and tears down itself.

use std::{collections::BTreeMap, env, path::Path};

use drove::backend::{Backend, herdr::HerdrClient};
use serde_json::json;

fn client() -> Option<HerdrClient> {
    let has_explicit_target =
        env::var_os("HERDR_SOCKET_PATH").is_some() || env::var_os("HERDR_SESSION").is_some();
    if !has_explicit_target {
        return None;
    }
    let client = HerdrClient::discover(None, None);
    // An explicit target that fails to ping is a real failure, not "no
    // Herdr running" — surface it instead of silently skipping the test.
    client
        .ping()
        .expect("ping the explicitly configured Herdr target");
    Some(client)
}

/// Closes a workspace when dropped, including on panic/unwind, so a smoke
/// test failure partway through never leaves a stray workspace behind in
/// the isolated session.
struct WorkspaceGuard<'a> {
    client: &'a HerdrClient,
    workspace_id: String,
}

impl Drop for WorkspaceGuard<'_> {
    fn drop(&mut self) {
        let _ = self.client.close_workspace(&self.workspace_id);
    }
}

#[test]
#[allow(
    unsafe_code,
    reason = "test-only env manipulation to simulate adoption"
)]
fn drove_converges_a_tab_incrementally_against_a_live_herdr() {
    let Some(client) = client() else {
        eprintln!("skipping: no reachable Herdr socket");
        return;
    };

    let workspace_id = client
        .create_workspace("drove-smoke", Path::new("."))
        .expect("create workspace");
    let _workspace_guard = WorkspaceGuard {
        client: &client,
        workspace_id: workspace_id.clone(),
    };

    let root = json!({
        "type": "pane",
        "label": "first",
    });
    let layout = client
        .apply_layout(&workspace_id, None, "smoke", root)
        .expect("apply layout");
    let tab_id = layout.tab_id.clone();
    let first_pane = layout
        .pane_ids_preorder()
        .into_iter()
        .next()
        .expect("root pane");

    // split, adding a second pane
    let second_pane = client
        .split_pane(
            &first_pane,
            drove::model::SplitDirection::Down,
            0.5,
            Some(&["true".into()]),
            None,
        )
        .expect("split pane");
    assert_ne!(second_pane, first_pane);

    // set_ratio, adjusting the split just created
    client.set_ratio(&tab_id, &[0.3]).expect("set split ratio");

    // rename_pane
    client
        .rename_pane(&second_pane, "renamed-by-smoke")
        .expect("rename pane");

    // token round trip: write, then read back through session.snapshot
    let mut tokens = BTreeMap::new();
    tokens.insert("drove_name".to_owned(), "smoke".to_owned());
    tokens.insert("drove_digest".to_owned(), "deadbeef".to_owned());
    client
        .report_metadata(&second_pane, &tokens)
        .expect("report pane metadata");
    client
        .report_metadata(&workspace_id, &tokens)
        .expect("report workspace metadata");

    let snapshot = client.snapshot().expect("snapshot");
    let pane = snapshot
        .pane(&second_pane)
        .expect("split pane present in snapshot");
    assert_eq!(pane.tokens.get("drove_name"), Some(&"smoke".to_owned()));
    assert_eq!(
        pane.tokens.get("drove_digest"),
        Some(&"deadbeef".to_owned())
    );
    let workspace = snapshot
        .workspace(&workspace_id)
        .expect("workspace present in snapshot");
    assert_eq!(
        workspace.tokens.get("drove_name"),
        Some(&"smoke".to_owned())
    );

    // process_info: the split pane ran `true` and exited, so its process
    // info is best-effort — the call itself must still succeed.
    client
        .pane_process_info(&first_pane)
        .expect("process info call succeeds");

    // adopt: caller_pane_id reads HERDR_PANE_ID, unset outside a managed
    // pane (D24) — simulate being adopted by setting it to a live pane.
    // SAFETY: this test is single-threaded within the process and no other
    // test reads HERDR_PANE_ID.
    unsafe {
        env::set_var("HERDR_PANE_ID", &first_pane);
    }
    assert_eq!(client.caller_pane_id(), Some(first_pane.clone()));
    unsafe {
        env::remove_var("HERDR_PANE_ID");
    }
    assert_eq!(client.caller_pane_id(), None);

    // output(): recent text of the pane that ran `true`.
    let output = client
        .output(&first_pane, std::time::Duration::from_secs(2))
        .expect("pane output");
    let _ = output; // content is unpredictable; the call succeeding is the check.

    client.close_pane(&second_pane).expect("close split pane");
}
