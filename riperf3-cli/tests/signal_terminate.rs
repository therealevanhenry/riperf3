//! #210 — iperf_got_sigend parity (iperf_api.c:5144-5188): a signal mid-test
//! dumps the accumulated stats, sends CLIENT_TERMINATE/SERVER_TERMINATE on the
//! control socket so the peer ends cleanly, and exits via the signal-normal
//! path (`iperf3: interrupt - <who> has terminated by signal …`, exit 0 for
//! TERM/INT/HUP). Live-captured shapes against iperf 3.20+ are pinned here.

#![cfg(unix)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::ChildGuard;

fn free_port() -> u16 {
    riperf3_test_support::free_port()
}

fn wait_with_output_bounded(
    mut child: ChildGuard,
    timeout: Duration,
    who: &str,
) -> (String, String, i32) {
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

fn spawn(args: &[&str]) -> ChildGuard {
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn"),
    )
}

/// SIGTERM to the CLIENT mid-test: it dumps the partial end block from local
/// data, prints the signal-normal interrupt line, exits 0 — and the SERVER
/// learns via CLIENT_TERMINATE, printing its own partial results and
/// "the client has terminated" (live iperf3 shapes).
#[test]
fn client_sigterm_dumps_stats_and_terminates_the_server() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "10", "-i", "1"]);
    std::thread::sleep(Duration::from_secs(2));

    let cpid = client.0.id() as i32;
    unsafe {
        libc::kill(cpid, libc::SIGTERM);
    }

    let (cout, cerr, ccode) = wait_with_output_bounded(client, Duration::from_secs(5), "client");
    // The partial end block renders from local data (iperf3's DISPLAY_RESULTS
    // flip): a sender line over ~the elapsed window plus the zeroed receiver.
    assert!(
        cout.contains("sender") && cout.contains("receiver"),
        "client dumps the partial end block: {cout}"
    );
    assert!(
        cerr.contains("interrupt - the client has terminated by signal"),
        "the signal-normal line: {cerr:?}"
    );
    assert_eq!(ccode, 0, "TERM takes the exit-normal path");

    // The server saw CLIENT_TERMINATE: its own partial results + the line.
    let (sout, serr, _) = wait_with_output_bounded(server, Duration::from_secs(5), "server");
    assert!(
        sout.contains("receiver"),
        "server dumps its partial results: {sout}"
    );
    assert!(
        serr.contains("the client has terminated"),
        "server reports the peer's terminate: {serr:?}"
    );
}

/// SIGTERM to the SERVER mid-test: it dumps its partial stats and sends
/// SERVER_TERMINATE; the client renders its partial summary and reports
/// "the server has terminated" (the #170 path), exit 1 on the client.
#[test]
fn server_sigterm_dumps_stats_and_terminates_the_client() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "10", "-i", "1"]);
    std::thread::sleep(Duration::from_secs(2));

    let spid = server.0.id() as i32;
    unsafe {
        libc::kill(spid, libc::SIGTERM);
    }

    let (sout, serr, scode) = wait_with_output_bounded(server, Duration::from_secs(5), "server");
    assert!(
        sout.contains("receiver"),
        "server dumps its partial stats on sigend: {sout}"
    );
    assert!(
        serr.contains("interrupt - the server has terminated by signal"),
        "server's signal-normal line: {serr:?}"
    );
    assert_eq!(scode, 0, "TERM takes the exit-normal path");

    let (cout, cerr, ccode) = wait_with_output_bounded(client, Duration::from_secs(8), "client");
    assert!(
        cout.contains("sender") && cout.contains("receiver"),
        "client renders the partial summary (#170): {cout}"
    );
    assert!(
        cerr.contains("the server has terminated"),
        "client reports the server's terminate: {cerr:?}"
    );
    assert_eq!(ccode, 1, "the client's terminate-by-peer is the error path");
}

/// #210 review r1 f1: the server's -J document carries the terminate message
/// in its `error` key (live iperf3: keys [start, intervals, end, error],
/// stderr EMPTY) — the message must not leak to stderr in JSON mode.
#[test]
fn server_json_doc_carries_the_terminate_error_key() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps, "-J"]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "10"]);
    std::thread::sleep(Duration::from_secs(2));

    let cpid = client.0.id() as i32;
    unsafe {
        libc::kill(cpid, libc::SIGTERM);
    }
    let _ = wait_with_output_bounded(client, Duration::from_secs(5), "client");

    let (sout, serr, _) = wait_with_output_bounded(server, Duration::from_secs(5), "server -J");
    assert!(
        serr.trim().is_empty(),
        "JSON mode keeps stderr silent (iperf_err): {serr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).expect("server stdout is the JSON document");
    assert_eq!(
        doc["error"].as_str(),
        Some("the client has terminated"),
        "the doc carries IECLIENTTERM: {doc}"
    );
}

/// #225: a `-J` CLIENT whose server terminates mid-test emits EXACTLY ONE
/// document — the lib already rendered the partial report with the error key
/// before returning ServerTerminated, and the CLI's generic error path must
/// not append a second `error_document`. Two concatenated docs break every
/// JSON consumer; iperf3 emits one (stderr empty, error rides the doc).
#[test]
fn server_sigterm_json_client_emits_exactly_one_doc() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "10", "-i", "1", "-J"]);
    std::thread::sleep(Duration::from_secs(2));

    let spid = server.0.id() as i32;
    unsafe {
        libc::kill(spid, libc::SIGTERM);
    }
    let _ = wait_with_output_bounded(server, Duration::from_secs(5), "server");

    let (cout, cerr, ccode) = wait_with_output_bounded(client, Duration::from_secs(8), "client -J");
    assert_eq!(ccode, 1, "terminate-by-peer is the error path");
    assert!(
        cerr.trim().is_empty(),
        "-J keeps stderr empty (the error rides the doc): {cerr:?}"
    );
    let doc: serde_json::Value = serde_json::from_str(cout.trim())
        .unwrap_or_else(|e| panic!("client -J stdout must be EXACTLY one document ({e}): {cout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some("the server has terminated"),
        "the single doc carries IESERVERTERM: {doc}"
    );
    assert!(
        doc["intervals"].as_array().is_some_and(|a| !a.is_empty()),
        "the doc is the lib's PARTIAL report (data ran ~2 s), not the CLI's \
         empty error doc: {doc}"
    );
}

/// #225, stream flavor: a `--json-stream` client on server-terminate emits
/// exactly one error event and one end event — not the lib's pair plus the
/// CLI's `error_stream_events` pair.
#[test]
fn server_sigterm_json_stream_client_single_error_end_pair() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&[
        "-c",
        "127.0.0.1",
        "-p",
        &ps,
        "-t",
        "10",
        "-i",
        "1",
        "--json-stream",
    ]);
    std::thread::sleep(Duration::from_secs(2));

    let spid = server.0.id() as i32;
    unsafe {
        libc::kill(spid, libc::SIGTERM);
    }
    let _ = wait_with_output_bounded(server, Duration::from_secs(5), "server");

    let (cout, cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(8), "client --json-stream");
    assert_eq!(ccode, 1);
    assert!(cerr.trim().is_empty(), "stderr silent: {cerr:?}");
    let errors = cout.matches("{\"event\":\"error\"").count();
    let ends = cout.matches("{\"event\":\"end\"").count();
    assert_eq!(
        (errors, ends),
        (1, 1),
        "exactly one error+end pair (lib renders, CLI must not re-render):\n{cout}"
    );
}

/// #210 review r2 d: the server's json-stream emits the discrete `error`
/// event BEFORE `end` on terminate (iperf_json_finish is role-agnostic);
/// stderr stays empty.
#[test]
fn server_json_stream_emits_the_error_event_before_end() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps, "--json-stream"]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "10"]);
    std::thread::sleep(Duration::from_secs(2));

    let cpid = client.0.id() as i32;
    unsafe {
        libc::kill(cpid, libc::SIGTERM);
    }
    let _ = wait_with_output_bounded(client, Duration::from_secs(5), "client");

    let (sout, serr, _) =
        wait_with_output_bounded(server, Duration::from_secs(5), "server json-stream");
    assert!(serr.trim().is_empty(), "stderr silent: {serr:?}");
    let err_pos = sout
        .find("{\"event\":\"error\",\"data\":\"the client has terminated\"}")
        .expect("the error event must be emitted");
    let end_pos = sout.find("{\"event\":\"end\"").expect("end event");
    assert!(
        err_pos < end_pos,
        "error precedes end, like iperf_json_finish:\n{sout}"
    );
}
