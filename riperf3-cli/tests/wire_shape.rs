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

    let mock =
        std::thread::spawn(move || drive_mock_round_full(port, final_action, mid_test, params));
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
