//! CLI integration test: byte/block-limited (`-n`/`-k`) runs must report the
//! ACTUAL measured elapsed in the summary window, not the default `-t` duration
//! (#103). A `-n 1G` transfer finishes far inside the default 10 s window, so
//! `end.sum_*.seconds` must be the measured time (and the derived bitrate with
//! it), while `start.test_start.duration` stays the requested `-t` parameter.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

/// Run the client to completion (with refused-retry) and return its stdout.
fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> String {
    common::run_client_ok(args, timeout, who).stdout
}

fn spawn_server(port_str: &str) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", port_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    )
}

/// `-n` (byte-limited) `-J` summary must use the measured elapsed, not the
/// default 10 s duration window (#103).
#[test]
fn json_byte_limited_summary_uses_measured_elapsed() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    // 1 GB forward transfer: completes in well under a second on loopback, far
    // inside the default 10 s window.
    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-n", "1G", "-J"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -J is not valid JSON ({e}): {out}"));

    let secs = v["end"]["sum_sent"]["seconds"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing end.sum_sent.seconds: {out}"));
    // Pre-#103 this is the default 10.0; the real transfer takes well under 5 s.
    assert!(
        secs > 0.0 && secs < 5.0,
        "summary seconds {secs} should be the measured elapsed, not the default 10 s window"
    );

    // The derived bitrate must be consistent with bytes / measured seconds, not
    // bytes / 10 s.
    let bytes = v["end"]["sum_sent"]["bytes"]
        .as_f64()
        .expect("sum_sent.bytes");
    let bps = v["end"]["sum_sent"]["bits_per_second"]
        .as_f64()
        .expect("sum_sent.bits_per_second");
    let expected_bps = bytes * 8.0 / secs;
    assert!(
        (bps - expected_bps).abs() <= expected_bps * 0.02,
        "bits_per_second {bps} should match bytes*8/seconds {expected_bps}"
    );

    // For a byte-limited (-n) run, iperf3 zeroes start.test_start.duration — the
    // -t window doesn't apply (#114). Distinct from the measured summary window
    // above, which uses the real elapsed (#103).
    let param = v["start"]["test_start"]["duration"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing start.test_start.duration: {out}"));
    assert_eq!(
        param, 0.0,
        "test_start.duration should be 0 for a byte-limited (-n) run (#114), got {param}"
    );
}

/// The server's own `-s -J` summary must likewise use the measured elapsed for a
/// byte-limited test, not the default duration window (#103).
#[test]
fn server_json_byte_limited_summary_uses_measured_elapsed() {
    let port = free_port();
    let ps = port.to_string();
    let bin = env!("CARGO_BIN_EXE_riperf3");

    // One-off server in JSON mode; capture its end-of-test report.
    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-J", "-p", &ps])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    // Forward byte-limited transfer: the server is the receiver.
    let _client = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-n", "1G"],
        Duration::from_secs(20),
        "client",
    );

    let mut out = String::new();
    server
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("server -J is not valid JSON ({e}): {out}"));
    let secs = v["end"]["sum_received"]["seconds"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing end.sum_received.seconds: {out}"));
    assert!(
        secs > 0.0 && secs < 5.0,
        "server summary seconds {secs} should be the measured elapsed, not the default 10 s window"
    );
    let param = v["start"]["test_start"]["duration"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing start.test_start.duration: {out}"));
    assert_eq!(
        param, 0.0,
        "server test_start.duration should be 0 for a byte-limited (-n) run (#114), got {param}"
    );
}

/// Block-limited (`-k`) shares the same summary path; lock it in too (#103).
#[test]
fn json_block_limited_summary_uses_measured_elapsed() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    // 8000 blocks × 128 KB default ≈ 1 GB: finishes well inside 10 s.
    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-k", "8000", "-J"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -J is not valid JSON ({e}): {out}"));
    let secs = v["end"]["sum_sent"]["seconds"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing end.sum_sent.seconds: {out}"));
    assert!(
        secs > 0.0 && secs < 5.0,
        "block-limited summary seconds {secs} should be the measured elapsed"
    );
}

/// Reverse byte-limited (`-n -R`): the client is the receiver, so its summary
/// comes from `sum_received`; it too must use the measured elapsed (#103). The
/// reverse `-n` hang was fixed in #60, so this terminates.
#[test]
fn json_reverse_byte_limited_summary_uses_measured_elapsed() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-n", "500M", "-R", "-J"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -R -J is not valid JSON ({e}): {out}"));
    let secs = v["end"]["sum_received"]["seconds"]
        .as_f64()
        .unwrap_or_else(|| panic!("missing end.sum_received.seconds: {out}"));
    assert!(
        secs > 0.0 && secs < 5.0,
        "reverse byte-limited summary seconds {secs} should be the measured elapsed"
    );
}
