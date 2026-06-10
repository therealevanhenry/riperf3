//! #178 regression: the test window must not open before the stream data
//! threads exist.
//!
//! On loaded windows-latest runners, OS-thread creation for the
//! `spawn_blocking` UDP data threads stalled 1.2-3.5 s past TestStart while
//! the async control plane kept the `-t` clock advancing — the whole test
//! window elapsed before a single data thread ran, and a `-u --bidir` run
//! completed "normally" with zero bytes both ways (the late receiver went
//! straight to its post-test drain and *discarded* everything the peer had
//! sent). DBG178-instrumented CI runs on the issue pin the timeline.
//!
//! This reproduces that deterministically on any platform: cap the runtime's
//! blocking pool and saturate it with hogs that outlive the test duration, so
//! the stream threads queue behind them exactly like the loaded runner. The
//! readiness gate must hold the test window open until the data threads have
//! checked in; without it both directions report zero bytes.

use std::time::Duration;

use riperf3::{ClientBuilder, RiperfError, ServerBuilder, TransportProtocol};

mod common;

#[test]
fn udp_bidir_window_waits_for_stream_threads() {
    // Hogs == max_blocking_threads, so every spawn_blocking'd stream thread
    // queues until the hogs exit — 2.5 s, past the whole 1 s test duration
    // with margin: the wave starts at attempt start, so the control handshake
    // would have to outlive it for the test to pass without exercising the
    // gate (review r2 — vacuous-pass, not false-fail, is the failure mode of
    // a too-short wave).
    const HOGS: usize = 4;
    const HOG_LIFETIME: Duration = Duration::from_millis(2500);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(HOGS)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let port = common::free_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let srv = tokio::spawn(async move { server.run().await });

        // 100 Mbit/s keeps the run light but shrinks the sender's pacing
        // batch interval from ~8 s (loopback blksize at the 1 Mbit/s default)
        // to ~80 ms, so teardown joins promptly instead of parking in the
        // pacing sleep for most of 10 s of wall time.
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .bidir(true)
            .duration(1)
            .bandwidth(100_000_000)
            .build()
            .unwrap();

        // The server binds inside run(), so the control connect can race it;
        // retry on refusal. Each attempt saturates the pool with a FRESH hog
        // wave so the stall covers the successful attempt's CreateStreams —
        // and a refused attempt first waits out the previous wave, so waves
        // never stack up FIFO-ahead of the stream threads (stacked waves
        // would serialize at 1.5 s each and could blow the 10 s gate budget,
        // failing the test in exactly the loaded environment it targets).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let results = loop {
            for _ in 0..HOGS {
                tokio::task::spawn_blocking(move || std::thread::sleep(HOG_LIFETIME));
            }
            match client.run().await {
                Ok(r) => break r,
                Err(RiperfError::Io(e))
                    if e.kind() == std::io::ErrorKind::ConnectionRefused
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(HOG_LIFETIME).await;
                }
                Err(e) => panic!("client run failed: {e}"),
            }
        };
        srv.await.unwrap().expect("server run");

        // The server's exchanged results carry both streams: its receiving
        // stream (forward bytes received) and its sending stream (reverse
        // bytes sent). Pre-fix, the hogged pool keeps every data thread off
        // the CPU until the window has already closed, and both are zero.
        assert_eq!(results.streams.len(), 2, "bidir run has two streams");
        for s in &results.streams {
            assert!(
                s.bytes > 0,
                "stream {} moved no bytes — test window opened before the \
                 data threads started (#178): {results:?}",
                s.id
            );
        }
    });
}
