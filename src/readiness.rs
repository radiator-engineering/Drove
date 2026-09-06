//! Readiness probes (spec D12, D23).
//!
//! `output()` needs backend-owned pane text, so it is resolved by the
//! caller (`Backend::output`, declared ready via
//! `Capabilities::readiness_output`) and passed in as a closure. `port()`
//! and `cmd()` are host-side: Drove runs them itself against the local
//! machine, so they work the same for every backend, Radiator included.

use std::{
    net::{TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
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
/// a timeout kills the child rather than leaving it to run unbounded.
pub fn probe_cmd(argv: &[String], timeout: Duration) -> Result<bool> {
    let [program, args @ ..] = argv else {
        bail!("cmd() readiness probe declares an empty argv");
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::{net::TcpListener, thread};

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
        // An IANA-unassigned port (<https://www.iana.org/assignments/service-names-port-numbers>)
        // rather than bind-then-drop, which races another process for the
        // freed port.
        assert!(!probe_port(47, Duration::from_millis(200)));
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
    fn cmd_probe_rejects_an_empty_argv() {
        let error = probe_cmd(&[], Duration::from_secs(1)).expect_err("empty argv");
        assert!(error.to_string().contains("empty argv"));
    }
}
