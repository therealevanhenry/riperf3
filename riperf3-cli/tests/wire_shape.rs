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

/// #325: GT honors CLIENT_TERMINATE at any message point — a terminate
/// landing in the END loop (after DisplayResults) still dumps the
/// client-terminated shape (iperf_server_api.c:289-308), where the old
/// tolerant arm swallowed it and reported clean success.
#[test]
fn end_loop_client_terminate_takes_the_terminated_shape() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port, "-J"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let sdoc_reader =
        riperf3_test_support::drain_reader(server.0.stdout.take().expect("piped stdout"));
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn({
        let port: u16 = port.parse().unwrap();
        move || {
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
            assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
            write_json(
                &mut ctrl,
                r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#,
            );
            assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
            let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
            data.write_all(&cookie).unwrap();
            assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
            assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
            data.write_all(&[0u8; 4096]).unwrap();
            ctrl.write_all(&[4u8]).unwrap(); // TestEnd
            assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
            write_json(
                &mut ctrl,
                r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":4096,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
            );
            read_json(&mut ctrl); // server results
            assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
                                                         // The end-loop terminate: 12 instead of IperfDone(16).
            ctrl.write_all(&[12u8]).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(5))
            .expect("server exits");
    let sdoc = sdoc_reader.join().expect("stdout");
    let serr = serr_reader.join().expect("stderr");
    let doc: serde_json::Value =
        serde_json::from_str(sdoc.trim()).unwrap_or_else(|e| panic!("one -J doc ({e}): {sdoc}"));
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

/// #325: an UNKNOWN control byte errors with GT's IEMESSAGE sentence
/// (iperf_error.c:302) — GT's state switch has no tolerant default. The
/// byte lands where the server reads STATES (the end loop).
#[test]
fn unknown_control_byte_takes_gt_iemessage() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);

    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );
    let serr_reader =
        riperf3_test_support::drain_reader(server.0.stderr.take().expect("piped stderr"));
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mock = std::thread::spawn({
        let port: u16 = port.parse().unwrap();
        move || {
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
            assert_eq!(read_exact(&mut ctrl, 1)[0], 9);
            write_json(
                &mut ctrl,
                r#"{"tcp":true,"omit":0,"time":1,"num":0,"blockcount":0,"parallel":1,"len":131072,"pacing_timer":1000,"client_version":"riperf3 0.0.0"}"#,
            );
            assert_eq!(read_exact(&mut ctrl, 1)[0], 10);
            let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
            data.write_all(&cookie).unwrap();
            assert_eq!(read_exact(&mut ctrl, 1)[0], 1);
            assert_eq!(read_exact(&mut ctrl, 1)[0], 2);
            data.write_all(&[0u8; 4096]).unwrap();
            ctrl.write_all(&[4u8]).unwrap(); // TestEnd
            assert_eq!(read_exact(&mut ctrl, 1)[0], 13);
            write_json(
                &mut ctrl,
                r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":4096,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
            );
            read_json(&mut ctrl); // server results
            assert_eq!(read_exact(&mut ctrl, 1)[0], 14); // DisplayResults
            ctrl.write_all(&[99u8]).unwrap(); // the unknown byte
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    mock.join().expect("mock");
    let status =
        riperf3_test_support::wait_bounded(&mut server.0, std::time::Duration::from_secs(5))
            .expect("server exits");
    let serr = serr_reader.join().expect("stderr");
    assert!(
        serr.contains(
            "received an unknown control message (ensure other side is iperf3 and not iperf)"
        ),
        "GT's IEMESSAGE sentence: {serr}"
    );
    assert!(!status.success(), "the IEMESSAGE run errors like GT");
}
