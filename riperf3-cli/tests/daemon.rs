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
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
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

    // Give the listener a moment to bind before connecting.
    std::thread::sleep(Duration::from_millis(300));

    // Run a short byte-limited client against the daemon. Spawn it as a child
    // and bound the wait: before the fix the daemon never serves and the client
    // hangs, so a plain blocking call would hang the whole test suite.
    let mut client = Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", &port_s, "-n", "1M"])
        .spawn()
        .expect("failed to spawn client");

    let deadline = Instant::now() + Duration::from_secs(15);
    let exit = loop {
        match client.try_wait().expect("try_wait on client") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = client.kill();
                let _ = client.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    let exit = exit.expect("client hung against the daemon — server never served (#81)");
    assert!(exit.success(), "client failed against the daemon: {exit:?}");

    // Disarm the reaper only here, on full success: the one-off daemon has served
    // the test and is exiting on its own, so there's nothing to kill and we avoid
    // SIGTERMing a possibly-recycled pid. Any earlier panic (notably the hang on
    // line above) leaves the reaper armed so it reaps the leaked daemon.
    reaper.pid = None;
}
