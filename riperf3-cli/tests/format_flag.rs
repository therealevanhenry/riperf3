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
    // The full bit-rate unit family (r1 blocker: " Mbits/sec" — exactly
    // what the adaptive default renders at -b 10M — was missing, so a
    // reverted render site slipped past with the OTHER site satisfying
    // the positive assert).
    for reject in [
        " Kbits/sec",
        " Mbits/sec",
        " Gbits/sec",
        " Tbits/sec",
        " bits/sec",
    ] {
        assert!(
            !sout.contains(reject),
            "bit-rate row ({reject}) on a -f K server: {sout}"
        );
    }
    assert!(
        sout.contains("MBytes  ") || sout.contains("KBytes  "),
        "the Transfer column stays adaptive bytes (#221's always-'A' rule): {sout}"
    );
}

/// --get-server-output relays the SERVER's own format in the captured block
/// (GT live, r1: a -f K server's relayed rows say KBytes/sec even for a
/// plain client) — pins the capture-path render site, which the two-sided
/// test above cannot see (r1 mutation c1 survived without this).
#[test]
fn get_server_output_carries_the_server_format() {
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
            "--get-server-output",
        ],
        Duration::from_secs(30),
        "client --get-server-output",
    );
    let _ = collect(server, "server -f K");

    assert_eq!(client.status.code(), Some(0), "{}", client.stderr);
    let block = client
        .stdout
        .split("Server output:")
        .nth(1)
        .unwrap_or_else(|| panic!("no Server output block: {}", client.stdout));
    assert!(
        block.contains(" KBytes/sec"),
        "the captured server block renders the SERVER's -f K: {block}"
    );
    for reject in [" Kbits/sec", " Mbits/sec", " Gbits/sec", " bits/sec"] {
        assert!(
            !block.contains(reject),
            "bit-rate leak ({reject}) in the captured block: {block}"
        );
    }
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

/// #263: GT parses only `optarg[0]` (iperf_api.c:1241), so a full word is
/// its first letter — `-f kilobits` renders Kbits/sec rows exactly like
/// `-f k` (live-verified: GT accepts it and fails later on connect, never
/// with a format rejection).
#[test]
fn full_word_format_parses_as_first_char() {
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
            "kilobits",
        ],
        Duration::from_secs(30),
        "client -f kilobits",
    );
    let _ = collect(server, "server");

    assert_eq!(client.status.code(), Some(0), "{}", client.stderr);
    assert!(
        client.stdout.contains(" Kbits/sec"),
        "-f kilobits is -f k: {out}",
        out = client.stdout
    );
}

/// #263: GT warns `warning: Report format (-f) flag ignored with JSON
/// output (-J)` on STDERR when -J rides with an explicit -f
/// (iperf_api.c:2016, warning() prints bare `warning: %s` — no program
/// prefix). The JSON document still lands on stdout. --json-stream sets
/// json_output too (iperf_api.c:1281), so it warns identically. Text mode
/// never warns.
#[test]
fn json_with_format_warns_on_stderr() {
    const WARNING: &str = "warning: Report format (-f) flag ignored with JSON output (-J)";
    let bin = env!("CARGO_BIN_EXE_riperf3");

    // -J client (connect-refused, so the run is short and errors).
    let out = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", "1", "-J", "-f", "k"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.trim(),
        WARNING,
        "the warning is stderr's ONLY line under -J (errors go in the doc)"
    );
    let doc: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim())
            .expect("stdout still carries the JSON document");
    assert!(doc["error"].as_str().is_some());

    // --json-stream implies json_output in GT, so it warns the same way.
    let out = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", "1", "--json-stream", "-f", "m"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.trim(), WARNING, "--json-stream + -f warns too");

    // Text mode: no warning (unit_format simply applies).
    let out = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", "1", "-f", "k"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("warning"),
        "no warning without JSON output: {stderr}"
    );

    // The server role warns at startup too (GT's check is role-agnostic,
    // end of iperf_parse_arguments).
    let ps = common::free_port().to_string();
    let mut server = spawn_server(&["-J", "-f", "k"], &ps);
    std::thread::sleep(Duration::from_millis(400));
    let _ = server.0.kill();
    let mut err = String::new();
    use std::io::Read;
    server
        .0
        .stderr
        .take()
        .expect("piped")
        .read_to_string(&mut err)
        .expect("read server stderr");
    assert!(
        err.contains(WARNING),
        "server -J -f warns at startup: {err:?}"
    );
}
