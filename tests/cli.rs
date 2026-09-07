use std::{fs, path::PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

fn example_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(relative)
}

#[test]
fn status_reports_not_running_for_missing_socket() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            directory
                .path()
                .join("missing.sock")
                .to_str()
                .expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("not running"));
}

#[test]
fn status_reaches_radiator_when_selected_by_flag() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--backend",
            "radiator",
            "--socket",
            directory
                .path()
                .join("missing.sock")
                .to_str()
                .expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("cannot reach radiator"));
}

#[test]
fn status_reaches_the_drovefile_declared_backend() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "backend(\"radiator\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            directory
                .path()
                .join("missing.sock")
                .to_str()
                .expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("cannot reach radiator"));
}

#[test]
fn status_rejects_an_unknown_backend_flag() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--backend",
            "nope",
            "status",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown backend"));
}

#[test]
fn lint_warns_about_a_stale_was_and_a_task_without_check() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review", was = "shell"),
        ])]),
    ],
    tasks = [task(name = "build", run = ["true"])],
)
"#,
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "lint"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("was = \"shell\"").and(predicate::str::contains("no `check`")),
        );
}

#[test]
fn ls_lists_every_profile_with_backend_target_and_reachability() {
    let drovefile = example_path("log-driven/Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            "/nonexistent/drove-ls-test.sock",
            "ls",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("default: backend=herdr")
                .and(predicate::str::contains("core: backend=herdr"))
                .and(predicate::str::contains(
                    "monitoring: backend=herdr target=drove-mon reachable=false",
                )),
        );
}

#[test]
fn ls_targets_the_profiles_own_session_over_the_ambient_herdr_session() {
    // D46: `HERDR_SESSION` says where the caller happens to be running, not
    // where the profile wants to go. `monitoring` declares
    // `session = "drove-mon"`, so it must resolve to `drove-mon` even when
    // the ambient host env says `HERDR_SESSION=drove`.
    let drovefile = example_path("log-driven/Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("HERDR_SESSION", "drove")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            "/nonexistent/drove-ls-test.sock",
            "ls",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "monitoring: backend=herdr target=drove-mon reachable=false",
        ));
}

#[test]
fn ls_json_reports_one_row_per_profile() {
    let drovefile = example_path("log-driven/Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    let output = command
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            "/nonexistent/drove-ls-test.sock",
            "ls",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let rows: serde_json::Value = serde_json::from_slice(&output).expect("valid JSON");
    let rows = rows.as_array().expect("array of rows");
    assert_eq!(rows.len(), 3);
    let monitoring = rows
        .iter()
        .find(|row| row["profile"] == "monitoring")
        .expect("monitoring row");
    assert_eq!(monitoring["backend"], "herdr");
    assert_eq!(monitoring["target"], "drove-mon");
    assert_eq!(monitoring["reachable"], false);
}

#[test]
fn no_profile_given_resolves_to_the_declared_default() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"default\")\nprofile(name = \"other\")",
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `default`"));
}

#[test]
fn no_profile_given_falls_back_to_the_only_declared_profile() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"solo\")").expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `solo`"));
}

#[test]
fn no_profile_given_with_several_non_default_profiles_exits_2_with_the_list() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"one\")\nprofile(name = \"two\")",
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "lint"])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("no profile given")
                .and(predicate::str::contains("one"))
                .and(predicate::str::contains("two")),
        );
}

#[test]
fn unknown_profile_exits_2_with_the_list() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "lint",
            "nope",
        ])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains("unknown profile `nope`")
                .and(predicate::str::contains("default")),
        );
}

#[test]
fn positional_and_flag_profile_forms_resolve_the_same_profile() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"default\")\nprofile(name = \"other\")",
    )
    .expect("Drovefile");

    let mut positional = Command::cargo_bin("drove").expect("binary");
    positional
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "lint",
            "other",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `other`"));

    let mut flagged = Command::cargo_bin("drove").expect("binary");
    flagged
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--profile",
            "other",
            "lint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `other`"));
}

#[test]
fn disagreeing_positional_and_flag_profile_fails() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"default\")\nprofile(name = \"other\")",
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--profile",
            "default",
            "lint",
            "other",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must match"));
}

#[test]
fn invalid_drovefile_fails_before_contacting_herdr() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"default\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "plan"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate profile"));
}

/// Writes an executable fake `herdr` at `<directory>/herdr` (Unix only:
/// `HERDR_BIN_PATH` shells out, and there is no portable stand-in for a
/// shell script on Windows) that appends its arguments, space-joined, to
/// `log` before running `body`, so a `down` test can assert both the
/// recorded argv (D47's "stop, then delete, in that order") and the
/// simulated exit behavior of `stop_session`'s two shelled-out calls.
#[cfg(unix)]
fn write_fake_herdr(directory: &std::path::Path, log: &std::path::Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = directory.join("herdr");
    fs::write(
        &script,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", log.display()),
    )
    .expect("write fake herdr script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
        .expect("make fake herdr script executable");
    script
}

#[cfg(unix)]
#[test]
fn down_stops_then_deletes_the_declared_session_after_hooks_and_detach() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "herdr.session(\"x\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");
    let log = directory.path().join("argv.log");
    let herdr = write_fake_herdr(directory.path(), &log, "exit 0");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "down"])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing owned by profile `default`",
        ))
        .stdout(predicate::str::contains("stopped session x"));

    let calls = fs::read_to_string(&log).expect("argv log");
    let mut lines = calls.lines();
    assert_eq!(lines.next(), Some("session stop x --json"));
    assert_eq!(lines.next(), Some("session delete x --json"));
    assert_eq!(lines.next(), None);
}

#[cfg(unix)]
#[test]
fn down_json_reports_the_stopped_session() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "herdr.session(\"x\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");
    let log = directory.path().join("argv.log");
    let herdr = write_fake_herdr(directory.path(), &log, "exit 0");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--json",
            "down",
        ])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"session\":{\"deleted\":true,\"name\":\"x\",\"stopped\":true}",
        ));
}

#[test]
fn down_with_no_declared_session_never_touches_herdr() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");
    let herdr = directory.path().join("no-such-herdr-binary");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "down"])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing owned by profile `default`",
        ))
        .stdout(predicate::str::contains("session").not());
}

#[test]
fn down_never_touches_the_default_session() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "herdr.session(\"default\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");
    let herdr = directory.path().join("no-such-herdr-binary");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "down"])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing owned by profile `default`",
        ))
        .stdout(predicate::str::contains("session").not());
}

#[cfg(unix)]
#[test]
fn down_still_deletes_a_session_that_was_already_stopped() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "herdr.session(\"x\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");
    let log = directory.path().join("argv.log");
    let herdr = write_fake_herdr(
        directory.path(),
        &log,
        "if [ \"$2\" = \"stop\" ]; then echo session_stop_failed >&2; exit 1; fi\nexit 0",
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "down"])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted session x"));

    let calls = fs::read_to_string(&log).expect("argv log");
    let mut lines = calls.lines();
    assert_eq!(lines.next(), Some("session stop x --json"));
    assert_eq!(lines.next(), Some("session delete x --json"));
    assert_eq!(lines.next(), None);
}

#[test]
fn down_with_a_missing_herdr_binary_still_reports_the_detach_then_fails() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "herdr.session(\"x\")\nprofile(name = \"default\")",
    )
    .expect("Drovefile");
    let herdr = directory.path().join("no-such-herdr-binary");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .args(["--file", drovefile.to_str().expect("UTF-8 path"), "down"])
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_BIN_PATH", &herdr)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "nothing owned by profile `default`",
        ))
        .stderr(predicate::str::contains("cannot run the herdr binary"));
}
