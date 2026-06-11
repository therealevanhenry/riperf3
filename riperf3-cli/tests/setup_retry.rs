//! #195 harness behavior: a connection RESET during setup — before the client
//! has produced any output — is retried like a refused connect, bounded by the
//! same retry window. The shape this pins: loaded runners (macOS in PR #203's
//! CI) RST the control handshake with `Connection reset by peer (os error 54)`
//! while stdout is still empty; the run would complete cleanly if simply tried
//! again. A reset after the run produced output stays fatal — that guard is
//! unit-tested in riperf3-test-support.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

mod common;
use common::ChildGuard;

/// The two tests here each stage a multi-process saboteur dance against the
/// SAME bounded retry window they exist to exercise; concurrently on a 2-core
/// runner they contend the window away from each other (observed: the -J
/// variant's real server spawned too late once under load, 1/13 rounds).
/// Serialize within the binary, like #191's udp_serial.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Make the drop of this socket send a real RST everywhere. Closing with
/// unread data RSTs on Linux/Windows, but FreeBSD's CI run delivered the
/// FIN-before-RST ordering (the client saw a clean "peer disconnected"
/// EOF) — SO_LINGER(0) removes the ambiguity on every Unix; Windows keeps
/// the unread-data behavior (std has no stable set_linger).
#[cfg(unix)]
fn force_rst_on_drop(sock: &std::net::TcpStream) {
    use std::os::fd::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            std::ptr::from_ref(&linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "SO_LINGER setsockopt failed");
}
#[cfg(not(unix))]
fn force_rst_on_drop(_sock: &std::net::TcpStream) {}

/// The client's first two connects land on a saboteur listener that accepts,
/// reads ONE byte of the cookie, and closes with the rest unread — closing a
/// socket with undrained receive data sends RST on every platform, no
/// SO_LINGER needed (std's `set_linger` is unstable). The listener then
/// vanishes and a real one-off server takes the port; the gap between drop
/// and re-bind is covered by the existing refused-retry. Pre-#195 the harness
/// only retried REFUSED runs, so the first RST killed the test.
#[test]
fn pre_data_reset_is_retried_until_a_real_server_arrives() {
    let _serial = serial();
    let port = common::free_port();

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind saboteur");
    let saboteur = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut sock, _) = listener.accept().expect("accept");
            // Reading one byte guarantees the client's cookie write has
            // arrived, so >0 bytes sit unread when we drop → RST, not FIN.
            let mut byte = [0u8; 1];
            let _ = sock.read_exact(&mut byte);
            force_rst_on_drop(&sock);
            drop(sock);
        }
        // listener drops here; the port frees for the real server
    });

    // Once both sabotaged connects have happened, stand up the real server.
    let server = std::thread::spawn(move || {
        saboteur.join().expect("saboteur thread");
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            // Guarded immediately: a panicking client assert must not leak
            // a -s -1 process (r1 n7).
            .map(ChildGuard)
            .expect("spawn real server")
    });

    let ps = port.to_string();
    let run = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(40),
        "client",
    );
    let mut server = server.join().expect("server thread");
    let _ = server.0.wait();

    // run_client_ok already asserted success; the end block proves the final
    // attempt was a full real run, not a vacuous exit.
    assert!(
        run.stdout.contains("- - - - -"),
        "the retried run must complete and print its end block: {out}",
        out = run.stdout
    );
}

/// The -J flavor: #198 routes the setup error INTO the document on stdout
/// (stderr empty), so the pre-data classifier must read the doc — connected
/// never populated, zero intervals — not just "stdout empty". This was the
/// quiet-host residual: 4/20 two-core rounds died exactly here pre-fix.
#[test]
fn pre_data_reset_is_retried_in_json_mode() {
    let _serial = serial();
    let port = common::free_port();

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind saboteur");
    // ONE sabotaged connect here (the text test keeps two): this variant
    // proves the JSON classifier path, and each sabotaged attempt spends
    // real time from the bounded retry window — under 2-core load the
    // two-RST version once starved the real server past the deadline.
    let saboteur = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut byte = [0u8; 1];
        let _ = sock.read_exact(&mut byte);
        force_rst_on_drop(&sock);
        drop(sock);
    });
    let server = std::thread::spawn(move || {
        saboteur.join().expect("saboteur thread");
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            // Guarded immediately: a panicking client assert must not leak
            // a -s -1 process (r1 n7).
            .map(ChildGuard)
            .expect("spawn real server")
    });

    let ps = port.to_string();
    let run = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1", "-J"],
        Duration::from_secs(40),
        "client -J",
    );
    let mut server = server.join().expect("server thread");
    let _ = server.0.wait();

    let doc: serde_json::Value = serde_json::from_str(run.stdout.trim())
        .unwrap_or_else(|e| panic!("one clean doc after retries ({e}): {out}", out = run.stdout));
    assert!(
        doc["start"]["connected"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the final run really connected: {doc}"
    );
    assert!(
        doc.get("error").is_none(),
        "no error key on the clean run: {doc}"
    );
}
