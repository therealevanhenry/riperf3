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
use std::time::{Duration, Instant};

use serde_json::Value;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
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

/// Kills the wrapped child on drop, so a spawned server is reaped even if the
/// test panics before it is waited on.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait for a child to exit, bounded by `timeout` (kill + panic on timeout), so a
/// hang fails the test cleanly instead of stalling the whole suite.
fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
    who: &str,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{who}: timed out");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Spawn `riperf3`, bound its run, and return captured stdout.
fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{who}: spawn failed: {e}"));

    wait_bounded(&mut child, timeout, who);
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
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
    std::thread::sleep(Duration::from_millis(300));

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
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
    std::thread::sleep(Duration::from_millis(300));

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
            "1",
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
    std::thread::sleep(Duration::from_millis(300));

    // Drive one test, bounded so a non-serving server can't hang the suite.
    let mut client = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-i", "1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn client");
    let status = wait_bounded(&mut client, Duration::from_secs(20), "client");
    assert!(status.success(), "client failed: {status:?}");

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
