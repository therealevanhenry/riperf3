//! CLI integration test for the single-socket UDP server demux (#80).
//!
//! Regression: on native winsock, the server's per-stream connected-socket
//! design (`-P` streams sharing one port via `SO_REUSEADDR`, recycled after each
//! connect) hangs `-P > 1` setup — winsock silently drops a new source's
//! datagram once a connected and a wildcard UDP socket share a port, so streams
//! 2..N never complete their connect handshake and the client retries to its
//! 30 s timeout. The fix binds ONE unconnected server socket and demultiplexes
//! streams by client source address in userspace.
//!
//! The demux path is the default on Windows; the in-process
//! `udp_bidir_parallel_completes` integration test is its red→green there. This
//! test exercises the *same* platform-independent demux code on Unix by forcing
//! it via `RIPERF3_UDP_SERVER_DEMUX=1` on the server child — so the fix is
//! validated on a host the CI Linux runner can actually run. It spawns the real
//! server + client binaries and, for forward / reverse / bidir at `-P 4`,
//! asserts (a) the client completes instead of hanging (the #80 symptom) and
//! (b) every expected stream carries bytes, which a misrouting demux would not
//! produce.
//!
//! Unix-gated: the env override only changes behavior off Windows (on Windows
//! demux is already the default), and the server child plumbing here is Unix.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

/// Kills the wrapped child on drop, so a spawned server is reaped even if the
/// test panics before it is waited on.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait for a child to exit, bounded by `timeout`. On timeout, kill it and
/// return `None` — the caller turns that into the "#80 hang" failure with
/// context, instead of stalling the whole suite.
fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
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

/// Run one UDP mode against a demux-forced one-off server and return the client's
/// parsed `-J` report. `extra` carries the direction flag (`-R`, `--bidir`, or
/// nothing for forward).
fn run_demux_udp(extra: &[&str], who: &str) -> Value {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port();
    let port_s = port.to_string();

    // One-off server with the demux path forced on (the env var only affects
    // this child). `-1` makes it exit after serving the single test.
    let server = Command::new(bin)
        .args(["-s", "-1", "-p", &port_s])
        .env("RIPERF3_UDP_SERVER_DEMUX", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{who}: spawn server failed: {e}"));
    let mut server = ChildGuard(server);

    // Let the listener bind before connecting.
    std::thread::sleep(Duration::from_millis(300));

    // Short duration-limited UDP run at -P 4: the #80 hang is in multi-stream
    // setup, so completing at all is the core assertion. `-J` lets us also check
    // routing produced bytes on every stream.
    let mut client = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", &port_s, "-u", "-t", "2", "-P", "4"])
        .args(extra)
        .arg("-J")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{who}: spawn client failed: {e}"));

    // Capture stdout concurrently, then bound the wait: a #80 regression hangs
    // the client at multi-stream setup until its 30 s connect timeout.
    let stdout = client.stdout.take().expect("client stdout");
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = std::io::BufReader::new(stdout).read_to_string(&mut s);
        s
    });

    let exit = wait_bounded(&mut client, Duration::from_secs(20))
        .unwrap_or_else(|| panic!("{who}: client hung — UDP demux -P 4 setup wedged (#80)"));
    let out = reader.join().expect("join stdout reader");
    assert!(
        exit.success(),
        "{who}: client exited non-zero: {exit:?}\n{out}"
    );

    // The one-off server should now have served and exited on its own.
    let _ = wait_bounded(&mut server.0, Duration::from_secs(5));

    serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{who}: client -J is not JSON ({e}): {out}"))
}

/// Assert the report has exactly `expected_streams` streams and every one of
/// them moved a nonzero number of UDP bytes — i.e. the demux routed each client
/// to its own stream rather than dropping or collapsing them.
fn assert_all_streams_have_bytes(report: &Value, expected_streams: usize, who: &str) {
    let streams = report["end"]["streams"]
        .as_array()
        .unwrap_or_else(|| panic!("{who}: end.streams is not an array: {report}"));
    assert_eq!(
        streams.len(),
        expected_streams,
        "{who}: expected {expected_streams} streams, got {}",
        streams.len()
    );
    for (i, s) in streams.iter().enumerate() {
        let bytes = s["udp"]["bytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("{who}: stream {i} has no udp.bytes: {s}"));
        assert!(
            bytes > 0,
            "{who}: stream {i} moved 0 bytes (demux misrouted?)"
        );
    }
}

#[test]
fn udp_demux_forward_parallel_completes() {
    let report = run_demux_udp(&[], "forward");
    assert_all_streams_have_bytes(&report, 4, "forward");
}

#[test]
fn udp_demux_reverse_parallel_completes() {
    let report = run_demux_udp(&["-R"], "reverse");
    assert_all_streams_have_bytes(&report, 4, "reverse");
}

#[test]
fn udp_demux_bidir_parallel_completes() {
    // The exact #80 case: 4 receiving + 4 sending streams over one server socket.
    let report = run_demux_udp(&["--bidir"], "bidir");
    assert_all_streams_have_bytes(&report, 8, "bidir");
}
