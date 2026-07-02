//! CLI integration test for the single-socket UDP server demux (#80).
//!
//! Regression: on native winsock, the server's per-stream connected-socket
//! design (`-P` streams sharing one port via `SO_REUSEADDR`, recycled after each
//! connect) hangs `-P > 1` setup — winsock silently drops a new source's
//! datagram once a connected and a wildcard UDP socket share a port, so streams
//! 2..N never complete their connect handshake and the client retries to its
//! 30 s timeout. The fix binds ONE unconnected server socket and demultiplexes
//! streams by client source address in userspace.
//!
//! The demux path is the default on Windows; the in-process
//! `udp_bidir_parallel_completes` integration test is its red→green there. This
//! test exercises the *same* platform-independent demux code on Unix by forcing
//! it via `RIPERF3_UDP_SERVER_DEMUX=1` on the server child — so the fix is
//! validated on a host the CI Linux runner can actually run. It spawns the real
//! server + client binaries and, for forward / reverse / bidir at `-P 4`,
//! asserts (a) the client completes instead of hanging (the #80 symptom) and
//! (b) every expected stream carries bytes, which a misrouting demux would not
//! produce.
//!
//! Unix-gated: the env override only changes behavior off Windows (on Windows
//! demux is already the default), and the server child plumbing here is Unix.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

// Reaper guard + bounded wait now live in riperf3-test-support (#192).
use common::{udp_serial, wait_bounded, ChildGuard};

/// Run one UDP mode against a demux-forced one-off server and return the client's
/// parsed `-J` report. `extra` carries the direction flag (`-R`, `--bidir`, or
/// nothing for forward).
fn run_demux_udp(extra: &[&str], who: &str) -> Value {
    // #191-class: concurrent UDP-connect handshakes starve each other under
    // load (this binary went from 3 to 4 UDP tests with the #288 pin, and the
    // fourth tipped it over locally). Serialize within the binary.
    let _serial = udp_serial();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port();
    let port_s = port.to_string();

    // One-off server with the demux path forced on (the env var only affects
    // this child). `-1` makes it exit after serving the single test.
    let server = Command::new(bin)
        .args(["-s", "-1", "-p", &port_s])
        .env("RIPERF3_UDP_SERVER_DEMUX", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{who}: spawn server failed: {e}"));
    let mut server = ChildGuard(server);

    // No fixed bind sleep (#177): the client retries on a REFUSED connect for
    // a bounded window instead (the #176 pattern — a refused connect never
    // reaches accept(), so retrying is safe for the one-off server). stderr
    // is captured (was nulled) so refusal is classifiable and failures
    // aren't semi-blind.
    let retry_deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        // Short duration-limited UDP run at -P 4: the #80 hang is in
        // multi-stream setup, so completing at all is the core assertion.
        // `-J` lets us also check routing produced bytes on every stream.
        let mut client = Command::new(bin)
            .args(["-c", "127.0.0.1", "-p", &port_s, "-u", "-t", "2", "-P", "4"])
            .args(extra)
            .arg("-J")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{who}: spawn client failed: {e}"));

        // Capture stdout concurrently, then bound the wait: a #80 regression
        // hangs the client at multi-stream setup until its 30 s connect
        // timeout.
        let stdout = client.stdout.take().expect("client stdout");
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = std::io::BufReader::new(stdout).read_to_string(&mut s);
            s
        });
        let stderr_pipe = client.stderr.take().expect("client stderr");
        let err_reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut s = String::new();
            let _ = std::io::BufReader::new(stderr_pipe).read_to_string(&mut s);
            s
        });

        let exit = wait_bounded(&mut client, Duration::from_secs(20))
            .unwrap_or_else(|| panic!("{who}: client hung — UDP demux -P 4 setup wedged (#80)"));
        let out = reader.join().expect("join stdout reader");
        let err = err_reader.join().expect("join stderr reader");

        // #198: -J error text lands in stdout (the document), stderr empty —
        // scan both for the refused tokens.
        let combined = format!("{err}\n{out}");
        if common::refused(&exit, &combined) && Instant::now() < retry_deadline {
            // Server not listening yet — give it a beat and go again.
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        assert!(
            exit.success(),
            "{who}: client exited non-zero: {exit:?}\nstderr: {err}\n{out}"
        );
        break out;
    };

    // The one-off server should now have served and exited on its own.
    let _ = wait_bounded(&mut server.0, Duration::from_secs(5));

    serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{who}: client -J is not JSON ({e}): {out}"))
}

/// Assert the report has exactly `expected_streams` streams and every one of
/// them moved a nonzero number of UDP bytes — i.e. the demux routed each client
/// to its own stream rather than dropping or collapsing them.
fn assert_all_streams_have_bytes(report: &Value, expected_streams: usize, who: &str) {
    let streams = report["end"]["streams"]
        .as_array()
        .unwrap_or_else(|| panic!("{who}: end.streams is not an array: {report}"));
    assert_eq!(
        streams.len(),
        expected_streams,
        "{who}: expected {expected_streams} streams, got {}",
        streams.len()
    );
    for (i, s) in streams.iter().enumerate() {
        let bytes = s["udp"]["bytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("{who}: stream {i} has no udp.bytes: {s}"));
        assert!(
            bytes > 0,
            "{who}: stream {i} moved 0 bytes (demux misrouted?)"
        );
    }
}

#[test]
fn udp_demux_forward_parallel_completes() {
    let report = run_demux_udp(&[], "forward");
    assert_all_streams_have_bytes(&report, 4, "forward");
}

#[test]
fn udp_demux_reverse_parallel_completes() {
    let report = run_demux_udp(&["-R"], "reverse");
    assert_all_streams_have_bytes(&report, 4, "reverse");
}

#[test]
fn udp_demux_bidir_parallel_completes() {
    // The exact #80 case: 4 receiving + 4 sending streams over one server socket.
    let report = run_demux_udp(&["--bidir"], "bidir");
    assert_all_streams_have_bytes(&report, 8, "bidir");
}

/// #288 (r1 mutation B): the demux server's `-J` `connected[]` must map each
/// stream to the CLIENT's real source port (`peer_addr: Some(client_addr)` in
/// the demux route table), with every local port being the shared demux
/// socket's. A dropped/None peer_addr — or a stream/route mix-up — shows here.
#[test]
fn demux_server_connected_block_maps_streams_to_client_ports() {
    let _serial = udp_serial();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port();
    let port_s = port.to_string();

    let server = Command::new(bin)
        .args(["-s", "-1", "-p", &port_s, "-J"])
        .env("RIPERF3_UDP_SERVER_DEMUX", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = ChildGuard(server);

    let retry_deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        let client = Command::new(bin)
            .args([
                "-c",
                "127.0.0.1",
                "-p",
                &port_s,
                "-u",
                "-t",
                "1",
                "-P",
                "2",
                "-J",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run client");
        // -J puts the refusal text in the stdout DOCUMENT, stderr empty
        // (#198), so classify on BOTH — the stderr-only check silently
        // never retried and failed on the first compile-load-delayed accept.
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&client.stdout),
            String::from_utf8_lossy(&client.stderr)
        );
        if common::refused(&client.status, &combined) && Instant::now() < retry_deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        assert!(client.status.success(), "client failed: {combined}");
        break String::from_utf8_lossy(&client.stdout).into_owned();
    };

    // Server exits after the one test (-1); read its whole doc.
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.0.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut sdoc = String::new();
    use std::io::Read;
    server
        .0
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut sdoc)
        .expect("read server doc");

    let cv: Value = serde_json::from_str(&out).expect("client doc parses");
    let sv: Value = serde_json::from_str(&sdoc).expect("server doc parses");

    let client_local_ports: std::collections::BTreeSet<u64> = cv["start"]["connected"]
        .as_array()
        .expect("client connected[]")
        .iter()
        .map(|c| c["local_port"].as_u64().expect("client local_port"))
        .collect();
    let server_entries = sv["start"]["connected"]
        .as_array()
        .expect("server connected[]");
    assert_eq!(server_entries.len(), 2, "one entry per stream: {sv}");
    let server_remote_ports: std::collections::BTreeSet<u64> = server_entries
        .iter()
        .map(|c| c["remote_port"].as_u64().expect("server remote_port"))
        .collect();
    assert_eq!(
        server_remote_ports, client_local_ports,
        "each server stream maps to a real client source port: {sv}"
    );
    for c in server_entries {
        assert_eq!(
            c["local_port"].as_u64(),
            Some(u64::from(port)),
            "every demux stream shares the one server socket: {sv}"
        );
    }
}
