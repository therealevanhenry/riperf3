//! #222 — the text lines riperf3 never printed, pinned against live iperf
//! 3.21 captures (the pinned interop build, 2026-06-11):
//!
//! 1. `Connecting to host …` / `Accepted connection from …` are
//!    UNCONDITIONAL in iperf3 (iperf_on_connect, iperf_api.c:995/1017);
//!    riperf3 verbose-gated them.
//! 2. The per-stream preamble — `[  5] local 127.0.0.1 port 42660 connected
//!    to 127.0.0.1 port 7786` — prints on both roles.
//! 3. `iperf Done.` closes every clean client run, blank line before it
//!    (iperf_client_api.c:853).
//! 4. The `-V` detail block, in iperf3's order: version line, system info,
//!    `Control connection MSS N` (client), `Time: <RFC1123 GMT>`, banner,
//!    6-space-indented `Cookie: <cookie>`, `TCP MSS: N (default)`, preamble,
//!    `Starting Test: protocol: TCP, N streams, N byte blocks, omitting N
//!    seconds, N second test, tos N`, ticks, separator, `Test Complete.
//!    Summary Results:`, the summary, `CPU Utilization: local/<role> …
//!    remote/<role> …`, and (Linux) snd/rcv_tcp_congestion.
//!
//! JSON modes are unaffected: iperf3's -J prints no text lines.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;
use common::ChildGuard;

fn spawn_server(args: &[&str], port: &str) -> ChildGuard {
    let mut full = vec!["-s", "-1", "-p", port];
    full.extend_from_slice(args);
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(&full)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    )
}

fn finish(mut child: ChildGuard, who: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.0.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "{who}: did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    if let Some(mut s) = child.0.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    out
}

/// The connect banners and per-stream preambles print WITHOUT -V on both
/// roles, and the client run closes with a blank line + `iperf Done.`.
#[test]
fn banners_preamble_and_done_print_unconditionally() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&[], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(40),
        "client",
    );
    let sout = finish(server, "server");

    assert!(
        client
            .stdout
            .contains(&format!("Connecting to host 127.0.0.1, port {ps}")),
        "iperf_on_connect's client banner is unconditional: {out}",
        out = client.stdout
    );
    let preamble_re = |s: &str| {
        s.lines().any(|l| {
            l.starts_with('[')
                && l.contains("] local 127.0.0.1 port ")
                && l.contains(" connected to 127.0.0.1 port ")
        })
    };
    assert!(
        preamble_re(&client.stdout),
        "client per-stream preamble: {out}",
        out = client.stdout
    );
    assert!(
        client.stdout.ends_with("\n\niperf Done.\n"),
        "the run closes with a blank line + 'iperf Done.': {tail:?}",
        tail = &client.stdout[client.stdout.len().saturating_sub(40)..]
    );

    assert!(
        sout.contains("Accepted connection from 127.0.0.1, port "),
        "the server banner is unconditional: {sout}"
    );
    assert!(
        !client.stdout.contains("Reverse mode"),
        "no reverse banner on a forward run: {out}",
        out = client.stdout
    );
    assert!(preamble_re(&sout), "server per-stream preamble: {sout}");
    assert!(
        !sout.contains("iperf Done."),
        "iperf Done. is client-only: {sout}"
    );
}

/// The -V detail block carries iperf3's lines in iperf3's order.
#[test]
fn verbose_detail_block_in_iperf3_order() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&[], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-V"],
        Duration::from_secs(40),
        "client -V",
    );
    let _ = finish(server, "server");
    let out = &client.stdout;

    // Ordered invariant tokens (each must exist AFTER the previous one).
    let tokens = [
        "riperf3 ",                                  // version line opens the output
        "Control connection MSS ",                   // client-side, post-connect
        "Time: ",                                    // RFC1123 GMT
        "Connecting to host 127.0.0.1, ",            // the banner
        "      Cookie: ",                            // 6-space indent, iperf3's shape
        "      TCP MSS: ",                           // ditto
        "] local 127.0.0.1 port ",                   // preamble
        "Starting Test: protocol: TCP, 1 streams, ", // params line
        "Test Complete. Summary Results:",           // -V's summary header prefix
        "CPU Utilization: local/sender ",            // exchanged CPU line
        "iperf Done.",
    ];
    let mut pos = 0usize;
    for t in tokens {
        match out[pos..].find(t) {
            Some(i) => pos += i + t.len(),
            None => panic!("missing or out of order: {t:?} (after byte {pos}) in:\n{out}"),
        }
    }
    // The Time line is RFC1123 GMT like the -J timestamp.
    let time_line = out
        .lines()
        .find(|l| l.starts_with("Time: "))
        .expect("Time: line");
    assert!(
        time_line.ends_with(" GMT"),
        "RFC1123 GMT timestamp: {time_line}"
    );
    // Starting Test parrots the run's parameters.
    assert!(
        out.contains("byte blocks, omitting 0 seconds, 1 second test, tos 0"),
        "the Starting Test parameter tail: {out}"
    );
    // Linux: the congestion algorithm lines (snd before rcv).
    #[cfg(target_os = "linux")]
    {
        let snd = out.find("snd_tcp_congestion ").expect("snd line");
        let rcv = out.find("rcv_tcp_congestion ").expect("rcv line");
        assert!(snd < rcv, "snd before rcv: {out}");
    }

    // And the negative: NONE of the -V-only lines print without -V.
    let ps2 = common::free_port().to_string();
    let server2 = spawn_server(&[], &ps2);
    std::thread::sleep(Duration::from_millis(300));
    let plain = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps2, "-t", "1"],
        Duration::from_secs(40),
        "client plain",
    );
    let _ = finish(server2, "server2");
    for t in [
        "Control connection MSS ",
        "Time: ",
        "Cookie: ",
        "Starting Test:",
        "Test Complete. Summary Results:",
        "CPU Utilization:",
    ] {
        assert!(
            !plain.stdout.contains(t),
            "{t:?} is -V-only (iperf3 parity): {out}",
            out = plain.stdout
        );
    }
}

/// JSON modes stay text-free: the new unconditional lines must not leak
/// into -J stdout (iperf3 -J prints only the document).
#[test]
fn json_mode_carries_no_text_lines() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&[], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-J"],
        Duration::from_secs(40),
        "client -J",
    );
    let _ = finish(server, "server");
    let doc: serde_json::Value = serde_json::from_str(client.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "-J stdout is exactly one document ({e}): {out}",
            out = client.stdout
        )
    });
    assert!(doc["end"].is_object());
}

/// -R: the unconditional "Reverse mode, remote host … is sending" banner
/// (iperf_api.c:995-998), and -V -R prints NO CPU line (GT gates it on the
/// sending side, iperf_api.c:4563).
#[test]
fn reverse_mode_banner_and_no_cpu_line() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&[], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-R", "-V"],
        Duration::from_secs(40),
        "client -R -V",
    );
    let _ = finish(server, "server");
    assert!(
        client
            .stdout
            .contains("Reverse mode, remote host 127.0.0.1 is sending"),
        "{out}",
        out = client.stdout
    );
    assert!(
        !client.stdout.contains("CPU Utilization:"),
        "GT prints no CPU line on the receiving side: {out}",
        out = client.stdout
    );
}
