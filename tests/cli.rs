use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

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
