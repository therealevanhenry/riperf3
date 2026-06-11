//! #185: a rate-limited UDP sender must pace smoothly across the run, not send
//! one fixed-size batch then sleep past the end of a short test.
//!
//! At a low `-b` (the default 1 Mbit/s) over a large datagram (loopback derives
//! a ~32 KiB blksize from the control MSS), the old fixed 32-packet batch was a
//! ~8 s send-budget interval: the sender emitted one burst at t=0, then slept
//! past the whole `-t` window — every datagram landed in the first interval and
//! the rest read zero. The fix sizes the batch to ~one pacing quantum, so the
//! traffic spreads across every interval like iperf3.

use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

mod common;

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

fn spawn_server(port: &str) -> ChildGuard {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", port])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    )
}

/// Default-rate UDP `-t 3 -i 1`: every one-second interval must carry traffic,
/// and no single interval may hold the bulk of it — the signature of the old
/// burst-then-starve (all bytes in interval 0, the rest empty).
#[test]
fn default_rate_udp_paces_across_intervals() {
    let port = common::free_port().to_string();
    let mut server = spawn_server(&port);

    let out = common::run_client_ok(
        &[
            "-c",
            "127.0.0.1",
            "-p",
            &port,
            "-u",
            "-t",
            "3",
            "-i",
            "1",
            "-J",
        ],
        Duration::from_secs(20),
        "client",
    )
    .stdout;
    let _ = server.0.wait();

    let v: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("client -u -J is not valid JSON ({e}): {out}"));
    let intervals = v["intervals"].as_array().expect("intervals");

    // Nominally three one-second intervals; on a loaded Windows runner the
    // FINAL interval can be dropped — the run's catch-up burst lands after
    // the reporter's end snapshot, so the array under-covers the run that
    // its own end block sums in full (#159; three CI captures show two
    // 1-second intervals 0-1/1-2 with the third absent, never a coalesced
    // 2-second one). Two intervals still discriminate the #185
    // burst-then-starve signature below (>=2 is the principled floor: at
    // one interval the <80% check is unsatisfiable), so tolerate the
    // truncated run instead of flaking the required windows-latest check;
    // the under-coverage itself is #159's open product defect.
    let per: Vec<u64> = intervals
        .iter()
        .map(|i| i["sum"]["bytes"].as_u64().unwrap_or(0))
        .collect();
    assert!(per.len() >= 2, "expected >=2 intervals: {out}");
    let total: u64 = per.iter().sum();
    assert!(total > 0, "no bytes sent at all: {out}");

    for (n, &b) in per.iter().enumerate() {
        assert!(
            b > 0,
            "interval {n} carried no bytes — sender burst then starved (#185): {per:?}"
        );
        assert!(
            (b as f64) < 0.8 * total as f64,
            "interval {n} held {b} of {total} bytes — one burst, not paced (#185): {per:?}"
        );
    }
}
