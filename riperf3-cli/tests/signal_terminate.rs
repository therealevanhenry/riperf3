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

/// #231: a SIGTERM while the client waits BETWEEN states (here: a raw server
/// that accepts the control connection, reads the cookie, then goes silent —
/// the client parks in its central recv_state wait, pre-ParamExchange) must
/// take the same iperf_got_sigend path as a mid-test signal: dump, attempt
/// CLIENT_TERMINATE, print the signal-normal line, exit 0. GT's
/// iperf_catch_sigend is armed for the whole run with no phase gate
/// (iperf_api.c: the client condition in iperf_got_sigend). Pre-#231 the
/// unarmed wait ignored the first signal entirely (only #211's second-signal
/// hard exit, which skips the dump, ever fired).
#[test]
fn client_sigterm_in_setup_wait_exits_promptly() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let hold = std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut cookie = [0u8; 37];
            let _ = s.read_exact(&mut cookie);
            // Stay silent; the socket drops when the thread ends.
            std::thread::sleep(Duration::from_secs(15));
        }
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-t", "5"]);
    // Enough for connect + cookie write + parking in the state wait, even on
    // a loaded runner; well inside the mock's 15 s silence.
    std::thread::sleep(Duration::from_millis(700));
    let cpid = client.0.id() as i32;
    let killed_at = Instant::now();
    unsafe {
        libc::kill(cpid, libc::SIGTERM);
    }

    let (cout, cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(8), "client in setup wait");
    // PROMPTNESS is the pin: the CLI's #211 fallback produces the same exit
    // shape after its 5 s dump-window timeout, so only the latency separates
    // "the lib honored the watch" from "the fallback fired".
    let reacted_in = killed_at.elapsed();
    assert!(
        reacted_in < Duration::from_secs(3),
        "the signal must be honored AT the wait, not via the CLI's 5 s \
         fallback window (#231): took {reacted_in:?}"
    );
    // And the DUMP is the other pin: iperf_got_sigend dumps for clients in
    // EVERY phase (no phase gate); the fallback path prints no summary.
    assert!(
        cout.contains("- - - - -"),
        "the accumulated-stats dump (empty rows pre-data, like GT): {cout}"
    );
    assert!(
        cerr.contains("interrupt - the client has terminated by signal"),
        "the signal-normal line from the setup-phase wait: {cerr:?}"
    );
    assert_eq!(ccode, 0, "TERM takes the exit-normal path: {cerr:?}");
    drop(hold); // detached; the spawned thread dies with the process
}

/// #231 r2 pin (mutation A): a dump for a test that never STARTED reports a
/// ZERO-second window — GT's pre-data sigend dump says 0/0/0 where the old
/// code asserted the requested -t that never ran.
#[test]
fn pre_data_interrupt_dump_reports_a_zero_window() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let _hold = std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut cookie = [0u8; 37];
            let _ = s.read_exact(&mut cookie);
            std::thread::sleep(Duration::from_secs(15));
        }
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-t", "5", "-J"]);
    std::thread::sleep(Duration::from_millis(700));
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(8), "client -J pre-data");
    assert_eq!(ccode, 0);
    let doc: serde_json::Value =
        serde_json::from_str(cout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {cout}"));
    for key in ["sum_sent", "sum_received"] {
        assert_eq!(
            doc["end"][key]["seconds"].as_f64(),
            Some(0.0),
            "a never-started test reports a zero window (GT 0/0/0), not the \
             requested -t: {doc}"
        );
    }
    // #281 (GT captures on the issue): a PRE-ParamExchange interrupt doc
    // carries ONLY connected/version/system_info in `start` — GT stages the
    // rest at on_connect (post-param-exchange) and TestStart respectively.
    let start = doc["start"].as_object().expect("start object");
    for present in ["connected", "version", "system_info"] {
        assert!(
            start.contains_key(present),
            "pre-PE interrupt start keeps {present}: {doc}"
        );
    }
    for absent in [
        "timestamp",
        "connecting_to",
        "cookie",
        "tcp_mss_default",
        "target_bitrate",
        "fq_rate",
        "sock_bufsize",
        "sndbuf_actual",
        "rcvbuf_actual",
        "test_start",
    ] {
        assert!(
            !start.contains_key(absent),
            "pre-PE interrupt start must omit {absent} (GT stage 0): {doc}"
        );
    }
    // The interrupt end is the FULL zero structure — including a PRESENT
    // (empty) streams key — NOT the refusal's bare `end: {{}}` (#261/#281).
    assert_eq!(
        doc["end"]["streams"].as_array().map(Vec::len),
        Some(0),
        "interrupt end carries streams: [] (GT), not an omitted key: {doc}"
    );
    assert!(
        doc["end"]["cpu_utilization_percent"].is_object(),
        "interrupt end keeps the cpu figure: {doc}"
    );
    // #281 r1 F1: the stream-less TCP forward dump carries GT's role-level
    // `sum_sent.retransmits: 0` (platform-gated exactly like GT: present
    // where TCP_INFO retransmits exist — the Linux/FreeBSD/macOS CI legs —
    // absent elsewhere, e.g. the Windows native leg).
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    assert_eq!(
        doc["end"]["sum_sent"]["retransmits"].as_i64(),
        Some(0),
        "stream-less TCP forward dump carries retransmits: 0 (GT): {doc}"
    );
}

/// #281: the POST-param-exchange / pre-TestStart interrupt window (GT stage 1
/// — the second capture on the issue). The mock completes the param exchange
/// (cookie → ParamExchange → params read) then stalls; the SIGTERM dump must
/// carry the on_connect metadata (real timestamp, cookie, connecting_to,
/// tcp_mss_default, target_bitrate, fq_rate) while still omitting the four
/// TestStart-stage fields, with the same full-zeros end incl. streams: [].
#[test]
fn post_exchange_prestart_interrupt_dump_takes_gt_stage1_shape() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let _mock = std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut cookie = [0u8; 37];
            let _ = s.read_exact(&mut cookie);
            let _ = s.write_all(&[9u8]); // ParamExchange
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).is_ok() {
                let n = u32::from_be_bytes(len) as usize;
                let mut params = vec![0u8; n];
                let _ = s.read_exact(&mut params);
            }
            std::thread::sleep(Duration::from_secs(15)); // stall pre-TestStart
        }
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-t", "5", "-J"]);
    std::thread::sleep(Duration::from_millis(900));
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(8), "client -J post-PE");
    assert_eq!(ccode, 0);
    let doc: serde_json::Value =
        serde_json::from_str(cout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {cout}"));

    let start = doc["start"].as_object().expect("start object");
    for present in [
        "connected",
        "version",
        "system_info",
        "timestamp",
        "connecting_to",
        "cookie",
        "target_bitrate",
        "fq_rate",
    ] {
        assert!(
            start.contains_key(present),
            "post-PE interrupt start keeps {present} (GT stage 1): {doc}"
        );
    }
    for absent in [
        "sock_bufsize",
        "sndbuf_actual",
        "rcvbuf_actual",
        "test_start",
    ] {
        assert!(
            !start.contains_key(absent),
            "post-PE interrupt start must omit {absent} (GT stage 1): {doc}"
        );
    }
    assert_ne!(
        doc["start"]["timestamp"]["timesecs"],
        serde_json::json!(0),
        "the stage-1 timestamp is the real on_connect wall-clock: {doc}"
    );
    assert_eq!(
        doc["end"]["streams"].as_array().map(Vec::len),
        Some(0),
        "interrupt end carries streams: []: {doc}"
    );
    for key in ["sum_sent", "sum_received"] {
        assert_eq!(
            doc["end"][key]["seconds"].as_f64(),
            Some(0.0),
            "zero window: {doc}"
        );
    }
    // #281 r1 F1: the stream-less TCP forward dump carries GT's role-level
    // `sum_sent.retransmits: 0` (platform-gated exactly like GT: present
    // where TCP_INFO retransmits exist — the Linux/FreeBSD/macOS CI legs —
    // absent elsewhere, e.g. the Windows native leg).
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    assert_eq!(
        doc["end"]["sum_sent"]["retransmits"].as_i64(),
        Some(0),
        "stream-less TCP forward dump carries retransmits: 0 (GT): {doc}"
    );
}

/// #231 r2 pin (mutation B): an interrupt AFTER ExchangeResults keeps the
/// already-exchanged peer halves in the dump, like GT's merged sigend dump.
/// The mock speaks the full protocol through the exchange (crafted peer
/// bytes 424242), then stalls before DisplayResults.
#[test]
fn post_exchange_interrupt_dump_keeps_the_peer_halves() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let exchanged = Arc::new(AtomicBool::new(false));
    let exchanged_w = exchanged.clone();

    let _mock = std::thread::spawn(move || {
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
                                         // Drain the -n payload so the sender never blocks, until TestEnd(4)
                                         // arrives on ctrl.
        let drain = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            while data.read(&mut buf).map(|n| n > 0).unwrap_or(false) {}
        });
        assert_eq!(read_exact(&mut ctrl, 1)[0], 4, "TestEnd");
        ctrl.write_all(&[13u8]).unwrap(); // ExchangeResults
        read_json(&mut ctrl); // the client's results
        write_json(
            &mut ctrl,
            r#"{"cpu_util_total":1.5,"cpu_util_user":1.0,"cpu_util_system":0.5,"sender_has_retransmits":0,"streams":[{"id":1,"bytes":424242,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
        );
        exchanged_w.store(true, Ordering::SeqCst);
        // Stall before DisplayResults: the client parks in the state wait.
        std::thread::sleep(Duration::from_secs(15));
        drop(drain);
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-n", "100K", "-J"]);
    let t0 = Instant::now();
    while !exchanged.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "mock never reached the exchange"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300)); // park in the state wait
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(8), "client post-exchange");
    assert_eq!(ccode, 0);
    let doc: serde_json::Value =
        serde_json::from_str(cout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {cout}"));
    assert_eq!(
        doc["end"]["sum_received"]["bytes"].as_i64(),
        Some(424242),
        "the exchanged peer half must survive into the interrupt dump \
         (GT merges exchanged data before its sigend dump): {doc}"
    );
}

/// #267: an abruptly-lost control connection (server SIGKILLed mid-test) is
/// GT's IECTRLCLOSE class — text prints exactly
/// `error - control socket has closed unexpectedly` (no summary block), and
/// the `-J` doc is the POPULATED-so-far document: full start (the test
/// reached TestStart), collected intervals, and a bare `end: {}` (GT's
/// errexit never fills json_end) — live-captured on the issue.
#[test]
fn killed_server_takes_gt_ctrl_closed_shape() {
    // --- text mode ---
    let port = free_port();
    let ps = port.to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "5"]);
    std::thread::sleep(Duration::from_millis(1500));
    unsafe { libc::kill(server.0.id() as i32, libc::SIGKILL) };
    let (_, cerr, ccode) = wait_with_output_bounded(client, Duration::from_secs(10), "text client");
    assert_eq!(ccode, 1, "GT exits 1 on IECTRLCLOSE");
    assert!(
        cerr.contains("error - control socket has closed unexpectedly"),
        "GT's IECTRLCLOSE wording (#267): {cerr}"
    );
    assert!(
        !cerr.contains("peer disconnected"),
        "the old wording is gone: {cerr}"
    );

    // --- -J mode ---
    let port = free_port();
    let ps = port.to_string();
    let server = spawn(&["-s", "-1", "-p", &ps]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "5", "-J"]);
    std::thread::sleep(Duration::from_millis(1500));
    unsafe { libc::kill(server.0.id() as i32, libc::SIGKILL) };
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(10), "-J client");
    assert_eq!(ccode, 1);
    let doc: serde_json::Value =
        serde_json::from_str(cout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {cout}"));
    assert!(
        doc["start"]["test_start"].is_object(),
        "the populated start survives (GT errexit keeps json_top): {doc}"
    );
    assert!(
        !doc["intervals"].as_array().unwrap().is_empty(),
        "collected intervals survive: {doc}"
    );
    assert_eq!(
        doc["end"].as_object().map(serde_json::Map::len),
        Some(0),
        "the errexit end is GT's bare `end: {{}}`: {doc}"
    );
    assert_eq!(
        doc["error"].as_str(),
        Some("control socket has closed unexpectedly"),
        "{doc}"
    );
}

/// #267 r1 F1: the PRE-DATA window (server vanishes with a clean FIN after
/// the param exchange, before TestStart) — GT emits exactly ONE -J doc:
/// Connected-stage start (on_connect metadata, no test_start), bare end{},
/// the IECTRLCLOSE error. A second skeleton doc (the CLI re-render) is the
/// #225 violation this pin holds shut.
#[test]
fn predata_ctrl_close_emits_one_staged_doc() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let _mock = std::thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut cookie = [0u8; 37];
            let _ = s.read_exact(&mut cookie);
            let _ = s.write_all(&[9u8]); // ParamExchange
            let mut len = [0u8; 4];
            if s.read_exact(&mut len).is_ok() {
                let n = u32::from_be_bytes(len) as usize;
                let mut params = vec![0u8; n];
                let _ = s.read_exact(&mut params);
            }
            // clean FIN: drop the socket
        }
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-t", "5", "-J"]);
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(10), "-J pre-data FIN");
    assert_eq!(ccode, 1);
    let doc: serde_json::Value = serde_json::from_str(cout.trim()).unwrap_or_else(|e| {
        panic!("exactly ONE -J doc (r1 F1: the CLI must not re-render) ({e}): {cout}")
    });
    let start = doc["start"].as_object().expect("start object");
    assert!(
        start.contains_key("cookie") && start.contains_key("timestamp"),
        "Connected-stage start (on_connect metadata): {doc}"
    );
    assert!(
        !start.contains_key("test_start"),
        "pre-TestStart: no late fields: {doc}"
    );
    assert_eq!(
        doc["end"].as_object().map(serde_json::Map::len),
        Some(0),
        "bare end: {doc}"
    );
    assert_eq!(
        doc["error"].as_str(),
        Some("control socket has closed unexpectedly"),
        "{doc}"
    );
}

/// #268: a peer that WEDGES mid-results-payload (length prefix + partial
/// JSON, then nothing) must not outlive a signal — the bulk recv_results
/// read joins the #231 interrupt surface. Pre-fix the client hung in the
/// uninterruptible read until the CLI's #211 second-signal hard exit.
#[test]
fn mid_exchange_wedge_still_exits_on_signal() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port().to_string();
    let wedged = Arc::new(AtomicBool::new(false));
    let wedged_w = wedged.clone();

    let _mock = std::thread::spawn(move || {
        let read_exact = |s: &mut std::net::TcpStream, n: usize| -> Vec<u8> {
            let mut b = vec![0u8; n];
            s.read_exact(&mut b).expect("mock read");
            b
        };
        let read_json = |s: &mut std::net::TcpStream| {
            let len = u32::from_be_bytes(read_exact(s, 4).try_into().unwrap()) as usize;
            read_exact(s, len)
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
                              // The wedge: a 500-byte payload announced, 40 bytes delivered.
        ctrl.write_all(&500u32.to_be_bytes()).unwrap();
        ctrl.write_all(&[b'{'; 40]).unwrap();
        wedged_w.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_secs(15)); // stall mid-payload
        drop(drain);
    });

    let client = spawn(&["-c", "127.0.0.1", "-p", &port, "-n", "100K", "-J"]);
    let t0 = Instant::now();
    while !wedged.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "mock never reached the wedge"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300)); // park in the bulk read
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let (cout, _cerr, ccode) =
        wait_with_output_bounded(client, Duration::from_secs(5), "client mid-exchange wedge");
    assert_eq!(ccode, 0, "signal-normal exit despite the wedged read");
    let doc: serde_json::Value =
        serde_json::from_str(cout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {cout}"));
    assert!(
        doc["error"]
            .as_str()
            .is_some_and(|e| e.contains("terminated")),
        "the sigend dump carries the interrupt error key: {doc}"
    );
}

/// #322 r1 F3: the SECOND server read — the IperfDone wait — takes the
/// same interrupt surface. The mock completes the results exchange, reads
/// DisplayResults, then never sends IperfDone.
#[test]
fn server_survives_a_client_that_never_sends_iperf_done() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let port = free_port();
    let ps = port.to_string();
    let server = spawn(&["-s", "-1", "-p", &ps, "-J"]);
    std::thread::sleep(Duration::from_millis(400));

    let parked = Arc::new(AtomicBool::new(false));
    let parked_w = parked.clone();
    let _mock = std::thread::spawn(move || {
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
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json(
            &mut ctrl,
            r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#,
        );
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1, "TestStart");
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2, "TestRunning");
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13, "ExchangeResults");
        write_json(
            &mut ctrl,
            r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":4096,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
        );
        read_json(&mut ctrl); // the server's results
        assert_eq!(read_exact(&mut ctrl, 1)[0], 14, "DisplayResults");
        parked_w.store(true, Ordering::SeqCst);
        // Never send IperfDone; hold both sockets open.
        std::thread::sleep(Duration::from_secs(15));
        drop(data);
    });

    let t0 = Instant::now();
    while !parked.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "mock never parked the server"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300));
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    let (sout, _serr, scode) =
        wait_with_output_bounded(server, Duration::from_secs(2), "server IperfDone wedge");
    assert_eq!(scode, 0, "signal-normal exit despite the missing IperfDone");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert!(
        doc["error"]
            .as_str()
            .is_some_and(|e| e.contains("terminated")),
        "the sigend dump carries the interrupt error key: {doc}"
    );
}

/// #319 (sibling of #268): the SERVER's exchange reads must not outlive a
/// signal when the CLIENT wedges mid-results-payload. Pre-fix the server
/// hung in recv_results past SIGTERM.
#[test]
fn server_survives_a_mid_exchange_client_wedge() {
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let port = free_port();
    let ps = port.to_string();
    let server = spawn(&["-s", "-1", "-p", &ps, "-J"]);
    std::thread::sleep(Duration::from_millis(400));

    let wedged = Arc::new(AtomicBool::new(false));
    let wedged_w = wedged.clone();
    let _mock = std::thread::spawn(move || {
        let read_exact = |s: &mut std::net::TcpStream, n: usize| -> Vec<u8> {
            let mut b = vec![0u8; n];
            s.read_exact(&mut b).expect("mock read");
            b
        };
        let write_json = |s: &mut std::net::TcpStream, payload: &str| {
            s.write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
            s.write_all(payload.as_bytes()).unwrap();
        };
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        write_json(
            &mut ctrl,
            r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#,
        );
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1, "TestStart");
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2, "TestRunning");
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(read_exact(&mut ctrl, 1)[0], 13, "ExchangeResults");
        // The wedge: announce 500 bytes of results, deliver 40, stall.
        ctrl.write_all(&500u32.to_be_bytes()).unwrap();
        ctrl.write_all(&[b'{'; 40]).unwrap();
        wedged_w.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_secs(15));
        drop(data);
    });

    let t0 = Instant::now();
    while !wedged.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "mock never reached the wedge"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300)); // park in the bulk read
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    // #322 r1 F2: 2s pins the GRACEFUL exit — the CLI's #211 bailout is 5s,
    // so a revert that only exits via the bailout goes red here.
    let (sout, _serr, scode) =
        wait_with_output_bounded(server, Duration::from_secs(2), "server mid-exchange wedge");
    assert_eq!(scode, 0, "signal-normal exit despite the wedged read");
    let doc: serde_json::Value =
        serde_json::from_str(sout.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sout}"));
    assert!(
        doc["error"]
            .as_str()
            .is_some_and(|e| e.contains("terminated")),
        "the sigend dump carries the interrupt error key: {doc}"
    );
}

/// #346 (M4 gate): a server SIGTERM'd MID-TEST emits EXACTLY ONE document —
/// the round's partial report already carried the message (#210), and the
/// serve loop's idle-interrupt skeleton (#346) must NOT append a second doc
/// when the interrupted round produced a report. Two concatenated docs
/// break every JSON consumer.
#[cfg(unix)]
#[test]
fn server_sigterm_mid_test_emits_exactly_one_doc() {
    let ps = free_port().to_string();
    let server = spawn(&["-s", "-1", "-p", &ps, "-J"]);
    std::thread::sleep(Duration::from_millis(300));
    let client = spawn(&["-c", "127.0.0.1", "-p", &ps, "-t", "8", "-J"]);
    std::thread::sleep(Duration::from_secs(2));

    let spid = server.0.id() as i32;
    unsafe {
        libc::kill(spid, libc::SIGTERM);
    }
    let (sout, serr, scode) = wait_with_output_bounded(server, Duration::from_secs(8), "server");
    let _ = wait_with_output_bounded(client, Duration::from_secs(8), "client");

    assert_eq!(scode, 0, "signal-normal exit");
    assert!(serr.trim().is_empty(), "-J keeps stderr silent: {serr:?}");
    // from_str on the WHOLE stdout: a second appended doc fails the parse.
    let doc: serde_json::Value = serde_json::from_str(sout.trim())
        .expect("server stdout is EXACTLY ONE JSON document");
    assert!(
        doc["error"].is_string(),
        "the single doc carries the interrupt/terminate key: {doc}"
    );
}
