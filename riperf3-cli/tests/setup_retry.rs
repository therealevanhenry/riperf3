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

/// The client's first two connects land on a saboteur listener that accepts,
/// reads ONE byte of the cookie, and closes with the rest unread — closing a
/// socket with undrained receive data sends RST on every platform, no
/// SO_LINGER needed (std's `set_linger` is unstable). The listener then
/// vanishes and a real one-off server takes the port; the gap between drop
/// and re-bind is covered by the existing refused-retry. Pre-#195 the harness
/// only retried REFUSED runs, so the first RST killed the test.
#[test]
fn pre_data_reset_is_retried_until_a_real_server_arrives() {
    let port = common::free_port();

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind saboteur");
    let saboteur = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut sock, _) = listener.accept().expect("accept");
            // Reading one byte guarantees the client's cookie write has
            // arrived, so >0 bytes sit unread when we drop → RST, not FIN.
            let mut byte = [0u8; 1];
            let _ = sock.read_exact(&mut byte);
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
            .expect("spawn real server")
    });

    let ps = port.to_string();
    let run = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &ps, "-t", "1"],
        Duration::from_secs(40),
        "client",
    );
    let mut server = ChildGuard(server.join().expect("server thread"));
    let _ = server.0.wait();

    // run_client_ok already asserted success; the end block proves the final
    // attempt was a full real run, not a vacuous exit.
    assert!(
        run.stdout.contains("- - - - -"),
        "the retried run must complete and print its end block: {out}",
        out = run.stdout
    );
}
