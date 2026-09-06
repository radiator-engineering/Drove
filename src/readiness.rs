//! Readiness probes (spec D12, D23).
//!
//! `output()` needs backend-owned pane text, so it is resolved by the
//! caller (`Backend::output`, declared ready via
//! `Capabilities::readiness_output`) and passed in as a closure. `port()`
//! and `cmd()` are host-side: Drove runs them itself against the local
//! machine, so they work the same for every backend, Radiator included.

use std::{
    net::{TcpStream, ToSocketAddrs},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};

use crate::model::Readiness;

/// Polling interval for `probe_cmd`'s non-blocking wait.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Evaluates one `Readiness` declaration. `pane_output` is called only for
/// `Readiness::Output`; it is a closure so callers without a live pane (or
/// a backend that does not support `output()`) never need to satisfy it.
pub fn check(
    readiness: &Readiness,
    timeout: Duration,
    pane_output: impl FnOnce() -> Result<String>,
) -> Result<bool> {
    match readiness {
        Readiness::Output { value } => Ok(pane_output()?.contains(value.as_str())),
        Readiness::Port { value } => Ok(probe_port(*value, timeout)),
        Readiness::Cmd { value } => probe_cmd(value, timeout),
    }
}

/// True if a TCP connection to `127.0.0.1:port` succeeds within `timeout`.
pub fn probe_port(port: u16, timeout: Duration) -> bool {
    let Ok(mut addresses) = ("127.0.0.1", port).to_socket_addrs() else {
        return false;
    };
    let Some(address) = addresses.next() else {
        return false;
    };
    TcpStream::connect_timeout(&address, timeout).is_ok()
}

/// True if `argv` runs to completion within `timeout` and exits zero.
/// Any other outcome (nonzero exit, spawn failure, timeout) is "not ready";
/// a timeout kills the whole process group rather than leaving descendants
/// (e.g. children spawned by a shell script probe) to run unbounded.
pub fn probe_cmd(argv: &[String], timeout: Duration) -> Result<bool> {
    let [program, args @ ..] = argv else {
        bail!("cmd() readiness probe declares an empty argv");
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Make the child its own process group leader so a timeout can kill
        // its whole group, not just the direct child.
        command.process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return Ok(false);
    };

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Ok(false);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn kill_process_group(child: &mut Child) {
    // The child is its own process group leader (see `process_group(0)`
    // above), so a negative pid signals the whole group. Shelling out to
    // `kill` avoids reaching for libc/unsafe for a single signal.
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{}", child.id()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn kill_process_group(child: &mut Child) {
    // `Child::kill()` only kills the direct child (e.g. `sh.exe`), not
    // descendants spawned by a shell script probe (e.g. `sleep 30 &`).
    // `taskkill /T` kills the whole process tree rooted at the child's pid.
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use std::{fs, net::TcpListener, thread};

    use super::*;

    #[test]
    fn output_probe_matches_a_substring_of_pane_text() {
        let readiness = Readiness::Output {
            value: "watching".into(),
        };
        let ready = check(&readiness, Duration::from_millis(10), || {
            Ok("scaffold: watching for changes".to_owned())
        })
        .expect("check");
        assert!(ready);
    }

    #[test]
    fn output_probe_rejects_a_missing_substring() {
        let readiness = Readiness::Output {
            value: "watching".into(),
        };
        let ready = check(&readiness, Duration::from_millis(10), || {
            Ok("scaffold: starting up".to_owned())
        })
        .expect("check");
        assert!(!ready);
    }

    #[test]
    fn output_probe_propagates_backend_errors() {
        let readiness = Readiness::Output {
            value: "watching".into(),
        };
        let error = check(&readiness, Duration::from_millis(10), || bail!("pane gone"))
            .expect_err("propagates");
        assert!(error.to_string().contains("pane gone"));
    }

    #[test]
    fn port_probe_finds_a_listening_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = thread::spawn(move || {
            let _ = listener.accept();
        });
        assert!(probe_port(port, Duration::from_millis(500)));
        drop(TcpStream::connect(("127.0.0.1", port)));
        server.join().ok();
    }

    #[test]
    fn port_probe_fails_on_a_closed_port() {
        // Port 0 cannot have a listener bound to it, so connecting always
        // fails deterministically (unlike a fixed port, which a local
        // service could in principle be listening on).
        assert!(!probe_port(0, Duration::from_millis(200)));
    }

    #[test]
    fn cmd_probe_reports_success() {
        let ready = probe_cmd(&["true".into()], Duration::from_secs(5)).expect("probe");
        assert!(ready);
    }

    #[test]
    fn cmd_probe_reports_failure() {
        let ready = probe_cmd(&["false".into()], Duration::from_secs(5)).expect("probe");
        assert!(!ready);
    }

    #[test]
    fn cmd_probe_kills_a_command_that_outlives_the_timeout() {
        let ready =
            probe_cmd(&["sleep".into(), "5".into()], Duration::from_millis(100)).expect("probe");
        assert!(!ready);
    }

    #[test]
    fn cmd_probe_reports_not_ready_when_the_program_does_not_exist() {
        let ready = probe_cmd(
            &["drove-readiness-probe-does-not-exist".into()],
            Duration::from_secs(1),
        )
        .expect("spawn failure is not an error");
        assert!(!ready);
    }

    #[test]
    // MSYS sh.exe fork emulation does not preserve the native parent-pid
    // chain that taskkill /T walks, so the kill cannot be observed through
    // this test on Windows; see D39.
    #[cfg_attr(
        windows,
        ignore = "MSYS sh.exe fork emulation does not preserve the native parent-pid chain that taskkill /T walks, so the kill cannot be observed through this test on Windows; see D39"
    )]
    fn cmd_probe_kills_a_descendant_spawned_by_a_shell_probe() {
        // The descendant reports its own pid immediately (well within the
        // probe's timeout) so this test can check liveness directly with
        // `kill -0`, rather than racing a fixed sleep against how long the
        // descendant would otherwise run for.
        let directory = tempfile::tempdir().expect("tempdir");
        let pid_file = directory.path().join("descendant-pid");
        // MSYS `sh` treats `\` as an escape character, so a raw Windows
        // temp path (`C:\Users\...`) fed into the script unquoted mangles
        // the redirect target. Forward slashes and single quotes are both
        // safe for MSYS `sh` on a Windows path (`C:/Users/...`).
        let pid_file_for_script = pid_file.to_str().expect("utf8 path").replace('\\', "/");
        let script = format!("sleep 30 & echo $! > '{pid_file_for_script}'; wait");
        let ready = probe_cmd(
            &["sh".into(), "-c".into(), script],
            Duration::from_millis(300),
        )
        .expect("probe");
        assert!(!ready);

        let pid = fs::read_to_string(&pid_file)
            .expect("descendant pid file")
            .trim()
            .to_owned();

        // The kill signal and reaping happen asynchronously with respect to
        // probe_cmd's return, so poll briefly rather than asserting at a
        // single instant.
        let mut still_alive = true;
        for _ in 0..20 {
            still_alive = Command::new("kill")
                .args(["-0", &pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("check descendant")
                .success();
            if !still_alive {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !still_alive,
            "descendant (pid {pid}) survived the probe's timeout"
        );
    }

    #[test]
    fn cmd_probe_rejects_an_empty_argv() {
        let error = probe_cmd(&[], Duration::from_secs(1)).expect_err("empty argv");
        assert!(error.to_string().contains("empty argv"));
    }
}
