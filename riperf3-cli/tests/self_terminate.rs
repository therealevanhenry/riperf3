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
        &["-c", "127.0.0.1", "-p", &ps, "-t", "5"],
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
        &["-c", "127.0.0.1", "-p", &ps, "-t", "5", "-J"],
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
}

/// -J server: the full document still emits (intervals and all) with the
/// error key carrying iperf_err's `error - ` prefix wart, stderr empty,
/// exit 0 — live-verified against iperf 3.21.
#[test]
fn bitrate_limit_json_server_doc_carries_prefixed_error() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-bitrate-limit", "1K", "-J"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let _client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "5"],
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
}

/// #237: the duration timer must be ONE absolute deadline. With
/// --server-bitrate-limit also set, the 1 Hz under-limit rate ticks re-enter
/// the select loop; a sleep() recreated per iteration restarts from zero on
/// every tick, so any max duration > ~1s never fires and a -t 10 run goes
/// the full 10 s. A 1T limit never trips, even on fast loopback (100G
/// does); the 1 s max duration must still cut the run at ~1 s with the
/// same SERVER_ERROR(160) shapes as the plain timer path. NOTE: shares #230's test interplay —
/// the upfront requested-duration check will reject `-t 10` vs a 1 s limit
/// at param exchange when it lands; this test then needs the within-limit
/// wall-clock-overrun trigger too (noted on #230).
#[test]
fn max_duration_timer_survives_rate_ticks() {
    let ps = common::free_port().to_string();
    let server = spawn_server(
        &["--server-bitrate-limit", "1T", "--server-max-duration", "1"],
        &ps,
    );
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "10"],
        Duration::from_secs(40),
        "client -t 10",
    );
    let elapsed = start.elapsed();
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    assert!(
        elapsed < Duration::from_secs(6),
        "rate ticks must not reset the duration timer (#237) — a 1 s max \
         duration left this -t 10 run running for {elapsed:?}"
    );
    assert_eq!(client.status.code(), Some(1));
    assert!(
        client
            .stderr
            .contains("riperf3: SERVER ERROR - server test duration expired"),
        "client adopts strerror(IESERVERTESTDURATIONEXPIRED): {stderr}",
        stderr = client.stderr
    );
    assert!(
        serr.contains(
            "riperf3: error - server test duration expired - test is terminated by the server"
        ),
        "the server's literal timer line: {serr}"
    );
    assert_eq!(scode, 0);
}

/// --server-max-duration via the wall-clock timer: the literal server line
/// vs the client's strerror, the same SERVER_ERROR shapes. NOTE (r1 review,
/// live-verified): iperf3's upfront check rejects `-n` runs too (it tests
/// the default `-t 10` sent alongside, iperf_api.c:2666) — so when #230
/// lands faithfully, this test's `-n` run gets rejected at param exchange
/// and the TIMER arm needs a new trigger (e.g. a `-t`-within-limit run that
/// overruns on wall clock). Revisit this test in #230; noted there.
#[test]
fn max_duration_timer_relays_expired() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["--server-max-duration", "1"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-n", "1T"],
        Duration::from_secs(40),
        "client -n",
    );
    let elapsed = start.elapsed();
    let (_sout, serr, scode) = finish(server, Duration::from_secs(10), "server");

    assert!(
        elapsed < Duration::from_secs(20),
        "the server timer must cut an unbounded -n run: {elapsed:?}"
    );
    assert_eq!(client.status.code(), Some(1));
    assert!(
        client
            .stderr
            .contains("riperf3: SERVER ERROR - server test duration expired"),
        "client adopts strerror(IESERVERTESTDURATIONEXPIRED): {stderr}",
        stderr = client.stderr
    );
    assert!(
        client
            .stderr
            .contains("riperf3: error - server test duration expired"),
        "the errexit line: {stderr}",
        stderr = client.stderr
    );
    assert!(
        serr.contains(
            "riperf3: error - server test duration expired - test is terminated by the server"
        ),
        "the server's literal timer line (iperf_err in server_timer_proc): {serr}"
    );
    assert_eq!(scode, 0);
}
