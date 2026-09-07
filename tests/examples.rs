use std::collections::BTreeMap;
use std::path::PathBuf;

use drove::dsl::compile;
use drove::ir::Ir;
use drove::model::Tab;

fn example_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(relative)
}

fn fixture_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

/// Every pane's `(name, content digest)` in the `default` profile's IR.
fn pane_content_digests(drovefile: &std::path::Path) -> BTreeMap<String, String> {
    let compiled = compile(drovefile).expect("compile");
    let ir = compiled
        .config
        .profile("default")
        .expect("default profile")
        .to_ir();
    ir.resources
        .iter()
        .filter(|resource| resource.kind == "pane")
        .map(|resource| (resource.name.clone(), resource.digest.clone()))
        .collect()
}

/// The pane content digest each example must still produce (D30): originally
/// the value the v2 IR recorded, captured before the core/flavor seam split
/// placement out of the pane's content digest, so a v2 state file could
/// upgrade to v3 with every pane seen as converged.
///
/// D53 changed `Resource.digest` again, from one hash of a pane's whole
/// content to a composite JSON object of per-category hashes (`cwd`, `env`,
/// `serve`, ...), so the planner can classify which category changed instead
/// of only that something did. Each category hash below is still exactly the
/// pre-D53 `content_digest` of that category (unchanged since D30), so this
/// test still catches drift in what a category's content digest covers; the
/// v2-to-v3 upgrade guarantee no longer applies verbatim, since a v2- or
/// pre-D53-v3-recorded digest string does not parse as this composite shape
/// and replans once, as any digest-format change forces (see D53's
/// `diff_categories` fallback in `src/planner.rs`).
const BASIC_V2_PANE_DIGESTS: &[(&str, &str)] = &[
    (
        "editor",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"728c981e3aad62fd5b893e8a2ab14c0d7dde019d6dcef6d0d71248d5a3225ceb\"}",
    ),
    (
        "tests",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"728c981e3aad62fd5b893e8a2ab14c0d7dde019d6dcef6d0d71248d5a3225ceb\"}",
    ),
];

const LOG_DRIVEN_V2_PANE_DIGESTS: &[(&str, &str)] = &[
    (
        "controller",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"b306b3084d85d40c8bcc555474e499eebe504f74fcbad8dfed59b9400ba374e4\",\"serve\":\"728c981e3aad62fd5b893e8a2ab14c0d7dde019d6dcef6d0d71248d5a3225ceb\"}",
    ),
    (
        "eventlog",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"751666fb7db0bebbc2ec905bf493260c99ab3742562075b1a5507c06e79d1378\"}",
    ),
    (
        "agentmon",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"3c81c9927eae8c04a29cc1ae76f52f508b97ef2369be4e7db9cc6b7dcfa93272\"}",
    ),
    (
        "spiceedit",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"3a5e63d8178dbc33b2d979bee8fa8ddae86993ab81eea796758c91f75ec5256c\"}",
    ),
    (
        "commit-reactor",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"ed157801ab1bd3ac9b981a99e8dcd02ac45c7d21724d95c18b1fdd1c9023cc27\",\"other\":\"631493e03b384122dce9880bbd98ff13009ec3a82930fa9d2141844c5250d9e2\",\"serve\":\"7a305d7cbbe31204a9ad54a1600cdcec53e59ae635cc053e7f0bc7b2596a0205\"}",
    ),
    (
        "gitlog",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"3849dacfee5ac53976eb31d0d65015c1171fae907d9a344b7c97e787129077e5\"}",
    ),
    (
        "doc-sync-reactor",
        "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"7cec09ef17a109f71b9955dce4b8747dfd5fc19ab8f3b26b03a69c1ad43b1258\",\"other\":\"4a0b8a4fa07e674dfff30da3fe3dd2b7a3005133e6336a84ddc4a994f6ed61de\",\"serve\":\"42a5a79093e145dbf08596abc41cf6adc431dea488213af3d3caa57201b5bcda\"}",
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
    assert!(default_profile.tasks.is_empty());

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
        .find(|pane| pane.name == "committer-reactor")
        .expect("committer-reactor pane");
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

    let log_driven = compile(&fixture_path("v3/log-driven/Drovefile"))
        .expect("frozen v3 fixture")
        .config
        .profile("default")
        .expect("default profile")
        .to_ir();
    for (name, expected) in LOG_DRIVEN_V2_PANE_DIGESTS {
        assert_eq!(
            &pane_digest(&log_driven, name),
            expected,
            "log-driven pane `{name}` content digest drifted from its v2 value"
        );
    }
}

#[test]
fn migrated_examples_are_in_v3_form_with_no_warnings() {
    // The shipped examples use only the v3 surface: no deprecation warnings.
    for example in ["basic/Drovefile", "log-driven/Drovefile"] {
        let compiled = compile(&example_path(example)).expect("compile example");
        assert!(
            compiled.warnings.is_empty(),
            "{example} still uses a v2 form: {:?}",
            compiled.warnings
        );
    }
}

#[test]
fn v2_fixtures_still_compile_but_warn() {
    // The frozen v2 copies exercise the shims; each must still compile and, by
    // definition, raise at least one deprecation warning.
    for fixture in ["v2/basic/Drovefile", "v2/log-driven/Drovefile"] {
        let compiled = compile(&fixture_path(fixture)).expect("compile v2 fixture");
        assert!(
            !compiled.warnings.is_empty(),
            "{fixture} is a v2 form but raised no warning"
        );
    }
}

#[test]
fn v2_and_v3_forms_share_every_pane_content_digest() {
    // D30: compare the frozen pre-eventlog v3 layout with its v2 form.
    // Upgrading a Drovefile from its v2 form to the migrated v3 form must
    // leave every pane's content digest identical, so `drove up` after the
    // upgrade proposes no restarts.
    for (v2, v3) in [
        ("v2/basic/Drovefile", "basic/Drovefile"),
        ("v2/log-driven/Drovefile", "v3/log-driven/Drovefile"),
    ] {
        let v2_digests = pane_content_digests(&fixture_path(v2));
        let v3_path = if v3.starts_with("v3/") {
            fixture_path(v3)
        } else {
            example_path(v3)
        };
        let v3_digests = pane_content_digests(&v3_path);
        assert_eq!(
            v2_digests, v3_digests,
            "pane content digests drifted between the v2 form `{v2}` and the v3 form `{v3}`"
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

#[cfg(unix)]
#[test]
fn model_command_commit_scrubs_unrelated_provider_env_but_preserves_cursor_and_git_env() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for script in [
        ".context/bin/model-command.py",
        "examples/log-driven/.context/bin/model-command.py",
    ] {
        let directory = tempfile::tempdir().expect("tempdir");
        let bin = directory.path().join("bin");
        fs::create_dir(&bin).expect("bin dir");
        let capture = directory.path().join("env.txt");
        let stub = bin.join("cursor-agent");
        fs::write(
            &stub,
            r#"#!/bin/sh
{
  printf 'ANTHROPIC_API_KEY=%s\n' "${ANTHROPIC_API_KEY-unset}"
  printf 'CLAUDECODE=%s\n' "${CLAUDECODE-unset}"
  printf 'OPENAI_API_KEY=%s\n' "${OPENAI_API_KEY-unset}"
  printf 'GEMINI_API_KEY=%s\n' "${GEMINI_API_KEY-unset}"
  printf 'GOOGLE_API_KEY=%s\n' "${GOOGLE_API_KEY-unset}"
  printf 'CURSOR_API_KEY=%s\n' "${CURSOR_API_KEY-unset}"
  printf 'SSH_AUTH_SOCK=%s\n' "${SSH_AUTH_SOCK-unset}"
  printf 'GIT_AUTHOR_NAME=%s\n' "${GIT_AUTHOR_NAME-unset}"
  printf 'LOG_DRIVEN_WORKER=%s\n' "${LOG_DRIVEN_WORKER-unset}"
} > "$MODEL_ENV_CAPTURE"
"#,
        )
        .expect("write cursor stub");
        let mut permissions = fs::metadata(&stub).expect("stub metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions).expect("chmod cursor stub");
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let output = Command::new("python3")
            .arg(repo.join(script))
            .arg("commit")
            .current_dir(&repo)
            .env("PATH", path)
            .env("MODEL_ENV_CAPTURE", &capture)
            .env("EVENTLOG_SEQ", "42")
            .env("EVENTLOG_REF", "ref")
            .env("EVENTLOG_PATHS", "src/lib.rs")
            .env("ANTHROPIC_API_KEY", "anthropic")
            .env("CLAUDECODE", "claude-code")
            .env("OPENAI_API_KEY", "openai")
            .env("GEMINI_API_KEY", "gemini")
            .env("GOOGLE_API_KEY", "google")
            .env("CURSOR_API_KEY", "cursor")
            .env("SSH_AUTH_SOCK", "/tmp/signing-agent.sock")
            .env("GIT_AUTHOR_NAME", "Drove Test")
            .output()
            .expect("run model command");
        assert!(
            output.status.success(),
            "{script} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let env = fs::read_to_string(&capture).expect("read env capture");
        assert!(env.contains("ANTHROPIC_API_KEY=unset"), "{script}");
        assert!(env.contains("CLAUDECODE=unset"), "{script}");
        assert!(env.contains("OPENAI_API_KEY=unset"), "{script}");
        assert!(env.contains("GEMINI_API_KEY=unset"), "{script}");
        assert!(env.contains("GOOGLE_API_KEY=unset"), "{script}");
        assert!(env.contains("CURSOR_API_KEY=cursor"), "{script}");
        assert!(
            env.contains("SSH_AUTH_SOCK=/tmp/signing-agent.sock"),
            "{script}"
        );
        assert!(env.contains("GIT_AUTHOR_NAME=Drove Test"), "{script}");
        assert!(
            env.contains("LOG_DRIVEN_WORKER=cursor-committer"),
            "{script}"
        );
    }
}
