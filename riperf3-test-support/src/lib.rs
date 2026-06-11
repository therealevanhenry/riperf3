//! Dev-only shared test helpers for the riperf3 workspace (#192).
//!
//! Single source for the harness pieces that were copy-pasted (and had begun
//! to drift) across `riperf3-cli/tests/common`, `riperf3/tests/common`, and
//! ad-hoc per-file variants: the #176 port allocator, the #191 per-binary UDP
//! serialization lock, the #176 refused-retry CLI runner, and the child
//! reaper guard. `publish = false`: this crate is a `[dev-dependencies]`
//! implementation detail, not API.
//!
//! Statics here (the port counter, the UDP lock) are still **per test
//! binary**: each binary links its own copy of the rlib, and cargo's harness
//! parallelism is within a binary, so the #191 lock semantics are unchanged.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
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

/// Serialize the handshake-heavy UDP tests within a test binary (#191): on a
/// 2-core CI runner, concurrent UDP-connect handshakes starve each other past
/// their setup timeouts. A per-binary lock suffices (cargo runs test binaries
/// sequentially; only in-binary tests parallelize). `into_inner` tolerates a
/// poisoned lock so one failing UDP test doesn't cascade.
pub fn udp_serial() -> std::sync::MutexGuard<'static, ()> {
    static UDP_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    UDP_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
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

/// Is this run a REFUSED connect (server not listening yet)? Matches the
/// pre-#151 Debug rendering (`ConnectionRefused`), the POSIX strerror text
/// (`Connection refused`), and Windows' WSAECONNREFUSED rendering (no
/// "refused"-with-that-casing substring: "No connection could be made
/// because the target machine actively refused it. (os error 10061)") —
/// drop any one and the refused-retry silently dies on that platform,
/// reopening the #176 bind-race flake class (#194 review r2 found exactly
/// that on windows-latest).
pub fn refused(status: &ExitStatus, output: &str) -> bool {
    !status.success()
        && (output.contains("ConnectionRefused")
            || output.contains("Connection refused")
            || output.contains("(os error 10061)"))
}

/// Run a riperf3 CLI binary to completion with a hard timeout, retrying while
/// the run is REFUSED for up to a bounded retry window. This replaces fixed
/// server-bind sleeps: on loaded CI runners the (debug, cold) server can take
/// longer to bind, the client dies on ECONNREFUSED, and with stderr nulled the
/// only symptom was `not valid JSON (EOF at line 1 column 0)` (the udp_bidir
/// flake, #176). Port-probing alternatives were rejected: a 127.0.0.1 probe
/// never fires on BSD/Windows (specific-over-wildcard binds are legal there),
/// a wildcard probe can steal the port from the server's own bind, and a
/// connect-probe consumes a `-s -1` one-off's single accept. A refused connect
/// never reaches accept(), so retrying the client is safe for one-off servers
/// and race-free by construction.
///
/// `bin` is the caller's `env!("CARGO_BIN_EXE_riperf3")` — the env var only
/// exists when the *caller's* crate builds that binary, so it cannot be read
/// here.
pub fn run_client_with(bin: &str, args: &[&str], timeout: Duration, who: &str) -> ClientRun {
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

        // #198 moved -J/--json-stream error text into STDOUT (the document /
        // the error event) with stderr empty — scan both sinks for the
        // refused tokens.
        let combined = format!("{stderr}\n{stdout}");
        if refused(&status, &combined) && Instant::now() < retry_deadline {
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

/// `run_client_with` + assert success: the common case for tests whose client
/// must finish cleanly. Panics with status and stderr so harness failures are
/// diagnosable (not the old stderr-nulled JSON-EOF riddle).
pub fn run_client_ok_with(bin: &str, args: &[&str], timeout: Duration, who: &str) -> ClientRun {
    let run = run_client_with(bin, args, timeout, who);
    assert!(
        run.status.success(),
        "{who}: exited unsuccessfully ({status}); stderr: {stderr}",
        status = run.status,
        stderr = run.stderr,
    );
    run
}

/// Kills the wrapped child on drop, so a spawned server is reaped even if the
/// test panics before it is waited on.
pub struct ChildGuard(pub Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait for a child to exit, bounded by `timeout`. On timeout, kill it and
/// return `None` — the caller turns that into a hang failure with context,
/// instead of stalling the whole suite.
pub fn wait_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
