//! #246 — the server's `-V` placeholder rows for the unmeasured half
//! (iperf 3.21 GT, live-captured 2026-06-11; iperf_locale.c:468-471):
//!
//!   [  5] (sender statistics not available)     <- before a receiver row
//!   [  5] (receiver statistics not available)   <- after a sender row
//!   [SUM] (sender statistics not available)     <- the P>1 aggregate twin
//!
//! Every GT site is gated on `test->verbose` (iperf_api.c:4280/4324/4371/
//! 4395, SUM :4451): a NON-verbose server prints nothing for the unmeasured
//! half (riperf3 already matched that), and the client never prints
//! placeholders at all — it measures/exchanges both halves.

use std::process::{Command, Stdio};
use std::time::Duration;

mod common;
use common::ChildGuard;

const SENDER_NA: &str = "] (sender statistics not available)";
const RECEIVER_NA: &str = "] (receiver statistics not available)";

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

fn run(server_extra: &[&str], client_extra: &[&str]) -> (String, String) {
    let ps = common::free_port().to_string();
    let server = spawn_server(server_extra, &ps);
    std::thread::sleep(Duration::from_millis(300));
    let mut args = vec!["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-b", "5M"];
    args.extend_from_slice(client_extra);
    let client = common::run_client(&args, Duration::from_secs(30), "client");
    let sout = collect(server, "server");
    assert_eq!(client.status.code(), Some(0), "{}", client.stderr);
    (sout, client.stdout)
}

/// The final block of the server's output (after the dashed separator).
fn final_block(sout: &str) -> &str {
    sout.rsplit("- - - - - - -")
        .next()
        .expect("final summary separator")
}

/// Forward -V: GT prints the sender placeholder BETWEEN the header and the
/// receiver row (the sender row's canonical slot).
#[test]
fn verbose_forward_server_prints_sender_placeholder_before_receiver_row() {
    let (sout, _) = run(&["-V"], &[]);
    let block = final_block(&sout);
    let ph = block
        .find(SENDER_NA)
        .unwrap_or_else(|| panic!("no sender placeholder in the -V final block: {block}"));
    let recv_row = block
        .find("            receiver")
        .or_else(|| block.find("  receiver"))
        .expect("receiver row");
    assert!(
        ph < recv_row,
        "the placeholder takes the sender row's slot, BEFORE the receiver row: {block}"
    );
    assert!(
        !block.contains(RECEIVER_NA),
        "the measured half gets no placeholder: {block}"
    );
}

/// Reverse -V: the receiver placeholder comes AFTER the sender row.
#[test]
fn verbose_reverse_server_prints_receiver_placeholder_after_sender_row() {
    let (sout, _) = run(&["-V"], &["-R"]);
    let block = final_block(&sout);
    let ph = block
        .find(RECEIVER_NA)
        .unwrap_or_else(|| panic!("no receiver placeholder in the -V final block: {block}"));
    let send_row = block.find("  sender").expect("sender row");
    assert!(
        ph > send_row,
        "the placeholder takes the receiver row's slot, AFTER the sender row: {block}"
    );
    assert!(!block.contains(SENDER_NA), "{block}");
}

/// P=2 -V: one placeholder per stream plus the [SUM] twin, each in its
/// direction's slot (GT live: `[SUM] (sender statistics not available)`
/// directly before the SUM receiver row).
#[test]
fn verbose_parallel_server_prints_per_stream_and_sum_placeholders() {
    let (sout, _) = run(&["-V"], &["-P", "2"]);
    let block = final_block(&sout);
    assert_eq!(
        block.matches(SENDER_NA).count(),
        3,
        "two per-stream placeholders + the SUM twin: {block}"
    );
    let sum_lines: Vec<&str> = block.lines().filter(|l| l.starts_with("[SUM]")).collect();
    assert_eq!(
        sum_lines.len(),
        2,
        "exactly the SUM placeholder + the SUM receiver row: {block}"
    );
    assert!(
        sum_lines[0].contains(SENDER_NA) && sum_lines[1].contains("receiver"),
        "the SUM placeholder precedes the SUM receiver row (GT order): {block}"
    );
}

/// The verbose gate: a PLAIN server prints no placeholder (GT same), and a
/// -V CLIENT never prints one on either side (it measures both halves).
#[test]
fn placeholders_are_verbose_only_and_server_only() {
    let (sout, _) = run(&[], &[]);
    assert!(
        !sout.contains("statistics not available"),
        "non-verbose server prints nothing for the unmeasured half: {sout}"
    );

    let (_, cout) = run(&[], &["-V"]);
    assert!(
        !cout.contains("statistics not available"),
        "the client never prints placeholders: {cout}"
    );
}

/// r1 blocker: GT's UDP summary-sum block has NO placeholder branch
/// (iperf_api.c:4517-4538 silently skips the unmeasured half's SUM row;
/// only the TCP/SCTP block has the [SUM] twins at :4451/:4463/:4483) — a
/// UDP -P 2 -V server prints per-stream placeholders but NEVER a [SUM] one.
#[test]
fn verbose_udp_parallel_server_prints_no_sum_placeholder() {
    let (sout, _) = run(&["-V"], &["-u", "-P", "2"]);
    let block = final_block(&sout);
    assert_eq!(
        block.matches(SENDER_NA).count(),
        2,
        "per-stream UDP placeholders only — GT prints no UDP [SUM] twin: {block}"
    );
    assert!(
        !block.contains(&format!("[SUM{SENDER_NA}")),
        "no [SUM] placeholder on UDP (GT's UDP sum block has no such branch): {block}"
    );
    assert!(
        block.lines().any(|l| l.starts_with("[SUM]") && l.contains("receiver")),
        "the measured UDP SUM receiver row still prints: {block}"
    );
}

/// Bidir -V: both placeholder kinds appear, each adjacent to its stream's
/// measured row, and placeholder lines carry NO bidir role tag — GT prints
/// a plain `[  5] (sender statistics not available)`, never `[  5][RX-S]`
/// (live capture; r1 mutation (c) survived without this pin).
#[test]
fn verbose_bidir_server_placeholders_carry_no_role_tag() {
    let (sout, _) = run(&["-V"], &["--bidir"]);
    let block = final_block(&sout);
    let phs: Vec<&str> = block
        .lines()
        .filter(|l| l.contains("statistics not available"))
        .collect();
    assert_eq!(
        phs.len(),
        2,
        "one placeholder per direction (sender twin for the RX stream, \
         receiver twin for the TX stream): {block}"
    );
    for ph in &phs {
        assert!(
            !ph.contains("][") && !ph.contains("-S]"),
            "placeholder rows carry NO role tag (GT shape): {ph}"
        );
    }
    assert!(
        block.contains(SENDER_NA) && block.contains(RECEIVER_NA),
        "both directions get their twin: {block}"
    );
}
