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
    drive_server_scenario_with(&[], json, mock)
}

/// [`drive_server_scenario`] with extra server flags — the #344 timestamp
/// pins arm `--timestamps` on the same mock rounds.
fn drive_server_scenario_with(
    extra_args: &[&str],
    json: bool,
    mock: impl FnOnce(u16) + Send + 'static,
) -> (String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut args = vec!["-s", "-1", "-p", &port_s];
    args.extend_from_slice(extra_args);
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
    run_holding_scenario_params(final_byte, junk_mid_test, MOCK_PARAMS, 0)
}

/// `pre_end_delay_ms`: dwell in TEST_RUNNING before sending TestEnd. The
/// reverse (sender-park) cell needs the server's sender to FILL the
/// unread-peer buffers and park in write_all() BEFORE `done` is set — an
/// immediate TestEnd lets the sender exit via its done-check without ever
/// parking, and the pin can't discriminate (buffers fill in ~ms at loopback
/// speed; the dwell is ample).
fn run_holding_scenario_params(
    final_byte: u8,
    junk_mid_test: bool,
    params: &'static str,
    pre_end_delay_ms: u64,
) -> (String, std::process::ExitStatus) {
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
        write_json_blob(&mut ctrl, params);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        if !junk_mid_test {
            if pre_end_delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(pre_end_delay_ms));
            }
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

/// #331: the SUCCESS path — a COMPLETED round (IperfDone received, results
/// exchanged, doc emitted) against a peer that then HOLDS its sockets open.
/// GT closes every stream socket at TEST_END (iperf_server_api.c:272-275)
/// and its one-off exits on its own clock; riperf3's success-path joins
/// parked in the receivers' read() for the peer's whole hold (live: >6 s,
/// 3/3 on the pre-fix tree).
#[test]
fn clean_finish_exits_bounded_while_peer_holds() {
    let (serr, status) = run_holding_scenario(16, false);
    assert!(status.success(), "clean one-off exit 0");
    assert!(
        serr.trim().is_empty(),
        "a completed round prints nothing to stderr: {serr:?}"
    );
}

/// The reverse round's params — the server streams are SENDERS.
const MOCK_PARAMS_REVERSE: &str = r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"reverse":true,"client_version":"riperf3 0.0.0"}"#;

/// #331 (r2 F2): the SENDER arm — a REVERSE round where the peer completes
/// the protocol but never drains the data socket parks the server's sender
/// in write_all() against the full buffers (pre-fix: wedged the peer's whole
/// hold, live 12 s; post-fix: ms exit). The abort is direction-blind; this
/// pin keeps the sender arm covered explicitly.
#[test]
fn clean_finish_reverse_exits_bounded_while_peer_holds() {
    let (serr, status) = run_holding_scenario_params(16, false, MOCK_PARAMS_REVERSE, 500);
    assert!(status.success(), "clean reverse one-off exit 0");
    assert!(
        serr.trim().is_empty(),
        "a completed reverse round prints nothing to stderr: {serr:?}"
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

/// #341: GT prints the expected size through `%d` (iperf_api.c:3056 — the
/// uint32_t hsize two's-complement-wraps), so a hostile 0xFFFFFFF0 prefix
/// warns "expected -16", not the unsigned 4294967280.
#[test]
fn exchange_huge_size_warning_wraps_like_gt_percent_d() {
    let (_sout, serr, status) = run_exchange_fail_scenario(false, Some((0xFFFF_FFF0, b"")));
    assert!(
        serr.contains(
            "warning: JSON size of data read does not correspond to offered length - \
             expected -16 bytes but received 0; errno=0"
        ),
        "GT's %d wrap on the expected count: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #347 r2 F1: the chunk-loop bookkeeping (`take` cap + `extend ..n`) needs
/// discriminating coverage — a paced >64 KiB blob with coalesced trailing
/// junk. The mid-blob pause forces a partial chunk read (an `extend ..take`
/// mutant appends stale garbage); the junk written together with the tail
/// coalesces into the final read (a `take = chunk.len()` mutant over-reads
/// past the promised length). Correct code reads exactly the promised
/// length like GT (iperf_api.c:3044/:3053), completes the exchange, and the
/// junk then lands in the END LOOP as GT's IEMESSAGE — proving it was never
/// consumed by the blob read.
#[test]
fn exchange_multi_chunk_paced_blob_with_trailing_junk_completes() {
    let (_sout, serr, status) = drive_server_scenario(false, |port| {
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

        // A valid 100 000-byte results doc: MOCK_RESULTS plus JSON-legal
        // trailing spaces inside the promised length (spans 2 chunks).
        let mut blob = MOCK_RESULTS.as_bytes().to_vec();
        blob.resize(100_000, b' ');
        ctrl.write_all(&(blob.len() as u32).to_be_bytes()).unwrap();
        ctrl.write_all(&blob[..30_000]).unwrap();
        // Pause mid-blob: the server's in-flight chunk read returns partial.
        std::thread::sleep(std::time::Duration::from_millis(400));
        // Tail + junk in ONE write so they coalesce into the final read.
        let mut tail = blob[30_000..].to_vec();
        tail.extend_from_slice(&[b'J'; 512]);
        ctrl.write_all(&tail).unwrap();

        // The exchange completes: the server's results + DisplayResults.
        read_json_blob(&mut ctrl);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop((ctrl, data));
    });
    assert!(
        !serr.contains("warning:"),
        "the paced multi-chunk blob parses clean — no warning arm fires: {serr}"
    );
    assert!(
        serr.contains(IEMESSAGE),
        "the junk stays queued past the blob read and lands in the end loop \
         as GT's IEMESSAGE: {serr}"
    );
    assert!(status.success(), "one-off exits 0 like GT");
}

/// #340: a hostile 4 GiB size prefix must not COMMIT the memory. GT callocs
/// (lazy zero pages, ~4 MB RSS through the whole read window); riperf3's
/// try_reserve+resize memset every page in (~4.2 GB RSS, unauthenticated,
/// repeatable per round). The read loop must commit pages only as bytes
/// arrive. Linux-only: samples the child's VmRSS from /proc.
#[cfg(target_os = "linux")]
#[test]
fn exchange_huge_size_prefix_does_not_commit_rss() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    let pid = server.0.id();
    std::thread::sleep(std::time::Duration::from_millis(400));

    // The full dance to ExchangeResults, then the hostile prefix + a HOLD so
    // the blob read window stays open while RSS is sampled.
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
        ctrl.write_all(&0xFFFF_FFF0u32.to_be_bytes()).unwrap(); // hostile prefix
        std::thread::sleep(std::time::Duration::from_secs(3)); // hold the window
        drop((ctrl, data));
    });

    // Sample peak RSS while the server sits in the bounded blob read.
    let mut peak_kb = 0u64;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        if let Some(line) = status.lines().find(|l| l.starts_with("VmRSS:")) {
            let kb: u64 = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            peak_kb = peak_kb.max(kb);
        }
    }
    mock.join().expect("mock");
    let _ = riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(20));
    assert!(
        peak_kb < 262_144, // 256 MB — GT sits at ~4.5 MB, the defect at ~4.2 GB
        "hostile prefix committed {peak_kb} kB RSS — pages must commit only as bytes arrive"
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

/// r1 F2 / M4: the cookie-path SERVER_ERROR wire-back i_errno (106). The two
/// cookie tests above close before reading it; here the mock half-closes its
/// write side (a FIN so the server's cookie read EOFs) but keeps the read side
/// open, so it can observe the wire-back bytes.
#[test]
fn cookie_failure_wire_back_is_server_error_ierecvcookie() {
    let (_sout, _serr, status) = drive_server_scenario(false, |port| {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        ctrl.write_all(b"short").unwrap(); // < 37 cookie bytes
        ctrl.shutdown(std::net::Shutdown::Write).unwrap(); // FIN, keep read open
        let back = read_exact(&mut ctrl, 9);
        assert_eq!(back[0], 0xfe, "SERVER_ERROR state (-2): {back:?}");
        assert_eq!(
            u32::from_be_bytes(back[1..5].try_into().unwrap()),
            106,
            "IERECVCOOKIE i_errno"
        );
        assert_eq!(
            u32::from_be_bytes(back[5..9].try_into().unwrap()),
            0,
            "honest errno 0"
        );
    });
    assert!(status.success());
}

/// #339 r2b F1: GT bounds EVERY Nread (net.c:75-76) — the cookie read
/// included; iperf_server_api.c:194-200's own comment names the timeout case
/// ("the inability to read the correct amount of data (i.e. timed out)"),
/// and live GT self-recovers from a connect-and-hold peer in ~20 s with the
/// IERECVCOOKIE surface. Unbounded, riperf3's serial serve loop parked
/// forever behind one hostile peer.
#[test]
fn pretest_cookie_hold_exits_bounded_with_ierecvcookie() {
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

    // Detached holder: connects and sends NOTHING. The server must not wait
    // for it (the thread outlives the assert; the process exit reaps it).
    std::thread::spawn(move || {
        let ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        std::thread::sleep(std::time::Duration::from_secs(40));
        drop(ctrl);
    });

    // The 10 s idle bound fires; the 20 s assert window leaves slack for a
    // loaded 2-core CI runner (the exchange_half_size hold precedent).
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(20))
            .expect("server exits on GT's read bound while the peer holds");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    assert_eq!(
        serr,
        format!("riperf3: error - {RECV_COOKIE_MSG}: \n"),
        "the bounded cookie read takes the IERECVCOOKIE surface: {serr:?}"
    );
}

/// #339 r2b F1, params half: full cookie, then 2 of the 4 length-prefix
/// bytes and a HOLD. GT's get_parameters Nread times out and IERECVPARAMS
/// renders; riperf3 previously parked in the unbounded json_read.
#[test]
fn pretest_params_hold_exits_bounded_with_ierecvparams() {
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
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        ctrl.write_all(&[0u8, 0u8]).unwrap(); // 2 of 4 size bytes, then HOLD
        std::thread::sleep(std::time::Duration::from_secs(40));
        drop(ctrl);
    });

    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(20))
            .expect("server exits on GT's read bound while the peer holds");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    assert_eq!(
        serr,
        format!("riperf3: error - {RECV_PARAMS_MSG}: \n"),
        "the bounded params read takes the IERECVPARAMS surface: {serr:?}"
    );
}

/// #339 r2b F2: iperf_err prefixes its stderr line with the --timestamps
/// stamp (iperf_error.c:51-57, :77) — the pre-test emit site must ride the
/// same output_timestamp_prefix() the stdout banner does. A literal strftime
/// format keeps the pin deterministic (formats pass through verbatim, #202).
#[test]
fn pretest_error_line_carries_the_timestamps_prefix() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s, "--timestamps=XTSX "])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || drive_cookie_failure(port));
    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(5))
            .expect("server exits");
    assert!(status.success(), "one-off exits 0 like GT");
    let serr = serr_reader.join().expect("stderr");
    let suffix = format!("riperf3: error - {RECV_COOKIE_MSG}: \n");
    assert!(
        serr.ends_with(&suffix) && serr.len() > suffix.len(),
        "a nonempty timestamp prefix precedes the iperf_err line: {serr:?}"
    );
    // Unix renders the literal strftime format verbatim; Windows uses the
    // documented HH:MM:SS fallback (macros.rs render_timestamp) and ignores
    // the format, so the byte-exact pin is unix-only.
    #[cfg(unix)]
    assert_eq!(
        serr,
        format!("XTSX {suffix}"),
        "the literal format renders verbatim: {serr:?}"
    );
}

// ---------------------------------------------------------------------------
// #330 (r1 F1): the serve loop's RESIDUAL generic Err arm rides the same
// iperf_err sink. A #188-class validation rejection (valid JSON that
// deserializes but fails config derivation — a negative block size) reaches
// that arm; under -J it must be silent on stderr with the message in a
// skeleton doc, not the raw stderr line GT never emits in JSON mode. The
// wording is riperf3's own #188 deviation (not a GT class), so the pin is on
// the SINK SHAPE.
// ---------------------------------------------------------------------------

/// Valid JSON, deserializes, but `len: -5` fails config derivation (#188).
const GENERIC_ARM_PARAMS: &str = r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":-5,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#;

fn drive_generic_arm_failure(port: u16) {
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(&[b'x'; 37]).unwrap();
    assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
    write_json_blob(&mut ctrl, GENERIC_ARM_PARAMS);
    std::thread::sleep(std::time::Duration::from_millis(400));
}

#[test]
fn generic_arm_failure_text_prints_one_error_line() {
    let (_sout, serr, status) = drive_server_scenario(false, drive_generic_arm_failure);
    assert!(
        serr.starts_with("riperf3: error - ") && serr.lines().count() == 1,
        "one iperf_err text line: {serr:?}"
    );
    assert!(
        status.success(),
        "one-off server exits 0 after a rejected test"
    );
}

#[test]
fn generic_arm_failure_json_is_silent_stderr_with_skeleton_doc() {
    let (sout, serr, status) = drive_server_scenario(true, drive_generic_arm_failure);
    assert!(
        serr.trim().is_empty(),
        "-J routes the residual error to the doc, not stderr: {serr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert!(
        doc["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("error - ")),
        "the skeleton doc carries the error key: {doc}"
    );
    assert!(
        doc["intervals"].as_array().expect("intervals").is_empty(),
        "skeleton intervals:[]"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "skeleton bare end{{}}"
    );
    assert!(status.success());
}

// ---------------------------------------------------------------------------
// #342: GT's cleanup_server best-effort relays SERVER_ERROR(-2) + htonl(i_errno)
// + htonl(errno) to a still-live peer before closing (iperf_server_api.c:
// 460-473 — it keys on the i_errno GLOBAL at the run loop's exit, :1001,
// regardless of return path). Live-probed (iperf 3.21): an unknown control
// byte wires back fe 0000006e 00000000 (IEMESSAGE=110); a failed results read
// fe 00000075 00000000 (IERECVRESULTS=117); a ctrl half-close
// fe 0000006d 00000000 (IECTRLCLOSE=109, r1 F2); CLIENT_TERMINATE
// fe 00000077 (IECLIENTTERM=119, r1 F1 — value/errno deviations on the
// terminate pin). The mock reads the frame rather than waiting for the
// server's close, so the pin observes the bytes regardless of close timing
// (r1 F3: GT sends the frame and closes at once).
// ---------------------------------------------------------------------------

/// The 9-byte SERVER_ERROR relay: state(-2) + htonl(i_errno) + htonl(errno=0).
fn wireback_frame(i_errno: u8) -> Vec<u8> {
    vec![0xfe, 0, 0, 0, i_errno, 0, 0, 0, 0]
}

/// Bounded read of whatever the server sends after the failure: up to the
/// 9-byte relay frame, EOF, or a 6 s timeout (a server that relays nothing
/// yields an empty read — the red shape, and the terminate deviation's pin).
fn read_wireback(ctrl: &mut std::net::TcpStream) -> Vec<u8> {
    ctrl.set_read_timeout(Some(Duration::from_secs(6)))
        .expect("set_read_timeout");
    let mut got = Vec::new();
    let mut buf = [0u8; 9];
    while got.len() < 9 {
        match ctrl.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
        }
    }
    got
}

/// Like [`run_holding_scenario`], but after the final action the mock READS
/// the control socket for the #342 relay frame (then closes), returning the
/// bytes. `final_action`: Some(byte) sends the byte; None HALF-closes the
/// write side (`shutdown(SHUT_WR)`) and keeps the read half open — the EOF
/// cells, where a full drop would discard the relay before the pin sees it.
fn run_wireback_scenario(final_action: Option<u8>, junk_mid_test: bool) -> Vec<u8> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
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
        if !junk_mid_test {
            ctrl.write_all(&[4u8]).unwrap(); // TestEnd
            assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
            write_json_blob(&mut ctrl, MOCK_RESULTS);
            read_json_blob(&mut ctrl); // server results
            assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
        }
        match final_action {
            Some(b) => ctrl.write_all(&[b]).unwrap(),
            None => ctrl.shutdown(std::net::Shutdown::Write).unwrap(),
        }
        let frame = read_wireback(&mut ctrl);
        drop((ctrl, data));
        frame
    });

    let frame = mock.join().expect("mock");
    riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
        .expect("server exits");
    frame
}

/// An unmapped byte during the data phase relays IEMESSAGE(110) — the
/// Err(UnknownControlMessage) arm.
#[test]
fn mid_test_unknown_byte_wires_back_iemessage() {
    assert_eq!(
        run_wireback_scenario(Some(99), true),
        wireback_frame(110),
        "SERVER_ERROR + htonl(IEMESSAGE) + htonl(0), like GT's cleanup_server"
    );
}

/// A KNOWN state that is a stray here (ParamExchange mid-test) takes the same
/// relay — GT's default: arm switches on the byte value, mapped or not; the
/// riperf3 Ok(_) arm is a separate code site from the unmapped-byte arm.
#[test]
fn mid_test_known_stray_wires_back_iemessage() {
    assert_eq!(
        run_wireback_scenario(Some(9), true),
        wireback_frame(110),
        "the known-stray arm relays like the unmapped-byte arm"
    );
}

/// The end loop's IEMESSAGE (junk where IperfDone was due) relays too.
#[test]
fn end_loop_unknown_byte_wires_back_iemessage() {
    assert_eq!(
        run_wireback_scenario(Some(99), false),
        wireback_frame(110),
        "the end-loop arm shares GT's handle_message default relay"
    );
}

/// CLIENT_TERMINATE relays IECLIENTTERM(119): the terminate arm sets the
/// i_errno global (iperf_server_api.c:290) and cleanup_server relays it at
/// the loop's normal exit (:1001, :466) — the relay does NOT key on an error
/// return (r1 F1). TWO RECORDED DEVIATIONS, both value-level: (i) GT's
/// mid-test value is NONDETERMINISTIC — 119 vs a 206 IESTREAMREAD clobber
/// (post-teardown stream reads overwrite the plain global; either value can
/// dominate depending on timing — r1 and r2 observed opposite majorities);
/// riperf3 pins the intended 119. (ii) GT's end-loop frame carries a
/// LEFTOVER errno word (fe 00000077 00000009 live — EBADF from its own
/// closed-socket reads); riperf3 pins errno 0, the #336 honest-errno-0
/// convention.
#[test]
fn end_loop_client_terminate_wires_back_ieclientterm() {
    assert_eq!(
        run_wireback_scenario(Some(12), false),
        wireback_frame(119),
        "SERVER_ERROR + htonl(IECLIENTTERM) + htonl(0) on the end-loop terminate"
    );
}

/// The mid-test terminate arm relays the same frame (GT's arm is shared;
/// riperf3's two sites are distinct).
#[test]
fn mid_test_client_terminate_wires_back_ieclientterm() {
    assert_eq!(
        run_wireback_scenario(Some(12), true),
        wireback_frame(119),
        "SERVER_ERROR + htonl(IECLIENTTERM) + htonl(0) on the mid-test terminate"
    );
}

/// A ctrl HALF-close (shutdown(SHUT_WR), read half open) mid-test: GT's
/// rval==0 arm sets IECTRLCLOSE (iperf_server_api.c:251-254) and
/// cleanup_server relays fe 0000006d 00000000, deterministic live (r1 F2).
#[test]
fn mid_test_ctrl_half_close_wires_back_iectrlclose() {
    assert_eq!(
        run_wireback_scenario(None, true),
        wireback_frame(109),
        "SERVER_ERROR + htonl(IECTRLCLOSE) + htonl(0) on the mid-test EOF"
    );
}

/// The same half-close where IperfDone was due — the end loop's EOF arm.
#[test]
fn end_loop_ctrl_half_close_wires_back_iectrlclose() {
    assert_eq!(
        run_wireback_scenario(None, false),
        wireback_frame(109),
        "SERVER_ERROR + htonl(IECTRLCLOSE) + htonl(0) on the end-loop EOF"
    );
}

/// The NEGATIVE half of the relay matrix (r2 F1): IPERF_DONE is GT's CLEAN
/// arm — i_errno stays IENONE, so cleanup_server relays NOTHING (live ×5).
/// Without this pin a spurious relay at the clean arm survives every test
/// (the clean-cell suites never read ctrl after the final byte).
#[test]
fn mid_test_iperf_done_wires_back_nothing() {
    assert_eq!(
        run_wireback_scenario(Some(16), true),
        Vec::<u8>::new(),
        "clean EOF, no SERVER_ERROR frame, on the mid-test IPERF_DONE"
    );
}

/// The end-loop clean cell — a conforming client's normal finish.
#[test]
fn end_loop_iperf_done_wires_back_nothing() {
    assert_eq!(
        run_wireback_scenario(Some(16), false),
        Vec::<u8>::new(),
        "clean EOF, no SERVER_ERROR frame, on the normal completion"
    );
}

/// A zero-size results prefix from a peer that HOLDS its socket: GT wires
/// back fe 00000075 00000000 (IERECVRESULTS=117, live-probed) — riperf3's
/// exchange_recv_failed arm must relay before the finalize phases.
#[test]
fn exchange_failure_wires_back_recv_results() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
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
        ctrl.write_all(&0u32.to_be_bytes()).unwrap(); // zero-size prefix, HOLD
        let frame = read_wireback(&mut ctrl);
        drop((ctrl, data));
        frame
    });

    let frame = mock.join().expect("mock");
    riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
        .expect("server exits");
    assert_eq!(
        frame,
        wireback_frame(117),
        "SERVER_ERROR + htonl(IERECVRESULTS) + htonl(0), like GT's cleanup_server"
    );
}

// ---------------------------------------------------------------------------
// #344: iperf_err stamps EVERY stderr error line with the --timestamps prefix
// (iperf_error.c:51-57, :77) — the serve-loop arms must ride the same
// output_timestamp_prefix() the pre-test emit gained in #339. GT's warning()
// lines stay BARE (live-probed) — the exchange pin discriminates the two.
// A literal strftime format keeps the pins deterministic on unix; Windows
// uses the documented HH:MM:SS fallback and ignores the format, so the
// byte-exact half of each pin is unix-only (the #339 lesson).
// ---------------------------------------------------------------------------

const TS_ARGS: &[&str] = &["--timestamps=XTSX "];

/// Portable half: a nonempty prefix precedes the expected line. Unix half:
/// the literal format renders verbatim.
fn assert_stamped(line: &str, bare: &str) {
    assert!(
        line.ends_with(bare) && line.len() > bare.len(),
        "a nonempty timestamp prefix precedes the line: {line:?}"
    );
    #[cfg(unix)]
    assert_eq!(
        line,
        &format!("XTSX {bare}"),
        "the literal format renders verbatim: {line:?}"
    );
}

/// The mid-test ctrl-EOF arm (IECTRLCLOSE sentence).
#[test]
fn mid_test_eof_line_carries_the_timestamps_prefix() {
    let (_sout, serr, status) = drive_server_scenario_with(TS_ARGS, false, |port| {
        drive_mock_round_full(port, None, true, MOCK_PARAMS)
    });
    assert!(status.success());
    assert_stamped(
        serr.trim_end_matches('\n'),
        &format!("riperf3: {CTRL_CLOSED}"),
    );
}

/// The client-terminated arm (bare IECLIENTTERM sentence, no "error - ").
#[test]
fn end_loop_client_terminate_line_carries_the_timestamps_prefix() {
    let (_sout, serr, status) = drive_server_scenario_with(TS_ARGS, false, |port| {
        drive_mock_round_full(port, Some(12), false, MOCK_PARAMS)
    });
    assert!(status.success());
    assert_stamped(
        serr.trim_end_matches('\n'),
        "riperf3: the client has terminated",
    );
}

/// The recv-results arm: the ERROR line is stamped, the read-site warning()
/// line above it stays BARE like GT's (iperf_error.c has no stamp in
/// warning-class output; live-probed).
#[test]
fn exchange_eof_error_line_stamped_warning_stays_bare() {
    let (_sout, serr, status) = drive_server_scenario_with(TS_ARGS, false, |port| {
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
        drop((ctrl, data)); // EOF where the results were due
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    assert!(status.success());
    let lines: Vec<&str> = serr.lines().collect();
    assert_eq!(lines.len(), 2, "warning then the error line: {serr:?}");
    assert_eq!(
        lines[0], "warning: Failed to read JSON data size - read returned 0; errno=0",
        "the warning-class line stays BARE like GT's"
    );
    assert_stamped(lines[1], &format!("riperf3: {RECV_RESULTS_ERR}"));
}

/// The unknown-message arm (end loop; the mid-test arm shares the print site).
#[test]
fn end_loop_unknown_byte_line_carries_the_timestamps_prefix() {
    let (_sout, serr, status) = drive_server_scenario_with(TS_ARGS, false, |port| {
        drive_mock_round_full(port, Some(99), false, MOCK_PARAMS)
    });
    assert!(status.success());
    assert_stamped(
        serr.trim_end_matches('\n'),
        &format!("riperf3: error - {IEMESSAGE}"),
    );
}

/// The serve loop's residual generic arm (#188-class rejection wording is
/// riperf3's own, so the pin is on the PREFIX, not the sentence).
#[test]
fn generic_arm_failure_line_carries_the_timestamps_prefix() {
    let (_sout, serr, status) =
        drive_server_scenario_with(TS_ARGS, false, drive_generic_arm_failure);
    assert!(status.success());
    let line = serr.trim_end_matches('\n');
    assert!(
        line.contains("riperf3: error - ") && !line.starts_with("riperf3:"),
        "a nonempty timestamp prefix precedes the residual-arm line: {line:?}"
    );
    #[cfg(unix)]
    assert!(
        line.starts_with("XTSX riperf3: error - "),
        "the literal format renders verbatim: {line:?}"
    );
}

// ---------------------------------------------------------------------------
// #338: the CREATE_STREAMS wait must watch the control socket and carry GT's
// no-progress bound. Live-probed (GT 3.21, probe338.py on the issue):
//
// EOF variant ({"tcp":true} params, ctrl closed, no data conns): GT notices
// the EOF at once (dt~0.00s) — text stderr = the bare IECTRLCLOSE sentence;
// -J = a POPULATED setup-phase start (empty connected:[], listener bufsizes,
// timestamp, accepted_connection, cookie, tcp_mss_default/target_bitrate/
// fq_rate zeros) + intervals:[] + bare end:{} + the bare error key; exit 0.
//
// HOLD variant (ctrl held open, no data conns): GT bounds at rcv_timeout
// (--rcv-timeout, default 120000 ms) — wire-back SERVER_ERROR + IENOMSG(144)
// + errno 0 on the held ctrl, text stderr `iperf3: error - idle timeout for
// receiving data`, -J error key `error - idle timeout for receiving data`
// (prefixed), exit 0. Pre-fix riperf3 parked unbounded in BOTH variants.
// ---------------------------------------------------------------------------

/// Valid JSON that deserializes via serde defaults but promises data streams
/// the peer never connects.
const INCOMPLETE_PARAMS: &str = r#"{"tcp":true}"#;

fn drive_setup_eof(port: u16) {
    let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
    ctrl.write_all(&[b'x'; 37]).unwrap();
    assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
    write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
    assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
    drop(ctrl); // EOF with no data connections
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// EOF variant, text: the server exits bounded with GT's IECTRLCLOSE line.
#[test]
fn setup_phase_ctrl_eof_exits_bounded_in_text() {
    let (_sout, serr, status) = drive_server_scenario(false, drive_setup_eof);
    assert!(status.success(), "one-off exits 0 like GT");
    assert_eq!(
        serr.trim(),
        format!("riperf3: {CTRL_CLOSED}"),
        "GT's bare read-site sentence: {serr:?}"
    );
}

/// EOF variant, -J: the setup-phase doc — populated start, empty shells,
/// bare error key, silent stderr.
#[test]
fn setup_phase_ctrl_eof_takes_gt_doc_shape_in_json() {
    let (sout, serr, status) = drive_server_scenario(true, drive_setup_eof);
    assert!(status.success());
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(CTRL_CLOSED),
        "bare IECTRLCLOSE key: {doc}"
    );
    let start = &doc["start"];
    assert_eq!(
        start["connected"].as_array().map(Vec::len),
        Some(0),
        "connected:[] present and EMPTY: {start}"
    );
    assert_eq!(
        start["accepted_connection"]["host"].as_str(),
        Some("127.0.0.1"),
        "accepted_connection present: {start}"
    );
    // 36, not 37: the doc drops the wire cookie's trailing NUL slot (the
    // shipped convention at server.rs's other cookie-render sites; a real
    // iperf3 cookie is 36 chars + NUL, so both tools render identically for
    // conforming clients — only this mock's 37 non-NUL bytes differ).
    assert_eq!(
        start["cookie"].as_str(),
        Some("x".repeat(36).as_str()),
        "cookie present: {start}"
    );
    for key in ["sndbuf_actual", "rcvbuf_actual", "timestamp"] {
        assert!(
            !start[key].is_null(),
            "{key} present in the setup-phase start: {start}"
        );
    }
    for key in [
        "sock_bufsize",
        "tcp_mss_default",
        "target_bitrate",
        "fq_rate",
    ] {
        assert_eq!(
            start[key].as_u64(),
            Some(0),
            "{key} present as 0 like GT: {start}"
        );
    }
    assert!(
        doc["intervals"].as_array().expect("intervals").is_empty(),
        "intervals:[]"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}"
    );
}

const IDLE_TIMEOUT_MSG: &str = "idle timeout for receiving data";

/// HOLD-variant driver: park in CREATE_STREAMS with the ctrl open, read the
/// wire-back frame, return it with the captured streams.
fn run_setup_hold_scenario(json: bool) -> (Vec<u8>, String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut args = vec!["-s", "-1", "-p", &port_s, "--rcv-timeout", "3000"];
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
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        // HOLD: no data connections, ctrl stays open. The wire-back should
        // arrive at the ~3 s bound; the pre-fix park never sends it.
        let frame = read_wireback(&mut ctrl);
        drop(ctrl);
        frame
    });

    let frame = mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits at the rcv-timeout bound while the peer holds");
    (
        frame,
        sout_reader.join().expect("stdout"),
        serr_reader.join().expect("stderr"),
        status,
    )
}

/// HOLD variant, text: bounded at --rcv-timeout with GT's IENOMSG surface —
/// the wire-back frame, the stderr line, exit 0.
#[test]
fn setup_phase_ctrl_hold_bounds_at_rcv_timeout_in_text() {
    let (frame, _sout, serr, status) = run_setup_hold_scenario(false);
    assert_eq!(
        frame,
        wireback_frame(144),
        "SERVER_ERROR + htonl(IENOMSG) + htonl(0), like GT's cleanup_server"
    );
    assert!(status.success(), "one-off exits 0 like GT");
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {IDLE_TIMEOUT_MSG}"),
        "GT's IENOMSG line: {serr:?}"
    );
}

/// HOLD variant, -J: the prefixed error key over the setup-phase doc.
#[test]
fn setup_phase_ctrl_hold_takes_gt_doc_shape_in_json() {
    let (frame, sout, serr, status) = run_setup_hold_scenario(true);
    assert_eq!(frame, wireback_frame(144));
    assert!(status.success());
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {IDLE_TIMEOUT_MSG}").as_str()),
        "the prefixed IENOMSG key: {doc}"
    );
    assert!(
        !doc["start"]["accepted_connection"].is_null(),
        "setup-phase start populated: {doc}"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}"
    );
}

// ---------------------------------------------------------------------------
// #356 r1 F1: GT dispatches setup-phase ctrl bytes through the SAME
// handle_message_server switch as mid-test (iperf_server_api.c:236-311) —
// the CREATE_STREAMS wait is inside its event loop. Live-probed (GT 3.21,
// probe356b.py on issue #338):
//
// - 0x0c CLIENT_TERMINATE: instant; relay fe 00000077 00000000 (119, errno
//   0 — deterministic in this cell, no post-teardown clobber); text stderr
//   the bare "the client has terminated" + the report-skeleton pair on
//   stdout (separator + plain header, reporter_callback at DISPLAY_RESULTS
//   with zero streams); -J = the setup start + a FULL-zeros end (streams:[],
//   zero sum_sent/sum_received, real host cpu_utilization_percent) + the
//   bare error key; --json-stream = error event + zeros-end end event.
//   GT also closes every accepted data socket at once (r1 F7).
// - 0x10 IPERF_DONE: instant clean exit — NO relay, NO stderr; -J = the
//   errorless setup doc (bare end{}); --json-stream = one bare end event.
// - 0x01 TEST_START: GT's no-op arm — the wait continues (and the byte
//   resets GT's no-progress clock: last_receive_time semantics).
// - anything else: the IEMESSAGE default — relay fe 0000006e 00000000,
//   prefixed doc key, bare end.
// - 0x04 TEST_END: RECORDED DEVIATION — GT runs its ghost end processing
//   (report headers, an EXCHANGE_RESULTS byte, then a stale-errno
//   IERECVRESULTS tangle: "unable to receive results: Bad file
//   descriptor"); riperf3 takes the IEMESSAGE arm above.
// ---------------------------------------------------------------------------

const CLIENT_TERMINATED_MSG: &str = "the client has terminated";
const UNKNOWN_CTRL_MSG: &str =
    "received an unknown control message (ensure other side is iperf3 and not iperf)";

/// Dispatch-cell driver: park in CREATE_STREAMS, send one ctrl byte, read
/// the relay until EOF (empty = no relay, the IPERF_DONE shape).
fn run_setup_byte_scenario(
    extra_args: &[&str],
    byte: u8,
) -> (Vec<u8>, String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut args = vec!["-s", "-1", "-p", &port_s, "--rcv-timeout", "3000"];
    args.extend_from_slice(extra_args);
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
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        ctrl.write_all(&[byte]).unwrap();
        let frame = read_wireback(&mut ctrl);
        drop(ctrl);
        frame
    });

    let frame = mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits promptly on the dispatched byte");
    (
        frame,
        sout_reader.join().expect("stdout"),
        serr_reader.join().expect("stderr"),
        status,
    )
}

/// CLIENT_TERMINATE mid-setup, text: the 119 relay, GT's bare sentence, the
/// report-skeleton pair on stdout — NOT the rcv-timeout park's IENOMSG.
#[test]
fn setup_phase_client_terminate_dispatches_instantly_in_text() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&[], 0x0c);
    assert_eq!(
        frame,
        wireback_frame(119),
        "SERVER_ERROR + htonl(IECLIENTTERM) + htonl(0), like GT's cleanup_server"
    );
    assert!(status.success(), "one-off exits 0 like GT");
    assert_eq!(
        serr.trim(),
        format!("riperf3: {CLIENT_TERMINATED_MSG}"),
        "GT's bare IECLIENTTERM sentence, not the IENOMSG park: {serr:?}"
    );
    assert!(
        sout.contains("- - - - - - - - - - - - - - - - - - - - - - - - -"),
        "the terminate skeleton's separator: {sout:?}"
    );
    assert!(
        sout.contains("[ ID] Interval           Transfer     Bitrate"),
        "the terminate skeleton's header: {sout:?}"
    );
}

/// CLIENT_TERMINATE mid-setup, -J: the setup doc with GT's FULL-zeros end.
#[test]
fn setup_phase_client_terminate_takes_gt_doc_shape_in_json() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&["-J"], 0x0c);
    assert_eq!(frame, wireback_frame(119));
    assert!(status.success());
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(CLIENT_TERMINATED_MSG),
        "bare sentence key: {doc}"
    );
    let end = &doc["end"];
    assert_eq!(
        end["streams"].as_array().map(Vec::len),
        Some(0),
        "streams:[] present and empty: {end}"
    );
    for sum in ["sum_sent", "sum_received"] {
        assert_eq!(
            end[sum]["bytes"].as_u64(),
            Some(0),
            "{sum} zeros block present: {end}"
        );
        assert_eq!(end[sum]["sender"].as_bool(), Some(false), "{sum}.sender");
    }
    assert!(
        end["cpu_utilization_percent"]["host_total"].is_number(),
        "real host cpu figures like GT: {end}"
    );
    assert_eq!(
        end["cpu_utilization_percent"]["remote_total"].as_f64(),
        Some(0.0),
        "remote zeros like GT: {end}"
    );
    assert!(
        !doc["start"]["accepted_connection"].is_null(),
        "setup-phase start populated: {doc}"
    );
}

/// CLIENT_TERMINATE mid-setup, --json-stream: GT's error + zeros-end pair.
#[test]
fn setup_phase_client_terminate_stream_events() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&["--json-stream"], 0x0c);
    assert_eq!(frame, wireback_frame(119));
    assert!(status.success());
    assert!(
        serr.trim().is_empty(),
        "--json-stream keeps stderr silent: {serr:?}"
    );
    let events: Vec<serde_json::Value> = sout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("event line ({e}): {l}")))
        .collect();
    assert_eq!(events.len(), 2, "error + end pair: {sout:?}");
    assert_eq!(events[0]["event"].as_str(), Some("error"));
    assert_eq!(events[0]["data"].as_str(), Some(CLIENT_TERMINATED_MSG));
    assert_eq!(events[1]["event"].as_str(), Some("end"));
    assert_eq!(
        events[1]["data"]["streams"].as_array().map(Vec::len),
        Some(0),
        "zeros-end event data like GT: {}",
        events[1]
    );
}

/// A stray byte mid-setup takes GT's IEMESSAGE default, not the park.
#[test]
fn setup_phase_stray_byte_takes_iemessage_surface_in_text() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&[], 0x63);
    assert_eq!(
        frame,
        wireback_frame(110),
        "SERVER_ERROR + htonl(IEMESSAGE) + htonl(0)"
    );
    assert!(status.success());
    assert_eq!(
        serr.trim(),
        format!("riperf3: error - {UNKNOWN_CTRL_MSG}"),
        "GT's prefixed IEMESSAGE line: {serr:?}"
    );
    assert!(
        !sout.contains("- - - - -"),
        "no report skeleton in the stray cell (GT prints none): {sout:?}"
    );
}

/// A stray byte mid-setup, -J: the prefixed key over the bare-end setup doc.
#[test]
fn setup_phase_stray_byte_prefixed_key_in_json() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&["-J"], 0x63);
    assert_eq!(frame, wireback_frame(110));
    assert!(status.success());
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {UNKNOWN_CTRL_MSG}").as_str()),
        "prefixed IEMESSAGE key: {doc}"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}} in the stray cell: {doc}"
    );
}

/// IPERF_DONE mid-setup is GT's clean arm: no relay, no stderr, an
/// errorless setup doc, exit 0.
#[test]
fn setup_phase_iperf_done_is_clean_and_errorless_in_json() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&["-J"], 0x10);
    assert!(
        frame.is_empty(),
        "GT's IPERF_DONE arm relays nothing: {frame:?}"
    );
    assert!(status.success());
    assert!(
        serr.trim().is_empty(),
        "clean exit keeps stderr silent: {serr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert!(
        doc.get("error").is_none(),
        "NO error key in the clean-done doc: {doc}"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}: {doc}"
    );
    assert!(
        !doc["start"]["accepted_connection"].is_null(),
        "setup-phase start populated: {doc}"
    );
}

/// IPERF_DONE mid-setup, --json-stream: GT emits a single bare end event.
#[test]
fn setup_phase_iperf_done_stream_events() {
    let (frame, sout, serr, status) = run_setup_byte_scenario(&["--json-stream"], 0x10);
    assert!(frame.is_empty());
    assert!(status.success());
    assert!(serr.trim().is_empty());
    let events: Vec<&str> = sout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(events.len(), 1, "one bare end event: {sout:?}");
    let ev: serde_json::Value = serde_json::from_str(events[0]).expect("end event");
    assert_eq!(ev["event"].as_str(), Some("end"));
    assert_eq!(ev["data"], serde_json::json!({}), "bare data: {ev}");
}

/// TEST_START mid-setup keeps the wait alive — the watch survives it and
/// a later EOF still takes the IECTRLCLOSE surface (this byte-then-EOF
/// sub-cell matches GT). RECORDED DEVIATION (r2 F2): in the accepts-after
/// sub-cell GT's Nread has overwritten test->state, so GT ACCESS_DENIED's
/// a subsequent correct-cookie connect; riperf3 keeps accepting.
#[test]
fn setup_phase_test_start_byte_keeps_waiting() {
    let (_sout, serr, status) =
        drive_server_scenario_with(&["--rcv-timeout", "3000"], false, |port| {
            let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
            ctrl.write_all(&[b'x'; 37]).unwrap();
            assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
            write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
            assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
            ctrl.write_all(&[0x01]).unwrap(); // TEST_START: GT's no-op
            std::thread::sleep(std::time::Duration::from_millis(700));
            drop(ctrl); // now EOF — the watch must still be armed
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
    assert!(status.success());
    assert_eq!(
        serr.trim(),
        format!("riperf3: {CTRL_CLOSED}"),
        "the no-op byte left the EOF watch armed: {serr:?}"
    );
}

/// r1 F7: the terminate return closes already-accepted data streams like
/// GT's cleanup_server — the peer's data socket sees EOF bounded, not a
/// detached task holding it until the peer gives up. PERSISTENT server on
/// purpose: a one-off's process exit closes every socket and would pass
/// with the abort removed — only a still-running server discriminates
/// abort from detach (the mutation-vacuous first draft of this pin).
#[test]
fn setup_phase_terminate_closes_accepted_streams() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &port_s, "--rcv-timeout", "3000", "-J"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _sout_reader =
        riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let _serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, r#"{"tcp":true,"parallel":2}"#);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        // Connect ONE of the two promised streams (correct cookie), then
        // terminate on ctrl.
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&[b'x'; 37]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        ctrl.write_all(&[0x0c]).unwrap();
        let frame = read_wireback(&mut ctrl);
        // The server must close the accepted data socket (GT closes every
        // stream socket in its terminate arm) — bounded EOF while the
        // server keeps serving, not a detached task holding it.
        data.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        let data_read = data.read(&mut buf);
        (frame, data_read.map_err(|e| e.kind()))
    });

    let (frame, data_read) = mock.join().expect("mock");
    assert_eq!(frame, wireback_frame(119));
    assert_eq!(
        data_read,
        Ok(0),
        "the accepted data socket sees EOF bounded (GT closes stream socks)"
    );
    // Persistent server: still alive and serving — ChildGuard kills it.
    assert!(
        server.0.try_wait().expect("try_wait").is_none(),
        "the persistent server kept serving after the terminate round"
    );
}

/// r1 F4: the setup doc's timestamp is GT's wait-start stamp (on-connect
/// metadata), not the emit time — at the default 120 s bound those differ
/// by two minutes.
#[test]
fn setup_phase_doc_timestamp_is_stamped_at_accept_not_emit() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s, "--rcv-timeout", "3000", "-J"])
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
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        let t_cs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // HOLD to the 3 s bound.
        let _ = read_wireback(&mut ctrl);
        t_cs
    });

    let t_cs = mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits at the bound");
    assert!(status.success());
    let sout = sout_reader.join().expect("stdout");
    drop(serr_reader.join().expect("stderr"));
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    let stamped = doc["start"]["timestamp"]["timemillisecs"]
        .as_u64()
        .expect("timemillisecs");
    assert!(
        stamped <= t_cs + 500,
        "stamped at the wait start like GT (accept metadata), not at emit \
         ({stamped} vs CREATE_STREAMS at {t_cs} — emit would be ~+3000)"
    );
}

/// r2 F1: the setup-phase ctrl-EOF relays IECTRLCLOSE(109) like its
/// mid-test and end-loop siblings (#342; GT cleanup_server,
/// iperf_server_api.c:466-473) — observable by a HALF-closed peer whose
/// read half is still open; best-effort no-op on a full close.
#[test]
fn setup_phase_ctrl_eof_relays_iectrlclose_on_half_close() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s, "--rcv-timeout", "3000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _sout = riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let _serr = riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, INCOMPLETE_PARAMS);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        // HALF-close: EOF on the server's read, our read half stays open
        // for the relay frame.
        ctrl.shutdown(std::net::Shutdown::Write)
            .expect("shutdown WR");
        read_wireback(&mut ctrl)
    });

    let frame = mock.join().expect("mock");
    assert_eq!(
        frame,
        wireback_frame(109),
        "SERVER_ERROR + htonl(IECTRLCLOSE) + htonl(0), like GT's cleanup_server"
    );
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("server exits");
    assert!(status.success());
}

/// r2 F3: the rcv-timeout is a NO-PROGRESS clock, not an absolute setup
/// deadline — each accepted stream (and each dispatched ctrl byte) resets
/// it, GT's last_receive_time semantics. A -P2 peer connecting one stream
/// every ~2 s under --rcv-timeout 3000 must reach TEST_START, though the
/// whole setup takes longer than 3 s.
#[test]
fn setup_phase_rcv_timeout_resets_on_stream_progress() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port_s, "--rcv-timeout", "3000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _sout = riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let _serr = riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, r#"{"tcp":true,"parallel":2}"#);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        // Slow-but-progressing: one stream per ~2 s. Total setup ~4 s,
        // every gap under the 3 s bound.
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let mut d1 = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data1");
        d1.write_all(&[b'x'; 37]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let mut d2 = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data2");
        d2.write_all(&[b'x'; 37]).unwrap();
        // The next ctrl byte decides: TEST_START(1) = survived (green);
        // a SERVER_ERROR frame (0xfe = IENOMSG at ~t+3 s) = an absolute
        // deadline killed the progressing peer (red). Bounded read (r3
        // nit): a regression that silences the socket entirely must fail
        // the pin, not park the join.
        ctrl.set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .expect("set_read_timeout");
        let first = read_exact(&mut ctrl, 1)[0];
        drop((d1, d2, ctrl));
        first
    });

    let first = mock.join().expect("mock");
    assert_eq!(
        first, 1,
        "a progressing -P2 peer reaches TEST_START; 0xfe means the bound \
         fired mid-progress (absolute-deadline regression)"
    );
    // The mock vanished mid-test; the server's own machinery bounds the
    // round — only the reached-TEST_START byte is under test here.
    let _ = riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(20));
}

// ---------------------------------------------------------------------------
// #345: post-cookie send_state(ParamExchange) failure = GT's IESENDMESSAGE
// (111) — iperf_error.c:305-308 sentence + a LIVE deterministic strerror
// (ENOTCONN; the peer's RST broke the write). GT live-probed (N=12/mode):
// text deterministic; -J silent stderr + skeleton doc with the prefixed key;
// 4/12 the RST lost the race and the run took the IERECVPARAMS class
// instead — BOTH classes stay reachable, so these pins retry-classify.
// The SERVER_ERROR+111 relay is best-effort and unobservable (the peer
// RST'd) — unpinned by design, like #347's try_reserve guard.
// ---------------------------------------------------------------------------

const SENDMSG_SENTENCE: &str =
    "unable to send control message - port may not be available, the other side may have stopped running, etc.";

/// One #345 attempt: cookie, then an immediate SO_LINGER(0) RST. Returns
/// (stdout, stderr, exit). LINUX-ONLY (the #339 SO_LINGER lesson, refined):
/// on macOS/FreeBSD the RST surfaces through an EARLIER syscall (CI:
/// macOS = a kind-only InvalidInput via the generic pre-test arm;
/// FreeBSD = ECONNABORTED at the accept/configure site, deterministic)
/// and the send-write race is essentially never won — the mapping under
/// test is platform-independent; only Linux timing exercises it (#362
/// tracks the non-Linux surfaces).
#[cfg(target_os = "linux")]
fn drive_cookie_then_rst(json: bool) -> (String, String, std::process::ExitStatus) {
    drive_server_scenario(json, |port| {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        // SO_LINGER(0): the drop sends a real RST (the setup_retry.rs
        // helper's pattern — unix-only, which is why these pins are).
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        let rc = unsafe {
            use std::os::fd::AsRawFd;
            libc::setsockopt(
                ctrl.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                std::ptr::from_ref(&linger).cast(),
                std::mem::size_of::<libc::linger>() as libc::socklen_t,
            )
        };
        assert_eq!(rc, 0, "SO_LINGER setsockopt failed");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        drop(ctrl); // RST races the server's ParamExchange state write
        std::thread::sleep(std::time::Duration::from_millis(200));
    })
}

/// #345 text: within a bounded number of race attempts, the send-failure
/// class must appear with GT's IESENDMESSAGE sentence (red = it never
/// appears; pre-fix the path takes a raw Io message via the generic arm).
#[cfg(target_os = "linux")]
#[test]
fn pretest_send_state_failure_takes_iesendmessage_text() {
    let mut seen = Vec::new();
    for _ in 0..12 {
        let (_sout, serr, status) = drive_cookie_then_rst(false);
        assert!(status.success(), "keep-serving one-off exits 0: {serr:?}");
        let line = serr.trim().to_string();
        if line.contains(SENDMSG_SENTENCE) {
            assert!(
                line.starts_with(&format!("riperf3: error - {SENDMSG_SENTENCE}: ")),
                "GT's sentence + live strerror suffix: {line:?}"
            );
            return;
        }
        seen.push(line);
    }
    panic!("IESENDMESSAGE never surfaced in 12 RST races; saw: {seen:#?}");
}

/// #345 -J: same race loop; when the send class hits, the skeleton doc
/// carries the prefixed key with the live strerror (no dangling ": ").
#[cfg(target_os = "linux")]
#[test]
fn pretest_send_state_failure_takes_iesendmessage_json() {
    let mut seen = Vec::new();
    for _ in 0..12 {
        let (sout, serr, status) = drive_cookie_then_rst(true);
        assert!(status.success());
        assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
        let doc: serde_json::Value = serde_json::from_str(sout.trim())
            .unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
        let key = doc["error"].as_str().unwrap_or_default().to_string();
        if key.contains(SENDMSG_SENTENCE) {
            assert!(
                key.starts_with(&format!("error - {SENDMSG_SENTENCE}: ")),
                "prefixed key + live strerror: {key:?}"
            );
            assert!(
                !key.ends_with(": "),
                "the live-strerror class must NOT carry the errno-0 dangling suffix: {key:?}"
            );
            assert!(
                doc["end"].as_object().expect("end").is_empty(),
                "skeleton bare end: {doc}"
            );
            return;
        }
        seen.push(key);
    }
    panic!("IESENDMESSAGE never surfaced in 12 RST races; saw: {seen:#?}");
}

// ---------------------------------------------------------------------------
// #346: SIGTERM while LISTENING (no client ever). GT live-probed: -J =
// skeleton doc with the interrupt-class key (NO "error - " prefix), silent
// stderr, exit 0; --json-stream = the error+bare-end event pair; text =
// stderr `iperf3: interrupt - the server has terminated by signal
// Terminated(15)`, exit 0. riperf3 pre-fix emitted NOTHING in the JSON
// modes on this path.
// ---------------------------------------------------------------------------

const SIGTERM_KEY: &str = "interrupt - the server has terminated by signal Terminated(15)";

#[cfg(unix)]
fn drive_sigterm_listening(args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_s = port.to_string();
    let mut all = vec!["-s", "-p", &port_s];
    all.extend_from_slice(args);
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(&all)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let sout = riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let serr = riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(600));
    // SIGTERM to the LISTENING server — no client ever connected.
    unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) };
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("signal-normal exit");
    (
        sout.join().expect("stdout"),
        serr.join().expect("stderr"),
        status,
    )
}

/// #346 -J: the skeleton doc with the interrupt key, silent stderr, exit 0.
#[cfg(unix)]
#[test]
fn sigterm_while_listening_emits_skeleton_doc_in_json() {
    let (sout, serr, status) = drive_sigterm_listening(&["-J"]);
    assert!(status.success(), "GT signormalexit exits 0");
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout:?}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(SIGTERM_KEY),
        "the interrupt-class key, NO error- prefix: {doc}"
    );
    assert_eq!(
        doc["start"]["connected"].as_array().map(Vec::len),
        Some(0),
        "skeleton start: {doc}"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end: {doc}"
    );
}

/// #346 --json-stream: GT's error + bare-end event pair.
#[cfg(unix)]
#[test]
fn sigterm_while_listening_stream_events() {
    let (sout, serr, status) = drive_sigterm_listening(&["--json-stream"]);
    assert!(status.success());
    assert!(serr.trim().is_empty(), "silent stderr: {serr:?}");
    let events: Vec<serde_json::Value> = sout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("event ({e}): {l}")))
        .collect();
    assert_eq!(events.len(), 2, "error + end pair: {sout:?}");
    assert_eq!(events[0]["event"].as_str(), Some("error"));
    assert_eq!(events[0]["data"].as_str(), Some(SIGTERM_KEY));
    assert_eq!(events[1]["event"].as_str(), Some("end"));
    assert_eq!(events[1]["data"], serde_json::json!({}));
}

/// #346 text: the stderr interrupt line + exit 0 (the pre-fix-green
/// baseline half of the cell, pinned against regressions).
#[cfg(unix)]
#[test]
fn sigterm_while_listening_text_line_and_exit_zero() {
    let (_sout, serr, status) = drive_sigterm_listening(&[]);
    assert!(status.success(), "GT signormalexit exits 0");
    assert_eq!(
        serr.trim(),
        format!("riperf3: {SIGTERM_KEY}"),
        "the CLI's interrupt line: {serr:?}"
    );
}

// ---------------------------------------------------------------------------
// #348: GT stamps CLIENT-side stderr error lines too — iperf_errexit routes
// through the same strftime stamp as iperf_err (iperf_error.c:100-127).
// Live-probed (GT 3.21, --timestamps="XTSX "): connect-refused = `XTSX
// iperf3: error - ...`; the SERVER-ERROR relay = BOTH lines stamped (`XTSX
// iperf3: SERVER ERROR - ...` then `XTSX iperf3: error - ...`); the
// interrupt line = stamped on BOTH roles. GT prints NO second-signal line
// at all (one interrupt line even on a double SIGINT), so riperf3's
// second-signal emergency lines stay bare by design (#158's raw-handler
// write can't render a stamp async-signal-safely).
// ---------------------------------------------------------------------------

/// #348: the client's connect-refused line carries the stamp.
#[test]
fn client_error_line_carries_the_timestamps_prefix() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = free.local_addr().unwrap().port();
    drop(free); // nothing listens: connect refused
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args([
            "-c",
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "--timestamps=XTSX ",
        ])
        .output()
        .expect("run client");
    assert_eq!(out.status.code(), Some(1), "iperf_errexit exits 1");
    let serr = String::from_utf8_lossy(&out.stderr);
    let line = serr.trim();
    assert!(
        line.contains("riperf3: error - ") && !line.starts_with("riperf3:"),
        "a nonempty stamp precedes the client error line: {line:?}"
    );
    #[cfg(unix)]
    assert!(
        line.starts_with("XTSX riperf3: error - "),
        "the literal format renders verbatim: {line:?}"
    );
}

/// #348: the client's SERVER-ERROR relay surface — BOTH stderr lines carry
/// the stamp, like GT's iperf_err + iperf_errexit pair.
#[test]
fn client_relay_lines_carry_the_timestamps_prefix() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps, "--server-bitrate-limit", "1000"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args([
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-b",
            "1M",
            "-t",
            "2",
            "--timestamps=XTSX ",
        ])
        .output()
        .expect("run client");
    let _ = riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8));
    assert_eq!(out.status.code(), Some(1), "refused-run errexit");
    let serr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = serr.lines().filter(|l| !l.trim().is_empty()).collect();
    let relay = lines
        .iter()
        .find(|l| l.contains("SERVER ERROR - "))
        .unwrap_or_else(|| panic!("the relay line is present: {serr:?}"));
    let exitl = lines
        .iter()
        .find(|l| l.contains("riperf3: error - "))
        .unwrap_or_else(|| panic!("the errexit line is present: {serr:?}"));
    for (name, l) in [("relay", relay), ("errexit", exitl)] {
        assert!(
            !l.starts_with("riperf3:"),
            "{name} line carries a stamp: {l:?}"
        );
        #[cfg(unix)]
        assert!(l.starts_with("XTSX riperf3: "), "{name} literal: {l:?}");
    }
}

/// #348: the interrupt line is stamped on BOTH roles (the main.rs:660
/// shared site — GT live: `XTSX iperf3: interrupt - ...` each role).
#[cfg(unix)]
#[test]
fn interrupt_lines_carry_the_timestamps_prefix_both_roles() {
    // Server role: SIGTERM while listening, text mode.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &ps, "--timestamps=XTSX "])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _so = riperf3_test_support::drain_reader(server.0.stdout.take().unwrap());
    let se = riperf3_test_support::drain_reader(server.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(600));
    unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) };
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(8))
            .expect("signal-normal exit");
    assert!(status.success());
    let serr = se.join().expect("stderr");
    assert!(
        serr.trim()
            .starts_with("XTSX riperf3: interrupt - the server has terminated by signal"),
        "server interrupt line stamped: {serr:?}"
    );

    // Client role: SIGINT mid-test.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port2 = listener.local_addr().unwrap().port();
    drop(listener);
    let ps2 = port2.to_string();
    let mut srv2 = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps2])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server2"),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut client = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args([
                "-c",
                "127.0.0.1",
                "-p",
                &ps2,
                "-t",
                "10",
                "--timestamps=XTSX ",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn client"),
    );
    let _co = riperf3_test_support::drain_reader(client.0.stdout.take().unwrap());
    let ce = riperf3_test_support::drain_reader(client.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_secs(2));
    unsafe { libc::kill(client.0.id() as i32, libc::SIGINT) };
    let cstatus =
        riperf3_test_support::wait_bounded(&mut client.0, std::time::Duration::from_secs(8))
            .expect("client signal exit");
    assert!(cstatus.success(), "signal-normal exit");
    let cerr = ce.join().expect("client stderr");
    assert!(
        cerr.trim()
            .starts_with("XTSX riperf3: interrupt - the client has terminated by signal"),
        "client interrupt line stamped: {cerr:?}"
    );
    let _ = riperf3_test_support::wait_bounded(&mut srv2.0, std::time::Duration::from_secs(8));
}

/// #348 (r1 F1): the stamp renders AT PRINT TIME, per line, like GT's
/// strftime — a prefix captured before the run would stamp the interrupt
/// line with the START time. The literal-format pins can't see this;
/// `%s` (epoch seconds) can: the interrupt line's epoch must be later
/// than the banner's (SIGINT lands ≥2 s in; slow runners only widen it).
#[cfg(unix)]
#[test]
fn interrupt_stamp_renders_at_print_time_not_capture_time() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut srv = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    let mut client = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", &ps, "-t", "10", "--timestamps=%s "])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn client"),
    );
    let co = riperf3_test_support::drain_reader(client.0.stdout.take().unwrap());
    let ce = riperf3_test_support::drain_reader(client.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_secs(2));
    unsafe { libc::kill(client.0.id() as i32, libc::SIGINT) };
    let status =
        riperf3_test_support::wait_bounded(&mut client.0, std::time::Duration::from_secs(8))
            .expect("client signal exit");
    assert!(status.success());
    let cout = co.join().expect("stdout");
    let cerr = ce.join().expect("stderr");
    let _ = riperf3_test_support::wait_bounded(&mut srv.0, std::time::Duration::from_secs(8));

    let epoch_of = |line: &str| -> Option<u64> { line.split(' ').next()?.parse().ok() };
    let banner_epoch = cout
        .lines()
        .find_map(epoch_of)
        .expect("a %s-stamped stdout line exists");
    let interrupt_line = cerr
        .lines()
        .find(|l| l.contains("interrupt - the client has terminated"))
        .unwrap_or_else(|| panic!("the interrupt line is present: {cerr:?}"));
    let interrupt_epoch = epoch_of(interrupt_line)
        .unwrap_or_else(|| panic!("the interrupt line is %s-stamped: {interrupt_line:?}"));
    assert!(
        interrupt_epoch > banner_epoch,
        "print-time rendering: interrupt epoch {interrupt_epoch} must postdate \
         the banner's {banner_epoch} (a pre-run capture freezes them equal)"
    );
}

// ---------------------------------------------------------------------------
// #343 (DECISION: MIRROR): GT parses every length-prefixed blob with
// cJSON_Parse(require_null_terminated=0) — the FIRST JSON value wins and
// trailing bytes inside the declared length are ignored (live-probed both
// blob sites: garbage-suffixed params → CREATE_STREAMS follows;
// garbage-suffixed results → clean full round, rc 0). Deliberate wire
// leniency GT depends on, not bug noise (the #328 atoi-fidelity
// precedent) — a peer that over-declares its length interoperates with
// iperf3 and must interoperate with riperf3. Garbage from byte 0 still
// rejects in both tools.
// ---------------------------------------------------------------------------

/// #343: a params blob of valid JSON + trailing garbage is ACCEPTED — the
/// round advances to CREATE_STREAMS like GT.
#[test]
fn params_blob_with_trailing_garbage_is_accepted() {
    let (_sout, _serr, status) = drive_server_scenario(false, |port| {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        let mut blob = MOCK_PARAMS.as_bytes().to_vec();
        // NUL-FREE suffix on purpose (r1 F3): a NUL-led one also passes GT
        // via JSON_read's strlen truncation — this pin must prove the
        // cJSON-side leniency (require_null_terminated=0), which GT grants
        // NUL-free garbage too (live-probed).
        blob.extend_from_slice(b" GARBAGE-NO-NUL");
        ctrl.write_all(&(blob.len() as u32).to_be_bytes()).unwrap();
        ctrl.write_all(&blob).unwrap();
        assert_eq!(
            read_exact(&mut ctrl, 1)[0],
            10,
            "CreateStreams follows a garbage-suffixed params blob (GT parity)"
        );
        drop(ctrl);
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    // The mock EOF'd at CREATE_STREAMS — the round ends via the #338
    // IECTRLCLOSE surface; the pin is the state byte above.
    assert!(status.success());
}

/// #343 negative cell: garbage from byte 0 still rejects (IERECVPARAMS).
#[test]
fn params_blob_garbage_from_byte_zero_still_rejects() {
    let (_sout, serr, status) = drive_server_scenario(false, |port| {
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&[b'x'; 37]).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json_blob(&mut ctrl, "\x00\x7fGARBAGE from byte zero");
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(ctrl);
    });
    assert!(status.success());
    assert!(
        serr.contains("unable to receive parameters from client"),
        "byte-0 garbage keeps the IERECVPARAMS surface: {serr:?}"
    );
}

/// #343: a results blob of valid JSON + trailing garbage completes the
/// exchange — the server sends its own results back and the round is
/// clean, like GT.
#[test]
fn results_blob_with_trailing_garbage_completes_the_exchange() {
    let (_sout, serr, status) = drive_server_scenario(false, |port| {
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
        let mut blob = MOCK_RESULTS.as_bytes().to_vec();
        blob.extend_from_slice(b"\x00\xffGARBAGE");
        ctrl.write_all(&(blob.len() as u32).to_be_bytes()).unwrap();
        ctrl.write_all(&blob).unwrap();
        // GT parity: the exchange COMPLETES — the server's own results
        // come back as a length-prefixed blob.
        let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
        assert!(
            (2..1_000_000).contains(&len),
            "a plausible server-results blob follows: {len}"
        );
        let body = read_exact(&mut ctrl, len);
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body).is_ok(),
            "the server's results parse"
        );
        drop((ctrl, data));
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    assert!(status.success());
    assert!(
        !serr.contains("unable to receive results"),
        "no IERECVRESULTS on the garbage-suffixed exchange: {serr:?}"
    );
}

// ---------------------------------------------------------------------------
// #350 (DECISION: RECORD-DEVIATION): on the runtime IETOTALRATE breach GT
// relays the SERVER_ERROR frame TWICE (the explicit rate-path write, then
// cleanup_server re-reading the stale i_errno global at loop exit — live:
// two back-to-back fe 0000001b 00000000 frames). Same stale-global class
// as the #349 terminate-arm clobber; unobservable by conforming clients
// (they stop reading after the first frame). riperf3 pins EXACTLY ONE
// frame then EOF.
// ---------------------------------------------------------------------------

/// #350: the runtime-breach relay is exactly one 9-byte frame, then EOF.
#[test]
fn runtime_rate_breach_relays_exactly_one_frame() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps, "--server-bitrate-limit", "8000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _so = riperf3_test_support::drain_reader(server.0.stdout.take().unwrap());
    let _se = riperf3_test_support::drain_reader(server.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        use std::io::Read;
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
        // Pump well past 8 kbit/s so the 1 Hz rate check fires, then read
        // EVERYTHING off ctrl until EOF: exactly one fe-frame expected.
        for _ in 0..40 {
            if data.write_all(&[0u8; 8192]).is_err() {
                break; // server tore the data socket down at the breach
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        ctrl.set_read_timeout(Some(std::time::Duration::from_secs(8)))
            .expect("set_read_timeout");
        let mut wire = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            match ctrl.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => wire.extend_from_slice(&buf[..n]),
            }
        }
        drop((ctrl, data));
        wire
    });

    let wire = mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(12))
            .expect("server exits after the breach");
    assert!(status.success());
    assert_eq!(
        wire,
        wireback_frame(27),
        "EXACTLY one SERVER_ERROR+IETOTALRATE frame then EOF — GT's \
         stale-global double-relay is the recorded deviation: {wire:?}"
    );
}

// ---------------------------------------------------------------------------
// #351: GT's TEST_RUNNING data-idle watchdog (IENOMSG=144). GT source
// (iperf_server_api.c:720-739): fires only when the server RECEIVES
// (mode != SENDER — reverse tests exempt) and `blocks_received` hasn't
// advanced for rcv_timeout; ctrl traffic does NOT reset it. riperf3's
// progress signal is received BYTES — a recorded RATE-scoped deviation
// (GT's blocks_received is full-block-quantized at ANY bound; see the
// negative-cells test + the arm comment). Live-probed
// (--rcv-timeout 3000, silent client holding both sockets): wire
// fe 00000090 00000000 at dt=3.0; text stderr `iperf3: error - idle
// timeout for receiving data`, zero-byte interval rows keep ticking, NO
// summary, rc=0; -J = ONE doc with the accumulated intervals + bare
// end{} + the prefixed key, silent stderr.
// ---------------------------------------------------------------------------

/// #351 driver: reach TEST_RUNNING, send one chunk, go SILENT holding both
/// sockets; read the relay until EOF.
fn run_running_idle_scenario(json: bool) -> (Vec<u8>, String, String, std::process::ExitStatus) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut args = vec!["-s", "-1", "-p", &ps, "--rcv-timeout", "3000"];
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
    let sout = riperf3_test_support::drain_reader(server.0.stdout.take().unwrap());
    let serr = riperf3_test_support::drain_reader(server.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
        // A LONG test so the duration watchdog can't be what bounds us.
        write_json_blob(
            &mut ctrl,
            r#"{"tcp":true,"omit":0,"time":300,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"x 0.0.0"}"#,
        );
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
        data.write_all(&[0u8; 4096]).unwrap();
        // SILENT from here, both sockets held open: only the idle watchdog
        // can end this round (the test claims 300 s).
        let frame = read_wireback(&mut ctrl);
        drop((ctrl, data));
        frame
    });

    let frame = mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(10))
            .expect("the idle watchdog bounds the round");
    (
        frame,
        sout.join().expect("stdout"),
        serr.join().expect("stderr"),
        status,
    )
}

/// #351 text: the 144 relay at the bound, GT's stderr line, ticking
/// zero-byte rows, NO summary block, exit 0.
#[test]
fn running_idle_watchdog_takes_ienomsg_in_text() {
    let (frame, sout, serr, status) = run_running_idle_scenario(false);
    assert_eq!(
        frame,
        wireback_frame(144),
        "SERVER_ERROR + htonl(IENOMSG) + htonl(0) on the running-idle bound"
    );
    assert!(status.success(), "keep-serving class exits 0");
    assert!(
        serr.contains(&format!("riperf3: error - {IDLE_TIMEOUT_MSG}")),
        "GT's IENOMSG line: {serr:?}"
    );
    assert!(
        sout.contains("0.00 Bytes"),
        "zero-byte interval rows tick while idle: {sout:?}"
    );
    assert!(
        !sout.contains("- - - - -"),
        "NO summary block on the idle-killed round (GT): {sout:?}"
    );
}

/// #351 -J: ONE doc — accumulated intervals present, bare end, the
/// prefixed key, silent stderr.
#[test]
fn running_idle_watchdog_doc_shape_in_json() {
    let (frame, sout, serr, status) = run_running_idle_scenario(true);
    assert_eq!(frame, wireback_frame(144));
    assert!(status.success());
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("ONE -J doc ({e}): {sout:?}"));
    assert_eq!(
        doc["error"].as_str(),
        Some(format!("error - {IDLE_TIMEOUT_MSG}").as_str()),
        "the prefixed IENOMSG key: {doc}"
    );
    assert!(
        !doc["intervals"].as_array().expect("intervals").is_empty(),
        "the accumulated (zero-byte) intervals are PRESENT: {doc}"
    );
    assert!(
        doc["end"].as_object().expect("end").is_empty(),
        "bare end{{}}: {doc}"
    );
}

/// #351 negative cells: a reverse round longer than the bound completes
/// (GT's mode != SENDER gate — the server is the sender, exempt), and a
/// slow-but-flowing forward round completes. The second half is a
/// RECORDED DEVIATION, not GT parity (PR #369 r1+r2, live-probed): GT's
/// blocks_received is quantized to COMPLETED `len` blocks (running-phase
/// reads are Nrecv_no_select — timeout-free full-block accumulate), so GT
/// kills this exact sub-block trickle at the bound ("idle timeout" while
/// data flows) — at ANY bound, for any flow slower than len*8/bound
/// (~8.7 kbit/s at stock defaults). riperf3's byte-based progress
/// deliberately survives it — the liveness-preserving reading of "idle"
/// (the #356 TEST_START precedent).
#[test]
fn running_idle_watchdog_negative_cells() {
    // Reverse, real client: -R -t 5 under --rcv-timeout 3000.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps, "--rcv-timeout", "3000"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let _so = riperf3_test_support::drain_reader(server.0.stdout.take().unwrap());
    let se = riperf3_test_support::drain_reader(server.0.stderr.take().unwrap());
    std::thread::sleep(std::time::Duration::from_millis(400));
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", &ps, "-R", "-t", "5", "-b", "1M"])
        .output()
        .expect("reverse client");
    assert!(
        cli.status.success(),
        "a reverse round outliving the bound completes (mode != SENDER gate): {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(10))
            .expect("server exits");
    assert!(status.success());
    let serr = se.join().expect("stderr");
    assert!(
        !serr.contains(IDLE_TIMEOUT_MSG),
        "no IENOMSG on the healthy reverse round: {serr:?}"
    );

    // Slow-but-flowing forward: one chunk per second for ~5 s under the
    // 3 s bound — progress resets the clock, the round must NOT be killed.
    let (frame, _sout, serr2, status2) = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let ps = port.to_string();
        let mut server = common::ChildGuard(
            std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
                .args(["-s", "-1", "-p", &ps, "--rcv-timeout", "3000"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn server"),
        );
        let _so = riperf3_test_support::drain_reader(server.0.stdout.take().unwrap());
        let se2 = riperf3_test_support::drain_reader(server.0.stderr.take().unwrap());
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
            for _ in 0..5 {
                data.write_all(&[0u8; 1024]).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1000));
            }
            ctrl.write_all(&[4u8]).unwrap(); // TestEnd: finish cleanly
            assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
            let blob = MOCK_RESULTS.as_bytes().to_vec();
            ctrl.write_all(&(blob.len() as u32).to_be_bytes()).unwrap();
            ctrl.write_all(&blob).unwrap();
            let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
            let _ = read_exact(&mut ctrl, len);
            Vec::<u8>::new()
        });
        let frame = mock.join().expect("mock");
        let status =
            riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(12))
                .expect("server exits");
        (frame, String::new(), se2.join().expect("stderr"), status)
    };
    assert!(frame.is_empty());
    assert!(status2.success());
    assert!(
        !serr2.contains(IDLE_TIMEOUT_MSG),
        "byte-based progress keeps a flowing sub-block trickle alive (the \
         RECORDED DEVIATION — GT's block-quantized signal kills this cell): {serr2:?}"
    );
}
