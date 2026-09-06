//! Exercises `RadiatorClient` against a real `radiator hub` daemon.
//!
//! Skipped (not failed) unless `RADIATOR_SMOKE_SOCKET` names a live hub
//! socket — `scripts/smoke-radiator.sh` builds `radiator-cli`, starts a hub
//! on a temp socket, sets the variable, and runs this test. Mirrors
//! `tests/herdr_contract.rs`'s "absent tool, skip silently" shape so CI
//! without `radiator-cli` checked out still passes.

use std::{collections::BTreeMap, env, path::PathBuf};

use drove::backend::{Backend, radiator::RadiatorClient};

#[test]
fn radiator_hub_smoke() {
    let Ok(socket) = env::var("RADIATOR_SMOKE_SOCKET") else {
        return;
    };
    let client = RadiatorClient::new(PathBuf::from(socket));

    client.ping().expect("hub.ping");

    let workspace_id = client
        .open_workspace("drove-smoke")
        .expect("workspace.open");

    let pane_id = client
        .open_pane(
            &workspace_id,
            "term",
            "shell",
            Some("true"),
            &[],
            None,
            &BTreeMap::new(),
        )
        .expect("pane.open");

    let snapshot = client.snapshot().expect("hub.snapshot via Backend");
    assert!(
        snapshot.panes.iter().any(|pane| pane.pane_id == pane_id),
        "opened pane missing from snapshot"
    );
    assert!(
        snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == workspace_id)
    );

    let mut tokens = BTreeMap::new();
    tokens.insert("drove_name".to_owned(), "smoke".to_owned());
    client
        .report_tokens(&pane_id, &tokens)
        .expect("report_tokens degrades cleanly with or without pane.set_metadata");

    // radiator-cli PR 21 (merged to main) added `pane.set_metadata`,
    // `PaneInfo.process`, `workspace.rename`, and `hub.capabilities`.
    // Against a hub that build, `resolve_ownership`/`process_info` take
    // their primary path (the hub itself, gated on `hub.capabilities`), not
    // the local-journal fallback this backend also supports for an older
    // hub (D35).
    let ownership = client
        .resolve_ownership(&pane_id)
        .expect("resolve_ownership");
    assert_eq!(
        ownership,
        drove::backend::radiator::Ownership::Known(tokens),
        "hub.snapshot should report back the metadata pane.set_metadata just wrote"
    );

    let process = client
        .process_info(&pane_id)
        .expect("process_info")
        .expect("hub reports PaneInfo.process for a spawned term pane");
    assert_eq!(process.command, vec!["true".to_owned()]);

    client
        .rename_workspace(&workspace_id, "drove-smoke-renamed")
        .expect("workspace.rename should succeed against a hub that supports it");

    client.close_pane(&pane_id).expect("pane.close");
    client
        .close_workspace(&workspace_id)
        .expect("workspace.close");
}
