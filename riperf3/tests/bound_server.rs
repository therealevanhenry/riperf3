//! #291: `Server::bind()` — the bind-once building block. A library caller
//! serving N sequential tests holds the port the whole time (no rebind gap
//! for another process to steal, no re-listen race) and can learn a
//! port-0 ephemeral assignment before the first client connects.

use std::time::Duration;

use riperf3::{ClientBuilder, ServerBuilder};

async fn run_client(port: u16) -> riperf3::Report {
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .bytes(256 * 1024)
        .json_output(true)
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), client.run())
        .await
        .expect("client hung")
        .expect("client errored")
        .report
}

/// One bind, two sequential tests on the same listener — the accept()-style
/// contract run_once's per-call rebind couldn't give (#291).
#[tokio::test]
async fn bind_once_serves_sequential_tests_on_one_port() {
    let server = ServerBuilder::new()
        .port(Some(0)) // ephemeral: the port is learnable only via bind()
        .json_output(true)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().expect("local_addr").port();
    assert_ne!(port, 0, "bind resolves the ephemeral port");

    for i in 0..2 {
        let server_run = async { bound.run_once().await };
        let client_run = async {
            // Give the accept loop a beat; the listener is already bound, so
            // even a too-early connect would queue rather than be refused.
            tokio::time::sleep(Duration::from_millis(50)).await;
            run_client(port).await
        };
        let (srv, cli) = tokio::join!(server_run, client_run);
        let srv = srv
            .unwrap_or_else(|e| panic!("server run_once #{i} errored: {e}"))
            .report;
        assert!(
            srv.end.sum_received.as_ref().unwrap().bytes > 0,
            "test #{i}: the server measured the transfer"
        );
        assert!(
            cli.end.sum_sent.as_ref().unwrap().bytes > 0,
            "test #{i}: the client moved bytes"
        );
    }
}

/// `Server::run_once` keeps its exact contract (it now delegates through
/// bind()): one test served on the configured port, report returned.
#[tokio::test]
async fn run_once_still_serves_one_test() {
    // run_once on port 0 can't learn the port externally — take a free
    // ephemeral port from the shared helper instead of pinning one (r2 nit:
    // a hardcoded port is a collision-flake risk on shared runners).
    let port = riperf3_test_support::free_port();
    let server = ServerBuilder::new().port(Some(port)).build().unwrap();
    let server_task = tokio::spawn(async move { server.run_once().await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let cli = run_client(port).await;
    assert!(cli.end.sum_sent.as_ref().unwrap().bytes > 0);
    let srv = server_task.await.unwrap().expect("run_once").report;
    assert!(srv.end.sum_received.as_ref().unwrap().bytes > 0);
}

/// #427 (the Windows stack-overflow incident): `run_once`'s future must
/// stay SMALL — `Box::pin` at the setup await moves the setup machinery's
/// state (incl. the UDP arms' 64 KB bufs) to the heap. Without the box the
/// composed state is ~70 KB and, worse, the extra generator layer's MSVC
/// debug poll frames overflowed Windows' 1 MB main-thread stack (the CLI's
/// `block_on`) at the first accept — invisible on Linux (8 MB mains) and
/// on 2 MB worker/test threads, so this size bound is the only LOCAL
/// tripwire for the class (Windows CI is the authoritative gate). 16 KB
/// bounds "the box stays" and "no future large-buffer re-inlining"
/// (currently ~5.5 KB).
#[tokio::test]
async fn run_once_future_stays_small() {
    let server = ServerBuilder::new()
        .port(Some(0))
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let fut = bound.run_once();
    let size = std::mem::size_of_val(&fut);
    assert!(
        size < 16 * 1024,
        "run_once future grew to {size} B — a dropped Box::pin or a \
         re-inlined large buffer risks the Windows 1 MB main-thread \
         stack (#427)"
    );
}
