//! CLI process-level test: conflicting end conditions (#140) must reject like
//! iperf3's IEENDCONDITIONS — exit 1, the message on stderr, and BEFORE any
//! side effects (iperf3 raises it in parse_arguments; no pidfile is created).
#![cfg(unix)]

use std::process::Command;

mod common;

#[test]
fn conflicting_end_conditions_exit_before_side_effects() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let pidfile = std::env::temp_dir().join(format!("riperf3-endcond-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);

    let out = Command::new(bin)
        .args([
            "-c",
            "127.0.0.1",
            "-t",
            "5",
            "-n",
            "1G",
            "-I",
            pidfile.to_str().unwrap(),
        ])
        .output()
        .expect("spawn riperf3");

    assert_eq!(
        out.status.code(),
        Some(1),
        "iperf3 exits 1 on IEENDCONDITIONS"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "parameter error - only one test end condition (-t, -n, -k) may be specified"
        ),
        "stderr must carry GT's parameter-error IEENDCONDITIONS shape (#270), got: {stderr}"
    );
    assert!(
        stderr.contains("Usage:") && stderr.contains("--help"),
        "the usage trailer rides the parameter-error class (#270): {stderr}"
    );
    assert!(
        !pidfile.exists(),
        "the rejection must fire BEFORE side effects (iperf3 creates no pidfile); found {pidfile:?}"
    );
    let _ = std::fs::remove_file(&pidfile);
}

/// #321 r1: the UDP half — the senders' self-enforced deadline (#5) must
/// not arm at 0 either, or a UDP `-t 0` run silently sends ZERO bytes
/// forever (the senders self-terminate instantly while the process keeps
/// running). The nonzero-bytes assert is what kills that mutation.
#[cfg(unix)]
#[test]
fn udp_time_zero_keeps_sending_until_signaled() {
    let port = common::free_port();
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));

    let mut client = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", &ps, "-u", "-t", "0", "-J"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn client"),
    );
    std::thread::sleep(std::time::Duration::from_millis(2_500));
    assert!(
        client.0.try_wait().expect("try_wait").is_none(),
        "-u -t 0 runs unbounded like GT"
    );
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let out = riperf3_test_support::wait_bounded(&mut client.0, std::time::Duration::from_secs(8))
        .expect("client exits on signal");
    assert!(out.success(), "signal-normal exit");
    let mut doc = String::new();
    use std::io::Read;
    client
        .0
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut doc)
        .expect("read doc");
    let v: serde_json::Value = serde_json::from_str(doc.trim()).expect("one -J doc");
    let sent = v["end"]["sum_sent"]["bytes"].as_u64().unwrap_or(0);
    assert!(
        sent > 100_000,
        "the senders kept sending (~1 Mbit/s default) instead of \
         self-terminating on a zero deadline: sent={sent}"
    );
    let _ = server.0.kill();
}

/// #321: GT arms NO end timer for `-t 0` (iperf_client_api.c:229's
/// `if (duration != 0)` gate) — the run is unbounded until signaled.
/// Pre-fix riperf3 completed instantly (a zero-length sleep).
#[cfg(unix)]
#[test]
fn time_zero_runs_until_signaled() {
    let port = common::free_port();
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &ps])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    std::thread::sleep(std::time::Duration::from_millis(400));

    let client = common::ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", &ps, "-t", "0", "-J"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn client"),
    );
    // The client must STILL be running well past any instant-complete
    // window (pre-fix it exited in <1s).
    std::thread::sleep(std::time::Duration::from_millis(2_500));
    let mut client = client;
    assert!(
        client.0.try_wait().expect("try_wait").is_none(),
        "-t 0 runs unbounded like GT"
    );
    unsafe {
        libc::kill(client.0.id() as i32, libc::SIGTERM);
    }
    let out = riperf3_test_support::wait_bounded(&mut client.0, std::time::Duration::from_secs(8))
        .expect("client exits on signal");
    assert!(out.success(), "signal-normal exit like GT's sigend");
    let _ = server.0.kill();
}
