//! #296: wire-shape pins for behavior the end-to-end suites can't see.
//!
//! The client's final `IperfDone` state byte is indistinguishable from a
//! fast close to a real server (its end loop tolerates a vanished client
//! by design), so only a mock asserting the byte BEFORE the socket closes
//! can pin it — GT's client always sends it (iperf_client_api.c, the
//! DISPLAY_RESULTS → IPERF_DONE transition).

use std::io::{Read, Write};
use std::time::{Duration, Instant};

mod common;

/// The mock speaks the full protocol through the results exchange, then
/// asserts the next control byte is IperfDone(16) — not EOF.
#[test]
fn client_sends_iperf_done_after_the_results_exchange() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();

    let mock = std::thread::spawn(move || -> u8 {
        let read_exact = |s: &mut std::net::TcpStream, n: usize| -> Vec<u8> {
            let mut b = vec![0u8; n];
            s.read_exact(&mut b).expect("mock read");
            b
        };
        let read_json = |s: &mut std::net::TcpStream| {
            let len = u32::from_be_bytes(read_exact(s, 4).try_into().unwrap()) as usize;
            read_exact(s, len)
        };
        let write_json = |s: &mut std::net::TcpStream, payload: &str| {
            s.write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
            s.write_all(payload.as_bytes()).unwrap();
        };

        let (mut ctrl, _) = listener.accept().expect("ctrl accept");
        read_exact(&mut ctrl, 37); // cookie
        ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
        read_json(&mut ctrl); // params
        ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
        let (mut data, _) = listener.accept().expect("data accept");
        read_exact(&mut data, 37); // data-stream cookie
        ctrl.write_all(&[1u8]).unwrap(); // TestStart
        ctrl.write_all(&[2u8]).unwrap(); // TestRunning
        let drain = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            while data.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
        });
        assert_eq!(read_exact(&mut ctrl, 1)[0], 4, "TestEnd");
        ctrl.write_all(&[13u8]).unwrap(); // ExchangeResults
        read_json(&mut ctrl); // the client's results
        write_json(
            &mut ctrl,
            r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":102400,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
        );
        ctrl.write_all(&[14u8]).unwrap(); // DisplayResults
                                          // The pin: the next control byte is IperfDone(16), not EOF.
        let done = read_exact(&mut ctrl, 1)[0];
        let _ = drain.join();
        done
    });

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &port, "-n", "100K", "-J"],
        Duration::from_secs(15),
        "iperf-done client",
    );
    assert_eq!(client.status.code(), Some(0), "{}", client.stderr);

    let deadline = Instant::now() + Duration::from_secs(5);
    let done_byte = loop {
        if mock.is_finished() {
            break mock.join().expect("mock");
        }
        assert!(Instant::now() < deadline, "mock never saw the final byte");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        done_byte, 16,
        "the client's last control byte is IperfDone(16), like GT"
    );
}

/// Mock-side wire primitives shared by the #325 scenarios.
fn read_exact(s: &mut std::net::TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).expect("mock read");
    b
}
fn read_json_blob(s: &mut std::net::TcpStream) -> Vec<u8> {
    let len = u32::from_be_bytes(read_exact(s, 4).try_into().unwrap()) as usize;
    read_exact(s, len)
}
fn write_json_blob(s: &mut std::net::TcpStream, payload: &str) {
    s.write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
    s.write_all(payload.as_bytes()).unwrap();
}

const MOCK_PARAMS: &str = r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#;
const MOCK_RESULTS: &str = r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":4096,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#;

/// Drive one mock-client round against a riperf3 server on `port`.
/// `junk_mid_test`: send `final_byte` in place of TestEnd(4) — the data
/// phase (#325 r2 F1). Otherwise the round runs through DisplayResults and
/// sends `final_byte` in place of IperfDone(16) — the end loop.
fn drive_mock_round(port: u16, final_byte: u8, junk_mid_test: bool) {
    drive_mock_round_full(port, Some(final_byte), junk_mid_test, MOCK_PARAMS);
}

/// `final_action`: Some(byte) sends the byte; None closes both sockets —
/// the abrupt-EOF cells (#330).
fn drive_mock_round_full(port: u16, final_action: Option<u8>, mid_test: bool, params: &str) {
    let cookie = [b'x'; 37];
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(&cookie).unwrap();
    assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
    write_json_blob(&mut ctrl, params);
    assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
    let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
    data.write_all(&cookie).unwrap();
    assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
    assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
    data.write_all(&[0u8; 4096]).unwrap();
    if !mid_test {
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
        write_json_blob(&mut ctrl, MOCK_RESULTS);
        read_json_blob(&mut ctrl); // server results
        assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
    }
    match final_action {
        Some(b) => ctrl.write_all(&[b]).unwrap(),
        None => drop((ctrl, data)), // abrupt EOF, both sockets (#330)
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
}

/// One scenario run against a one-off server: spawn, drive one mock round,
/// return (stdout, stderr, exit status).
fn run_scenario(
    json: bool,
    final_byte: u8,
    junk_mid_test: bool,
) -> (String, String, std::process::ExitStatus) {
    run_scenario_params(json, final_byte, junk_mid_test, MOCK_PARAMS)
}

fn run_scenario_params(
    json: bool,
    final_byte: u8,
    junk_mid_test: bool,
    params: &'static str,
) -> (String, String, std::process::ExitStatus) {
    run_scenario_full(json, Some(final_byte), junk_mid_test, params)
}

fn run_scenario_full(
    json: bool,
    final_action: Option<u8>,
    mid_test: bool,
    params: &'static str,
) -> (String, String, std::process::ExitStatus) {
    drive_server_scenario(json, move |port| {
        drive_mock_round_full(port, final_action, mid_test, params)
    })
}

/// Spawn a one-off (`-s -1`) server, run an arbitrary `mock` against it on the
/// bound port, and capture `(stdout, stderr, exit status)`. The shared spawn
/// body for [`run_scenario_full`] and the #330 pre-test-error scenarios (which
/// drive a mock that never reaches a real test).
fn drive_server_scenario(
    json: bool,
    mock: impl FnOnce(u16) + Send + 'static,
) -> (String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut args = vec!["-s", "-1", "-p", &port_s];
    if json {
        args.push("-J");
    }
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let sout_reader =
        riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || mock(port));
    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(5))
            .expect("server exits");
    (
        sout_reader.join().expect("stdout"),
        serr_reader.join().expect("stderr"),
        status,
    )
}

fn run_end_loop_scenario(json: bool, final_byte: u8) -> (String, String, std::process::ExitStatus) {
    run_scenario(json, final_byte, false)
}

const IEMESSAGE: &str =
    "received an unknown control message (ensure other side is iperf3 and not iperf)";
const SUMMARY_SEPARATOR: &str = "- - - - - - - - - - - - - - - - - - - - - - - - -";

/// #325: GT honors CLIENT_TERMINATE at any message point — a terminate
/// landing in the END loop (after DisplayResults) still dumps the
/// client-terminated shape (iperf_server_api.c:289-308), where the old
/// tolerant arm swallowed it and reported clean success.
#[test]
fn end_loop_client_terminate_takes_the_terminated_shape() {
    let (sout, serr, status) = run_end_loop_scenario(true, 12);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some("the client has terminated"),
        "the end-loop terminate dumps GT's IECLIENTTERM shape: {doc}"
    );
    assert!(
        serr.trim().is_empty(),
        "-J suppresses the stderr line like iperf_err's sink rule: {serr}"
    );
    assert!(
        status.success(),
        "CLIENT_TERMINATE sets IPERF_DONE and ends cleanly like GT"
    );
}

/// #325 r2 F2: the text half of the end-loop terminate. RECORDED DEVIATION:
/// GT prints the summary block TWICE (its TEST_END arm ran the reporter and
/// the terminate arm re-runs it under DISPLAY_RESULTS); riperf3 prints ONE
/// dump plus GT's bare terminate line.
#[test]
fn end_loop_client_terminate_prints_one_dump_and_the_line_in_text() {
    let (sout, serr, status) = run_end_loop_scenario(false, 12);
    assert_eq!(
        serr.trim(),
        "riperf3: the client has terminated",
        "GT's IECLIENTTERM line, no `error - ` prefix"
    );
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        1,
        "one summary dump (recorded deviation: GT double-dumps): {sout}"
    );
    assert!(status.success(), "exit 0 like GT");
}

/// #325 r1 F1/F2: an UNKNOWN control byte in the end loop is GT's IEMESSAGE
/// (iperf_server_api.c:309-311). Text mode prints main.c:174's line — the
/// bare sentence behind `error - `, EXACTLY once (no "protocol violation"
/// wrapper, no CLI double print) — after the summary block (the exchange
/// completed), and the one-off still exits 0: a failed test is rc -1, which
/// GT's main errexits on only below -1 (live-verified against GT 3.21).
#[test]
fn end_loop_unknown_byte_prints_gt_iemessage_once_in_text() {
    let (sout, serr, status) = run_end_loop_scenario(false, 99);
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "GT prints the IEMESSAGE line once, unwrapped"
    );
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        1,
        "the failed round still prints its summary (r2 F5): {sout}"
    );
    assert!(
        status.success(),
        "GT's one-off exits 0 on a failed test (main.c `rc < -1` only)"
    );
}

/// #325 r1 F4: under -J the end-loop IEMESSAGE run still emits the FULL
/// accumulated doc — populated `end` plus the `error - `-prefixed sentence
/// (main.c:174 through iperf_err's json sink) — with stderr silent, exit 0.
#[test]
fn end_loop_unknown_byte_takes_gt_iemessage_shape_in_json() {
    let (sout, serr, status) = run_end_loop_scenario(true, 99);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {IEMESSAGE}").as_str()),
        "GT's in-doc IEMESSAGE carries the `error - ` prefix: {doc}"
    );
    assert!(
        doc["end"]["streams"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the exchange completed — GT dumps the populated end block: {doc}"
    );
    assert!(
        serr.trim().is_empty(),
        "-J suppresses the stderr line like iperf_err's sink rule: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #325 r1 F3: GT's end-loop switch has arms for only TEST_START /
/// TEST_END / IPERF_DONE / CLIENT_TERMINATE — a KNOWN state landing here
/// (TEST_RUNNING=2, live-verified against GT 3.21) hits the same IEMESSAGE
/// default, where riperf3's old #145 arm tolerated it as clean success.
#[test]
fn end_loop_known_stray_state_is_iemessage_like_gt() {
    let (_sout, serr, status) = run_end_loop_scenario(false, 2);
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "a known-but-unhandled state takes GT's IEMESSAGE default"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #325 r2 F1: an unmapped byte DURING the data phase (in place of TestEnd)
/// is the same IEMESSAGE default, but GT's end processing never ran — the
/// -J doc keeps the accumulated intervals with a BARE `end: {}` (live-
/// verified skeleton), stderr silent, exit 0. Before this fix the run
/// produced no document at all.
#[test]
fn mid_test_unknown_byte_emits_the_accumulated_doc_in_json() {
    let (sout, serr, status) = run_scenario(true, 99, true);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {IEMESSAGE}").as_str()),
        "the doc carries GT's prefixed IEMESSAGE: {doc}"
    );
    assert!(
        doc["end"].as_object().is_some_and(|o| o.is_empty()),
        "the final stats dump never ran — GT's end is bare {{}}: {doc}"
    );
    assert!(
        serr.trim().is_empty(),
        "-J suppresses the stderr line like iperf_err's sink rule: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #325 r2 F1, text half: the mid-test IEMESSAGE prints GT's single stderr
/// line and NO summary block (GT's reporter never ran), exit 0.
#[test]
fn mid_test_unknown_byte_prints_the_line_and_no_summary_in_text() {
    let (sout, serr, status) = run_scenario(false, 99, true);
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "GT's single IEMESSAGE line"
    );
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        0,
        "no summary block mid-test (GT's reporter never ran): {sout}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #325 r2 F5: a PERSISTENT server prints the IEMESSAGE line once per
/// failed round and keeps serving — GT live-verified two junk rounds with
/// its banner renumbering between them.
#[test]
fn persistent_server_keeps_serving_after_iemessage() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    // Two full rounds, each ending in an end-loop junk byte. The second
    // round completing its handshake IS the keeps-serving proof.
    for _ in 0..2 {
        drive_mock_round(port, 99, false);
    }
    // Grace for the second round's finalize + print before the kill —
    // the file's 5 s bound convention (r3 F5: 1 s was the tightest margin
    // in the file and the likeliest 2-core-runner flake).
    std::thread::sleep(std::time::Duration::from_secs(5));
    server.0.kill().expect("kill persistent server");
    let _ = server.0.wait();
    let serr = serr_reader.join().expect("stderr");
    let line = format!("riperf3: error - {IEMESSAGE}");
    assert_eq!(
        serr.lines().filter(|l| *l == line).count(),
        2,
        "one IEMESSAGE line per failed round: {serr}"
    );
}

const MOCK_PARAMS_GSO: &str = r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"get_server_output":1,"client_version":"riperf3 0.0.0"}"#;

/// #325 r3 F2: --get-server-output's text capture must not render the
/// summary block on the mid-test IEMESSAGE path — GT prints only the
/// stderr line there (its reporter is dead; live-verified with
/// get_server_output: 1 in params).
#[test]
fn mid_test_unknown_byte_with_get_server_output_prints_no_summary() {
    let (sout, serr, status) = run_scenario_params(false, 99, true, MOCK_PARAMS_GSO);
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "GT's single IEMESSAGE line"
    );
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        0,
        "the capture render must not resurrect the summary (r3 F2): {sout}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// Holding-peer harness (#325 r3 F1 / r4 NF-1): drive the protocol, send
/// `final_byte` (mid-test or end-loop), then HOLD both sockets far past
/// the assertion window. Returns the server's stderr and exit status —
/// which must arrive while the peer still holds. Detached mock thread;
/// the sockets close when the test binary exits.
fn run_holding_scenario(final_byte: u8, junk_mid_test: bool) -> (String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        if !junk_mid_test {
            ctrl.write_all(&[4u8]).unwrap(); // TestEnd
            assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
            write_json_blob(&mut ctrl, MOCK_RESULTS);
            read_json_blob(&mut ctrl); // server results
            assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
        }
        ctrl.write_all(&[final_byte]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop((ctrl, data));
    });

    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits while the peer still holds");
    (serr_reader.join().expect("stderr"), status)
}

/// #325 r3 F1: a hostile peer that sends the junk byte and then HOLDS its
/// sockets open must not park the server — GT cleanup_servers immediately
/// when handle_message fails (iperf_server_api.c:764-767). Pre-fix the
/// stream-task joins waited out the peer's whole hold (live: a 30 s hold
/// held the one-off 33 s and blocked the stderr line with it).
#[test]
fn mid_test_unknown_byte_exits_bounded_while_peer_holds() {
    let (serr, status) = run_holding_scenario(99, true);
    assert!(status.success(), "one-off exits 0 like GT");
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "the line prints NOW, not after the peer relents"
    );
}

/// #325 r4 NF-1: the same hold one byte over — CLIENT_TERMINATE in the end
/// loop. GT's terminate arm closes the stream sockets INLINE
/// (iperf_server_api.c:301-305) and exits on its own clock; the joins must
/// not wait out the peer's hold.
#[test]
fn end_loop_client_terminate_exits_bounded_while_peer_holds() {
    let (serr, status) = run_holding_scenario(12, false);
    assert!(status.success(), "terminate ends cleanly like GT");
    assert_eq!(
        serr.trim(),
        "riperf3: the client has terminated",
        "GT's bare IECLIENTTERM line, printed while the peer still holds"
    );
}

/// #325 r5 nit: the mid-test terminate hold, pinned explicitly (the gates
/// are shared with the two cells above, but a pin per cell keeps a silent
/// regression impossible).
#[test]
fn mid_test_client_terminate_exits_bounded_while_peer_holds() {
    let (serr, status) = run_holding_scenario(12, true);
    assert!(status.success(), "terminate ends cleanly like GT");
    assert_eq!(
        serr.trim(),
        "riperf3: the client has terminated",
        "GT's bare IECLIENTTERM line, printed while the peer still holds"
    );
}

const CTRL_CLOSED: &str = "the client has unexpectedly closed the connection";

/// #330: an abrupt EOF DURING the data phase is GT's IECTRLCLOSE read-site
/// surface (iperf_server_api.c:249-254, live-probed): the doc carries the
/// BARE sentence (direct iperf_err — no `error - ` prefix) over the
/// accumulated intervals + bare end{}, stderr silent under -J, clean exit.
#[test]
fn mid_test_eof_takes_gt_ctrl_closed_shape_in_json() {
    let (sout, serr, status) = run_scenario_full(true, None, true, MOCK_PARAMS);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(CTRL_CLOSED),
        "GT's bare read-site sentence: {doc}"
    );
    assert!(
        doc["end"].as_object().is_some_and(|o| o.is_empty()),
        "the final stats dump never ran — GT's end is bare {{}}: {doc}"
    );
    assert!(serr.trim().is_empty(), "-J stderr silent: {serr}");
    assert!(status.success(), "GT sets IPERF_DONE — clean exit");
}

/// #330, text half: the single line, no summary block, exit 0
/// (live-probed).
#[test]
fn mid_test_eof_prints_the_line_and_no_summary_in_text() {
    let (sout, serr, status) = run_scenario_full(false, None, true, MOCK_PARAMS);
    assert_eq!(serr.trim(), format!("riperf3: {CTRL_CLOSED}"));
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        0,
        "no summary mid-test: {sout}"
    );
    assert!(status.success(), "clean exit like GT");
}

/// #330: EOF in the END loop — the fast-close-instead-of-IperfDone cell.
/// GT prints the same sentence with the POPULATED end (its reporter ran at
/// TEST_END; live-probed doc has sum_sent/sum_received/streams), where the
/// old arm broke silently clean with no error key at all.
#[test]
fn end_loop_eof_takes_gt_ctrl_closed_shape_in_json() {
    let (sout, serr, status) = run_scenario_full(true, None, false, MOCK_PARAMS);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(CTRL_CLOSED),
        "GT's bare sentence rides the completed doc: {doc}"
    );
    assert!(
        doc["end"]["streams"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the exchange completed — end stays populated: {doc}"
    );
    assert!(serr.trim().is_empty(), "-J stderr silent: {serr}");
    assert!(status.success(), "clean exit like GT");
}

/// #330, text half of the end-loop EOF: one summary dump + the line.
#[test]
fn end_loop_eof_prints_summary_and_the_line_in_text() {
    let (sout, serr, status) = run_scenario_full(false, None, false, MOCK_PARAMS);
    assert_eq!(serr.trim(), format!("riperf3: {CTRL_CLOSED}"));
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        1,
        "the completed round prints its summary: {sout}"
    );
    assert!(status.success(), "clean exit like GT");
}

/// #330: a KNOWN state (ParamExchange=9) landing mid-test is GT's same
/// IEMESSAGE default — its ONE message switch serves every phase
/// (live-probed: byte 9 mid-test = the prefixed doc key + exit 0). The old
/// #145 arm tolerated it as clean success.
#[test]
fn mid_test_known_stray_state_is_iemessage_like_gt() {
    let (_sout, serr, status) = run_scenario_full(false, Some(9), true, MOCK_PARAMS);
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "GT's IEMESSAGE default covers known strays mid-test"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #330: TEST_START (1) mid-test stays GT's no-op arm
/// (iperf_server_api.c:266-267) — the round completes clean after it.
#[test]
fn mid_test_test_start_byte_is_a_gt_noop() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[1u8]).unwrap(); // stray TEST_START: GT no-op
        ctrl.write_all(&[4u8]).unwrap(); // then the real TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
        write_json_blob(&mut ctrl, MOCK_RESULTS);
        read_json_blob(&mut ctrl);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 14);
        ctrl.write_all(&[16u8]).unwrap(); // IperfDone — clean round
        std::thread::sleep(std::time::Duration::from_millis(300));
    });

    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(5))
            .expect("server exits");
    let serr = serr_reader.join().expect("stderr");
    assert!(status.success(), "clean round");
    assert!(
        serr.trim().is_empty(),
        "no error surface for GT's no-op arm: {serr}"
    );
}

/// #330 r1 F1: a mid-test IPERF_DONE is GT's explicit CLEAN arm
/// (iperf_server_api.c:287-288 + the byte lands in test->state, exiting
/// its run loop): NO error key, NO stderr, bare end{}, exit 0
/// (live-probed) — not the IEMESSAGE default.
#[test]
fn mid_test_iperf_done_ends_clean_and_bare_in_json() {
    let (sout, serr, status) = run_scenario_full(true, Some(16), true, MOCK_PARAMS);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert!(
        doc["error"].is_null(),
        "GT's IPERF_DONE arm carries no error: {doc}"
    );
    assert!(
        doc["end"].as_object().is_some_and(|o| o.is_empty()),
        "the final stats dump never ran — bare end{{}}: {doc}"
    );
    assert!(serr.trim().is_empty(), "no stderr surface: {serr}");
    assert!(status.success(), "clean exit like GT");
}

/// #330 r1 F1, text half: nothing on stderr, no summary, exit 0.
#[test]
fn mid_test_iperf_done_ends_clean_in_text() {
    let (sout, serr, status) = run_scenario_full(false, Some(16), true, MOCK_PARAMS);
    assert!(serr.trim().is_empty(), "no stderr surface: {serr}");
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        0,
        "no summary mid-test: {sout}"
    );
    assert!(status.success(), "clean exit like GT");
}

/// #330 r1 F6: the -J twin of the mid-test known-stray cell — the
/// prefixed doc key over the bare end (live-probed byte 9 ≡ byte 99).
#[test]
fn mid_test_known_stray_state_takes_iemessage_shape_in_json() {
    let (sout, serr, status) = run_scenario_full(true, Some(9), true, MOCK_PARAMS);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {IEMESSAGE}").as_str()),
        "the prefixed IEMESSAGE key: {doc}"
    );
    assert!(
        doc["end"].as_object().is_some_and(|o| o.is_empty()),
        "bare end mid-test: {doc}"
    );
    assert!(serr.trim().is_empty(), "-J stderr silent: {serr}");
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #330 r1 F5: ctrl EOF while the peer HOLDS the data socket — the
/// abort-gate cell the both-sockets-close pins can't discriminate (data
/// tasks EOF on their own there). GT's cleanup closes the data sockets
/// and exits on its own clock.
#[test]
fn mid_test_ctrl_eof_with_data_held_exits_bounded() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    // Close ONLY the control socket; the data socket stays held far past
    // the assertion window (detached thread; closes at process exit).
    std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        drop(ctrl);
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(data);
    });

    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits while the peer still holds the data socket");
    assert!(status.success(), "clean exit like GT");
    let serr = serr_reader.join().expect("stderr");
    assert_eq!(
        serr.trim(),
        format!("riperf3: {CTRL_CLOSED}"),
        "the line prints NOW, not after the peer relents"
    );
}

/// #330 exchange-phase cells: full protocol through TestEnd; after the
/// server's ExchangeResults(13), `blob`: None = EOF before the results,
/// Some((promised_len, partial)) = a short blob then EOF.
fn run_exchange_fail_scenario(
    json: bool,
    blob: Option<(u32, &'static [u8])>,
) -> (String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut args = vec!["-s", "-1", "-p", &port_s];
    if json {
        args.push("-J");
    }
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let sout_reader =
        riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13); // ExchangeResults
        if let Some((promised, partial)) = blob {
            ctrl.write_all(&promised.to_be_bytes()).unwrap();
            ctrl.write_all(partial).unwrap();
        }
        drop((ctrl, data)); // EOF where the results were due
        std::thread::sleep(std::time::Duration::from_millis(300));
    });

    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits");
    (
        sout_reader.join().expect("stdout"),
        serr_reader.join().expect("stderr"),
        status,
    )
}

const RECV_RESULTS_ERR: &str = "error - unable to receive results: ";

/// #330: EOF where the client's results were due — GT's IERECVRESULTS
/// (live-probed): the Nread_json warning on stderr EVEN under -J (GT's
/// warning() bypasses every sink), the doc's error key in the #248
/// dangling-`: ` errno-0 perr form (RECORDED DEVIATION: GT appends a
/// STALE errno's strerror), POPULATED end, exit 0.
#[test]
fn exchange_eof_takes_gt_recv_results_shape_in_json() {
    let (sout, serr, status) = run_exchange_fail_scenario(true, None);
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(RECV_RESULTS_ERR),
        "the IERECVRESULTS key in the errno-0 perr form: {doc}"
    );
    assert!(
        doc["end"]["streams"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "TEST_END processing ran — populated end: {doc}"
    );
    assert_eq!(
        serr.trim(),
        "warning: Failed to read JSON data size - read returned 0; errno=0",
        "GT's read-site warning, sink-bypassing, and nothing else: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #330, text half: the warning, the error line, and the summary.
#[test]
fn exchange_eof_prints_warning_line_and_summary_in_text() {
    let (sout, serr, status) = run_exchange_fail_scenario(false, None);
    // No whole-string trim: the error line's dangling `: ` IS the pin.
    let lines: Vec<&str> = serr.lines().collect();
    assert_eq!(
        lines,
        vec![
            "warning: Failed to read JSON data size - read returned 0; errno=0",
            &format!("riperf3: {RECV_RESULTS_ERR}") as &str,
        ],
        "warning then the error line, once each: {serr}"
    );
    assert_eq!(
        sout.matches(SUMMARY_SEPARATOR).count(),
        1,
        "the completed round prints its summary: {sout}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #336 r1 F3: a peer that half-sends the SIZE and then HOLDS the socket
/// — GT's Nrecv self-recovers via its 10 s idle / 30 s overall read
/// bounds (net.c:75-76) and warns with the partial count; the exchange
/// must not park forever.
#[test]
fn exchange_half_size_then_hold_exits_bounded() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13); // ExchangeResults
        ctrl.write_all(&[0u8, 0u8]).unwrap(); // 2 of 4 size bytes, then HOLD
        std::thread::sleep(std::time::Duration::from_secs(40));
        drop((ctrl, data));
    });

    // The 10 s idle bound fires; the 20 s assert window leaves slack for a
    // loaded 2-core CI runner (r2 finding 7).
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(20))
            .expect("server exits on GT's read bound while the peer holds");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    assert!(
        serr.contains("warning: Failed to read JSON data size - read returned 2; errno=0"),
        "the timed-out read warns with the partial count: {serr}"
    );
}

/// #330: a short results blob then EOF — GT's expected/received warning
/// verbatim (this cell sends 4 blob bytes: "expected 500 bytes but
/// received 4; errno=0").
#[test]
fn exchange_short_blob_prints_gt_length_warning() {
    let (_sout, serr, status) = run_exchange_fail_scenario(false, Some((500, b"{\"cp")));
    assert!(
        serr.contains(
            "warning: JSON size of data read does not correspond to offered length - \
             expected 500 bytes but received 4; errno=0"
        ),
        "GT's short-blob warning with the real counts: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// Make the drop of this socket send a real RST. A plain close after the
/// peer has drained everything sends FIN (a clean EOF); SO_LINGER(0) forces
/// the abortive close so the server's in-flight read returns ECONNRESET,
/// not Ok(0). Mirrors setup_retry.rs. (std has no stable `set_linger`.)
#[cfg(unix)]
fn force_rst_on_drop(sock: &std::net::TcpStream) {
    use std::os::fd::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::from_ref(&linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "SO_LINGER setsockopt failed");
}
// (No non-unix stub: the only callers are the two RST pins below, both
// #[cfg(unix)] — forcing an RST needs SO_LINGER(0), which is unix-only.)

/// #336 r1 F1: a HARD read error mid-blob (an RST arriving after the SIZE
/// was read and the server is blocked on the blob) takes GT's rc<0 arm
/// "JSON data read failed; errno={e}" (iperf_api.c:3061) — NOT the
/// expected/received short-read arm (that one is for a clean partial+EOF,
/// where GT's Nread returned rc>=0). The mock promises 500 bytes, lets the
/// server consume the size and block, then RST-closes.
///
/// Unix-only (r3 finding 1): the mock leaves NO unread data before it drops
/// (the server already consumed the size), so the cross-platform
/// unread-data→RST path doesn't apply — a real RST needs SO_LINGER(0). The
/// warning arm itself is platform-independent Rust, exercised on every unix
/// CI target (Linux/macOS/*BSD); Windows would send a clean FIN and hit the
/// EOF arm instead.
#[cfg(unix)]
#[test]
fn exchange_blob_rst_takes_gt_read_failed_arm() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13); // ExchangeResults
        ctrl.write_all(&500u32.to_be_bytes()).unwrap(); // promise 500, send none
                                                        // Let the server read the size and block in the blob read, THEN abort.
        std::thread::sleep(std::time::Duration::from_millis(500));
        force_rst_on_drop(&ctrl);
        drop((ctrl, data));
    });

    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(10))
            .expect("server exits on the reset");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    assert!(
        serr.contains("warning: JSON data read failed; errno="),
        "a hard read error takes GT's rc<0 arm: {serr}"
    );
    assert!(
        !serr.contains("does not correspond to offered length"),
        "must NOT fall through to the expected/received short-read arm: {serr}"
    );
}

/// #336 r1 F4: a zero JSON size fails GT's hsize>0 gate
/// (iperf_api.c:3038/:3068) and warns the overflow line verbatim.
#[test]
fn exchange_zero_size_takes_gt_overflow_warning() {
    let (_sout, serr, status) = run_exchange_fail_scenario(false, Some((0, b"")));
    assert!(
        serr.contains("warning: JSON data length overflow - 0 bytes JSON size is not allowed"),
        "a zero JSON size warns GT's overflow line: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #330: a HARD read error during the SIZE read (an RST before the 4-byte
/// length arrives) takes GT's rc<0 size arm "read returned -2; errno={e}"
/// (GT's Nrecv returns NET_HARDERROR=-2, echoed raw; r2 finding 1) — the
/// size-stage twin of the blob rc<0 arm above. Unix-only for the same
/// reason (r3 finding 1): the SO_LINGER(0) RST is unix-only.
#[cfg(unix)]
#[test]
fn exchange_size_rst_takes_gt_read_failed_size_arm() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        write_json_blob(&mut ctrl, MOCK_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13); // ExchangeResults
                                                     // No size bytes at all — abort before the length arrives.
        force_rst_on_drop(&ctrl);
        drop((ctrl, data));
    });

    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(10))
            .expect("server exits on the reset");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    assert!(
        serr.contains("warning: Failed to read JSON data size - read returned -2; errno="),
        "an RST at the size read takes GT's rc<0 size arm: {serr}"
    );
}

/// #325 r2 F6: the CLIENT side of the same default — GT's client message
/// handler IEMESSAGEs an unmapped byte too (iperf_client_api.c:409-411).
/// The byte replaces ExchangeResults(13) at the client's direct state wait
/// (mid-TEST_RUNNING reads keep their pre-recorded Err→Closed deviation).
/// Text: the bare-sentence line behind `error - `, exit 1.
#[test]
fn client_state_wait_unknown_byte_takes_gt_iemessage() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();

    let mock = std::thread::spawn(move || {
        let (mut ctrl, _) = listener.accept().expect("ctrl accept");
        read_exact(&mut ctrl, 37); // cookie
        ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
        read_json_blob(&mut ctrl); // params
        ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
        let (mut data, _) = listener.accept().expect("data accept");
        read_exact(&mut data, 37); // data-stream cookie
        ctrl.write_all(&[1u8]).unwrap(); // TestStart
        ctrl.write_all(&[2u8]).unwrap(); // TestRunning
        let drain = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            while data.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
        });
        assert_eq!(read_exact(&mut ctrl, 1)[0], 4, "client's TestEnd");
        // The unknown byte in place of ExchangeResults(13).
        ctrl.write_all(&[99u8]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = drain.join();
    });

    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &port, "-n", "100K"],
        Duration::from_secs(15),
        "client iemessage",
    );
    mock.join().expect("mock");
    assert_eq!(
        client.stderr.trim(),
        format!("riperf3: error - {IEMESSAGE}"),
        "GT's client IEMESSAGE line"
    );
    assert_eq!(client.status.code(), Some(1), "GT's client errexits 1");
}

// ---------------------------------------------------------------------------
// #330: the pre-test SERVER generic-error surface (cookie + params).
//
// GT 3.21 live-probed: a control-connection that fails the cookie read or the
// param read/parse errors out through iperf_err's json sink — silent stderr
// under -J with the message in a SKELETON accumulated doc, one stderr line in
// text — plus a best-effort SERVER_ERROR(-2) + (i_errno, errno) wire-back via
// cleanup_server. One-off servers keep exit 0. riperf3 previously surfaced the
// raw "early eof" / serde class on stderr with no -J doc at all.
// ---------------------------------------------------------------------------

/// IERECVCOOKIE(106) — iperf_error.c: "unable to receive cookie at server".
const RECV_COOKIE_MSG: &str = "unable to receive cookie at server";
/// IERECVPARAMS(114) — iperf_error.c: "unable to receive parameters from client".
const RECV_PARAMS_MSG: &str = "unable to receive parameters from client";

/// Cookie failure: connect, send a truncated cookie, then EOF. GT's
/// iperf_accept read fails -> IERECVCOOKIE.
fn drive_cookie_failure(port: u16) {
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(b"short").unwrap(); // < 37 cookie bytes, then close
    drop(ctrl);
    std::thread::sleep(std::time::Duration::from_millis(400));
}

/// Params failure: full cookie, read ParamExchange(9), then send a
/// length-prefixed blob that is not valid JSON. GT's get_parameters ->
/// JSON_read cJSON_Parse fails -> IERECVPARAMS.
fn drive_param_failure(port: u16) {
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(&[b'x'; 37]).unwrap();
    assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
    write_json_blob(&mut ctrl, "this is not json");
    std::thread::sleep(std::time::Duration::from_millis(400));
}

#[test]
fn cookie_failure_text_prints_the_ierecvcookie_line() {
    let (_sout, serr, status) = drive_server_scenario(false, drive_cookie_failure);
    // The #248 perr dangling ": " at errno 0 — GT's honest form (its own
    // stale-errno strerror is a recorded deviation, like #336).
    assert_eq!(
        serr,
        format!("riperf3: error - {RECV_COOKIE_MSG}: \n"),
        "{serr:?}"
    );
    assert!(
        status.success(),
        "one-off server exits 0 after a cookie failure"
    );
}

#[test]
fn cookie_failure_json_emits_the_skeleton_doc_silent_stderr() {
    let (sout, serr, status) = drive_server_scenario(true, drive_cookie_failure);
    assert!(
        serr.trim().is_empty(),
        "-J silences the error line: {serr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {RECV_COOKIE_MSG}: ").as_str()),
        "the -J error key carries GT's IERECVCOOKIE sentence: {doc}"
    );
    assert_eq!(
        doc["start"]["connected"].as_array().map(Vec::len),
        Some(0),
        "skeleton start.connected is empty"
    );
    assert!(doc["start"]["version"].is_string());
    assert!(doc["start"]["system_info"].is_string());
    assert!(doc["intervals"].as_array().expect("intervals").is_empty());
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}"
    );
    assert!(status.success());
}

#[test]
fn param_failure_text_prints_the_ierecvparams_line() {
    let (_sout, serr, status) = drive_server_scenario(false, drive_param_failure);
    assert_eq!(
        serr,
        format!("riperf3: error - {RECV_PARAMS_MSG}: \n"),
        "{serr:?}"
    );
    assert!(
        status.success(),
        "one-off server exits 0 after a param failure"
    );
}

#[test]
fn param_failure_json_emits_the_skeleton_doc_silent_stderr() {
    let (sout, serr, status) = drive_server_scenario(true, drive_param_failure);
    assert!(
        serr.trim().is_empty(),
        "-J silences the error line: {serr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {RECV_PARAMS_MSG}: ").as_str()),
        "the -J error key carries GT's IERECVPARAMS sentence: {doc}"
    );
    assert!(doc["intervals"].as_array().expect("intervals").is_empty());
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}"
    );
    assert!(status.success());
}

/// The wire-back: GT's cleanup_server sends SERVER_ERROR(-2), then
/// htonl(i_errno), then htonl(errno). riperf3 mirrors it with errno pinned to
/// 0 (honest, like #336). Observable only for the param case — a
/// cookie-failure peer has already closed.
#[test]
fn param_failure_wire_back_is_server_error_ierecvparams() {
    let (_sout, _serr, status) = drive_server_scenario(false, |port| {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, "this is not json");
        let back = read_exact(&mut ctrl, 9);
        assert_eq!(back[0], 0xfe, "SERVER_ERROR state (-2): {back:?}");
        assert_eq!(
            u32::from_be_bytes(back[1..5].try_into().unwrap()),
            114,
            "IERECVPARAMS i_errno"
        );
        assert_eq!(
            u32::from_be_bytes(back[5..9].try_into().unwrap()),
            0,
            "honest errno 0"
        );
    });
    assert!(status.success());
}
