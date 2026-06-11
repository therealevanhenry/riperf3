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

/// Is this run a PRE-DATA reset — the peer (or a loaded kernel) RST the
/// connection during setup, before the client produced any output? The #195
/// macOS shape: under runner load the control handshake dies with
/// `Connection reset by peer (os error 54)` while stdout is still empty; the
/// run completes cleanly when simply tried again. Like the refused-retry,
/// retrying is observably safe because nothing has happened yet.
///
/// The guards, in order:
/// - non-success exit: a clean run is never reclassified;
/// - **empty stdout** as the pre-data proxy: once a run printed anything —
///   interval rows, or the -J document #198 routes errors into — a reset is
///   real and must stay fatal. (Mid-test control-plane resets also never
///   render raw: they map to named errors like "control socket has closed
///   unexpectedly". If connect banners ever become unconditional (#222),
///   this proxy needs refining — `setup_retry.rs` pins the behavior and
///   will go red there.)
/// - the three reset renderings, mirroring `refused`: the Debug form, the
///   POSIX strerror, and Windows' WSAECONNRESET text (which contains no
///   "reset"-cased substring at all).
///
/// One-off-server caveat: if the server's single accept was consumed before
/// the RST, the retried client meets ECONNREFUSED and the refused-retry spins
/// out the shared bounded window — the failure still surfaces, just later and
/// less precisely. Accepted: rare, bounded, and the alternative is the #195
/// empty-output kill that destroys the diagnosis entirely.
pub fn reset_pre_data(status: &ExitStatus, stdout: &str, stderr: &str) -> bool {
    if status.success() {
        return false;
    }
    if stdout.is_empty() {
        return reset_tokens(stderr);
    }
    // -J/--json-stream render setup errors INTO stdout (#198) with stderr
    // empty, so "empty stdout" cannot be the only pre-data proxy (the 2-core
    // rounds proved it: every quiet-host residual failure was a -J doc whose
    // connected list never populated). A rendered error whose run never
    // built streams and never ticked an interval is pre-data by
    // construction.
    reset_tokens(stdout) && json_output_is_setup_only(stdout)
}

/// The three reset renderings, mirroring `refused`: Debug form, POSIX
/// strerror, and Windows' WSAECONNRESET text (no "reset"-cased substring).
fn reset_tokens(s: &str) -> bool {
    s.contains("ConnectionReset")
        || s.contains("Connection reset")
        || s.contains("(os error 10054)")
}

/// Did this JSON-mode run die during SETUP — streams never connected, no
/// interval ever ticked? Both sinks are checked: a monolithic `-J` document
/// (start.connected empty + intervals empty) and a `--json-stream` event
/// sequence (no interval events). Non-JSON stdout returns false: text-mode
/// output means the run was past setup.
fn json_output_is_setup_only(stdout: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        let connected_empty = v["start"]["connected"]
            .as_array()
            .is_none_or(|a| a.is_empty());
        let no_intervals = v["intervals"].as_array().is_none_or(|a| a.is_empty());
        return connected_empty && no_intervals;
    }
    // NDJSON stream: any interval event means data flowed.
    stdout.contains("{\"event\":") && !stdout.contains("{\"event\":\"interval\"")
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
        // refused tokens. The reset classifier scans stderr alone: its
        // empty-stdout guard already excludes every doc-rendered error.
        let combined = format!("{stderr}\n{stdout}");
        if (refused(&status, &combined) || reset_pre_data(&status, &stdout, &stderr))
            && Instant::now() < retry_deadline
        {
            // Server not listening yet (refused) or it RST us mid-setup
            // (#195) — give it a beat and go again.
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
        "{who}: exited unsuccessfully ({status}); stderr: {stderr}; stdout: {stdout}",
        status = run.status,
        stderr = run.stderr,
        // stdout too: #198 routes -J/--json-stream errors into the document
        // on STDOUT with stderr empty — a stderr-only panic hides exactly
        // the error this runner exists to surface (#195 rounds, round-7
        // "empty stderr" mystery).
        stdout = run.stdout,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;

    #[cfg(unix)]
    fn status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        // wait(2) encoding: exit code in the high byte.
        ExitStatus::from_raw(code << 8)
    }
    #[cfg(windows)]
    fn status(code: i32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }

    /// #195: each platform's reset rendering classifies as a pre-data reset
    /// when the run failed with an empty stdout. Drop any one rendering and
    /// the retry silently dies on that platform (the `refused` lesson, #194).
    #[test]
    fn reset_pre_data_matches_each_platform_rendering() {
        let failed = status(1);
        for stderr in [
            // POSIX strerror (Linux 104 / macOS 54 — same text, the #195 hit)
            "riperf3: error - Connection reset by peer (os error 104)",
            "riperf3: error - Connection reset by peer (os error 54)",
            // Windows WSAECONNRESET: no "reset"-cased substring at all
            "riperf3: error - An existing connection was forcibly closed by \
             the remote host. (os error 10054)",
            // io::ErrorKind Debug rendering (unwrap/expect paths)
            "thread 'main' panicked: ConnectionReset",
        ] {
            assert!(
                reset_pre_data(&failed, "", stderr),
                "must classify as pre-data reset: {stderr}"
            );
        }
    }

    /// The -J shapes (#195 quiet-round residue): a setup-only error doc
    /// (connected never populated, zero intervals) classifies; a doc whose
    /// run carried data does not, whatever its error key says.
    #[test]
    fn json_mode_pre_data_reset_classifies() {
        let failed = status(1);
        let setup_doc = r#"{
  "start": {"connected": [], "version": "riperf3 0.7.3"},
  "intervals": [],
  "end": {},
  "error": "Connection reset by peer (os error 104)"
}"#;
        assert!(reset_pre_data(&failed, setup_doc, ""));

        let mid_test_doc = r#"{
  "start": {"connected": [{"socket": 1}]},
  "intervals": [{"sum": {"bytes": 1}}],
  "end": {},
  "error": "Connection reset by peer (os error 104)"
}"#;
        assert!(!reset_pre_data(&failed, mid_test_doc, ""));

        // json-stream: error+end only = setup; any interval event = data ran.
        let stream_setup = "{\"event\":\"error\",\"data\":\"Connection reset by peer (os error 104)\"}\n{\"event\":\"end\",\"data\":{}}";
        assert!(reset_pre_data(&failed, stream_setup, ""));
        let stream_mid = "{\"event\":\"start\",\"data\":{}}\n{\"event\":\"interval\",\"data\":{}}\n{\"event\":\"error\",\"data\":\"Connection reset by peer (os error 104)\"}";
        assert!(!reset_pre_data(&failed, stream_mid, ""));
    }

    /// The guards: output already produced (data phase, or a -J error doc),
    /// a clean exit, or a refused connect — none of these may classify.
    #[test]
    fn reset_guards_hold() {
        let failed = status(1);
        let clean = status(0);
        // Once anything printed, a reset is real and stays fatal.
        assert!(!reset_pre_data(
            &failed,
            "[ ID] Interval           Transfer     Bitrate",
            "riperf3: error - Connection reset by peer (os error 104)",
        ));
        // A clean exit is never reclassified, whatever stderr says.
        assert!(!reset_pre_data(
            &clean,
            "",
            "Connection reset by peer (os error 104)",
        ));
        // Refused is the refused-retry's business, not this classifier's.
        assert!(!reset_pre_data(
            &failed,
            "",
            "riperf3: error - Connection refused (os error 111)",
        ));
    }
}
