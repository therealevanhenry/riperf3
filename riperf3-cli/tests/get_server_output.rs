//! CLI integration tests: `--get-server-output` must work like iperf3 (#33) —
//! the server returns its console output (text mode) or its full `-J` report
//! (JSON mode) in the results exchange, and the client prints/attaches it —
//! while the server console stays live (iperf3 dual-writes; it never
//! diverted). Pre-#33 the flag was a silent no-op.
#![cfg(unix)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run the client to completion (with refused-retry) and return its stdout.
fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> String {
    common::run_client_ok(args, timeout, who).stdout
}

fn spawn_server_capturing(extra: &[&str], port_str: &str) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let mut args = vec!["-s", "-1", "-p", port_str];
    args.extend_from_slice(extra);
    ChildGuard(
        Command::new(bin)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    )
}

fn collect_stdout(mut child: ChildGuard) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.0.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    out
}

/// text client × text server: the client prints `Server output:` followed by
/// the server's report lines, while the server's own stdout stays LIVE with
/// the same report (iperf3 dual-writes — console and exchange).
#[test]
fn text_client_gets_text_server_output() {
    let port = free_port();
    let ps = port.to_string();
    let server = spawn_server_capturing(&[], &ps);

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "client",
    );
    let server_out = collect_stdout(server);

    assert!(
        out.contains("Server output:"),
        "client must print the server's output section (#33): {out}"
    );
    assert!(
        out.contains("receiver"),
        "server's report (receiver summary) must appear in the client output: {out}"
    );
    assert!(
        server_out.contains("receiver"),
        "iperf3 dual-writes: the server console stays LIVE while the output \
         also rides the exchange (iperf_printf appends to server_output_list \
         AND fprintfs) — review r1: {server_out}"
    );
}

/// `-J` client × text server: the report carries a top-level
/// `server_output_text` string.
#[test]
fn json_client_gets_server_output_text() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server_capturing(&[], &ps);

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "-J",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    let text = v["server_output_text"]
        .as_str()
        .unwrap_or_else(|| panic!("missing top-level server_output_text (#33): {out}"));
    assert!(
        text.contains("receiver"),
        "server text must contain its receiver summary: {text}"
    );
}

/// `-J` client × `-J` server: the report carries `server_output_json` with the
/// server's full report shape.
#[test]
fn json_client_gets_server_output_json() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server_capturing(&["-J"], &ps);

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "-J",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    let sj = &v["server_output_json"];
    assert!(
        sj.is_object(),
        "missing top-level server_output_json (#33): {out}"
    );
    for k in ["start", "intervals", "end"] {
        assert!(sj.get(k).is_some(), "server_output_json missing {k}: {out}");
    }
}

/// Without the flag nothing changes: no Server output section, no keys.
#[test]
fn no_flag_no_server_output() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server_capturing(&[], &ps);

    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-J"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    assert!(v.get("server_output_text").is_none());
    assert!(v.get("server_output_json").is_none());
}

// ---------------------------------------------------------------------------
// #168: --get-server-output x --json-stream divergences (both roles) + the
// timestamps-in-capture gap. Ground truth iperf3 3.20 (iperf_api.c:3900,
// 5310-5323): a --json-stream SERVER keeps its JSON alive when the client
// requests output and attaches server_output_json; a --json-stream CLIENT
// emits server_output_text/_json NDJSON events BEFORE `end`; a capturing
// --timestamps server's returned text carries the prefixes.
// ---------------------------------------------------------------------------

/// A `--json-stream` server attaches its JSON report for a requesting client.
#[test]
fn json_stream_server_attaches_json_output() {
    let port = free_port();
    let ps = port.to_string();
    let server = spawn_server_capturing(&["--json-stream"], &ps);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "-J",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "json client vs json-stream server",
    );
    drop(server);
    let v: serde_json::Value = serde_json::from_str(&out).expect("client -J output");
    assert!(
        v.get("server_output_json").is_some(),
        "a --json-stream server must attach server_output_json (iperf3 keeps \
         json_top alive for get_server_output): {out}"
    );
}

/// A `--json-stream` client emits the returned server output as an NDJSON
/// event before `end` (iperf3 event order: start, interval*, server_output_*,
/// end).
#[test]
fn json_stream_client_emits_server_output_event_before_end() {
    let port = free_port();
    let ps = port.to_string();
    let server = spawn_server_capturing(&[], &ps);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "--json-stream",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "json-stream client",
    );
    drop(server);
    let mut saw_server_output_at = None;
    let mut saw_end_at = None;
    for (i, line) in out.lines().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("non-JSON NDJSON line ({e}): {line}"));
        match v.get("event").and_then(|e| e.as_str()) {
            Some("server_output_text") | Some("server_output_json") => {
                saw_server_output_at.get_or_insert(i);
            }
            Some("end") => {
                saw_end_at.get_or_insert(i);
            }
            _ => {}
        }
    }
    let so = saw_server_output_at
        .unwrap_or_else(|| panic!("no server_output_* event in the NDJSON: {out}"));
    let end = saw_end_at.unwrap_or_else(|| panic!("no end event: {out}"));
    assert!(
        so < end,
        "server_output_* must precede end (iperf3 order): {out}"
    );
}

/// `--timestamps` on a capturing server: the returned text carries the
/// prefixes (iperf3 buffers the PREFIXED linebuffer).
#[test]
fn timestamped_server_capture_carries_prefixes() {
    let port = free_port();
    let ps = port.to_string();
    let server = spawn_server_capturing(&["--timestamps"], &ps);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "--get-server-output",
        ],
        Duration::from_secs(20),
        "client vs timestamped server",
    );
    drop(server);
    let server_block: Vec<&str> = out
        .lines()
        .skip_while(|l| !l.contains("Server output:"))
        .skip(1)
        .filter(|l| l.contains("Mbits/sec") || l.contains("bits/sec"))
        .collect();
    assert!(
        !server_block.is_empty(),
        "no server report lines in the capture: {out}"
    );
    let ts = regex_lite_timestamp(server_block[0]);
    assert!(
        ts,
        "captured server report lines must carry the --timestamps prefix \
         (iperf3 tees the prefixed line): {:?}",
        server_block[0]
    );
}

/// HH:MM:SS-ish prefix check without a regex dependency.
fn regex_lite_timestamp(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() > 9
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
}
