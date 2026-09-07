use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    thread,
};

use assert_cmd::Command;
use interprocess::local_socket::{Listener, ListenerOptions, traits::Listener as _};
use predicates::prelude::*;
use serde_json::{Value, json};
use sha2::Digest;

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
    warm_up_fake_herdr(&script);
    // The warm-up run above may have appended to `log`; the caller's
    // assertions expect it to start empty.
    let _ = fs::write(log, "");
    script
}

/// A parallel `cargo test` run occasionally hits Linux's `ETXTBSY` ("text
/// file busy", os error 26) exec'ing a script immediately after writing and
/// chmod'ing it — a known kernel race between another thread's fork() and
/// this file's write-fd closing. Retrying a throwaway invocation until it
/// succeeds settles the race before the `drove` child process under test
/// execs this same script through `HERDR_BIN_PATH`.
fn warm_up_fake_herdr(script: &std::path::Path) {
    use std::{process::Command, thread, time::Duration};

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
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            // No session is declared, so the default socket resolution
            // (D50's live-snapshot probe) would otherwise reach whatever
            // ambient Herdr session the test happens to run inside; pin it
            // to a socket nothing is listening on so the test is hermetic.
            "--socket",
            directory
                .path()
                .join("missing.sock")
                .to_str()
                .expect("UTF-8 socket"),
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
            "nothing owned by profile `default`",
        ))
        // D50: nothing is listening on the pinned socket, so the
        // live-snapshot probe fails and prints its own warning; that
        // warning is not a session stop/delete.
        .stdout(predicate::str::contains(
            "warning: session not reachable; detaching without closing panes",
        ))
        .stdout(predicate::str::contains("stopped session").not())
        .stdout(predicate::str::contains("deleted session").not());
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
        // D50: the same unreachable-snapshot warning as above; `default` is
        // still never stopped or deleted (D47).
        .stdout(predicate::str::contains(
            "warning: session not reachable; detaching without closing panes",
        ))
        .stdout(predicate::str::contains("stopped session").not())
        .stdout(predicate::str::contains("deleted session").not());
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
        "if [ \"$2\" = \"stop\" ]; then echo '{\"code\":\"session_stop_failed\",\"message\":\"not running\"}' >&2; exit 1; fi\nexit 0",
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

// D50: `down` never aborts on a resource the session already lost (issue
// 29). `executor::down` used to call `close_pane` for every recorded pane
// under `--purge`, and the first `pane_not_found` aborted the teardown
// before anything was detached or saved, and before the D47 session stop
// ran. `down_command` now prunes the managed profile against the live
// snapshot the same way `up_command` does after D48, so a pane the session
// no longer has is detached from state without ever reaching the backend.

fn write_state_with_stale_pane(state_home: &Path, repo_root: &Path) {
    let state_path = state_file_path(state_home, repo_root);
    fs::create_dir_all(state_path.parent().expect("state dir")).expect("create state dir");
    let state = json!({
        "schema_version": 1,
        "repo_root": repo_root,
        "profiles": {
            "default": {
                "desired_digest": "",
                "resources": {
                    "dev": {
                        "kind": "workspace",
                        "backend_id": "w1",
                        "parent": null,
                        "digest": "digest",
                        "adopted": null,
                        "last_outcome": null,
                    },
                    "dev/main": {
                        "kind": "placement",
                        "backend_id": "w1:t1",
                        "parent": "dev",
                        "digest": "digest",
                        "adopted": null,
                        "last_outcome": null,
                    },
                    "gitlog": {
                        "kind": "pane",
                        "backend_id": "w1:p8",
                        "parent": "dev/main",
                        "digest": "digest",
                        "adopted": null,
                        "last_outcome": null,
                    }
                },
            }
        },
        "approvals": [],
        "journal": [],
    });
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("encode state"),
    )
    .expect("write state file");
}

#[test]
fn down_purge_prunes_a_pane_the_session_already_lost_instead_of_aborting() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "gitlog"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_pane(&state_home, directory.path());

    // The session still has the workspace and its tab, but not the pane
    // (`w1:p8`) local state recorded — the live failure from issue 29.
    let socket = directory.path().join("herdr.sock");
    let server = serve_one_snapshot(
        socket.clone(),
        json!({
            "version": "0.8.2",
            "protocol": 1,
            "workspaces": [{"workspace_id": "w1", "label": "dev", "tokens": {}}],
            "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "label": "main"}],
            "panes": [],
            "agents": [],
        }),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .env_remove("HERDR_SESSION")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .env_remove("DROVE_SESSION")
        .env_remove("DROVE_TARGET")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "down",
            "--purge",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("pruned gitlog (not in session)"));
    server.join().expect("fake Herdr server thread");

    let state_path = state_file_path(&state_home, directory.path());
    let saved: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("read state")).expect("state JSON");
    assert_eq!(saved["profiles"]["default"]["resources"], json!({}));
}

#[cfg(unix)]
#[test]
fn down_warns_and_still_stops_the_session_when_it_is_unreachable() {
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
            "--socket",
            directory
                .path()
                .join("missing.sock")
                .to_str()
                .expect("UTF-8 socket"),
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
            "warning: session not reachable; detaching without closing panes",
        ))
        .stdout(predicate::str::contains("stopped session x"));

    let calls = fs::read_to_string(&log).expect("argv log");
    let mut lines = calls.lines();
    assert_eq!(lines.next(), Some("session stop x --json"));
    assert_eq!(lines.next(), Some("session delete x --json"));
    assert_eq!(lines.next(), None);
}

// D48: local state is pruned against the live backend snapshot before a
// plan is built, so a resource whose recorded backend id no longer exists
// (the session was stopped and restarted, wiping Herdr's workspaces and id
// counter) is planned as a fresh create instead of trusted as already
// there (issue 24).

#[cfg(unix)]
fn bind_fake_herdr(path: &Path) -> std::io::Result<Listener> {
    use interprocess::local_socket::{GenericFilePath, prelude::*};

    ListenerOptions::new()
        .name(path.to_fs_name::<GenericFilePath>()?)
        .create_sync()
}

#[cfg(windows)]
fn bind_fake_herdr(path: &Path) -> std::io::Result<Listener> {
    use interprocess::local_socket::{GenericNamespaced, prelude::*};

    ListenerOptions::new()
        .name(
            path.to_string_lossy()
                .to_string()
                .to_ns_name::<GenericNamespaced>()?,
        )
        .create_sync()
}

/// Answers exactly one `session.snapshot` request with `snapshot`, then
/// exits — enough for one `drove status`/`drove plan` run, which opens one
/// connection per request and makes exactly one request when the snapshot
/// it gets back has no panes to probe process info for.
fn serve_one_snapshot(path: PathBuf, snapshot: Value) -> thread::JoinHandle<()> {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        let stream = listener.accept().expect("accept fake Herdr connection");
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).expect("read request");
        let request: Value = serde_json::from_str(&line).expect("request JSON");
        assert_eq!(request["method"], "session.snapshot");
        let response = json!({"id": request["id"], "result": {"snapshot": snapshot}});
        serde_json::to_writer(stream.get_mut(), &response).expect("write response");
        stream.get_mut().write_all(b"\n").expect("newline");
    })
}

fn state_file_path(state_home: &Path, repo_root: &Path) -> PathBuf {
    let digest = sha2::Sha256::digest(repo_root.to_string_lossy().as_bytes());
    state_home
        .join("projects")
        .join(format!("{}.json", hex::encode(digest)))
}

/// Answers a `session.snapshot` request with `snapshot`, then a `ping` with
/// `pong` — enough for one `drove up --no-focus` run against a session
/// that is already reachable and already in sync once D48 has pruned it, so
/// `ensure_session` finds it `Running` without shelling out and there is no
/// backend action or task left to apply.
fn serve_snapshot_then_ping(path: PathBuf, snapshot: Value) -> thread::JoinHandle<()> {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        for _ in 0..2 {
            let stream = listener.accept().expect("accept fake Herdr connection");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read request");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let result = match request["method"].as_str().expect("method") {
                "session.snapshot" => json!({"snapshot": snapshot}),
                "ping" => json!({"type": "pong"}),
                other => panic!("unexpected method {other}"),
            };
            let response = json!({"id": request["id"], "result": result});
            serde_json::to_writer(stream.get_mut(), &response).expect("write response");
            stream.get_mut().write_all(b"\n").expect("newline");
        }
    })
}

fn write_state_with_stale_workspace(state_home: &Path, repo_root: &Path) {
    let state_path = state_file_path(state_home, repo_root);
    fs::create_dir_all(state_path.parent().expect("state dir")).expect("create state dir");
    let state = json!({
        "schema_version": 1,
        "repo_root": repo_root,
        "profiles": {
            "default": {
                "desired_digest": "",
                "resources": {
                    "dev": {
                        "kind": "workspace",
                        "backend_id": "w9",
                        "parent": null,
                        "digest": "stale-digest",
                        "adopted": null,
                        "last_outcome": null,
                    }
                },
            }
        },
        "approvals": [],
        "journal": [],
    });
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("encode state"),
    )
    .expect("write state file");
}

/// A state file with one unfinished journal entry: an earlier `run` began
/// (`begin_action`) but never recorded completion, the shape a killed
/// process leaves behind (D52 point 4).
fn write_state_with_interrupted_journal_entry(state_home: &Path, repo_root: &Path, task: &str) {
    let state_path = state_file_path(state_home, repo_root);
    fs::create_dir_all(state_path.parent().expect("state dir")).expect("create state dir");
    let state = json!({
        "schema_version": 1,
        "repo_root": repo_root,
        "profiles": {},
        "approvals": [],
        "journal": [
            {
                "action": format!("task:{task}"),
                "digest": "interrupted-digest",
                "completed": false,
                "success": null,
            }
        ],
    });
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&state).expect("encode state"),
    )
    .expect("write state file");
}

#[test]
fn status_reports_an_interrupted_journal_entry() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_interrupted_journal_entry(&state_home, directory.path(), "scaffold");

    let socket = directory.path().join("herdr-interrupted.sock");
    let server = serve_one_snapshot(
        socket.clone(),
        json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []}),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .stdout(predicate::str::contains(
            "interrupted task:scaffold (interrupted-digest)",
        ));
    server.join().expect("fake Herdr server thread");
}

#[test]
fn run_of_a_task_with_an_interrupted_journal_entry_warns_before_rerunning() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    tasks = [task(name = "scaffold", run = ["true"])],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_interrupted_journal_entry(&state_home, directory.path(), "scaffold");

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "run",
            "scaffold",
            "--yes",
        ])
        .assert()
        .stdout(predicate::str::contains(
            "previous run of scaffold did not finish; rerunning",
        ));
}

#[test]
fn status_reports_recreate_for_a_workspace_the_live_session_no_longer_has() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());

    let socket = directory.path().join("herdr.sock");
    let server = serve_one_snapshot(
        socket.clone(),
        json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []}),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    let output = command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "--json",
            "status",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("\"pruned\""))
        .get_output()
        .stdout
        .clone();
    server.join().expect("fake Herdr server thread");

    let plan: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(plan["status"], "out_of_sync");
    assert_eq!(plan["pruned"], json!(["dev"]));
    let creates_dev = plan["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .any(|action| action["address"] == "dev" && action["kind"]["core"] == "create_workspace");
    assert!(
        creates_dev,
        "expected a CreateWorkspace action for `dev`: {plan}"
    );

    // `status` never writes: the state file still records the stale id.
    let state_path = state_file_path(&state_home, directory.path());
    let saved: Value =
        serde_json::from_slice(&fs::read(&state_path).expect("read state")).expect("state JSON");
    assert_eq!(
        saved["profiles"]["default"]["resources"]["dev"]["backend_id"],
        "w9"
    );
}

#[test]
fn status_text_reports_recreate_for_a_pruned_resource() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());

    let socket = directory.path().join("herdr-text.sock");
    let server = serve_one_snapshot(
        socket.clone(),
        json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []}),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains(
            "recreate dev: backend id no longer exists; recreating",
        ));
    server.join().expect("fake Herdr server thread");
}

#[test]
fn plan_prunes_for_its_own_output_but_never_writes_or_reports_pruned() {
    // Spec section 7 (D48): "plan leaves the state file byte-identical."
    // `plan` sees the same pruned snapshot `status` does (it still plans a
    // fresh create for `dev`), but it neither writes the prune back to
    // local state nor annotates its output with what was pruned — that
    // annotation is `status`-only.
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());
    let state_path = state_file_path(&state_home, directory.path());
    let before = fs::read(&state_path).expect("read state before plan");

    let socket = directory.path().join("herdr-plan.sock");
    let server = serve_one_snapshot(
        socket.clone(),
        json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []}),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    let output = command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "--json",
            "plan",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    server.join().expect("fake Herdr server thread");

    let plan: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(plan["status"], "out_of_sync");
    assert!(
        plan.as_object()
            .expect("plan object")
            .get("pruned")
            .is_none(),
        "`plan --json` must carry no \"pruned\" key at all, not even an empty one: {plan}"
    );
    let creates_dev = plan["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .any(|action| action["address"] == "dev" && action["kind"]["core"] == "create_workspace");
    assert!(
        creates_dev,
        "expected a CreateWorkspace action for `dev`: {plan}"
    );

    let after = fs::read(&state_path).expect("read state after plan");
    assert_eq!(
        before, after,
        "`plan` must leave the state file byte-identical"
    );
}

#[test]
fn up_saves_the_pruned_managed_set_before_applying() {
    // D48: `up` prunes local state against the live snapshot, like
    // `plan`/`status`, but — unlike them — saves the pruned set before
    // applying anything. Here the profile declares no workspaces, so once
    // the stale `dev` entry is pruned away the plan is empty: `up` never
    // touches the backend beyond the snapshot/ping pair `ensure_session`
    // needs, but the prune is still expected to have been saved.
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());
    let state_path = state_file_path(&state_home, directory.path());

    let socket = directory.path().join("herdr-up.sock");
    let server = serve_snapshot_then_ping(
        socket.clone(),
        json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []}),
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "up",
            "--no-focus",
            "--yes",
        ])
        .assert()
        .success();
    server.join().expect("fake Herdr server thread");

    let saved: Value = serde_json::from_slice(&fs::read(&state_path).expect("read state after up"))
        .expect("state JSON");
    assert!(
        saved["profiles"]["default"]["resources"]
            .as_object()
            .expect("resources object")
            .is_empty(),
        "up should have saved the pruned (now empty) resource set: {saved}"
    );
}
