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

// D51 point 4: `lint` fetches the live snapshot and prunes recorded state
// against it exactly as `plan`/`status` do, before deciding whether a
// `was =` reference is still live — a recorded resource whose backend id
// the session no longer has must not be reported as live just because
// local state still remembers it.
#[test]
fn lint_prunes_a_stale_recorded_resource_before_reporting_was_liveness() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "code", was = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());

    let socket = directory.path().join("herdr-lint.sock");
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
            "lint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "workspace `code` declares was = \"dev\" which matches nothing live",
        ));
    server.join().expect("fake Herdr server thread");
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

fn empty_snapshot() -> Value {
    json!({"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []})
}

#[test]
fn no_profile_given_resolves_to_the_declared_default() {
    // D51 point 4: `lint` now reaches the backend, so a reachable fake
    // socket is required for it to report `no lint warnings` rather than
    // `cannot reach herdr` (which would still contain the profile name
    // nowhere, since the two messages are mutually exclusive).
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        "profile(name = \"default\")\nprofile(name = \"other\")",
    )
    .expect("Drovefile");

    let socket = directory.path().join("herdr.sock");
    let server = serve_one_snapshot(socket.clone(), empty_snapshot());

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "lint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `default`"));
    server.join().expect("fake Herdr server thread");
}

#[test]
fn no_profile_given_falls_back_to_the_only_declared_profile() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"solo\")").expect("Drovefile");

    let socket = directory.path().join("herdr.sock");
    let server = serve_one_snapshot(socket.clone(), empty_snapshot());

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "lint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `solo`"));
    server.join().expect("fake Herdr server thread");
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

    // D51 point 4: `lint` now reaches the backend once per invocation; this
    // test runs `lint` twice, so the fake socket must answer two
    // `session.snapshot` requests in a row.
    let socket = directory.path().join("herdr.sock");
    let server = serve_scripted(
        socket.clone(),
        vec![
            FakeAnswer::Result(json!({"snapshot": empty_snapshot()})),
            FakeAnswer::Result(json!({"snapshot": empty_snapshot()})),
        ],
    );

    let mut positional = Command::cargo_bin("drove").expect("binary");
    positional
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
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
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "--profile",
            "other",
            "lint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("profile `other`"));
    server.join().expect("fake Herdr server thread");
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
        // D51 point 6: `herdr.session("default")` now normalises to "no
        // named session", which resolves to the real bare default socket
        // rather than a per-session path that could never coincidentally
        // exist. Point `HERDR_SOCKET_PATH` at a path that is guaranteed
        // unreachable so this test stays hermetic regardless of whether the
        // machine running it happens to have a real default Herdr session.
        .env("HERDR_SOCKET_PATH", directory.path().join("missing.sock"))
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

/// One scripted answer for [`serve_scripted`] (D51 point 8): either a
/// `result` value, or `{"error": {"code", "message"}}` — the shape needed to
/// exercise a real backend error (a transient `pane.process_info` failure, a
/// stale-session error) without a live Herdr.
enum FakeAnswer {
    Result(Value),
    Error {
        code: &'static str,
        message: &'static str,
    },
}

/// A fake Herdr socket that answers each accepted connection with the next
/// entry of `script`, in order, regardless of which method was requested —
/// enough to drive a fixed request sequence (the `HerdrClient` methods this
/// crate calls always connect fresh per request, so one accept == one
/// request). Any entry can be [`FakeAnswer::Error`], so a test can make any
/// step of a `drove` run fail exactly like a real Herdr API error would
/// (D51 point 8).
fn serve_scripted(path: PathBuf, script: Vec<FakeAnswer>) -> thread::JoinHandle<()> {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        for answer in script {
            let stream = listener.accept().expect("accept fake Herdr connection");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read request");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = match answer {
                FakeAnswer::Result(result) => json!({"id": request["id"], "result": result}),
                FakeAnswer::Error { code, message } => {
                    json!({"id": request["id"], "error": {"code": code, "message": message}})
                }
            };
            serde_json::to_writer(stream.get_mut(), &response).expect("write response");
            stream.get_mut().write_all(b"\n").expect("newline");
        }
    })
}

/// D51 point 8: `FakeAnswer::Error` drives a real Herdr API error — not
/// just a dropped connection — through a command. `status` treats a failed
/// `session.snapshot` the same way regardless of why it failed, so a
/// scripted `session_unreachable` error is reported exactly like a missing
/// socket would be.
#[test]
fn status_reports_not_running_for_a_scripted_herdr_error() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let socket = directory.path().join("herdr-error.sock");
    let server = serve_scripted(
        socket.clone(),
        vec![FakeAnswer::Error {
            code: "session_unreachable",
            message: "the session backend is not responding",
        }],
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(3)
        .stdout(predicate::str::contains("not running"));
    server.join().expect("fake Herdr server thread");
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

/// Answers `session.snapshot`, `ping`, then one `workspace.create` per
/// label in `labels`, in order: every label failing `workspace.create`
/// returns a Herdr API error instead of a `workspace_id` (D52: a real
/// backend-call failure, not a task or an in-process fake, exercised through
/// `drove up` end to end). One connection per request, matching every other
/// fake Herdr server here.
fn serve_up_with_failing_workspaces(
    path: PathBuf,
    labels: &'static [&'static str],
    fail: &'static [&'static str],
) -> thread::JoinHandle<()> {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        let mut next_workspace_id = 1;
        let mut label_index = 0;
        loop {
            let stream = listener.accept().expect("accept fake Herdr connection");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read request");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let response = match request["method"].as_str().expect("method") {
                "session.snapshot" => {
                    json!({"id": request["id"], "result": {"snapshot": {
                        "version": "0.8.2", "protocol": 1,
                        "workspaces": [], "tabs": [], "panes": [], "agents": [],
                    }}})
                }
                "ping" => json!({"id": request["id"], "result": {"type": "pong"}}),
                "workspace.create" => {
                    let label = labels[label_index];
                    label_index += 1;
                    if fail.contains(&label) {
                        json!({"id": request["id"], "error": {"code": "boom", "message": "boom"}})
                    } else {
                        let workspace_id = format!("w{next_workspace_id}");
                        next_workspace_id += 1;
                        json!({"id": request["id"], "result": {"workspace_id": workspace_id}})
                    }
                }
                other => panic!("unexpected method {other}"),
            };
            serde_json::to_writer(stream.get_mut(), &response).expect("write response");
            stream.get_mut().write_all(b"\n").expect("newline");
            if label_index == labels.len() {
                break;
            }
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

#[test]
fn up_reports_a_failed_backend_call_and_still_applies_an_independent_workspace() {
    // D52 points 2-3, exercised through the real CLI against a fake Herdr
    // that answers `workspace.create` for `ops` with an API error: `up`
    // must still create the independent `dev` workspace, print one `failed`
    // line for `ops`, and exit 1.
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev"),
        workspace(name = "ops"),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    let socket = directory.path().join("herdr-up-failing.sock");
    let server = serve_up_with_failing_workspaces(socket.clone(), &["dev", "ops"], &["ops"]);

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
        .code(1)
        .stdout(
            predicate::str::contains("partial failure")
                .and(predicate::str::contains(
                    "failed: ops: Herdr API error boom: boom",
                ))
                .and(predicate::str::contains("skipped:").not()),
        );
    server.join().expect("fake Herdr server thread");

    let state_path = state_file_path(&state_home, directory.path());
    let saved: Value = serde_json::from_slice(&fs::read(&state_path).expect("read state after up"))
        .expect("state JSON");
    let resources = saved["profiles"]["default"]["resources"]
        .as_object()
        .expect("resources object");
    assert!(
        resources.contains_key("dev"),
        "the independent, successful workspace must still be recorded: {saved}"
    );
    assert!(
        !resources.contains_key("ops"),
        "the failed workspace must not be recorded: {saved}"
    );
}

/// D51 point 1: when the target is unreachable at `up`'s initial probe, the
/// plan built beforehand comes from local state exactly as recorded, with no
/// chance to prune it against a live snapshot. Herdr wipes every workspace
/// and restarts its id counter on a fresh start (issue 24), so once
/// `ensure_session` reports the session was just `Started`, `up` must
/// re-fetch the now-live snapshot, re-prune, and rebuild the plan before
/// applying anything or saving state — never trust the pre-start plan. This
/// reproduces the bug by delaying the fake socket's bind until after `drove
/// up` has already found it unreachable and shelled out to (a no-op fake)
/// `herdr server`, forcing `ensure_session` through its real start-then-wait
/// path (D51 point 8).
#[cfg(unix)]
#[test]
fn up_against_a_session_that_was_just_started_prunes_before_applying() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_stale_workspace(&state_home, directory.path());
    let state_path = state_file_path(&state_home, directory.path());

    let log = directory.path().join("argv.log");
    let herdr = write_fake_herdr(directory.path(), &log, "exit 0");

    let socket = directory.path().join("herdr-started.sock");
    let socket_for_thread = socket.clone();
    let log_for_thread = log.clone();
    let server = thread::spawn(move || {
        // Only bind the fake socket once `drove` has actually shelled out
        // to `herdr server` (recorded in `log` by the fake script) — proof
        // that its own first `ping` already found nothing there and it took
        // the real start-then-wait path, rather than racing a fixed sleep
        // against however long the child process takes to reach that call.
        loop {
            if fs::read_to_string(&log_for_thread)
                .map(|contents| contents.contains("server --session"))
                .unwrap_or(false)
            {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        serve_scripted(
            socket_for_thread,
            vec![
                FakeAnswer::Result(json!({"type": "pong"})),
                FakeAnswer::Result(json!({
                    "snapshot": {"version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": []},
                })),
                FakeAnswer::Result(json!({"type": "pong"})),
            ],
        )
        .join()
        .expect("fake Herdr server thread");
    });

    let mut command = Command::cargo_bin("drove").expect("binary");
    let output = command
        .env("DROVE_STATE_HOME", &state_home)
        .env("HERDR_BIN_PATH", &herdr)
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
            "--json",
            "up",
            "--no-focus",
            "--yes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    server.join().expect("fake Herdr socket thread");

    let report: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(
        report["session_started"], true,
        "up should report that it started the session: {report}"
    );

    let saved: Value = serde_json::from_slice(&fs::read(&state_path).expect("read state after up"))
        .expect("state JSON");
    assert!(
        saved["profiles"]["default"]["resources"]
            .as_object()
            .expect("resources object")
            .is_empty(),
        "the stale `dev` entry (backend id `w9`, absent from the freshly \
         started session's empty snapshot) must have been pruned before up \
         saved state, not trusted as still there: {saved}"
    );
}

/// Answers every accepted connection according to `request["method"]`,
/// recording each request and returning them all once a request whose
/// method is `stop_after` has been answered. Generic (any request sequence,
/// keyed purely by method) rather than positional, so the exact number and
/// order of backend calls a real `up` apply makes doesn't have to be
/// predicted exactly, only the methods that matter to the test.
#[cfg(unix)]
fn serve_until(
    path: PathBuf,
    stop_after: &'static str,
    respond: impl Fn(&str) -> Value + Send + 'static,
) -> thread::JoinHandle<Vec<Value>> {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let stream = listener.accept().expect("accept fake Herdr connection");
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).expect("read request");
            let request: Value = serde_json::from_str(&line).expect("request JSON");
            let method = request["method"].as_str().expect("method").to_owned();
            let result = respond(&method);
            let response = json!({"id": request["id"], "result": result});
            serde_json::to_writer(stream.get_mut(), &response).expect("write response");
            stream.get_mut().write_all(b"\n").expect("newline");
            let done = method == stop_after;
            requests.push(request);
            if done {
                return requests;
            }
        }
    })
}

/// D51 point 1's full spec test: "up against an unreachable target with
/// non-empty stale state creates every declared resource and never calls
/// focus on a stale id." The declared workspace `dev` must be created fresh
/// (the stale local record names a workspace id the freshly started
/// session's empty snapshot doesn't have) and, with focus enabled, the
/// `workspace.focus` call must carry the newly created id, not the stale
/// one.
#[cfg(unix)]
#[test]
fn up_against_an_unreachable_target_creates_every_resource_and_focuses_the_fresh_id() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "shell"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let state_home = directory.path().join("state");
    // The stale record names workspace id `w9` (see
    // `write_state_with_stale_workspace`) — absent from the freshly started
    // session below, whose only workspace comes back as `w42`.
    write_state_with_stale_workspace(&state_home, directory.path());
    let state_path = state_file_path(&state_home, directory.path());

    let log = directory.path().join("argv.log");
    let herdr = write_fake_herdr(directory.path(), &log, "exit 0");

    let socket = directory.path().join("herdr-focus.sock");
    let socket_for_thread = socket.clone();
    let log_for_thread = log.clone();
    let server = thread::spawn(move || {
        loop {
            if fs::read_to_string(&log_for_thread)
                .map(|contents| contents.contains("server --session"))
                .unwrap_or(false)
            {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        serve_until(socket_for_thread, "workspace.focus", |method| match method {
            "ping" => json!({"type": "pong"}),
            "session.snapshot" => json!({"snapshot": {
                "version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [], "panes": [], "agents": [],
            }}),
            "workspace.create" => json!({
                "workspace_id": "w42",
                "tab": {"tab_id": "w42:t1", "label": "1"},
            }),
            "layout.apply" => json!({
                "layout": {
                    "workspace_id": "w42",
                    "tab_id": "w42:t1",
                    "root": {"type": "pane", "pane_id": "w42:p1"},
                }
            }),
            "tab.rename" => json!({"type": "ok"}),
            "workspace.focus" => json!({"type": "ok"}),
            other => panic!("unexpected method {other}"),
        })
        .join()
        .expect("fake Herdr server thread")
    });

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", &state_home)
        .env("HERDR_BIN_PATH", &herdr)
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
            "up",
            "--yes",
        ])
        .assert()
        .success();
    let requests = server.join().expect("fake Herdr socket thread");

    let creates_workspace = requests
        .iter()
        .any(|request| request["method"] == "workspace.create");
    assert!(
        creates_workspace,
        "every declared resource must be created fresh: {requests:?}"
    );
    let focus = requests
        .iter()
        .find(|request| request["method"] == "workspace.focus")
        .expect("a workspace.focus call");
    assert_eq!(
        focus["params"]["workspace_id"], "w42",
        "focus must target the freshly created workspace id, never the \
         stale `w9` from local state: {requests:?}"
    );

    let saved: Value = serde_json::from_slice(&fs::read(&state_path).expect("read state after up"))
        .expect("state JSON");
    let saved_text = saved.to_string();
    assert!(
        !saved_text.contains("w9"),
        "the stale workspace id must not survive in saved state: {saved}"
    );
    assert!(
        saved_text.contains("w42"),
        "the freshly created workspace id must be recorded: {saved}"
    );
}

/// D51 point 3's full spec test: "caller env id absent from the snapshot
/// leads to a create, not an adopt." `HERDR_PANE_ID` names a pane the live
/// snapshot doesn't list, so the pane declaring `adopt = "caller"` must plan
/// as a normal create (the group's "not observed" reason), never as the
/// adopted-pane path.
#[test]
fn status_with_a_caller_pane_id_absent_from_the_snapshot_plans_a_create_not_an_adopt() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(
        &drovefile,
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "shell", adopt = "caller"),
        ])]),
    ],
)
"#,
    )
    .expect("Drovefile");

    let socket = directory.path().join("herdr-caller.sock");
    let server = serve_one_snapshot(socket.clone(), empty_snapshot());

    let mut command = Command::cargo_bin("drove").expect("binary");
    command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .env("HERDR_PANE_ID", "w1:p9")
        .env_remove("RADIATOR_HUB")
        .env_remove("DROVE_BACKEND")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "status",
        ])
        .assert()
        .code(2)
        .stdout(
            predicate::str::contains(
                "placement group declared but not observed; applying as a new layout",
            )
            .and(predicate::str::contains("adopting the invoking pane").not()),
        );
    server.join().expect("fake Herdr server thread");
}

/// D51 point 2's spec test: "snapshot with one failing `pane.process_info`
/// still returns the other panes", exercised end to end through
/// `status --json`, which must list the pane under
/// `"process_info_unavailable"`.
#[test]
fn status_json_lists_a_pane_whose_process_info_call_failed() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, "profile(name = \"default\")").expect("Drovefile");

    let socket = directory.path().join("herdr-process-info.sock");
    let server = serve_scripted(
        socket.clone(),
        vec![
            FakeAnswer::Result(json!({"snapshot": {
                "version": "0.8.2", "protocol": 1, "workspaces": [], "tabs": [],
                "panes": [{"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1"}],
                "agents": [],
            }})),
            FakeAnswer::Error {
                code: "pane_not_found",
                message: "pane w1:p1 does not exist",
            },
        ],
    );

    let mut command = Command::cargo_bin("drove").expect("binary");
    let output = command
        .env("DROVE_STATE_HOME", directory.path().join("state"))
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "--json",
            "status",
        ])
        .assert()
        .get_output()
        .stdout
        .clone();
    server.join().expect("fake Herdr server thread");

    let report: Value = serde_json::from_slice(&output).expect("valid JSON");
    assert_eq!(
        report["process_info_unavailable"],
        json!(["w1:p1"]),
        "status --json must list the pane whose process_info call failed: {report}"
    );
}

// D53 point 1: a pane's `cwd` can't change in place, so an edit to it plans
// as a destructive close-and-split, and `up` must refuse to apply it without
// `--yes`.

fn drovefile_with_pane_cwd(cwd: &str) -> String {
    format!(
        r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review", cwd = "{cwd}"),
        ])]),
    ],
)
"#
    )
}

/// Runs `drove render --json` against `drovefile` (no backend needed) and
/// returns the `review` pane resource's recorded digest, for a state file
/// that must parse as this feature's composite-digest shape to exercise its
/// category classification rather than the pre-D53 legacy fallback.
fn render_review_pane_digest(drovefile: &Path) -> String {
    let output = Command::cargo_bin("drove")
        .expect("binary")
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--json",
            "render",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ir: Value = serde_json::from_slice(&output).expect("valid IR JSON");
    ir["resources"]
        .as_array()
        .expect("resources array")
        .iter()
        .find(|resource| resource["kind"] == "pane" && resource["name"] == "review")
        .expect("review pane resource")["digest"]
        .as_str()
        .expect("digest string")
        .to_owned()
}

fn write_state_with_review_pane(state_home: &Path, repo_root: &Path, pane_digest: &str) {
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
                        "digest": "stale-digest",
                        "adopted": null,
                        "last_outcome": null,
                    },
                    "dev/main": {
                        "kind": "placement",
                        "backend_id": "w1:t1",
                        "parent": "dev",
                        "digest": "stale-digest",
                        "adopted": null,
                        "last_outcome": null,
                    },
                    "review": {
                        "kind": "pane",
                        "backend_id": "w1:p1",
                        "parent": "dev/main",
                        "digest": pane_digest,
                        "adopted": null,
                        "last_outcome": null,
                    },
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

/// Answers whatever sequence of requests one `drove plan`/`status`/`up` run
/// against this fixed one-workspace, one-tab, one-pane layout makes —
/// `ping`, `session.snapshot`, `pane.process_info`, and, for `up`'s destructive
/// close-and-split, `layout.export` + `pane.split` + `pane.rename`. Spawned
/// detached (not joined): the exact number of requests a run makes is an
/// implementation detail this test does not want to pin down, and the
/// unapproved `ClosePane` half of the pair is never even sent.
fn serve_cwd_edit_layout(path: PathBuf) {
    let listener = bind_fake_herdr(&path).expect("bind fake Herdr socket");
    thread::spawn(move || {
        loop {
            let stream = match listener.accept() {
                Ok(stream) => stream,
                Err(_) => return,
            };
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            if stream.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let result = match request["method"].as_str().unwrap_or_default() {
                "ping" => json!({}),
                "session.snapshot" => json!({"snapshot": {
                    "version": "0.8.2",
                    "protocol": 1,
                    "workspaces": [{"workspace_id": "w1", "label": "dev", "tokens": {}}],
                    "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "label": "main"}],
                    "panes": [{"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1"}],
                    "agents": [],
                }}),
                "pane.process_info" => json!({"process_info": {"foreground_processes": []}}),
                "layout.export" => json!({"layout": {
                    "workspace_id": "w1",
                    "tab_id": "w1:t1",
                    "root": {"type": "pane", "pane_id": "w1:p1"},
                }}),
                "pane.split" => json!({"pane": {"pane_id": "w1:p9"}}),
                "pane.rename" => json!({}),
                _ => json!({}),
            };
            let response = json!({"id": request["id"], "result": result});
            let _ = serde_json::to_writer(stream.get_mut(), &response);
            let _ = stream.get_mut().write_all(b"\n");
        }
    });
}

#[test]
fn e2e_up_after_a_cwd_edit_shows_destructive_and_refuses_without_approval() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    fs::write(&drovefile, drovefile_with_pane_cwd("a")).expect("Drovefile");
    let old_digest = render_review_pane_digest(&drovefile);

    let state_home = directory.path().join("state");
    write_state_with_review_pane(&state_home, directory.path(), &old_digest);

    // The declared cwd changed since the recorded digest: `a` -> `b`.
    fs::write(&drovefile, drovefile_with_pane_cwd("b")).expect("Drovefile edit");

    let plan_socket = directory.path().join("herdr-plan.sock");
    serve_cwd_edit_layout(plan_socket.clone());
    Command::cargo_bin("drove")
        .expect("binary")
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            plan_socket.to_str().expect("UTF-8 socket"),
            "plan",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("[destructive]"))
        .stdout(predicate::str::contains(
            "cwd changed; a pane cannot change directory in place",
        ));

    let up_socket = directory.path().join("herdr-up.sock");
    serve_cwd_edit_layout(up_socket.clone());
    Command::cargo_bin("drove")
        .expect("binary")
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            up_socket.to_str().expect("UTF-8 socket"),
            "up",
            "--no-focus",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "warning: a destructive action needs approval; re-run with --yes to apply it",
        ));
}

// D53 migration: every state file recorded before this feature carries the
// old single-hash digest, which can't be diffed by category. `up` must not
// read that alone as an edit to a converged layout — flagged by the
// controller on review — and must re-stamp the fresh composite digest so
// this one-time amnesty doesn't persist forever.
#[test]
fn up_treats_a_legacy_digest_as_converged_and_restamps_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let drovefile = directory.path().join("Drovefile");
    // Unchanged from what the (legacy) state file already recorded: nothing
    // here actually needs to change.
    fs::write(&drovefile, drovefile_with_pane_cwd("a")).expect("Drovefile");

    let state_home = directory.path().join("state");
    write_state_with_review_pane(&state_home, directory.path(), "stale-digest");

    let socket = directory.path().join("herdr-restamp.sock");
    serve_cwd_edit_layout(socket.clone());
    Command::cargo_bin("drove")
        .expect("binary")
        .env("DROVE_STATE_HOME", &state_home)
        .args([
            "--file",
            drovefile.to_str().expect("UTF-8 path"),
            "--socket",
            socket.to_str().expect("UTF-8 socket"),
            "up",
            "--no-focus",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "already running, brought to front",
        ));

    let state_path = state_file_path(&state_home, directory.path());
    let saved: Value = serde_json::from_slice(&fs::read(&state_path).expect("read state after up"))
        .expect("state JSON");
    for id in ["dev", "dev/main", "review"] {
        let digest = saved["profiles"]["default"]["resources"][id]["digest"]
            .as_str()
            .unwrap_or_else(|| panic!("{id} digest string: {saved}"));
        assert_ne!(
            digest, "stale-digest",
            "the legacy digest for `{id}` should have been re-stamped: {saved}"
        );
        assert!(
            digest.starts_with('{'),
            "the re-stamped digest for `{id}` should be the composite JSON shape: {digest}"
        );
    }
}
