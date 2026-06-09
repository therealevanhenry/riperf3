//! CLI integration tests: `-O/--omit` must behave like iperf3 (#31), not just
//! tag interval lines. iperf3 runs for `omit + time` seconds, resets statistics
//! at the omit boundary (the interval timeline restarts at 0), and reports the
//! summary over the post-omit window only — so slow-start is genuinely
//! excluded. Pre-#31 riperf3 ran only `-t` seconds and divided cumulative
//! bytes (including warm-up) by the full duration.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const SERVER_BIND_WAIT: Duration = Duration::from_secs(2);

fn run_capturing(args: &[&str], timeout: Duration, who: &str) -> (String, Duration) {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let started = Instant::now();
    let mut child = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{who}: spawn failed: {e}"));

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{who}: timed out");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let elapsed = started.elapsed();
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    (out, elapsed)
}

fn spawn_server(port_str: &str) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let g = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", port_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    std::thread::sleep(SERVER_BIND_WAIT);
    g
}

/// `-O 1 -t 2`: the run lasts omit+time (~3 s wall for the data phase), the
/// summary window is the post-omit `-t` (end.sum_sent.seconds ≈ 2), and the
/// interval timeline restarts at 0 after the warm-up.
#[test]
fn omit_extends_run_and_resets_timeline() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let (out, elapsed) = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-O",
            "1",
            "-t",
            "2",
            "-i",
            "1",
            "-J",
        ],
        Duration::from_secs(25),
        "client",
    );
    let _ = server.0.wait();

    // Run extended by the omit period: data phase is ~3 s, so the client
    // process must live noticeably longer than the unextended 2 s + overhead.
    assert!(
        elapsed >= Duration::from_millis(2800),
        "run must last omit+time like iperf3 (#31); took {elapsed:?}"
    );

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -J is not valid JSON ({e}): {out}"));

    assert_eq!(
        v["start"]["test_start"]["omit"].as_i64(),
        Some(1),
        "test_start.omit: {out}"
    );

    // Summary covers the post-omit window only.
    let secs = v["end"]["sum_sent"]["seconds"].as_f64().expect("seconds");
    assert!(
        (1.5..=2.5).contains(&secs),
        "summary window must be the post-omit -t (≈2 s), got {secs}: {out}"
    );

    // Warm-up intervals are flagged; the timeline restarts at 0 afterward.
    let intervals = v["intervals"].as_array().expect("intervals");
    assert!(!intervals.is_empty());
    assert_eq!(
        intervals[0]["sum"]["omitted"].as_bool(),
        Some(true),
        "first interval is warm-up: {out}"
    );
    let first_real = intervals
        .iter()
        .find(|i| i["sum"]["omitted"] == Value::Bool(false))
        .unwrap_or_else(|| panic!("no post-omit interval: {out}"));
    assert_eq!(
        first_real["sum"]["start"].as_f64(),
        Some(0.0),
        "iperf3 restarts the interval timeline at 0 after the omit boundary: {out}"
    );
}

/// The summary must exclude warm-up bytes: post-omit interval sums must
/// account for (nearly) all reported bytes — if warm-up bytes leak into the
/// summary, the non-omitted intervals can't cover them.
#[test]
fn omit_summary_excludes_warmup_bytes() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let (out, _) = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-O",
            "1",
            "-t",
            "2",
            "-i",
            "1",
            "-J",
        ],
        Duration::from_secs(25),
        "client",
    );
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -J is not valid JSON ({e}): {out}"));
    let sum = &v["end"]["sum_sent"];
    let bytes = sum["bytes"].as_f64().expect("bytes");
    let secs = sum["seconds"].as_f64().expect("seconds");
    let bps = sum["bits_per_second"].as_f64().expect("bps");
    let derived = bytes * 8.0 / secs;
    assert!(
        (bps - derived).abs() <= derived * 0.02,
        "bits_per_second {bps} inconsistent with bytes*8/seconds {derived}: {out}"
    );

    let interval_bytes: u64 = v["intervals"]
        .as_array()
        .expect("intervals")
        .iter()
        .filter(|i| i["sum"]["omitted"] == Value::Bool(false))
        .filter_map(|i| i["sum"]["bytes"].as_u64())
        .sum();
    assert!(
        (bytes as u64) <= interval_bytes + interval_bytes / 5 + 1_000_000,
        "summary bytes {bytes} exceed post-omit interval bytes {interval_bytes} — warm-up leaked into the summary: {out}"
    );
}
