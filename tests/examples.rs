use std::path::PathBuf;

use drove::dsl::compile;
use drove::ir::Ir;
use drove::model::Tab;

fn example_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(relative)
}

/// The pane content digest each example must still produce (D30): these are
/// the values the v2 IR recorded, captured before the core/flavor seam split
/// placement out of the pane's content digest. They must not move — a v2 state
/// file has to upgrade to v3 with every pane seen as converged.
const BASIC_V2_PANE_DIGESTS: &[(&str, &str)] = &[
    (
        "editor",
        "999810892ca9ffcf59d6e38630d7291fc4a7968d6e844da5f2f3f2b1a33e423a",
    ),
    (
        "tests",
        "999810892ca9ffcf59d6e38630d7291fc4a7968d6e844da5f2f3f2b1a33e423a",
    ),
];

const LOG_DRIVEN_V2_PANE_DIGESTS: &[(&str, &str)] = &[
    (
        "controller",
        "2f216875f6a64cbc90eb85ae822d1974508a5468eb22bab4d3642a83f8607ea5",
    ),
    (
        "eventlog",
        "9b8ce54e9a6996defe651b29ead1c5308592987dcc16386ce52239be52610b6c",
    ),
    (
        "agentmon",
        "08f3d038c1492c025ce50a19eb1799c2d93f04283c87788163c65fe9aaa5592b",
    ),
    (
        "spiceedit",
        "eee8d28ddc2863087fe0a9a97525dc7eeb84edcfb0c6c36efff510fb0a6b0d61",
    ),
    (
        "commit-reactor",
        "23369cbc760fc95564d41d46658fe8062649db0af972284e06d56624a5866f17",
    ),
    (
        "gitlog",
        "1aab2f0cdbf7f5313d7e71911736e1ceff9a5adc9d491618850ea46f729ab8a2",
    ),
    (
        "doc-sync-reactor",
        "3ad096e0bd86bab962f8e423d2d89485070ca6e0fcfe9d7fa5a87e13ccbf799d",
    ),
];

fn pane_digest(ir: &Ir, name: &str) -> String {
    ir.resources
        .iter()
        .find(|resource| resource.kind == "pane" && resource.name == name)
        .unwrap_or_else(|| panic!("pane resource `{name}`"))
        .digest
        .clone()
}

fn example_ir(relative: &str) -> Ir {
    let compiled = compile(&example_path(relative)).expect("compile example");
    compiled
        .config
        .profile("default")
        .expect("default profile")
        .to_ir()
}

#[test]
fn basic_example_compiles() {
    let compiled = compile(&example_path("basic/Drovefile")).expect("compile basic example");
    let profile = compiled.config.profile("default").expect("default profile");
    assert_eq!(profile.workspaces.len(), 1);
}

#[test]
fn log_driven_example_compiles_with_reactors_and_extends() {
    let compiled =
        compile(&example_path("log-driven/Drovefile")).expect("compile log-driven example");

    let default_profile = compiled.config.profile("default").expect("default profile");
    assert_eq!(default_profile.workspaces.len(), 3);
    assert_eq!(default_profile.tasks.len(), 2);

    let maintenance = default_profile
        .workspaces
        .iter()
        .find(|workspace| workspace.name == "maintenance")
        .expect("maintenance workspace");
    assert_eq!(maintenance.tabs.len(), 3);
    let commit_reactor_pane = maintenance
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .find(|pane| pane.name == "commit-reactor")
        .expect("commit-reactor pane");
    assert!(commit_reactor_pane.on_start.is_some());

    let core_profile = compiled.config.profile("core").expect("core profile");
    assert_eq!(core_profile.workspaces.len(), 2);
    assert!(
        core_profile
            .workspaces
            .iter()
            .all(|workspace| workspace.name != "files"),
        "core profile should drop the `files` workspace via `without`"
    );

    let ir = default_profile.to_ir();
    assert!(
        ir.resources
            .iter()
            .all(|resource| !resource.digest.is_empty())
    );
}

#[test]
fn example_pane_content_digests_equal_the_v2_values() {
    // D30: splitting placement out of the content digest must leave every
    // pane's content digest exactly where v2 had it.
    let basic = example_ir("basic/Drovefile");
    for (name, expected) in BASIC_V2_PANE_DIGESTS {
        assert_eq!(
            &pane_digest(&basic, name),
            expected,
            "basic pane `{name}` content digest drifted from its v2 value"
        );
    }

    let log_driven = example_ir("log-driven/Drovefile");
    for (name, expected) in LOG_DRIVEN_V2_PANE_DIGESTS {
        assert_eq!(
            &pane_digest(&log_driven, name),
            expected,
            "log-driven pane `{name}` content digest drifted from its v2 value"
        );
    }
}

#[test]
fn moving_a_pane_between_tabs_keeps_content_but_changes_topology() {
    // D30: a pane carried from one Herdr group to another keeps its content
    // digest (nothing about the pane itself changed) while both groups' topology
    // digests move (each now holds a different set of panes).
    let compiled = compile(&example_path("basic/Drovefile")).expect("compile basic example");
    let mut profile = compiled
        .config
        .profile("default")
        .expect("default profile")
        .clone();

    let before = profile.to_ir();
    let before_editor = pane_digest(&before, "editor");
    let before_tests = pane_digest(&before, "tests");
    let before_main = before
        .placements
        .iter()
        .find(|group| group.name == "main")
        .expect("main group")
        .topology_digest
        .clone();

    // Add an empty second group and move `tests` into it.
    let workspace = &mut profile.workspaces[0];
    workspace.tabs.push(Tab {
        name: "side".into(),
        label: None,
        split: Default::default(),
        ratios: vec![],
        panes: vec![],
    });
    let moved = workspace.tabs[0].panes.remove(1);
    workspace.tabs[1].panes.push(moved);
    let after = profile.to_ir();

    assert_eq!(
        before_editor,
        pane_digest(&after, "editor"),
        "the pane left in place must keep its content digest"
    );
    assert_eq!(
        before_tests,
        pane_digest(&after, "tests"),
        "a moved pane must keep its content digest"
    );

    let after_main = after
        .placements
        .iter()
        .find(|group| group.name == "main")
        .expect("main group")
        .topology_digest
        .clone();
    assert_ne!(
        before_main, after_main,
        "the source group's topology digest must change when a pane leaves it"
    );
    let after_side = after
        .placements
        .iter()
        .find(|group| group.name == "side")
        .expect("side group");
    assert_eq!(
        after_side.panes,
        ["tests"],
        "the moved pane lands in `side`"
    );
}
