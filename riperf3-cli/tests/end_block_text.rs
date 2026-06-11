//! #184: the client's text end block must print iperf3's per-stream
//! sender/receiver line PAIRS in every mode, with iperf3's sender-line
//! datagram stats (`0.000 ms  0/<sent>`), role tags in bidir, and no
//! cross-direction `[SUM]`. Ground truth: iperf3 3.20 on the sandbox fleet
//! (captured on #184) — the client end block pairs both halves of every
//! stream (local + peer-reported), while the server prints one line per
//! stream in its own role only.

use std::process::{Command, Stdio};
use std::time::Duration;

mod common;

// The #191 per-binary UDP serialization lock and the child reaper now live in
// riperf3-test-support (#192); semantics unchanged (statics are per linked
// test binary).
use common::{udp_serial, ChildGuard};

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

/// The end block: every line after the `- - -` separator.
fn end_block(out: &str) -> Vec<&str> {
    let sep = out
        .lines()
        .position(|l| l.starts_with("- - -"))
        .unwrap_or_else(|| panic!("no end-block separator in: {out}"));
    out.lines()
        .skip(sep + 1)
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn run(port: &str, args: &[&str]) -> String {
    let mut full = vec!["-c", "127.0.0.1", "-p", port];
    full.extend_from_slice(args);
    common::run_client_ok(&full, Duration::from_secs(20), "client").stdout
}

/// UDP bidir: per stream a sender AND a receiver line (4 stream lines), the
/// sender lines carrying `0/<sent>` datagram totals — not blank columns — and
/// no `[SUM]` rows at P=1 (iperf3 prints none in the bidir end block).
#[test]
fn udp_bidir_end_block_pairs_each_stream() {
    let _serial = udp_serial();
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["-u", "--bidir", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    let senders: Vec<&&str> = end.iter().filter(|l| l.ends_with("sender")).collect();
    let receivers: Vec<&&str> = end.iter().filter(|l| l.ends_with("receiver")).collect();
    assert_eq!(
        (senders.len(), receivers.len()),
        (2, 2),
        "bidir end block must pair both streams (2 sender + 2 receiver lines): {out}"
    );
    assert!(
        !end.iter().any(|l| l.contains("SUM")),
        "iperf3 prints no [SUM] in a P=1 bidir end block: {out}"
    );
    for s in senders {
        assert!(
            s.contains("ms") && s.contains('/') && !s.contains("0/0 "),
            "sender lines carry `0.000 ms 0/<sent>` datagram stats with a real \
             sent total, not blank columns or 0/0: {s}"
        );
    }
    // Bidir lines are role-tagged like iperf3 ([TX-C]/[RX-C]).
    assert!(
        end.iter().any(|l| l.contains("TX-C")) && end.iter().any(|l| l.contains("RX-C")),
        "bidir end-block lines carry role tags: {out}"
    );
}

/// UDP reverse: the client's lone receiving stream still yields a PAIR — the
/// sender line comes from the server's results (its sent bytes/datagrams).
#[test]
fn udp_reverse_end_block_has_sender_line() {
    let _serial = udp_serial();
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["-u", "-R", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    let sender = end
        .iter()
        .find(|l| l.ends_with("sender"))
        .unwrap_or_else(|| {
            panic!("reverse end block must include the server's sender line: {out}")
        });
    assert!(
        sender.contains("ms") && sender.contains('/') && !sender.contains("0/0 "),
        "the sender line carries the peer's REAL sent datagram total — a 0/0 \
         means the exchange did not fill sender packets (#184): {sender}"
    );
    assert!(
        end.iter().any(|l| l.ends_with("receiver")),
        "and the local receiver line stays: {out}"
    );
}

/// UDP forward keeps its existing pair, and the sender line gains the
/// `0/<sent>` total instead of blank columns.
#[test]
fn udp_forward_sender_line_carries_sent_total() {
    let _serial = udp_serial();
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["-u", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    let sender = end
        .iter()
        .find(|l| l.ends_with("sender"))
        .unwrap_or_else(|| panic!("forward end block must include the sender line: {out}"));
    assert!(
        sender.contains("ms") && sender.contains('/'),
        "sender line shows `0.000 ms 0/<sent>` like iperf3: {sender}"
    );
    assert!(
        end.iter().any(|l| l.ends_with("receiver")),
        "pair intact: {out}"
    );
}

/// TCP reverse: the client's lone receiving stream pairs with the server's
/// sender line (its bytes + Retr from the exchange), exercising the `!is_udp`
/// peer-sender branch that UDP reverse does not.
#[test]
fn tcp_reverse_end_block_pairs_with_server_sender() {
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["-R", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    assert!(
        end.iter().any(|l| l.ends_with("sender")),
        "TCP reverse end block must include the server's sender line: {out}"
    );
    assert!(
        end.iter().any(|l| l.ends_with("receiver")),
        "and the local receiver line: {out}"
    );
}

/// `-P 2 --bidir`: four streams, four pairs, and exactly four direction-pure
/// `[SUM]` rows — one per (role, line-direction) group — never a SUM mixing the
/// two directions (the sum_summaries role grouping, #184). TCP, deliberately:
/// the role grouping is protocol-agnostic, and a 4-stream UDP run here starves
/// the async connect handshake under the parallel harness on a 2-core runner
/// (the #178 thread-contention family), hitting the 30 s UDP-connect budget and
/// resetting the control connection. TCP's kernel handshake has no such load.
#[test]
fn parallel_bidir_sums_are_direction_pure() {
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["--bidir", "-P", "2", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    let sums: Vec<&&str> = end.iter().filter(|l| l.contains("SUM")).collect();
    assert_eq!(
        sums.len(),
        4,
        "P=2 bidir yields 4 SUM rows (TX-C sender+receiver, RX-C sender+receiver): {out}"
    );
    for s in &sums {
        assert!(
            s.contains("TX-C") || s.contains("RX-C"),
            "every bidir SUM carries one direction's role tag: {s}"
        );
    }
}

/// TCP bidir: pairs too — Retr on sender lines only, no [SUM] at P=1, role tags.
#[test]
fn tcp_bidir_end_block_pairs_each_stream() {
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);
    let out = run(&port, &["--bidir", "-t", "1"]);
    let _ = server.0.wait();

    let end = end_block(&out);
    let senders = end.iter().filter(|l| l.ends_with("sender")).count();
    let receivers = end.iter().filter(|l| l.ends_with("receiver")).count();
    assert_eq!(
        (senders, receivers),
        (2, 2),
        "TCP bidir end block must pair both streams: {out}"
    );
    assert!(
        !end.iter().any(|l| l.contains("SUM")),
        "no [SUM] in a P=1 bidir end block: {out}"
    );
    assert!(
        end.iter().any(|l| l.contains("TX-C")) && end.iter().any(|l| l.contains("RX-C")),
        "bidir end-block lines carry role tags: {out}"
    );
}
