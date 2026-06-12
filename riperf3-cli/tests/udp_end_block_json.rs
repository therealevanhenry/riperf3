//! #214 — the UDP `-J` end-block aggregate set, live. Ground truth (iperf
//! 3.21, pinned interop build, 2026-06-11): a UDP **bidir** end block carries
//! SIX aggregates — `sum`, `sum_sent`, `sum_received`, `sum_bidir_reverse`,
//! `sum_sent_bidir_reverse`, `sum_received_bidir_reverse` — every one
//! UDP-shaped (packets/lost_packets/lost_percent/jitter_ms), where TCP bidir
//! carries four TCP-shaped ones and no `sum`/`sum_bidir_reverse` at all.
//! The builder-level shapes (sender flags, no-graft zeros, jitter averaging)
//! are pinned by unit tests in json_report.rs; these two tests pin the LIVE
//! wiring end-to-end on both roles.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

mod common;
use common::ChildGuard;

const SIX: [&str; 6] = [
    "sum",
    "sum_sent",
    "sum_received",
    "sum_bidir_reverse",
    "sum_sent_bidir_reverse",
    "sum_received_bidir_reverse",
];

/// Client `-u --bidir -J`: all six aggregates present and UDP-shaped.
#[test]
fn udp_bidir_client_end_has_six_udp_aggregates() {
    let _serial = common::udp_serial();
    let ps = common::free_port().to_string();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let out = common::run_client_ok(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-u",
            "--bidir",
            "-t",
            "1",
            "-J",
        ],
        // > the client's 30 s UDP_CONNECT_TOTAL_TIMEOUT (#195 lesson).
        Duration::from_secs(40),
        "client",
    )
    .stdout;
    let _ = server.0.wait();

    let v: serde_json::Value = serde_json::from_str(&out).expect("client doc parses");
    for key in SIX {
        let agg = &v["end"][key];
        assert!(agg.is_object(), "missing end.{key}: {v}");
        for f in ["packets", "lost_packets", "lost_percent", "jitter_ms"] {
            assert!(
                agg.get(f).is_some(),
                "end.{key} lacks {f} (the #214 tcp_sum leak): {agg}"
            );
        }
    }
    assert_eq!(v["end"]["sum"]["sender"], serde_json::json!(true));
    assert_eq!(
        v["end"]["sum_bidir_reverse"]["sender"],
        serde_json::json!(false)
    );
}

/// Server `-s -J` for the same run: six aggregates with iperf3's strict
/// no-graft zeros — `sum`/`sum_sent` carry 0 bytes for the direction the
/// server only received (the sender-figure rule), `sum_received_bidir_reverse`
/// is all-zero, and the receiving stream's `udp.bytes` is 0 (live-verified
/// iperf3 quirks, mirrored exactly).
#[test]
fn udp_bidir_server_end_no_graft_zeros() {
    let _serial = common::udp_serial();
    let ps = common::free_port().to_string();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-J", "-p", &ps])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let _ = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-u", "--bidir", "-t", "1"],
        Duration::from_secs(40),
        "client",
    );

    let mut out = String::new();
    server
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let _ = server.0.wait();

    let v: serde_json::Value = serde_json::from_str(&out).expect("server doc parses");
    for key in SIX {
        assert!(v["end"][key].is_object(), "missing end.{key}: {v}");
    }
    // The sender-figure rule: fwd direction was only RECEIVED here.
    assert_eq!(v["end"]["sum"]["bytes"], serde_json::json!(0));
    assert_eq!(v["end"]["sum_sent"]["bytes"], serde_json::json!(0));
    assert_eq!(
        v["end"]["sum_received_bidir_reverse"]["bytes"],
        serde_json::json!(0)
    );
    // But the measured fwd stats are real.
    assert!(
        v["end"]["sum_received"]["bytes"].as_u64().unwrap_or(0) > 0,
        "fwd received bytes are measured: {v}"
    );
    assert!(
        v["end"]["sum_bidir_reverse"]["bytes"].as_u64().unwrap_or(0) > 0,
        "reverse sent bytes are real: {v}"
    );
    // Per-stream: the receiving stream reports bytes=0 (sender figure it
    // lacks) while still carrying measured packets.
    let streams = v["end"]["streams"].as_array().expect("streams");
    let recv = streams
        .iter()
        .find(|s| s["udp"]["sender"] == serde_json::json!(false))
        .expect("receiving stream");
    assert_eq!(
        recv["udp"]["bytes"],
        serde_json::json!(0),
        "server receiving stream udp.bytes is 0 (iperf3 quirk): {recv}"
    );
    assert!(
        recv["udp"]["packets"].as_i64().unwrap_or(0) > 0,
        "but measured packets are real: {recv}"
    );
}

/// #235 r2 pin (mutation c): the attach NETS the peer's omitted baseline.
/// Under -O the riperf3 server exchanges gross counts + a nonzero omitted
/// baseline; the client's reverse sent figures must land on the NET count —
/// which equals net bytes / blksize exactly for full-block riperf3 senders —
/// where an un-netted attach reports the gross figure (larger by the omit
/// window's datagrams).
#[test]
fn udp_reverse_omit_run_consumes_the_netted_exchanged_count() {
    let _serial = common::udp_serial();
    let ps = common::free_port().to_string();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let out = common::run_client_ok(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-u",
            "-R",
            "-O",
            "1",
            "-t",
            "1",
            "-b",
            "5M",
            "-J",
        ],
        Duration::from_secs(40),
        "client -u -R -O 1",
    )
    .stdout;
    let _ = server.0.wait();

    let v: serde_json::Value = serde_json::from_str(&out).expect("client doc parses");
    let blksize = v["start"]["test_start"]["blksize"]
        .as_i64()
        .expect("blksize");
    for key in ["sum", "sum_sent"] {
        let bytes = v["end"][key]["bytes"].as_i64().expect("bytes");
        let packets = v["end"][key]["packets"].as_i64().expect("packets");
        assert_eq!(
            packets,
            bytes / blksize,
            "{key}: the exchanged figure must be the NET count (net bytes/blk \
             for a full-block riperf3 sender) — gross would exceed it by the \
             omit window: {v}"
        );
        assert!(packets > 0, "{key}: a real transfer happened: {v}");
    }
}
