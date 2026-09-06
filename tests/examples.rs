use std::path::PathBuf;

use drove::dsl::compile;

fn example_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(relative)
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
