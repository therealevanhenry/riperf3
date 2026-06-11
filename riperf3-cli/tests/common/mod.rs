//! Shared CLI-test helpers (`mod common;` per test binary).

#![allow(dead_code)] // each test binary uses a subset

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

/// Allocate a test server port from a window BELOW every CI platform's
/// ephemeral range (Linux 32768+, macOS/Windows/FreeBSD 49152+). The old
/// `bind(127.0.0.1:0)` approach handed out *ephemeral* ports — the same pool
/// every concurrent test's client draws its connect() SOURCE ports from — so
/// a foreign socket could already own the port when our server tried to bind
/// (reproduced at 3/10 under the parallel harness, PR #176 review). Windows
/// are PID-offset so concurrently-running test *binaries* don't share one,
/// and an atomic counter serializes callers within a binary; a bind-check
/// skips anything still occupied (e.g. lingering from an earlier test).
pub fn free_port() -> u16 {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    static NEXT: AtomicU16 = AtomicU16::new(0);
    let window = 7000 + (std::process::id() % 250) as u16 * 100;
    for _ in 0..100 {
        let port = window + NEXT.fetch_add(1, Ordering::Relaxed) % 100;
        // Availability check on both wildcard families: no server is racing
        // for this port yet (it hasn't been handed out), so briefly binding
        // it here is collision-free, unlike the rejected readiness probes.
        // SEQUENTIALLY — a held `::` listener (v6only=0 on Linux) claims the
        // v4 side too, so a simultaneous v4 check always fails against our
        // own probe. `.is_ok()` drops each listener before the next bind.
        if TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok()
            && TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
        {
            return port;
        }
    }
    panic!("no free port in test window {window}-{}", window + 99);
}

/// One finished client run.
pub struct ClientRun {
    pub stdout: String,
    pub stderr: String,
    pub status: ExitStatus,
    /// Wall time of the FINAL attempt only (earlier refused attempts excluded),
    /// so elapsed-time assertions stay meaningful.
    pub elapsed: Duration,
}

/// Run the riperf3 CLI to completion with a hard timeout, retrying while the
/// run is REFUSED — i.e. exits unsuccessfully with `ConnectionRefused` in
/// stderr — for up to a bounded retry window. This replaces the old fixed 2 s
/// server-bind sleep: on loaded CI runners the (debug, cold) server could
/// take longer to bind, the client died on ECONNREFUSED, and with stderr
/// nulled the only symptom was `not valid JSON (EOF at line 1 column 0)`
/// (the udp_bidir flake). Port-probing alternatives were rejected: a
/// 127.0.0.1 probe never fires on BSD/Windows (specific-over-wildcard binds
/// are legal there), a wildcard probe can steal the port from the server's
/// own bind, and a connect-probe consumes a `-s -1` one-off's single accept.
/// A refused connect never reaches accept(), so retrying the client is safe
/// for one-off servers and race-free by construction.
pub fn run_client(args: &[&str], timeout: Duration, who: &str) -> ClientRun {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let retry_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let started = Instant::now();
        let mut child = Command::new(bin)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{who}: spawn failed: {e}"));

        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait().expect("try_wait") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Post-kill the pipes are EOF — drain what the child got
                    // out before dying, for the panic message.
                    let mut out = String::new();
                    let mut err = String::new();
                    if let Some(mut s) = child.stdout.take() {
                        let _ = s.read_to_string(&mut out);
                    }
                    if let Some(mut s) = child.stderr.take() {
                        let _ = s.read_to_string(&mut err);
                    }
                    panic!("{who}: timed out; stdout so far: {out}; stderr so far: {err}");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        let elapsed = started.elapsed();

        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();

        if !status.success()
            && (stderr.contains("ConnectionRefused") || stderr.contains("Connection refused"))
            && Instant::now() < retry_deadline
        {
            // Server not listening yet — give it a beat and go again.
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        return ClientRun {
            stdout,
            stderr,
            status,
            elapsed,
        };
    }
}

/// `run_client` + assert success: the common case for tests whose client must
/// finish cleanly. Panics with status and stderr so harness failures are
/// diagnosable (not the old stderr-nulled JSON-EOF riddle).
pub fn run_client_ok(args: &[&str], timeout: Duration, who: &str) -> ClientRun {
    let run = run_client(args, timeout, who);
    assert!(
        run.status.success(),
        "{who}: exited unsuccessfully ({status}); stderr: {stderr}",
        status = run.status,
        stderr = run.stderr,
    );
    run
}
