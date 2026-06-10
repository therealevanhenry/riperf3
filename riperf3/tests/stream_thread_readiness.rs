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
//! readiness barrier must hold the test window open until the data threads
//! have checked in; without it both directions report zero bytes.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use riperf3::{ClientBuilder, RiperfError, ServerBuilder, TransportProtocol};

/// Sub-ephemeral, PID-windowed port allocation — same scheme as the CLI test
/// harness (`riperf3-cli/tests/common`): ephemeral-range picks collide with
/// concurrent test binaries' connect() source ports under the parallel
/// harness (#176).
fn free_port() -> u16 {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    static NEXT: AtomicU16 = AtomicU16::new(0);
    let window = 7000 + (std::process::id() % 250) as u16 * 100;
    for _ in 0..100 {
        let port = window + NEXT.fetch_add(1, Ordering::Relaxed) % 100;
        if TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok()
            && TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
        {
            return port;
        }
    }
    panic!("no free port in test window {window}-{}", window + 99);
}

#[test]
fn udp_bidir_window_waits_for_stream_threads() {
    // Hogs == max_blocking_threads, so every spawn_blocking'd stream thread
    // queues until the hogs exit (1.5 s — past the whole 1 s test duration).
    const HOGS: usize = 4;
    const HOG_LIFETIME: Duration = Duration::from_millis(1500);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(HOGS)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let port = free_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let srv = tokio::spawn(async move { server.run().await });

        // The server binds inside run(); retry the control connect briefly.
        // IMPORTANT: spawn the hogs only once the server is accepting, so
        // they stall stream-thread creation, not the control-plane setup.
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .bidir(true)
            .duration(1)
            .build()
            .unwrap();
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
                    tokio::time::sleep(Duration::from_millis(100)).await;
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
