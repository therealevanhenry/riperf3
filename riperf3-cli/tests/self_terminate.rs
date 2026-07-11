//! #410: the bitrate-breach cells run `-t 9` — GT's 5-sample moving-average
//! gate fires at ~5 s, and a `-t 5` client races the breach at exactly the
//! window boundary (the pre-#410 whole-test check fired at ~1 s).
//! #224 — server self-terminate parity (--server-bitrate-limit /
//! --server-max-duration). Ground truth (iperf 3.21, live-captured 2026-06-11
//! on the pinned interop build): both paths use the **SERVER_ERROR (-2)**
//! control state with an (i_errno, errno) u32-pair payload — NOT
//! SERVER_TERMINATE — and neither side dumps a final summary:
//!
//!   server (text):  stderr `iperf3: error - <msg>`, NO summary block, exit 0
//!   server (-J):    single doc, error key `error - <msg>` (iperf_err's
//!                   prefix wart, faithfully mirrored), stderr empty, exit 0
//!   client (text):  stderr `iperf3: SERVER ERROR - <strerror>` then
//!                   `iperf3: error - <strerror>`, NO summary, exit 1
//!   client (-J):    single doc, error key = the strerror, stderr empty
//!
//! The bitrate message is iperf_strerror(IETOTALRATE=27); the duration-timer
//! pair is the literal server line "server test duration expired - test is
//! terminated by the server" vs the client's strerror(IESERVERTESTDURATION-
//! EXPIRED=160) "server test duration expired".

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::ChildGuard;

const BITRATE_MSG: &str = "total required bandwidth is larger than server limit";

fn spawn_server(extra: &[&str], port: &str) -> ChildGuard {
    let mut args = vec!["-s", "-1", "-p", port];
    args.extend_from_slice(extra);
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn server"),
    )
}

/// Bounded wait that captures both pipes and the exit code.
fn finish(mut child: ChildGuard, timeout: Duration, who: &str) -> (String, String, i32) {
    let deadline = Instant::now() + timeout;
    while child.0.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "{who}: did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = child.0.try_wait().expect("try_wait").unwrap();
    let mut out = String::new();
    if let Some(mut s) = child.0.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut s) = child.0.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    (out, err, status.code().unwrap_or(-1))
}

/// Text mode, both roles: the error relays with iperf3's exact stderr shapes,
/// neither side prints a final summary, server exits 0 / client exits 1.
#[test]
fn bitrate_limit_text_relays_server_error_no_summaries() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1K"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "9"],
        Duration::from_secs(40),
        "client",
    );
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    // Client: both iperf3 stderr lines, error exit, no summary block.
    assert_eq!(client.status.code(), Some(1), "client errexits like iperf3");
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: SERVER ERROR - {BITRATE_MSG}")),
        "the relayed line (iperf_err on SERVER_ERROR receipt): {stderr}",
        stderr = client.stderr
    );
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: error - {BITRATE_MSG}")),
        "the errexit line with the ADOPTED i_errno: {stderr}",
        stderr = client.stderr
    );
    assert!(
        !client.stdout.contains("- - - - -"),
        "iperf3's client prints NO summary on SERVER_ERROR (the dump is the \
         SERVER_TERMINATE path, not this one): {out}",
        out = client.stdout
    );

    // Server: the iperf_err line, exit 0 (the one-off wart, faithfully), no
    // summary after its interval ticks.
    assert!(
        serr.contains(&format!("riperf3: error - {BITRATE_MSG}")),
        "server reports the limit breach on stderr: {serr}"
    );
    assert_eq!(
        scode, 0,
        "iperf3's one-off exits 0 on this path (live-verified)"
    );
    assert!(
        !sout.contains("- - - - -"),
        "no final summary block on self-terminate: {sout}"
    );
}

/// -J client: exactly ONE document (the whole stdout must parse as a single
/// JSON value), the error key carries the strerror, stderr stays empty.
#[test]
fn bitrate_limit_json_client_single_doc_with_relayed_error() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1K"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "9", "-J"],
        Duration::from_secs(40),
        "client -J",
    );
    let _ = finish(server, Duration::from_secs(10), "server");

    assert_eq!(client.status.code(), Some(1));
    assert!(
        client.stderr.trim().is_empty(),
        "-J keeps stderr empty (the error rides the doc): {stderr}",
        stderr = client.stderr
    );
    let doc: serde_json::Value = serde_json::from_str(client.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "client -J stdout must be EXACTLY one document ({e}): {out}",
            out = client.stdout
        )
    });
    assert_eq!(
        doc["error"].as_str(),
        Some(BITRATE_MSG),
        "the doc adopts the relayed strerror: {doc}"
    );
    // #404: the relay is a KILL, not a finalize — GT's reporter switch
    // no-ops on state SERVER_ERROR, so json_end is never filled and the
    // doc's `end` is BARE regardless of stage (live-probed 3.21, -t 20 vs
    // 1K limit: GT end keys [], riperf3 pre-fix rendered the populated
    // finalize end). The accumulated intervals stay.
    assert!(
        doc["end"].as_object().is_some_and(|m| m.is_empty()),
        "a mid-run SERVER_ERROR relay renders GT's bare end (#404): {doc}"
    );
    assert!(
        !doc["intervals"].as_array().expect("intervals").is_empty(),
        "the accumulated intervals stay in the doc: {doc}"
    );
}

/// -J server: the accumulated intervals emit over a BARE `end: {}` (#368) —
/// GT's rate-breach kill path (cleanup_server + return -1,
/// iperf_server_api.c:624-646) never runs end processing, so the reader's
/// document finishes with `end` as an empty object; the error key carries
/// iperf_err's `error - ` prefix wart, stderr empty, exit 0. Live-verified
/// against iperf 3.21 (`--server-bitrate-limit 1K` + `-t 5`: GT `end` keys
/// `[]`, riperf3 pre-#368 rendered the full finalize end).
#[test]
fn bitrate_limit_json_server_doc_bare_end_and_prefixed_error() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1K", "-J"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let _client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "9"],
        Duration::from_secs(40),
        "client",
    );
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server -J");

    assert!(
        serr.trim().is_empty(),
        "JSON mode keeps stderr silent: {serr}"
    );
    assert_eq!(scode, 0);
    let doc: serde_json::Value = serde_json::from_str(sout.trim())
        .unwrap_or_else(|e| panic!("server -J stdout must be one document ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {BITRATE_MSG}").as_str()),
        "iperf_err's in-doc prefix wart, mirrored exactly: {doc}"
    );
    // #368: the kill path skips end processing — bare `end: {}`, NOT the
    // populated finalize end (streams/sums/cpu/congestion).
    assert_eq!(
        doc["end"].as_object().map(serde_json::Map::len),
        Some(0),
        "GT's rate-breach end is bare {{}}: {doc}"
    );
    // The accumulated interval rows still ride the doc (the start block too);
    // only the end block is suppressed.
    assert!(
        doc["intervals"].is_array() && doc["start"].as_object().is_some_and(|m| !m.is_empty()),
        "intervals + start survive the bare-end path: {doc}"
    );
}

// ---------------------------------------------------------------------------
// #230 — the upfront requested-duration check (iperf 3.21 GT, live-captured
// 2026-06-11): with --server-max-duration set, the SERVER rejects at param
// exchange when (time + omit) > max OR time == 0 (iperf_api.c:2666) — and a
// -n/-k client sends time 0 (iperf_api.c:1981), so EVERY byte/block-limited
// run is "unbounded" and rejected. The relay is cleanup_server's
// SERVER_ERROR + (IEMAXSERVERTESTDURATIONEXCEEDED=37, errno) pair; the test
// never starts (no Accepted line, no streams, no summaries).
//
//   client (text): stdout ONLY "Connecting to host...", stderr
//                  `iperf3: SERVER ERROR - <strerror(37)>` +
//                  `iperf3: error - <strerror(37)>`, exit 1
//   server (text): stderr `iperf3: error - <strerror(37)>`, one-off exit 0,
//                  stdout has NO "Accepted connection" line
//   client (-J):   single doc, error key = strerror(37), stderr empty
//   server (-J):   the SKELETON doc — start {connected:[], version,
//                  system_info} only, intervals [], end {}, error key with
//                  iperf_err's "error - " prefix
//   server (--json-stream): {"event":"error","data":"error - <msg>"} then
//                  {"event":"end","data":{}} — no start event
// ---------------------------------------------------------------------------

const MAXDUR_MSG: &str = "client's requested duration exceeds the server's maximum permitted limit";

/// The headline text shapes, both roles: -t 6 vs max 2 is refused at param
/// exchange — instantly, with no test start on either side.
#[test]
fn upfront_reject_text_shapes() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "2"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "6"],
        Duration::from_secs(40),
        "client -t 6",
    );
    let elapsed = start.elapsed();
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    assert!(
        elapsed < Duration::from_secs(4),
        "the upfront check refuses BEFORE the test starts (GT: instantaneous), \
         not via any timer: {elapsed:?}"
    );
    assert_eq!(client.status.code(), Some(1), "client errexits like iperf3");
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: SERVER ERROR - {MAXDUR_MSG}")),
        "the relayed strerror(37) line: {stderr}",
        stderr = client.stderr
    );
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: error - {MAXDUR_MSG}")),
        "the errexit line with the adopted i_errno: {stderr}",
        stderr = client.stderr
    );
    assert!(
        client.stdout.contains("Connecting to host")
            && !client.stdout.contains("- - - - -")
            && !client.stdout.contains("connected to"),
        "client stdout is ONLY the Connecting line (GT capture): {out}",
        out = client.stdout
    );

    assert!(
        serr.contains(&format!("riperf3: error - {MAXDUR_MSG}")),
        "server reports the refusal on stderr: {serr}"
    );
    assert_eq!(scode, 0, "iperf3's one-off exits 0 on the refusal path");
    assert!(
        !sout.contains("Accepted connection"),
        "GT skips on_connect on the refusal path — no Accepted line: {sout}"
    );
    assert!(
        !sout.contains("- - - - -"),
        "no summary block — the test never ran: {sout}"
    );
}

/// -t 0 means an unbounded request: GT's `duration == 0` clause refuses it
/// against ANY max (live-verified vs max 5).
#[test]
fn upfront_reject_time_zero() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "5"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "0"],
        Duration::from_secs(40),
        "client -t 0",
    );
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    assert_eq!(client.status.code(), Some(1));
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: SERVER ERROR - {MAXDUR_MSG}")),
        "-t 0 is rejected by the duration==0 clause: {stderr}",
        stderr = client.stderr
    );
    assert!(serr.contains(&format!("riperf3: error - {MAXDUR_MSG}")));
    assert_eq!(scode, 0);
}

/// A -n run sends `time: 0` on the wire (iperf_api.c:1981) and is refused by
/// the `duration == 0` clause — even against max 11, which the old
/// "tests the default -t 10" theory would let pass. Pins BOTH the client's
/// time-zeroing and the server's 0-clause.
#[test]
fn upfront_reject_n_run_via_zero_duration_clause() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "11"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-n", "100M"],
        Duration::from_secs(40),
        "client -n 100M",
    );
    let elapsed = start.elapsed();
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    assert!(
        elapsed < Duration::from_secs(4),
        "refused upfront, not after a transfer: {elapsed:?}"
    );
    assert_eq!(client.status.code(), Some(1));
    assert!(
        client
            .stderr
            .contains(&format!("riperf3: SERVER ERROR - {MAXDUR_MSG}")),
        "a byte-limited run is unbounded-duration and must be refused \
         (GT live: -n vs max 11 rejects): {stderr}",
        stderr = client.stderr
    );
    assert!(serr.contains(&format!("riperf3: error - {MAXDUR_MSG}")));
    assert_eq!(scode, 0);
}

/// The boundary is `(time + omit) > max`, strictly: -t 2 -O 2 vs max 3 is
/// refused (4 > 3), while -t 2 -O 1 vs max 3 runs to completion (3 <= 3) —
/// the within-limit survival case, which also pins that the refusal logic
/// doesn't fire on requests it must allow.
#[test]
fn upfront_reject_omit_counts_and_boundary_is_strict() {
    // 2 + 2 > 3: refused.
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "3"], &ps);
    std::thread::sleep(Duration::from_millis(300));
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "2", "-O", "2"],
        Duration::from_secs(40),
        "client -t 2 -O 2",
    );
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(
        client.status.code(),
        Some(1),
        "omit counts toward the requested duration (GT: (time+omit) > max): {stderr}",
        stderr = client.stderr
    );
    assert!(client
        .stderr
        .contains(&format!("riperf3: SERVER ERROR - {MAXDUR_MSG}")));
    assert!(serr.contains(&format!("riperf3: error - {MAXDUR_MSG}")));
    assert_eq!(scode, 0);

    // 2 + 1 <= 3: runs to completion with a normal summary.
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "3"], &ps);
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "2", "-O", "1"],
        Duration::from_secs(40),
        "client -t 2 -O 1",
    );
    let elapsed = start.elapsed();
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(
        client.status.code(),
        Some(0),
        "within-limit requests must run (boundary is >, not >=): {stderr}",
        stderr = client.stderr
    );
    assert!(
        elapsed >= Duration::from_secs(3),
        "the within-limit run really ran its 2s + 1s omit: {elapsed:?}"
    );
    assert!(
        client.stdout.contains("- - - - -"),
        "normal summary block: {out}",
        out = client.stdout
    );
    assert!(
        serr.trim().is_empty(),
        "no refusal line for a within-limit run: {serr}"
    );
    assert_eq!(scode, 0);
    assert!(sout.contains("Accepted connection"));
}

/// -J shapes, both roles (GT capture 2026-06-11): single docs; the client's
/// error key carries the bare strerror; the server emits the SKELETON doc —
/// start has only connected/version/system_info (no accepted_connection, no
/// cookie, no test_start), intervals [], end {}, and the "error - " prefix.
#[test]
fn upfront_reject_json_doc_shapes() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "2", "-J"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "6", "-J"],
        Duration::from_secs(40),
        "client -J",
    );
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server -J");

    // Client doc.
    assert_eq!(client.status.code(), Some(1));
    assert!(
        client.stderr.trim().is_empty(),
        "-J keeps the client's stderr empty: {stderr}",
        stderr = client.stderr
    );
    let doc: serde_json::Value = serde_json::from_str(client.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "client -J stdout must be one document ({e}): {out}",
            out = client.stdout
        )
    });
    assert_eq!(
        doc["error"].as_str(),
        Some(MAXDUR_MSG),
        "client doc adopts the relayed strerror: {doc}"
    );
    // #261: the client's refusal doc is byte-faithful to GT's. The test never
    // reached TestStart, so GT OMITS the late start fields and emits `end: {}` —
    // while keeping the early metadata (timestamp/cookie/connecting_to). The
    // start.timestamp carries the REAL on_connect wall-clock, NOT epoch-0.
    let cstart = doc["start"].as_object().expect("client start object");
    for present in [
        "connected",
        "version",
        "system_info",
        "timestamp",
        "connecting_to",
        "cookie",
    ] {
        assert!(
            cstart.contains_key(present),
            "#261 client refusal start keeps {present}: {doc}"
        );
    }
    for absent in [
        "sock_bufsize",
        "sndbuf_actual",
        "rcvbuf_actual",
        "test_start",
    ] {
        assert!(
            !cstart.contains_key(absent),
            "#261 client refusal start must OMIT {absent} (GT shape): {doc}"
        );
    }
    assert_eq!(
        doc["end"].as_object().map(serde_json::Map::len),
        Some(0),
        "#261 client refusal end must be the bare `end: {{}}`: {doc}"
    );
    assert_ne!(
        doc["start"]["timestamp"]["timesecs"],
        serde_json::json!(0),
        "#261 refusal timestamp must be the real connect wall-clock, not epoch-0: {doc}"
    );
    // Exactly ONE `error` key (single clean key, not GT's #2051 duplicate) —
    // checked against the raw bytes, since the parsed Value de-duplicates.
    assert_eq!(
        client.stdout.matches("\"error\"").count(),
        1,
        "#261 exactly one error key in the client doc: {out}",
        out = client.stdout
    );

    // Server skeleton doc.
    assert!(serr.trim().is_empty(), "server -J stderr empty: {serr}");
    assert_eq!(scode, 0);
    let doc: serde_json::Value = serde_json::from_str(sout.trim())
        .unwrap_or_else(|e| panic!("server -J stdout must be one document ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {MAXDUR_MSG}").as_str()),
        "iperf_err's in-doc prefix wart: {doc}"
    );
    assert_eq!(
        doc["intervals"].as_array().map(Vec::len),
        Some(0),
        "no intervals — the test never ran: {doc}"
    );
    assert_eq!(
        doc["end"].as_object().map(serde_json::Map::len),
        Some(0),
        "end is an EMPTY object on the refusal skeleton (GT): {doc}"
    );
    let start_obj = doc["start"].as_object().expect("start object");
    assert_eq!(
        start_obj["connected"].as_array().map(Vec::len),
        Some(0),
        "start.connected is empty: {doc}"
    );
    assert!(
        start_obj.contains_key("version") && start_obj.contains_key("system_info"),
        "skeleton start keeps version/system_info: {doc}"
    );
    for absent in ["accepted_connection", "cookie", "test_start", "timestamp"] {
        assert!(
            !start_obj.contains_key(absent),
            "GT's refusal skeleton has NO {absent} key: {doc}"
        );
    }
}

/// --json-stream refuse shape (GT capture): exactly two events — the
/// prefixed error, then `end` with an EMPTY data object. No start event.
#[test]
fn upfront_reject_json_stream_events() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "2", "--json-stream"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let _client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "6"],
        Duration::from_secs(40),
        "client",
    );
    let (sout, serr, scode) = finish(server, Duration::from_secs(10), "server --json-stream");

    assert!(serr.trim().is_empty(), "json-stream stderr empty: {serr}");
    assert_eq!(scode, 0);
    let lines: Vec<&str> = sout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "exactly error + end events (GT: no start event on refusal): {sout}"
    );
    let ev0: serde_json::Value = serde_json::from_str(lines[0]).expect("error event parses");
    assert_eq!(ev0["event"].as_str(), Some("error"));
    assert_eq!(
        ev0["data"].as_str(),
        Some(format!("error - {MAXDUR_MSG}").as_str())
    );
    let ev1: serde_json::Value = serde_json::from_str(lines[1]).expect("end event parses");
    assert_eq!(ev1["event"].as_str(), Some("end"));
    assert_eq!(
        ev1["data"].as_object().map(serde_json::Map::len),
        Some(0),
        "end data is an empty object: {sout}"
    );
}

/// A persistent (non-one-off) server refuses test #1 and serves test #2
/// normally (GT live: the refusal doesn't poison the accept loop).
#[test]
fn upfront_reject_persistent_server_serves_next_test() {
    let ps = common::free_port().to_string();
    // No -1: spawn the persistent shape by hand.
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &ps, "--server-max-duration", "3"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn persistent server"),
    );
    std::thread::sleep(Duration::from_millis(300));

    let refused = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "6"],
        Duration::from_secs(40),
        "refused client",
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused
        .stderr
        .contains(&format!("riperf3: SERVER ERROR - {MAXDUR_MSG}")));

    let served = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(40),
        "served client",
    );
    assert_eq!(
        served.status.code(),
        Some(0),
        "the server must serve the next test after a refusal: {stderr}",
        stderr = served.stderr
    );
    assert!(served.stdout.contains("- - - - -"), "normal summary");

    let _ = server.0.kill();
    let _ = server.0.wait();
}

/// The in-flight 160-watchdog, post-#230: GT arms it for EVERY duration test
/// at (time + omit + 40s grace), flag-independent (create_server_timers,
/// iperf_server_api.c:380-395) — --server-max-duration arms NOTHING. A
/// wedged client (SIGSTOP mid-test) must trip it at ~duration+40, the server
/// must print server_timer_proc's literal line, CLOSE its streams (GT does;
/// a plain join hangs forever on the silent socket — the PR #247 r1 probe
/// measured 169 s), and one-off exit 0. --server-bitrate-limit 1T keeps the
/// 1 Hz rate ticks live the whole wait: the #237 tick-immunity pin at the
/// new anchor (a per-iteration recreated sleep would never fire).
#[cfg(unix)]
#[test]
fn watchdog_fires_at_duration_plus_grace_despite_rate_ticks() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1T"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", &ps, "-t", "5"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn client"),
    );
    let start = Instant::now();
    // Wedge the client mid-TEST_RUNNING: well past stream setup (~0.3s even
    // on loaded 2-core runners), well before its own 5s end.
    std::thread::sleep(Duration::from_millis(1500));
    let stop = Command::new("kill")
        .args(["-STOP", &client.0.id().to_string()])
        .status()
        .expect("send SIGSTOP");
    assert!(stop.success(), "SIGSTOP delivered");

    // The watchdog deadline is 5 + 0 + 40 = 45s from test start. Allow
    // scheduling slack above; the lower bound is structural (tokio timers
    // never fire early).
    let (_sout, serr, scode) = finish(server, Duration::from_secs(60), "server");
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(44),
        "the watchdog must not fire before duration+grace (45s): {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(58),
        "the watchdog must fire at ~45s and the server must EXIT (a hung \
         stream join means the GT socket-close mirror is missing): {elapsed:?}"
    );
    assert!(
        serr.contains(
            "riperf3: error - server test duration expired - test is terminated by the server"
        ),
        "server_timer_proc's literal line: {serr}"
    );
    assert_eq!(scode, 0, "one-off exits 0 on self-terminate");
    // ChildGuard SIGKILLs the wedged client on drop.
}

/// #260 r1 F6: GT's get_parameters adds `target_bitrate` to json_start BEFORE
/// running the refusal checks (iperf_api.c:2662), so a REFUSED client that
/// sent `-b` leaves the server's -J skeleton carrying it — for BOTH refusal
/// kinds. Clients without -b keep the plain #230 skeleton (pinned above).
#[test]
fn refusal_skeleton_carries_the_client_target_bitrate() {
    for (server_extra, client_extra) in [
        (
            &["--server-bitrate-limit", "1M"][..],
            &["-b", "2M", "-t", "2"][..],
        ),
        (
            &["--server-max-duration", "1"][..],
            &["-b", "2M", "-t", "10"][..],
        ),
    ] {
        let port = common::free_port();
        let ps = port.to_string();
        let mut args = vec!["-s", "-1", "-p", &ps, "-J"];
        args.extend_from_slice(server_extra);
        let server = ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_riperf3"))
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn server"),
        );

        let mut cargs = vec!["-c", "127.0.0.1", "-p", &ps];
        cargs.extend_from_slice(client_extra);
        let _ = common::run_client(&cargs, Duration::from_secs(20), "refused client");
        // -1 server exits after the refusal; bounded wait, then read its doc.
        let mut server = server;
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.0.try_wait().expect("try_wait").is_none() {
            assert!(Instant::now() < deadline, "server did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut sout = String::new();
        server
            .0
            .stdout
            .take()
            .expect("piped")
            .read_to_string(&mut sout)
            .expect("read server doc");
        let sdoc: serde_json::Value = serde_json::from_str(sout.trim())
            .unwrap_or_else(|e| panic!("server -J doc ({e}): {sout}"));
        assert_eq!(
            sdoc["start"]["target_bitrate"].as_u64(),
            Some(2_000_000),
            "{server_extra:?}: the refusal skeleton carries the refused \
             client's -b (GT json_start order: after system_info): {sdoc}"
        );
    }
}

// ---------------------------------------------------------------------------
// #344: iperf_err stamps its stderr lines with the --timestamps prefix
// (iperf_error.c:51-57, :77) — GT's self-terminate line rides iperf_err
// directly (server_timer_proc, iperf_server_api.c:328) and both refusal
// kinds reach main.c:174's iperf_err via the -1 return. A literal strftime
// format keeps the pins deterministic on unix; Windows uses the documented
// HH:MM:SS fallback and ignores the format, so the byte-exact half is
// unix-only (the #339 lesson).
// ---------------------------------------------------------------------------

/// Portable half: a nonempty prefix precedes the expected line somewhere in
/// stderr. Unix half: the literal format renders verbatim.
fn assert_stamped_line(serr: &str, bare: &str) {
    let line = serr
        .lines()
        .find(|l| l.ends_with(bare))
        .unwrap_or_else(|| panic!("no line ending with {bare:?} in {serr:?}"));
    assert!(
        line.len() > bare.len(),
        "a nonempty timestamp prefix precedes the line: {line:?}"
    );
    #[cfg(unix)]
    assert_eq!(
        line,
        &format!("XTSX {bare}"),
        "the literal format renders verbatim: {line:?}"
    );
}

/// The runtime breach line (the handle_one_test server_error emit site).
#[test]
fn bitrate_limit_line_carries_the_timestamps_prefix() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1K", "--timestamps=XTSX "], &ps);
    std::thread::sleep(Duration::from_millis(300));
    let _ = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "9"],
        Duration::from_secs(40),
        "client",
    );
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(scode, 0);
    assert_stamped_line(&serr, &format!("riperf3: error - {BITRATE_MSG}"));
}

/// The upfront total-rate refusal line (refuse_total_rate's text emit — the
/// message matches the runtime breach, so the scenario picks the site: a
/// refused param exchange never starts the test).
#[test]
fn upfront_total_rate_line_carries_the_timestamps_prefix() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1M", "--timestamps=XTSX "], &ps);
    std::thread::sleep(Duration::from_millis(300));
    let _ = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-b", "2M", "-t", "2"],
        Duration::from_secs(20),
        "refused client",
    );
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(scode, 0);
    assert_stamped_line(&serr, &format!("riperf3: error - {BITRATE_MSG}"));
}

/// The upfront max-duration refusal line (refuse_max_duration's text emit).
#[test]
fn upfront_max_duration_line_carries_the_timestamps_prefix() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "2", "--timestamps=XTSX "], &ps);
    std::thread::sleep(Duration::from_millis(300));
    let _ = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "6"],
        Duration::from_secs(20),
        "refused client",
    );
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(scode, 0);
    assert_stamped_line(&serr, &format!("riperf3: error - {MAXDUR_MSG}"));
}

/// #386, the signal cell (GT live-probed both cells 2026-07-10): a refused
/// round PARKS doc-less until client EOF — a SIGTERM landing in that park
/// abandons the unprinted refusal doc, and the server emits the interrupt
/// skeleton ALONE (one doc, carrying the parked round's target_bitrate —
/// GT's json_start was stamped at get_parameters and json_finish renders
/// it). riperf3 printed the refusal doc immediately, so the same sequence
/// yielded TWO docs (probed 30/30 vs GT 10/10 in #385's r1 hammer). The
/// park makes this cell deterministic: no race, the mock just holds.
#[cfg(unix)]
#[test]
fn sigterm_during_refusal_park_abandons_the_refusal_doc() {
    use std::io::Write;

    let port = common::free_port();
    let ps = port.to_string();
    let server = spawn_server(&["-J", "--server-max-duration", "1"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    // Raw mock: violate, read the fe+37 relay, HOLD the ctrl.
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(&[b'x'; 37]).unwrap();
    let mut b = [0u8; 1];
    ctrl.read_exact(&mut b).unwrap();
    assert_eq!(b[0], 9, "ParamExchange");
    let params = br#"{"tcp":true,"time":30,"parallel":1,"len":4096,"bandwidth":2000000}"#;
    ctrl.write_all(&(params.len() as u32).to_be_bytes())
        .unwrap();
    ctrl.write_all(params).unwrap();
    let mut relay = [0u8; 9];
    ctrl.read_exact(&mut relay).unwrap();
    assert_eq!(relay[0], 0xfe, "SERVER_ERROR state");

    // The park is stable state — signal lands inside it deterministically.
    std::thread::sleep(Duration::from_millis(300));
    let pid = server.0.id() as i32;
    unsafe { libc::kill(pid, libc::SIGTERM) };

    let (sout, _serr, scode) = finish(server, Duration::from_secs(10), "server");
    assert_eq!(scode, 0, "signal-normal exit");
    drop(ctrl);

    let docs: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&sout)
        .into_iter::<serde_json::Value>()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("server -J stream ({e}): {sout}"));
    assert_eq!(
        docs.len(),
        1,
        "the abandoned refusal must not print — the interrupt skeleton \
         ALONE (#386): {sout}"
    );
    let err = docs[0]["error"].as_str().unwrap_or_default();
    assert!(
        err.starts_with("interrupt - "),
        "the one doc is the interrupt skeleton: {err:?}"
    );
    assert_eq!(
        docs[0]["start"]["target_bitrate"].as_u64(),
        Some(2_000_000),
        "the skeleton carries the PARKED round's -b (GT cell B): {}",
        docs[0]
    );
}
