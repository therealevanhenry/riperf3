//! #241/#242 — the `-f` format surface, live text shapes (iperf 3.21 GT,
//! captured 2026-06-11):
//!
//! - UPPERCASE letters are byte-rates: `-f K` prints `1278 KBytes/sec` for a
//!   10 Mbit/s run (unit_snprintf converts bytes with 1024 divisors and only
//!   multiplies by 8 for lowercase, units.c:299-344).
//! - The Transfer column stays adaptive ('A') regardless of -f; the flag
//!   drives only the Bitrate column (#221's rule, both roles).
//! - The SERVER honors its own -f the same way (live: a `-f K` GT server
//!   prints `KBytes/sec` on its interval and receiver rows) — riperf3's
//!   server silently ignored -f entirely (#242).

use std::process::{Command, Stdio};
use std::time::Duration;

mod common;
use common::ChildGuard;

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

fn collect(mut child: ChildGuard, who: &str) -> String {
    use std::io::Read;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while child.0.try_wait().expect("try_wait").is_none() {
        assert!(std::time::Instant::now() < deadline, "{who}: did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    if let Some(mut s) = child.0.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    out
}

/// One run, both pins: a `-f K` server renders BYTE-rates on its own rows
/// (#242: the flag must reach the render sites at all; #241: uppercase must
/// mean KBytes/sec) while the `-f m` client renders Mbits/sec — each side's
/// own flag, independently, like iperf3.
#[test]
fn server_and_client_each_honor_their_own_format() {
    let ps = common::free_port().to_string();
    let server = spawn_server(&["-f", "K"], &ps);
    std::thread::sleep(Duration::from_millis(300));

    let client = common::run_client(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-t",
            "1",
            "-b",
            "10M",
            "-f",
            "m",
        ],
        Duration::from_secs(30),
        "client -f m",
    );
    let sout = collect(server, "server -f K");

    assert_eq!(client.status.code(), Some(0), "{}", client.stderr);

    // Client: its own -f m everywhere a rate renders; no adaptive leakage.
    assert!(
        client.stdout.contains(" Mbits/sec"),
        "client -f m renders Mbits/sec rows: {out}",
        out = client.stdout
    );
    assert!(
        !client.stdout.contains(" KBytes/sec"),
        "the server's format must not leak into the client: {out}",
        out = client.stdout
    );

    // Server: -f K = byte-rates on interval AND summary rows (GT live shape
    // `1278 KBytes/sec`), Transfer column still adaptive Bytes.
    assert!(
        sout.contains(" KBytes/sec"),
        "server -f K renders KBytes/sec rows (#242 wiring + #241 case): {sout}"
    );
    assert!(
        !sout.contains(" Kbits/sec") && !sout.contains(" Gbits/sec"),
        "no bit-rate rows on a -f K server: {sout}"
    );
    assert!(
        sout.contains("MBytes  ") || sout.contains("KBytes  "),
        "the Transfer column stays adaptive bytes (#221's always-'A' rule): {sout}"
    );
}

/// Case is semantic end-to-end: `-f k` (bits) and `-f K` (bytes) produce
/// different units on the same run shape. Today clap's ignore_case collapses
/// them to the same enum variant.
#[test]
fn client_uppercase_k_is_byte_rate_lowercase_is_bit_rate() {
    for (flag, want, reject) in [
        ("k", " Kbits/sec", " KBytes/sec"),
        ("K", " KBytes/sec", " Kbits/sec"),
    ] {
        let ps = common::free_port().to_string();
        let server = spawn_server(&[], &ps);
        std::thread::sleep(Duration::from_millis(300));

        let client = common::run_client(
            &[
                "-c",
                "127.0.0.1",
                "-p",
                &ps,
                "-t",
                "1",
                "-b",
                "10M",
                "-f",
                flag,
            ],
            Duration::from_secs(30),
            "client -f case",
        );
        let _ = collect(server, "server");

        assert_eq!(client.status.code(), Some(0), "{}", client.stderr);
        assert!(
            client.stdout.contains(want),
            "-f {flag} must render {want}: {out}",
            out = client.stdout
        );
        assert!(
            !client.stdout.contains(reject),
            "-f {flag} must NOT render {reject}: {out}",
            out = client.stdout
        );
    }
}
