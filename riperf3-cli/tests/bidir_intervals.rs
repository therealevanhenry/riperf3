//! CLI integration test: `--bidir -J` intervals must split the two directions
//! like iperf3 — `sum` for the forward (client→server) flow and
//! `sum_bidir_reverse` for the reverse — instead of lumping both into one
//! `sum` (#54). Per-stream interval entries and the end block were already
//! split; this locks in the per-interval aggregates on both roles.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

/// Run the client to completion (with refused-retry) and return its stdout.
fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> String {
    common::run_client_ok(args, timeout, who).stdout
}

fn spawn_server(port_str: &str) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", port_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    )
}

/// Every interval must carry the direction-split pair with the expected sender
/// flags; returns the (forward, reverse) byte totals across all intervals.
fn assert_split_intervals(v: &Value, fwd_sender: bool, who: &str) -> (u64, u64) {
    let intervals = v["intervals"]
        .as_array()
        .unwrap_or_else(|| panic!("{who}: missing intervals array: {v}"));
    assert!(!intervals.is_empty(), "{who}: no intervals: {v}");
    let (mut fwd_bytes, mut rev_bytes) = (0u64, 0u64);
    for (n, i) in intervals.iter().enumerate() {
        assert_eq!(
            i["sum"]["sender"].as_bool(),
            Some(fwd_sender),
            "{who}: interval {n} sum.sender: {i}"
        );
        let rev = i
            .get("sum_bidir_reverse")
            .unwrap_or_else(|| panic!("{who}: interval {n} missing sum_bidir_reverse: {i}"));
        assert_eq!(
            rev["sender"].as_bool(),
            Some(!fwd_sender),
            "{who}: interval {n} sum_bidir_reverse.sender: {rev}"
        );
        fwd_bytes += i["sum"]["bytes"].as_u64().unwrap_or(0);
        rev_bytes += rev["bytes"].as_u64().unwrap_or(0);
    }
    (fwd_bytes, rev_bytes)
}

/// Client `--bidir -J`: forward (its senders) in `sum` with sender=true, the
/// reverse flow in `sum_bidir_reverse` with sender=false and no retransmits.
#[test]
fn tcp_bidir_client_intervals_split_directions() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "--bidir",
            "-t",
            "2",
            "-i",
            "1",
            "-J",
        ],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client --bidir -J is not valid JSON ({e}): {out}"));
    let (fwd, rev) = assert_split_intervals(&v, true, "client");
    assert!(fwd > 0, "no forward bytes across intervals: {out}");
    assert!(rev > 0, "no reverse bytes across intervals: {out}");

    // A received-flow sum never carries a retransmit count, on any platform.
    for i in v["intervals"].as_array().unwrap() {
        assert!(
            i["sum_bidir_reverse"].get("retransmits").is_none(),
            "receive-direction sum must omit retransmits: {i}"
        );
    }

    // Per-stream interval entries cover both directions.
    let senders: Vec<bool> = v["intervals"][0]["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["sender"].as_bool().unwrap())
        .collect();
    assert!(
        senders.contains(&true) && senders.contains(&false),
        "interval streams should span both directions: {senders:?}"
    );
}

/// Server `-s -J` for the same bidir run: its forward flow is the received one,
/// so the roles flip — `sum` sender=false, `sum_bidir_reverse` sender=true.
#[test]
fn tcp_bidir_server_intervals_split_directions() {
    let port = free_port();
    let ps = port.to_string();
    let bin = env!("CARGO_BIN_EXE_riperf3");

    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-J", "-p", &ps])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    let _client = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "--bidir",
            "-t",
            "2",
            "-i",
            "1",
        ],
        Duration::from_secs(20),
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

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("server --bidir -J is not valid JSON ({e}): {out}"));
    let (fwd, rev) = assert_split_intervals(&v, false, "server");
    assert!(fwd > 0, "no forward bytes across intervals: {out}");
    assert!(rev > 0, "no reverse bytes across intervals: {out}");
}

/// UDP bidir: each direction's sum carries that direction's UDP detail — the
/// receiving flow reports measured jitter/loss, the sending flow only a sent
/// packet count, exactly like iperf3.
#[test]
fn udp_bidir_interval_sums_split_udp_stats() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-u",
            "--bidir",
            "-t",
            "2",
            "-i",
            "1",
            "-J",
        ],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -u --bidir -J is not valid JSON ({e}): {out}"));
    let (fwd, rev) = assert_split_intervals(&v, true, "client");
    assert!(fwd > 0 && rev > 0, "bytes must flow both ways: {out}");

    for i in v["intervals"].as_array().unwrap() {
        let sum = &i["sum"];
        // Client's forward direction sends: packet count only.
        assert!(sum.get("packets").is_some(), "sending sum has packets: {i}");
        assert!(
            sum.get("jitter_ms").is_none() && sum.get("lost_packets").is_none(),
            "sending sum must not carry measured receive stats: {i}"
        );
        // Reverse direction receives: measured jitter/loss.
        let rev = &i["sum_bidir_reverse"];
        assert!(
            rev.get("jitter_ms").is_some() && rev.get("lost_packets").is_some(),
            "receiving sum must carry measured stats: {i}"
        );
    }
}

/// #182: the client's end-block entry for its UDP *sending* stream must carry
/// the peer-measured datagram stats — iperf3 attaches the server's
/// receiver-side packets/jitter/loss to the sender entry in bidir exactly as
/// it does in forward mode (verified against iperf3 3.20 ground truth).
/// Pre-fix, a bidir sender matched neither stats source and reported a
/// zero-filled udp block: `packets: 0` despite real bytes.
#[test]
fn udp_bidir_sender_end_stream_carries_peer_measured_stats() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let out = run_capturing(
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
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -u --bidir -J is not valid JSON ({e}): {out}"));
    let streams = v["end"]["streams"]
        .as_array()
        .unwrap_or_else(|| panic!("missing end.streams: {v}"));
    assert_eq!(streams.len(), 2, "bidir run has two streams: {v}");

    for st in streams {
        let u = &st["udp"];
        let sender = u["sender"].as_bool().expect("udp.sender");
        let bytes = u["bytes"].as_u64().expect("udp.bytes");
        let packets = u["packets"].as_i64().expect("udp.packets");
        assert!(bytes > 0, "stream moved no bytes: {st}");
        // packets > 0 is the discriminator: pre-fix the sender entry was a
        // zero-filled block (jitter_ms/lost_packets keys are serialized
        // unconditionally, so key presence can't tell measured from filled).
        // The sender's values are the peer's receiver-side measurements,
        // matching iperf3's JSON.
        assert!(
            packets > 0,
            "end-block {} stream reports packets=0 despite bytes={bytes} (#182): {st}",
            if sender { "sender" } else { "receiver" },
        );
    }
}

/// A plain forward run must not grow a `sum_bidir_reverse` key.
#[test]
fn tcp_forward_intervals_have_no_bidir_reverse() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    // `-t 2 -i 1` so at least one boundary tick fires mid-run; a run ending
    // exactly on its only boundary can legitimately emit zero intervals (#55).
    let out = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "2", "-i", "1", "-J"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -J is not valid JSON ({e}): {out}"));
    let intervals = v["intervals"].as_array().expect("intervals");
    assert!(!intervals.is_empty());
    for i in intervals {
        assert!(
            i.get("sum_bidir_reverse").is_none(),
            "forward run must not emit sum_bidir_reverse: {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// #143/#187: bidir TEXT interval rows — role tags, per-direction SUMs, no
// cross-direction aggregate, and no SUM at all when a direction has a single
// stream. Ground truth: iperf3 3.20 (iperf_print_intermediate prints each
// direction's rows + ITS OWN SUM per pass; the SUM is gated on num_streams>1).
// ---------------------------------------------------------------------------

/// `-u --bidir` at P=1, text mode: every interval row carries the stream's
/// role tag; there is NO [SUM] row (one stream per direction); the TX (sender)
/// rows carry a trailing sent-packet count like iperf3's
/// report_bw_udp_sender_format.
#[test]
fn udp_bidir_text_interval_rows_role_tags_no_sum() {
    let port = free_port().to_string();
    let _server = spawn_server(&port);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &port,
            "-u",
            "--bidir",
            "-t",
            "2",
            "-i",
            "1",
        ],
        Duration::from_secs(25),
        "udp bidir text",
    );

    assert!(
        out.contains("][TX-C]") && out.contains("][RX-C]"),
        "bidir interval rows must carry role tags (iperf3 mbuf): {out}"
    );
    assert!(
        out.contains("[ ID][Role] Interval"),
        "the bidir interval header carries the [Role] column \
         (report_bw_*_header_bidir; review r1 n2): {out}"
    );
    // No interval [SUM] at P=1 — iperf3 gates the text SUM on num_streams>1
    // PER DIRECTION; the end block's separator-delimited summary is exempt.
    let interval_section = out.split("- - - - -").next().unwrap_or("");
    assert!(
        !interval_section.contains("[SUM]"),
        "no interval SUM may print at P=1 bidir (cross-direction SUM was the #187 bug): {out}"
    );
    // TX (sender) interval rows end with a sent-packet count (no jitter/loss).
    let tx_data_row = interval_section
        .lines()
        .find(|l| {
            l.contains("][TX-C]") && l.contains("Mbits/sec")
                || l.contains("][TX-C]") && l.contains("bits/sec")
        })
        .unwrap_or_else(|| panic!("no TX-C interval row: {out}"));
    let last = tx_data_row.split_whitespace().last().unwrap_or("");
    assert!(
        last.parse::<u64>().is_ok() || last == "(omitted)",
        "TX interval rows carry iperf3's trailing sent-packet count, got line: {tx_data_row}"
    );
}

/// TCP `--bidir -P 2`, text mode: per-direction [SUM] rows with role tags
/// (one per direction), never a combined cross-direction SUM.
#[test]
fn tcp_bidir_text_interval_sums_split_per_direction() {
    let port = free_port().to_string();
    let _server = spawn_server(&port);
    let out = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &port,
            "--bidir",
            "-P",
            "2",
            "-t",
            "2",
            "-i",
            "1",
        ],
        Duration::from_secs(25),
        "tcp bidir P2 text",
    );

    let interval_section = out.split("- - - - -").next().unwrap_or("");
    assert!(
        interval_section.contains("[SUM][TX-C]") && interval_section.contains("[SUM][RX-C]"),
        "bidir P2 must print one SUM per direction with role tags: {out}"
    );
    for l in interval_section.lines() {
        if l.contains("[SUM]") {
            assert!(
                l.contains("[SUM][TX-C]") || l.contains("[SUM][RX-C]"),
                "every interval SUM must be direction-tagged (no mixed SUM): {l}"
            );
        }
    }

    // iperf3's per-mode pass order: a direction's rows then ITS SUM — the
    // first TX SUM precedes the first RX row (review r1 n1).
    let lines: Vec<&str> = interval_section.lines().collect();
    let first_tx_sum = lines.iter().position(|l| l.contains("[SUM][TX-C]"));
    let first_rx_row = lines
        .iter()
        .position(|l| l.contains("][RX-C]") && !l.contains("[SUM]"));
    if let (Some(ts), Some(rr)) = (first_tx_sum, first_rx_row) {
        assert!(
            ts < rr,
            "SUM placement: TX SUM must close the TX pass before RX rows \
             (iperf3 per-direction pass): {out}"
        );
    }
}
