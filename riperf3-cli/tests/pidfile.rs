//! CLI integration test: the pidfile (`-I`/`--pidfile`) must be unlinked on
//! exit like iperf3 (#105) — both on a clean one-off (`-1`) completion and on
//! SIGTERM (iperf3's `iperf_got_sigend` → `iperf_signormalexit(0)` path, which
//! deletes the pidfile and exits 0). Pre-#105 riperf3 left the stale pidfile
//! behind in both cases.
#![cfg(unix)]

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

// Reaper guard shared via riperf3-test-support (#192).
use common::ChildGuard;

fn wait_for(cond: impl Fn() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `riperf3 -s -I <file>` then SIGTERM → the pidfile must be gone and the
/// exit status 0 (iperf3 treats SIGTERM as a normal signal exit).
#[test]
fn pidfile_unlinked_on_sigterm() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port().to_string();
    let dir = std::env::temp_dir();
    let pidfile = dir.join(format!("riperf3-pidfile-sigterm-{port}.pid"));
    let _ = std::fs::remove_file(&pidfile);

    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-p", &port, "-I", pidfile.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    wait_for(
        || pidfile.exists(),
        Duration::from_secs(5),
        "pidfile creation",
    );

    // SIGTERM specifically — Child::kill() sends SIGKILL, which bypasses any
    // handler and proves nothing.
    let rc = unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill(SIGTERM) failed");

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(st) = server.0.try_wait().expect("try_wait") {
            break st;
        }
        assert!(
            Instant::now() < deadline,
            "server did not exit within 5s of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    assert!(
        !Path::new(&pidfile).exists(),
        "pidfile must be unlinked on SIGTERM like iperf3 (#105)"
    );
    assert_eq!(
        status.code(),
        Some(0),
        "iperf3 exits 0 on SIGTERM (iperf_signormalexit); got {status:?}"
    );
}

/// A one-off server (`-s -1 -I <file>`) that completes a test and returns
/// normally must unlink its pidfile on the way out.
#[test]
fn pidfile_unlinked_after_one_off_run() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port().to_string();
    let dir = std::env::temp_dir();
    let pidfile = dir.join(format!("riperf3-pidfile-oneoff-{port}.pid"));
    let _ = std::fs::remove_file(&pidfile);

    let mut server = ChildGuard(
        Command::new(bin)
            .args(["-s", "-1", "-p", &port, "-I", pidfile.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    wait_for(
        || pidfile.exists(),
        Duration::from_secs(5),
        "pidfile creation",
    );

    // Quick client run so the one-off server completes and exits on its own.
    let mut client = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", &port, "-t", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn client");
    let deadline = Instant::now() + Duration::from_secs(20);
    while client.try_wait().expect("client try_wait").is_none() {
        assert!(Instant::now() < deadline, "client timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    let _ = client.stdout.take().unwrap().read_to_string(&mut out);

    let deadline = Instant::now() + Duration::from_secs(10);
    while server.0.try_wait().expect("server try_wait").is_none() {
        assert!(
            Instant::now() < deadline,
            "one-off server did not exit after the test"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !Path::new(&pidfile).exists(),
        "pidfile must be unlinked when a one-off server exits normally (#105)"
    );
}

/// #158: a SECOND signal during teardown exits immediately. A UDP peer
/// blasting at unlimited rate holds the server's shared receiver drain (up to
/// 10 s) after the first SIGTERM; the second must escape it via the raw libc
/// handler. Race guard: if teardown won before the second signal landed
/// (fast machine), the run exits cleanly on the FIRST signal — the hard
/// assertions (bounded exit, pidfile gone) hold either way.
#[cfg(unix)]
#[test]
fn second_signal_during_teardown_exits_immediately() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let dir = std::env::temp_dir();
    let pf = dir.join(format!("riperf3-2sig-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pf);
    let port = free_port().to_string();

    let server = Command::new(bin)
        .args(["-s", "-I"])
        .arg(&pf)
        .args(["-p", &port])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = ChildGuard(server);
    wait_for(
        || pf.exists(),
        Duration::from_secs(5),
        "server pidfile written",
    );

    // A blasting UDP client makes the post-signal drain non-trivial.
    let client = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", &port, "-u", "-b", "0", "-t", "8"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn client");
    let mut client = ChildGuard(client);
    std::thread::sleep(Duration::from_secs(2)); // mid-test

    let pid = server.0.id() as i32;
    // SAFETY: plain kill(2) on our own child.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(300));
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    // Bounded exit: well under the 10 s drain either way (second-signal
    // escape, or the teardown simply won the race).
    let exited = common::wait_bounded(&mut server.0, Duration::from_secs(4));
    assert!(
        exited.is_some(),
        "server must exit promptly after the second signal, not ride out the drain"
    );
    wait_for(
        || !pf.exists(),
        Duration::from_secs(2),
        "pidfile unlinked on the signal path",
    );
    let _ = client.0.kill();
}
