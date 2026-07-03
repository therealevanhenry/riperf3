//! #316: `--gsro` end-to-end — UDP GSO/GRO on both roles.
//!
//! On Linux these runs set real UDP_SEGMENT/UDP_GRO sockopts over loopback,
//! so a GRO-coalesced read can hand the receiver a multi-datagram train; the
//! zero-loss asserts are the phantom-loss regression net (the #327 r1
//! failure: 97% "loss" from parsing one header per read). Elsewhere the
//! probes fail like GT's `#else` stubs and the run must still complete with
//! the echo reading 0.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

mod common;

use common::{udp_serial, wait_bounded, ChildGuard};

/// One `--gsro` UDP run against a one-off server; returns the client's `-J`
/// doc. `demux` forces the server's shared-socket path (the recycling path
/// is the Unix default). `-P 2` EXERCISES the per-accept probe path but
/// cannot discriminate a pre-loop-only regression (r2 F2: a GRO-off second
/// stream still walks uncoalesced datagrams to zero loss and the echo
/// still folds 1) — the sockopt round-trip pin in net.rs plus code review
/// carry the placement.
fn run_gsro(demux: bool, who: &str) -> Value {
    let _serial = udp_serial();
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = common::free_port().to_string();

    let mut cmd = Command::new(bin);
    cmd.args(["-s", "-1", "-p", &port])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if demux {
        cmd.env("RIPERF3_UDP_SERVER_DEMUX", "1");
    }
    let mut server = ChildGuard(cmd.spawn().unwrap_or_else(|e| panic!("{who}: server: {e}")));

    let retry_deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        let client = common::run_client(
            &[
                "-c",
                "127.0.0.1",
                "-p",
                &port,
                "-u",
                "--gsro",
                "-t",
                "2",
                "-P",
                "2",
                "-J",
            ],
            Duration::from_secs(20),
            who,
        );
        let combined = format!("{}\n{}", client.stderr, client.stdout);
        if common::refused(&client.status, &combined) && Instant::now() < retry_deadline {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        assert!(
            client.status.success(),
            "{who}: client exited non-zero: {:?}\nstderr: {}\n{}",
            client.status,
            client.stderr,
            client.stdout
        );
        break client.stdout;
    };
    let _ = wait_bounded(&mut server.0, Duration::from_secs(5));
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{who}: not JSON ({e}): {out}"))
}

fn assert_gsro_run(doc: &Value, who: &str) {
    // The echo is POST-probe (GT zeroes settings->gso/gro on a failed
    // setsockopt, iperf_udp.c:459-515): 1 where both sockopts exist
    // (Linux), 0 where the stubs fail like GT's #else arms.
    let expect = i64::from(cfg!(target_os = "linux"));
    let ts = &doc["start"]["test_start"];
    assert_eq!(ts["gso"].as_i64(), Some(expect), "{who}: gso echo: {ts}");
    assert_eq!(ts["gro"].as_i64(), Some(expect), "{who}: gro echo: {ts}");

    // The phantom-loss net: with UDP_GRO live on the receiver, coalesced
    // trains must walk at the blksize stride, not book sequence gaps.
    let sum = &doc["end"]["sum"];
    assert!(
        sum["packets"].as_i64().is_some_and(|p| p > 0),
        "{who}: no packets moved: {sum}"
    );
    assert_eq!(
        sum["lost_packets"].as_i64(),
        Some(0),
        "{who}: phantom loss under GSO/GRO (#316): {sum}"
    );
}

#[test]
fn gsro_recycling_path_zero_loss_and_honest_echo() {
    let doc = run_gsro(false, "gsro recycling");
    assert_gsro_run(&doc, "gsro recycling");
}

#[test]
fn gsro_demux_path_zero_loss_and_honest_echo() {
    let doc = run_gsro(true, "gsro demux");
    assert_gsro_run(&doc, "gsro demux");
}

/// GT's usage line, verbatim (iperf_locale.c:222).
#[test]
fn gsro_help_line_matches_gt() {
    let out = Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .arg("--help")
        .output()
        .expect("--help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("enable UDP GSO/GRO on both client and server (client-only option)"),
        "GT's locale line for --gsro: {help}"
    );
}

/// GT warns at parse end when --gsro rides a client without local support
/// (iperf_api.c:1830-1839) — reachable only off-Linux in riperf3, where
/// both sockopts are absent (GT's both-missing arm). On Linux the client
/// must stay silent.
#[test]
fn gsro_platform_warning_matches_gt() {
    let _serial = udp_serial();
    let port = common::free_port().to_string();
    // No server: the run fails to connect, but the parse-time warning (or
    // its absence) is already decided before the connect attempt.
    let client = common::run_client(
        &["-c", "127.0.0.1", "-p", &port, "-u", "--gsro", "-t", "1"],
        Duration::from_secs(15),
        "gsro warning",
    );
    let warning = "warning: --gsro requested but UDP GSO/GRO not supported on this client; \
                   will only be enabled on server if supported";
    if cfg!(target_os = "linux") {
        assert!(
            !client.stderr.contains("warning: --gsro"),
            "no platform warning on Linux: {}",
            client.stderr
        );
    } else {
        assert!(
            client.stderr.contains(warning),
            "GT's parse-time warning off-Linux: {}",
            client.stderr
        );
    }
}
