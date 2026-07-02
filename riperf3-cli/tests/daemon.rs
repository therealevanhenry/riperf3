//! CLI integration test for `-s -D` daemon mode (#81).
//!
//! Regression: the server used to call `daemon()` *after* the multi-threaded
//! tokio runtime was built. `daemon()` forks, and forking a process that
//! already has a multi-threaded runtime leaves the child with only the calling
//! thread — no tokio worker threads — so the daemon accepted the control
//! connection but never actually served, and every client hung. The fix
//! daemonizes in the binary *before* the runtime is built (and writes the
//! pidfile from the daemon child, not the parent that forks away). This test
//! spawns the real daemon and runs a client against it. Before the fix it fails
//! one of two ways: the pidfile records the dead forking parent, so the
//! liveness probe below trips; or, failing that, the daemon never serves and
//! the bounded client wait times out. Either way the test goes red.
//!
//! Gated to the platforms that support `daemon()` (same set as the binary).

#![cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

mod common;

/// Best-effort cleanup: kill the reparented daemon and remove its pidfile when
/// the test ends (success or panic), so a failure can't leak a server holding
/// the port.
struct Reaper {
    pid: Option<i32>,
    pidfile: std::path::PathBuf,
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // SIGTERM the daemon; ignore errors (it may already be gone, e.g.
            // after a successful one-off run).
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let _ = std::fs::remove_file(&self.pidfile);
    }
}

/// Grab a currently-free ephemeral port. There's an inherent TOCTOU gap between
/// releasing it and the daemon binding it, but it avoids the real flakiness of a
/// hardcoded port (a leaked or concurrent server sitting on it).
fn free_port() -> u16 {
    // Sub-ephemeral, PID-windowed allocation — see common::free_port.
    common::free_port()
}

/// Poll for the pidfile to appear and contain a parseable pid.
fn wait_for_pid(pidfile: &Path, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(pidfile) {
            if let Ok(pid) = s.trim().parse::<i32>() {
                return Some(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn daemon_server_serves_a_client() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let port = free_port();
    let pidfile = std::env::temp_dir().join(format!("riperf3-daemon-test-{port}.pid"));
    let _ = std::fs::remove_file(&pidfile);

    // Spawn the daemon. With `-D` the foreground process forks and exits
    // immediately; the real server is reparented to init. `-1` (one-off) makes
    // the daemon exit after serving a single test, so it self-cleans.
    let port_s = port.to_string();
    let status = Command::new(bin)
        .args(["-s", "-D", "-1", "-p", &port_s, "-I"])
        .arg(&pidfile)
        .status()
        .expect("failed to spawn daemon");
    assert!(
        status.success(),
        "daemon foreground process exited non-zero: {status:?}"
    );

    // The daemon child writes the pidfile after fork(); wait for it.
    let pid = wait_for_pid(&pidfile, Duration::from_secs(5));
    let mut reaper = Reaper {
        pid,
        pidfile: pidfile.clone(),
    };
    let pid = pid.expect("daemon never wrote a pidfile (did it die at fork?)");

    // The daemon child must actually be running (fork succeeded, not dead).
    // `kill(pid, 0)` probes for existence without delivering a signal.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "daemon pid {pid} is not alive after fork"
    );

    // No fixed bind sleep (#177): run_client retries a REFUSED connect for a
    // bounded window (the #176 pattern), and its hard timeout bounds the
    // pre-fix #81 hang (daemon forked but never serves) instead of stalling
    // the suite.
    let run = common::run_client(
        &["-c", "127.0.0.1", "-p", &port_s, "-n", "1M"],
        Duration::from_secs(15),
        "client vs daemon (a timeout here = the #81 never-serves hang)",
    );
    assert!(
        run.status.success(),
        "client failed against the daemon: {status}; stderr: {stderr}",
        status = run.status,
        stderr = run.stderr,
    );

    // Disarm the reaper only here, on full success: the one-off daemon has served
    // the test and is exiting on its own, so there's nothing to kill and we avoid
    // SIGTERMing a possibly-recycled pid. Any earlier panic (notably the hang on
    // line above) leaves the reaper armed so it reaps the leaked daemon.
    reaper.pid = None;
}

/// #262: GT's accept banner carries a per-test counter —
/// "Server listening on <port> (test #N)" (iperf_server_api.c:137),
/// N starting at 1 and incrementing for each serve round, so the
/// re-printed banner between tests shows #2, #3, ...
#[test]
fn server_banner_numbers_each_test() {
    let port = common::free_port();
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &ps])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );

    for _ in 0..2 {
        let _ = common::run_client(
            &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
            Duration::from_secs(20),
            "client",
        );
    }
    let _ = server.0.kill();
    let mut out = String::new();
    use std::io::Read;
    server
        .0
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut out)
        .expect("read server stdout");
    assert!(
        out.contains(&format!("Server listening on {port} (test #1)")),
        "first banner numbered #1 (GT shape): {out}"
    );
    assert!(
        out.contains(&format!("Server listening on {port} (test #2)")),
        "the re-printed banner increments to #2: {out}"
    );
}

/// #262 r1 F3: idle-timeout expiries are GT's silent rc==2 restart
/// (iperf_server_api.c:133-135) — no banner re-print, no counter increment,
/// no stderr line. A client arriving after idle rounds is still test #1,
/// and the post-test re-print says #2.
#[test]
fn idle_restarts_do_not_advance_the_banner_counter() {
    let port = common::free_port();
    let ps = port.to_string();
    let mut server = common::ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-p", &ps, "--idle-timeout", "1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn server"),
    );

    // Sit through at least two idle expiries.
    std::thread::sleep(Duration::from_millis(2600));
    let _ = common::run_client(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(20),
        "client",
    );
    let _ = server.0.kill();
    let mut out = String::new();
    let mut err = String::new();
    use std::io::Read;
    server
        .0
        .stdout
        .take()
        .expect("piped")
        .read_to_string(&mut out)
        .expect("read");
    server
        .0
        .stderr
        .take()
        .expect("piped")
        .read_to_string(&mut err)
        .expect("read");

    assert!(
        out.contains(&format!("Server listening on {port} (test #1)")),
        "the post-idle test is STILL #1: {out}"
    );
    assert!(
        !out.contains("(test #3)") && !out.contains("(test #4)"),
        "idle rounds must not advance the counter: {out}"
    );
    assert_eq!(
        out.matches("Server listening").count(),
        2,
        "banners: the initial one + the post-test re-print, none per idle round: {out}"
    );
    assert!(
        !err.contains("idle timeout"),
        "GT's idle restart is silent on stderr: {err}"
    );
}
