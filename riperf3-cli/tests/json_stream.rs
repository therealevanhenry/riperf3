//! CLI integration test for `--json-stream` NDJSON output (#62).
//!
//! Regression: `--json-stream` used to print text banners (the `[ ID] Interval`
//! header, the `- - -` separator, the final summary lines) interleaved with
//! *bare* per-stream interval objects that had no `event`/`data` wrapping and no
//! `start`/`end` events — so the stream was neither valid line-delimited JSON nor
//! the iperf3 event schema. The fix makes both the client and the server emit
//! pure NDJSON: `{"event":"start",...}`, one `{"event":"interval",...}` per
//! interval, then `{"event":"end",...}`, with no banners.
//!
//! These tests spawn the real binary, capture the `--json-stream` side's stdout,
//! and assert every line is a valid JSON event in the right order. Before the
//! fix the banner lines fail to parse and the bare objects lack `event`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

/// Assert `stdout` is a valid `--json-stream` document: every non-empty line is a
/// JSON object with `event` + `data`, and the events run `start`, one or more
/// `interval`, `end` — in that order, with nothing else mixed in.
fn assert_valid_ndjson(stdout: &str, who: &str) {
    let mut events = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{who}: line {} is not valid JSON ({e}): {line:?}", i + 1));
        let obj = v
            .as_object()
            .unwrap_or_else(|| panic!("{who}: line {} is not a JSON object: {line:?}", i + 1));
        assert!(
            obj.contains_key("event") && obj.contains_key("data"),
            "{who}: line {} is not an event object (keys: {:?})",
            i + 1,
            obj.keys().collect::<Vec<_>>()
        );
        events.push(obj["event"].as_str().unwrap_or("<non-string>").to_string());
    }

    assert!(!events.is_empty(), "{who}: no events emitted");
    assert_eq!(
        events.first().unwrap(),
        "start",
        "{who}: first event must be `start` ({events:?})"
    );
    assert_eq!(
        events.last().unwrap(),
        "end",
        "{who}: last event must be `end` ({events:?})"
    );
    // Callers must run long enough to cross an interval boundary mid-test (e.g.
    // `-t 2 -i 1`, not `-t 1 -i 1`): with interval == duration the lone tick
    // coincides with the end and its boundary-aligned final interval is dropped
    // (#55), leaving only start+end — an intermittent 2-event flake under load.
    assert!(
        events.len() >= 3,
        "{who}: expected at least one interval between start and end ({events:?})"
    );
    for (i, e) in events.iter().enumerate() {
        let expected = if i == 0 {
            "start"
        } else if i == events.len() - 1 {
            "end"
        } else {
            "interval"
        };
        assert_eq!(
            e, expected,
            "{who}: event {i} should be `{expected}` ({events:?})"
        );
    }
}

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

/// Wait for a child to exit, bounded by `timeout` (kill + panic on timeout), so a
/// hang fails the test cleanly instead of stalling the whole suite. Thin
/// panicking shim over the shared bounded wait (#192).
fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
    who: &str,
) -> std::process::ExitStatus {
    common::wait_bounded(child, timeout).unwrap_or_else(|| panic!("{who}: timed out"))
}

/// Run the client to completion (with refused-retry) and return its stdout.
fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> String {
    common::run_client_ok(args, timeout, who).stdout
}

/// Client `--json-stream` against a plain one-off server.
#[test]
fn client_json_stream_tcp_is_valid_ndjson() {
    let port = free_port();
    let ps = port.to_string();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", &ps])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "2",
            "-i",
            "1",
            "--json-stream",
        ],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();
    assert_valid_ndjson(&out, "client");
}

/// Client `--json-stream` for UDP (exercises the datagram interval fields).
#[test]
fn client_json_stream_udp_is_valid_ndjson() {
    let port = free_port();
    let ps = port.to_string();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", &ps])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-u",
            "-b",
            "10M",
            "-t",
            "2",
            "-i",
            "1",
            "--json-stream",
        ],
        Duration::from_secs(20),
        "client-udp",
    );
    let _ = server.0.wait();
    assert_valid_ndjson(&out, "client-udp");
}

/// Server `--json-stream`: capture the server's stdout while a plain client runs.
#[test]
fn server_json_stream_is_valid_ndjson() {
    let port = free_port();
    let ps = port.to_string();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    // Guard so the server is reaped even if an assertion below panics.
    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", &ps, "--json-stream"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    // Drive one test, bounded so a non-serving server can't hang the suite;
    // its stdout is irrelevant here — only the server's NDJSON is asserted.
    let _ = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "2", "-i", "1"],
        Duration::from_secs(20),
        "client",
    );

    // The one-off server now finishes and closes stdout; bound that wait too.
    // (Output is a handful of small lines, well under the pipe buffer, so
    // waiting for exit before reading can't deadlock on a full pipe.)
    wait_bounded(&mut server.0, Duration::from_secs(20), "server");
    let mut out = String::new();
    server
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_valid_ndjson(&out, "server");
}

/// #213: --json-stream-full-output is the third leg of discard_json — the
/// NDJSON stream is followed by the complete monolithic -J document
/// (iperf_api.c:5323 keeps print_full_json under the flag), with populated
/// intervals.
#[test]
fn json_stream_full_output_appends_the_monolithic_document() {
    let ps = free_port().to_string();
    let server = Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-s", "-1", "-p", &ps])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = ChildGuard(server);
    std::thread::sleep(Duration::from_millis(200));
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "2",
            "-i",
            "1",
            "--json-stream",
            "--json-stream-full-output",
        ],
        Duration::from_secs(20),
        "json-stream full-output",
    );
    // The NDJSON part still leads (event-enveloped single lines)…
    let first = out.lines().next().expect("output");
    assert!(
        first.starts_with("{\"event\":"),
        "stream still leads: {first}"
    );
    // …and a pretty multi-line document follows the `end` event.
    let end_pos = out.find("{\"event\":\"end\"").expect("end event");
    let doc_pos = out[end_pos..]
        .find("{\n")
        .map(|i| i + end_pos)
        .expect("a pretty monolithic document must follow the end event");
    let doc: Value = serde_json::from_str(out[doc_pos..].trim()).expect("document parses");
    assert!(
        doc["intervals"].as_array().is_some_and(|a| !a.is_empty()),
        "the document carries populated intervals (discard_json off)"
    );
    assert!(doc["end"].is_object());
    let _ = server.0.kill();
}

/// #213 negative: without the flag, nothing follows the end event.
#[test]
fn json_stream_without_full_output_ends_at_the_end_event() {
    let ps = free_port().to_string();
    let server = Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-s", "-1", "-p", &ps])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = ChildGuard(server);
    std::thread::sleep(Duration::from_millis(200));
    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "2", "--json-stream"],
        Duration::from_secs(20),
        "json-stream plain",
    );
    let end_pos = out.find("{\"event\":\"end\"").expect("end event");
    let tail = out[end_pos..].lines().skip(1).collect::<Vec<_>>().join("");
    assert!(
        tail.trim().is_empty(),
        "nothing may follow the end event without the flag: {tail:?}"
    );
    let _ = server.0.kill();
}
