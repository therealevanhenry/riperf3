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

/// #158/#210: a SECOND signal exits immediately, dying BY the signal. The
/// first signal now takes the graceful sigend path (#210: dump + terminate
/// the peer), so the old blasting-UDP wedge converges too fast to race.
/// The deterministic wedge is a phase with no interrupt-aware await: the
/// cookie/param reads served until #361 made them interrupt-aware (GT
/// exits immediately there), so the wedge now parks in the CREATE_STREAMS
/// setup wait — params sent, no data connections; its select watches the
/// ctrl and the rcv_timeout (120 s default) but NOT the interrupt — so the
/// first signal's bounded dump window (5 s) holds deterministically while
/// the second signal hits the pre-armed raw handler.
#[cfg(unix)]
#[test]
fn second_signal_during_teardown_exits_immediately() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let dir = std::env::temp_dir();
    let pf = dir.join(format!("riperf3-2sig-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pf);
    let port = free_port();
    let ps = port.to_string();

    let server = Command::new(bin)
        .args(["-s", "-1", "-I"])
        .arg(&pf)
        .args(["-p", &ps])
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

    // The wedge (#361 moved it one phase later): complete the cookie/param
    // exchange, then connect NO data streams — the server parks in the
    // CREATE_STREAMS setup wait, which no interrupt arm covers.
    let _wedge = {
        use std::io::{Read as _, Write as _};
        let mut w = std::net::TcpStream::connect(("127.0.0.1", port)).expect("wedge connect");
        w.write_all(&[b'x'; 37]).expect("cookie");
        let mut b = [0u8; 1];
        w.read_exact(&mut b).expect("ParamExchange state");
        assert_eq!(b[0], 9);
        let params = br#"{"tcp":true}"#;
        w.write_all(&(params.len() as u32).to_be_bytes())
            .expect("len");
        w.write_all(params).expect("params");
        w.read_exact(&mut b).expect("CreateStreams state");
        assert_eq!(b[0], 10);
        w
    };
    std::thread::sleep(Duration::from_millis(300)); // let the setup wait park

    let pid = server.0.id() as i32;
    // SAFETY: plain kill(2) on our own child.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(300));
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    // Bounded exit AND death-by-SIGTERM: the wedged run cannot finish its
    // dump (it would otherwise hold the full 5 s window), so only the
    // hard-exit handler (SIG_DFL + raise + unblock) produces this status.
    let exited = common::wait_bounded(&mut server.0, Duration::from_secs(4));
    let status =
        exited.expect("server must exit promptly on the second signal, not ride out the window");
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(libc::SIGTERM),
            "the second signal must kill BY the signal (got {status:?})"
        );
    }
    wait_for(
        || !pf.exists(),
        Duration::from_secs(2),
        "pidfile unlinked on the hard path",
    );
}

/// Post-#223 regression (macOS CI red on main): an IDLE server's first
/// SIGTERM must exit promptly — the accept wait races the interrupt watch.
/// Without the arm, the signal burned the CLI's full 5 s dump window
/// (systemd-style stop felt a 5 s hang). Bound: well under the window.
#[cfg(unix)]
#[test]
fn idle_server_exits_promptly_on_sigterm() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let dir = std::env::temp_dir();
    let pf = dir.join(format!("riperf3-idle-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pf);
    let ps = free_port().to_string();

    let server = Command::new(bin)
        .args(["-s", "-I"])
        .arg(&pf)
        .args(["-p", &ps])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");
    let mut server = ChildGuard(server);
    wait_for(|| pf.exists(), Duration::from_secs(5), "pidfile written");

    let t0 = Instant::now();
    unsafe {
        libc::kill(server.0.id() as i32, libc::SIGTERM);
    }
    let exited = common::wait_bounded(&mut server.0, Duration::from_secs(2));
    let status = exited.unwrap_or_else(|| {
        panic!(
            "an idle server must exit well under the 5 s dump window; \
             still alive after {:?}",
            t0.elapsed()
        )
    });
    // The prompt exit is the GRACEFUL path (exit 0), not an escape hatch
    // (review r1 n1).
    assert_eq!(status.code(), Some(0), "graceful signal-normal exit");
    wait_for(|| !pf.exists(), Duration::from_secs(2), "pidfile unlinked");
}
