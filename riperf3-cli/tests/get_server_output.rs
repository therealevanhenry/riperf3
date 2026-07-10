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

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

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
    let server = spawn_server_capturing(&["-J"], &ps);

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
    let server_own = collect_stdout(server);

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

    // r1 F2 (#297): the server's OWN stdout doc is the SAME single build the
    // attachment rode (ReportSource::Built is reused, never rebuilt) — a
    // double build would empty this copy's intervals while the attachment
    // stays populated. Previously this pipe was captured but never read.
    let sv: Value = serde_json::from_str(&server_own)
        .unwrap_or_else(|e| panic!("server -J invalid ({e}): {server_own}"));
    let own_n = sv["intervals"].as_array().map(Vec::len).unwrap_or(0);
    assert!(
        own_n > 0,
        "the -J server's own doc lost its intervals (double-build class): {server_own}"
    );
    // #368 inverse guard: a NORMAL (non-self-terminated) server run keeps the
    // POPULATED finalize end — the bare-end flag must not fire on the happy
    // path (only the rate/duration/idle kill arms set it).
    assert!(
        sv["end"].as_object().is_some_and(|m| !m.is_empty()),
        "a normal server run's end must stay populated: {server_own}"
    );
    assert_eq!(
        Some(own_n),
        sj["intervals"].as_array().map(Vec::len),
        "the attachment and the server's own doc are the same single build"
    );
}

/// Depth-1 keys of the object at the first `"obj":` in `raw` — a raw-string
/// walk, because parsed `Value` asserts are order-blind (the json_report
/// unit pins' technique). Slice `raw` first to scope into a nested object.
fn raw_keys(raw: &str, obj: &str) -> Vec<String> {
    let from = raw.find(&format!("\"{obj}\":")).unwrap() + obj.len() + 3;
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    let mut out = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b'"' => {
                let close = raw[i + 1..].find('"').unwrap() + i + 1;
                if depth == 1 && bytes.get(close + 1) == Some(&b':') {
                    out.push(raw[i + 1..close].to_string());
                }
                i = close;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// #378: the embedded server doc must keep the SERVER's hand-serialized key
/// order end-to-end — GT splices the cJSON subtree as received, so a GT-GT
/// run shows `server_output_json.start` in the #355 server insertion order
/// (live-probed 3.21). A `serde_json::Value` round-trip alphabetizes the
/// whole subtree at BOTH hops (the server's attach and this client's
/// re-emit), destroying the #300-class wire order.
#[test]
fn server_output_json_keeps_the_server_key_order() {
    let port = free_port();
    let ps = port.to_string();
    let server = spawn_server_capturing(&["-J"], &ps);
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
    let _ = collect_stdout(server);

    let soj_at = out.find("\"server_output_json\":").expect("attachment");
    let embedded = &out[soj_at..];
    assert_eq!(
        raw_keys(embedded, "server_output_json"),
        ["start", "intervals", "end"],
        "the embedded doc keeps GT's top-level order (#378): {out}"
    );
    assert_eq!(
        raw_keys(embedded, "start"),
        [
            "connected",
            "version",
            "system_info",
            "sock_bufsize",
            "sndbuf_actual",
            "rcvbuf_actual",
            "timestamp",
            "accepted_connection",
            "cookie",
            "tcp_mss_default",
            "target_bitrate",
            "fq_rate",
            "test_start"
        ],
        "the embedded start keeps the #355 server insertion order, not \
         the alphabetized Value round-trip (#378): {out}"
    );
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
    let n_intervals = v["server_output_json"]["intervals"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        n_intervals >= 1,
        "the attached report carries populated intervals — discard_json's \
         whole purpose (review r1 n2): {out}"
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
    // Explicit format: the faithful default is "%c " (locale datetime, e.g.
    // "Wed Jun 11 ..."), which has no stable shape to assert (#202).
    let server = spawn_server_capturing(&["--timestamps=%H:%M:%S "], &ps);
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

/// #216: iperf3 prefixes EVERY iperf_printf line — the server's listening
/// banner and verbose lines included (iperf_api.c:995/1017,
/// iperf_server_api.c:137). A literal strftime format (no % directives)
/// renders verbatim, making the assertion deterministic.
#[test]
fn server_banner_carries_the_timestamp_prefix() {
    let ps = free_port().to_string();
    let server = spawn_server_capturing(&["--timestamps=TSTAMP "], &ps);
    let _ = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(20),
        "client for ts server",
    );
    let out = collect_stdout(server);
    let banner = out
        .lines()
        .find(|l| l.contains("Server listening on"))
        .expect("banner printed");
    assert!(
        banner.starts_with("TSTAMP "),
        "the listening banner must carry the prefix: {banner:?}"
    );
    let sep = out
        .lines()
        .find(|l| l.contains("-----------"))
        .expect("separator printed");
    assert!(
        sep.starts_with("TSTAMP "),
        "the separator banner line too: {sep:?}"
    );
}

/// #216: the client's verbose lines (vprintln!) carry the prefix — live
/// iperf3 prefixes "Connecting to host" under -V --timestamps.
#[test]
fn client_verbose_lines_carry_the_timestamp_prefix() {
    let ps = free_port().to_string();
    let server = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-s", "-1", "-p", &ps])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = common::ChildGuard(server);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "-V",
            "--timestamps=TSTAMP ",
        ],
        Duration::from_secs(20),
        "verbose timestamps",
    );
    let connecting = out
        .lines()
        .find(|l| l.contains("Connecting to host"))
        .expect("verbose connect line");
    assert!(
        connecting.starts_with("TSTAMP "),
        "vprintln lines must carry the prefix: {connecting:?}"
    );
    let _ = server.0.kill();
}
