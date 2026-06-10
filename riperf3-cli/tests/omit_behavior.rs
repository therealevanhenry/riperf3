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
        Duration::from_secs(60),
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
        Duration::from_secs(60),
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

/// #31 r1 blocker 1: `-n` + `-O` — the byte limit applies to the POST-omit
/// window (iperf3 gates end conditions on !omitting and resets byte progress
/// at the boundary). Pre-fix the whole budget was consumed during warm-up:
/// the run ended in ~0.3 s with warm-up bytes in the summary.
#[test]
fn omit_with_byte_limit_applies_post_omit() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let (out, elapsed) = run_capturing(
        &["-c", "127.0.0.1", "-p", &ps, "-O", "1", "-n", "100M", "-J"],
        Duration::from_secs(60),
        "client",
    );
    let _ = server.0.wait();

    assert!(
        elapsed >= Duration::from_millis(1000),
        "the warm-up second must elapse before the -n window: took {elapsed:?}"
    );
    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    // `-n 100M` is binary (unit_atoi): 104,857,600. The budget design lands
    // exactly on it; allow one 128 KiB block of slack. The old driver-poll
    // refill overshot 1.2-4x (review r2 blocker 3).
    let bytes = v["end"]["sum_sent"]["bytes"].as_u64().expect("bytes");
    assert!(
        (104_857_600..=104_988_672).contains(&bytes),
        "post-omit -n must land on the 104857600-byte target (r2): {bytes}: {out}"
    );
}

/// Reverse `-R -n + -O` (r3 blocker 1): iperf3's receive-side limit counts
/// GROSS bytes — `test->bytes_received` is never reset by `iperf_reset_stats`
/// (iperf_api.c:3675 zeroes only `bytes_sent`; end check at
/// iperf_client_api.c:771-772) — so a reverse run whose warm-up already moved
/// part of the target ends when warm-up + post-omit gross reaches `-n`, NOT
/// after a fresh post-omit N (the pre-r3 refill semantics, which also raced
/// the two reporters' boundaries into a ~50% hang). `-b 20M` pins the rate so
/// the shape is CI-stable: the 1 s warm-up moves ~2.5 MB of the 5 MB target,
/// so the post-omit (net) receive lands around target − warm-up ≈ 2.5 MB —
/// far below the ≈5 MB a fresh-N refill would transfer.
#[test]
fn omit_with_reverse_byte_limit_ends_on_gross_bytes() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let (out, elapsed) = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-R",
            "-O",
            "1",
            "-n",
            "5M",
            "-b",
            "20M",
            "-J",
        ],
        Duration::from_secs(60),
        "client",
    );
    let _ = server.0.wait();

    assert!(
        elapsed < Duration::from_secs(20),
        "reverse -n + -O must terminate (r2/r3 blocker 1)"
    );
    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    let bytes = v["end"]["sum_received"]["bytes"].as_u64().expect("bytes");
    assert!(
        (200_000..4_400_000).contains(&(bytes as usize)),
        "reverse post-omit receive must be gross-limited (≈ target − warm-up ≈ 2.5M), \
         got {bytes}: {out}"
    );
}

/// Bidir `-n + -O`: the same gross-received rule ends the whole test when
/// EITHER side's counter (sent net, received gross) reaches the target —
/// iperf3's OR-check (iperf_client_api.c:771-772). Pre-r3 this raced the two
/// reporters' boundaries (observed: one block short / intermittent hang).
#[test]
fn omit_with_bidir_byte_limit_terminates() {
    let port = free_port();
    let ps = port.to_string();
    let mut server = spawn_server(&ps);

    let (out, elapsed) = run_capturing(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "--bidir",
            "-O",
            "1",
            "-n",
            "5M",
            "-b",
            "20M",
            "-J",
        ],
        Duration::from_secs(60),
        "client",
    );
    let _ = server.0.wait();

    assert!(
        elapsed < Duration::from_secs(20),
        "bidir -n + -O must terminate"
    );
    let v: Value =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("client -J invalid ({e}): {out}"));
    // Both directions must be present and bounded: nothing may balloon past
    // the gross target plus generous stop-latency slop.
    for key in ["sum_received", "sum_sent"] {
        let bytes = v["end"][key]["bytes"].as_u64().expect(key);
        assert!(
            bytes < 8_000_000,
            "bidir {key} must stay near the 5M gross target, got {bytes}: {out}"
        );
    }
}

/// #31 r1 blocker 2: the server's reporter end time must be rebased to the
/// post-omit timeline — pre-fix the final server interval spanned
/// [last_tick, omit+t] (a 2 s window holding ~1 s of data, halving its rate).
#[test]
fn server_intervals_rebased_after_omit() {
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
    std::thread::sleep(SERVER_BIND_WAIT);

    let (_out, _) = run_capturing(
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
        ],
        Duration::from_secs(60),
        "client",
    );
    let mut sout = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.0.try_wait().expect("try_wait").is_none() {
        assert!(Instant::now() < deadline, "server did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    server
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut sout)
        .unwrap();

    let v: Value =
        serde_json::from_str(&sout).unwrap_or_else(|e| panic!("server -J invalid ({e}): {sout}"));
    for i in v["intervals"].as_array().expect("intervals") {
        let s = i["sum"]["start"].as_f64().unwrap();
        let e = i["sum"]["end"].as_f64().unwrap();
        assert!(
            e - s <= 1.6,
            "server interval [{s},{e}] spans the un-rebased warm-up (r1 blocker 2): {sout}"
        );
        assert!(
            e <= 2.6,
            "server timeline must end near the post-omit -t: {sout}"
        );
    }
}
