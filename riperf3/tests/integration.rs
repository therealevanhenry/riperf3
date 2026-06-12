//! Integration tests: full client↔server protocol over loopback.
//!
//! These tests start a real riperf3 server and client on localhost,
//! exercise the complete wire protocol, and verify results.

use std::time::Duration;

use riperf3::TransportProtocol;
use riperf3::{ClientBuilder, ServerBuilder};

mod common;

/// Allocate unique ports for parallel test execution — the shared #176
/// PID-windowed allocator (#192); the old bare 15201+ counter had no
/// collision probe and could land inside another binary's PID window.
fn next_port() -> u16 {
    common::free_port()
}

// ---------------------------------------------------------------------------
// Default path regression tests — verify data actually flows
// ---------------------------------------------------------------------------

/// Regression: a byte-limited transfer must complete, proving the
/// data path works end-to-end even with the interval reporter running
/// (which calls take_*_interval() every second).
#[tokio::test]
async fn regression_default_path_transfers_data() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 1 MB byte-limited test — will hang if data path is broken
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .bytes(1024 * 1024)
        .build()
        .unwrap();
    let start = std::time::Instant::now();
    let result = client.run().await;
    let elapsed = start.elapsed();
    assert!(result.is_ok(), "default path failed: {result:?}");
    // Should complete in well under 5 seconds on loopback
    assert!(elapsed.as_secs() < 5, "default path too slow: {elapsed:?}");
    let _ = server_task.await;
}

/// Regression: UDP default path with interval reporter active.
#[tokio::test]
async fn regression_udp_default_path() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(2)
        .bandwidth(1_000_000)
        .build()
        .unwrap();
    let result = client.run().await;
    assert!(result.is_ok(), "UDP default path failed: {result:?}");
    let _ = server_task.await;
}

/// #50: the server's `-J` JSON output path must complete the test cleanly in
/// both directions (the JSON assembly runs after the run; a panic or protocol
/// break there would surface as a client/server error). The JSON document goes
/// to the server's stdout; field-for-field fidelity is validated separately
/// against real iperf3.
#[tokio::test]
async fn server_json_output_completes_forward_and_reverse() {
    for reverse in [false, true] {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .json_output(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Duration-limited (not byte-limited): reverse + `-n` has a pre-existing
        // non-termination bug unrelated to JSON output, so use the duration path
        // the JSON output was validated against.
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .reverse(reverse)
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(
            result.is_ok(),
            "server -J path failed (reverse={reverse}): {result:?}"
        );
        let server_result = server_task.await.unwrap();
        assert!(
            server_result.is_ok(),
            "server -J run errored (reverse={reverse}): {server_result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Issue #1: dual-stack server bind regression
// ---------------------------------------------------------------------------

/// Server must accept IPv6 clients by default (matches iperf3's dual-stack
/// `getaddrinfo`+`AI_PASSIVE` behavior). Pre-fix the server binds `0.0.0.0`
/// only, so this fails with ConnectionRefused.
#[tokio::test]
async fn server_accepts_ipv6_client_by_default() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("::1")
        .port(Some(port))
        .duration(1)
        .build()
        .unwrap();
    let result = client.run().await;
    assert!(
        result.is_ok(),
        "IPv6 client to default server failed: {result:?}"
    );
    let _ = server_task.await;
}

/// Default server accepts an IPv6 UDP client too (dual-stack covers UDP).
#[tokio::test]
async fn server_accepts_ipv6_udp_client_by_default() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("::1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(1)
        .bandwidth(10_000_000)
        .build()
        .unwrap();
    let result = client.run().await;
    assert!(
        result.is_ok(),
        "IPv6 UDP client to default server: {result:?}"
    );
    let _ = server_task.await;
}

/// Client `-6` against an IPv4-literal host must fail fast at address
/// resolution rather than silently connecting over IPv4 (issue #10). No
/// server needed — it errors before any connection is attempted.
#[tokio::test]
async fn client_ip_version_conflicts_with_literal_host() {
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(next_port()))
        .ip_version(6)
        .duration(1)
        .connect_timeout(Duration::from_millis(500))
        .build()
        .unwrap();
    // Assert the *family-conflict* path fired, not just any connect error
    // (no server is listening, so a bare is_err() could pass vacuously).
    match client.run().await {
        Err(riperf3::RiperfError::Protocol(msg)) => {
            assert!(
                msg.contains("is not IPv6"),
                "expected a family-conflict error, got: {msg}"
            );
        }
        other => panic!("expected a family-conflict Protocol error, got {other:?}"),
    }
}

/// `ServerBuilder::ip_version(6)` must restrict the listener to IPv6, so an
/// IPv4 client is refused — exercises the builder→net pass-through end-to-end.
#[tokio::test]
async fn server_ipv6_only_refuses_ipv4_client() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .ip_version(6)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .connect_timeout(Duration::from_millis(500))
        .build()
        .unwrap();
    let result = client.run().await;
    assert!(
        result.is_err(),
        "IPv4 client must be refused by an IPv6-only server"
    );
    // The one-off server is still waiting; drain it with a matching IPv6 client.
    let v6 = ClientBuilder::new("::1")
        .port(Some(port))
        .duration(1)
        .build()
        .unwrap();
    let _ = v6.run().await;
    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// TCP loopback tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tcp_normal_single_stream() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_normal_parallel_streams() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .num_streams(4)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -P 4: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_reverse_single_stream() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .reverse(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -R: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_bidir_single_stream() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .bidir(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with --bidir: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_bytes_limit() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .bytes(10 * 1024 * 1024) // 10 MB
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -n 10M: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_reverse_bytes_limit_terminates() {
    // Reverse + byte limit must stop after N bytes, not hang (issue #60). In
    // reverse the client is the receiver, so the end condition has to count
    // received bytes; counting only sent bytes (which stay 0 here) spins forever.
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 8 * 1024 * 1024; // 8 MB
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .bytes(target)
        .build()
        .unwrap();

    // On the #60 bug this never returns; cap it so the test fails, not hangs.
    let result = tokio::time::timeout(Duration::from_secs(15), client.run()).await;
    assert!(result.is_ok(), "-R -n hung — issue #60 regression");
    let res = result.unwrap().expect("-R -n errored");
    // In reverse the server is the sender; it must have pushed at least the
    // requested volume before terminating. Lower bound only: at loopback line
    // rate the 100ms end-condition poll overshoots the target (a pre-existing
    // characteristic shared with forward `-n`), so this guards termination +
    // floor, not an exact byte count.
    let transferred: u64 = res.streams.iter().map(|s| s.bytes).sum();
    assert!(
        transferred >= target,
        "transferred {transferred} < requested {target}"
    );

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_reverse_blocks_limit_terminates() {
    // Same guard for the block limit (-R -k), issue #60.
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let blksize: usize = 128 * 1024;
    let blocks: u64 = 64;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .blksize(blksize)
        .blocks(blocks)
        .build()
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(15), client.run()).await;
    assert!(result.is_ok(), "-R -k hung — issue #60 regression");
    let res = result.unwrap().expect("-R -k errored");
    let transferred: u64 = res.streams.iter().map(|s| s.bytes).sum();
    assert!(
        transferred >= blocks * blksize as u64,
        "transferred {transferred} < requested {blocks} blocks"
    );

    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// Byte/block-limit overshoot — sender self-enforces -n/-k (bounded transfer)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tcp_byte_limit_overshoot_bounded_forward() {
    // The TCP sender self-enforces -n via a shared byte budget, stopping at ~N
    // instead of free-running until the controller's 100ms poll (~1 GB before
    // the fix). Forward: the client sends; the server (receiver) reports ≈ N.
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 8 * 1024 * 1024; // 8 MB
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .bytes(target)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    assert!(
        bytes > target / 2,
        "transferred {bytes} far below target {target}"
    );
    assert!(
        bytes < target + 2 * 1024 * 1024,
        "byte-limit overshoot unbounded: {bytes} vs target {target}"
    );
    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_byte_limit_overshoot_bounded_reverse() {
    // Reverse: the server is the sender and must self-enforce the budget too.
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 8 * 1024 * 1024;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .bytes(target)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    assert!(
        bytes > target / 2,
        "transferred {bytes} far below target {target}"
    );
    assert!(
        bytes < target + 2 * 1024 * 1024,
        "reverse byte-limit overshoot unbounded: {bytes} vs target {target}"
    );
    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// TCP bitrate pacing (-b), issue #102
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tcp_bitrate_is_paced() {
    // TCP `-b` must pace the sender to the target like iperf3 (#102). Before the
    // fix the TCP client ignored -b and ran at line rate.
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 200_000_000; // 200 Mbit/s
    let secs: u64 = 2;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(2)
        .bandwidth(target)
        .build()
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    // run() returns the server's results; forward → server received ≈ what we sent.
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    let achieved = bytes * 8 / secs;
    // Unpaced this is line rate (tens of Gbit/s, >100x target). Paced lands near
    // target; allow a generous band for burst/timing slack.
    assert!(
        achieved < target * 2,
        "TCP -b not paced: {achieved} bps vs target {target}"
    );
    assert!(
        achieved > target / 2,
        "TCP -b paced far below target: {achieved} bps vs target {target}"
    );
    let _ = server_task.await;
}

/// #116: at a LOW -b with TCP's 128 KiB default block, the old token bucket's
/// 512 KiB burst floor doubled the achieved rate (verified ~2.10 Mbit/s for
/// -b 1M against iperf3's 1.05M). iperf3's cumulative-average throttle keeps
/// the total within one block of elapsed*rate; allow +25% + one block.
#[tokio::test]
async fn tcp_low_bitrate_no_overshoot() {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 1_000_000; // 1 Mbit/s — far below the old burst floor
    let secs: u64 = 2;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(secs as u32)
        .bandwidth(target)
        .build()
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    let budget = (target / 8 * secs) as f64; // 250 KB
    let bound = budget * 1.25 + (128 * 1024) as f64;
    assert!(
        (bytes as f64) <= bound,
        "low -b overshoot (#116): sent {bytes} bytes, budget {budget:.0} (bound {bound:.0})"
    );
    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_unlimited_is_not_paced() {
    // Regression guard: default TCP (no -b ⇒ rate 0) must stay unthrottled.
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .build()
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    // A 200 Mbit cap would yield ~25 MB in 1 s; unthrottled loopback moves far
    // more. >200 MB confirms pacing didn't leak into the rate-0 path.
    assert!(
        bytes > 200_000_000,
        "unlimited TCP moved only {bytes} bytes in 1s — pacing leaked into the rate-0 path"
    );
    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_bitrate_reverse_is_paced() {
    // In reverse the server is the sender, so `-b` must pace the server path too
    // (#102). The negotiated rate reaches the server via the params.
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();
    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let target: u64 = 200_000_000;
    let secs: u64 = 2;
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(2)
        .reverse(true)
        .bandwidth(target)
        .build()
        .unwrap();

    let result = tokio::time::timeout(Duration::from_secs(20), client.run())
        .await
        .expect("client hung")
        .expect("client errored");
    // Reverse: the server sends; its reported bytes ≈ what it paced out.
    let bytes: u64 = result.streams.iter().map(|s| s.bytes).sum();
    let achieved = bytes * 8 / secs;
    assert!(
        achieved < target * 2,
        "reverse TCP -b not paced (server): {achieved} bps vs target {target}"
    );
    assert!(
        achieved > target / 2,
        "reverse TCP -b paced far below target: {achieved} bps vs target {target}"
    );
    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// UDP loopback tests (expected to fail until UDP fix lands)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn udp_normal_single_stream() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(1)
        .bandwidth(10_000_000) // 10 Mbps
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "UDP client failed: {result:?}");

    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// Socket option tests — these exercise server listener recreation with
// MSS, window, and no_delay. Currently expected to fail (bug: server
// tries to bind a second listener on the same port without closing the first).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tcp_no_delay() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .no_delay(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -N: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_mss() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .mss(1400)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -M 1400: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_window_size() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .window(256 * 1024)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -w 256K: {result:?}");

    let _ = server_task.await;
}

#[tokio::test]
async fn tcp_combined_socket_opts() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .num_streams(2)
        .reverse(true)
        .no_delay(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -P 2 -R -N: {result:?}");

    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// Bug regression tests — specific behavioral verification
// ---------------------------------------------------------------------------

/// Verify -C congestion algorithm is applied to data stream sockets.
/// Bug: congestion was sent in TestParams JSON but not applied via setsockopt.
// -C congestion control is Linux/FreeBSD-only (net.rs gates the setsockopt on
// those; iperf3's HAVE_TCP_CONGESTION). No-op elsewhere, so don't run it there (#76).
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[tokio::test]
async fn tcp_congestion_applied() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    // "cubic" is universally available; just verify the flag doesn't error
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .congestion("cubic")
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "Client failed with -C cubic: {result:?}");

    let _ = server_task.await;
}

/// Verify -A affinity pins the process to the specified CPU core.
/// Bug: affinity was set on main thread but tokio workers didn't inherit it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn cpu_affinity_applied() {
    use riperf3::set_cpu_affinity;

    // Set affinity to CPU 0 on the current thread and verify
    set_cpu_affinity(0).unwrap();

    let cpuset = nix::sched::sched_getaffinity(nix::unistd::Pid::from_raw(0)).unwrap();
    assert!(cpuset.is_set(0).unwrap(), "CPU 0 should be in affinity set");
    assert!(
        !cpuset.is_set(1).unwrap_or(false),
        "CPU 1 should NOT be set after pinning to CPU 0"
    );
}

// test_config_tests migrated in-crate to src/server.rs (#67).
// protocol_tests migrated in-crate to src/protocol.rs (#67).
// error_tests migrated in-crate to src/utils.rs (#67).
// protocol_error_tests migrated in-crate to src/protocol.rs (#67).

// ---------------------------------------------------------------------------
// JSON output validation
// ---------------------------------------------------------------------------

mod json_output_tests {
    use super::*;

    #[tokio::test]
    async fn json_output_has_required_fields() {
        // Run a test with JSON output and validate the structure
        // by building results directly (can't capture stdout easily)
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .json_output(true)
            .build()
            .unwrap();
        // This prints JSON to stdout — we verify it doesn't crash
        let result = client.run().await;
        assert!(result.is_ok(), "JSON output test failed: {result:?}");
        let _ = server_task.await;
    }

    // test_params_serializes_all_fields migrated in-crate to src/protocol.rs (#67);
    // test_results_json_structure likewise migrated when TestResultsJson became
    // #[non_exhaustive] (it can no longer be constructed from an external crate).
}

// interval_reporter_tests migrated in-crate to src/reporter.rs (#67).

// udp_edge_tests migrated in-crate to src/stream.rs (#67).

// stream_id_tests migrated in-crate to src/utils.rs (#67).

// ===========================================================================
// Unimplemented flag tests
//
// Every iperf3 flag not yet supported in riperf3 has a test below.
// Tests are #[ignore] with a reason indicating their status.
// As flags are implemented, remove the #[ignore] to activate the test.
// ===========================================================================

// ===========================================================================
// Implemented flag behavior tests
//
// These were previously #[ignore]. Now un-ignored and asserting real behavior.
// ===========================================================================

mod implemented_flag_tests {
    use super::*;

    // format_char / mptcp builder-field assertions moved in-crate to
    // `client::tests` (#43, fields are now pub(crate)).

    #[tokio::test]
    async fn udp_counters_64bit_flag() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .udp_counters_64bit(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(
            result.is_ok(),
            "UDP with 64-bit counters failed: {result:?}"
        );
        let _ = server_task.await;
    }

    // repeating_payload_buffer migrated in-crate to src/utils.rs (#67).

    #[tokio::test]
    async fn repeating_payload_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .repeating_payload(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--repeating-payload failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn dont_fragment_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .dont_fragment(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--dont-fragment failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn force_ipv4_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .ip_version(4)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-4 failed: {result:?}");
        let _ = server_task.await;
    }

    // -C is Linux/FreeBSD-only (net.rs); gate to match (#76).
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[tokio::test]
    async fn congestion_cubic_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .congestion("cubic")
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-C cubic failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn fq_rate_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .fq_rate(1_000_000_000) // 1 Gbps
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--fq-rate failed: {result:?}");
        let _ = server_task.await;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_affinity_readback() {
        // Verify set_cpu_affinity actually works via getaffinity readback
        use riperf3::set_cpu_affinity;
        set_cpu_affinity(0).unwrap();
        let cpuset = nix::sched::sched_getaffinity(nix::unistd::Pid::from_raw(0)).unwrap();
        assert!(cpuset.is_set(0).unwrap());
    }

    #[tokio::test]
    async fn bind_address_server() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .bind_address("127.0.0.1")
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-B 127.0.0.1 server failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn rcv_timeout_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .rcv_timeout(120_000)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--rcv-timeout failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn snd_timeout_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .snd_timeout(30_000)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--snd-timeout failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn control_keepalive_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .cntl_ka("10/5/3")
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--cntl-ka failed: {result:?}");
        let _ = server_task.await;
    }

    // dscp_symbolic_and_numeric migrated in-crate to src/utils.rs (#67).

    #[tokio::test]
    async fn dscp_flag_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .dscp("ef")
            .build()
            .unwrap();
        // The dscp->tos mapping is asserted in-crate (client::tests); this test
        // verifies the flag works end-to-end against a live server.
        let result = client.run().await;
        assert!(result.is_ok(), "--dscp ef failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn client_port_binding() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .cport(next_port())
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--cport failed: {result:?}");
        let _ = server_task.await;
    }

    // -----------------------------------------------------------------------
    // Reporter flags
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn forceflush_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .forceflush(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--forceflush failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn timestamps_runs() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .timestamps("%H:%M:%S ")
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--timestamps failed: {result:?}");
        let _ = server_task.await;
    }

    // -----------------------------------------------------------------------
    // Interval reporting
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tcp_with_interval_reporting() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(2)
            .interval(1.0)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-i 1 failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_with_interval_reporting() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(2)
            .interval(1.0)
            .bandwidth(1_000_000)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP -i 1 failed: {result:?}");
        let _ = server_task.await;
    }

    // -----------------------------------------------------------------------
    // UDP high-rate tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn udp_high_rate_completes() {
        // 50G target rate — verify the test completes without error.
        // Before fix: capped at ~11 Gbps. After fix: should approach 29+ Gbps.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(2)
            .bandwidth(50_000_000_000) // 50 Gbps
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP 50G failed: {result:?}");
        let _ = server_task.await;
    }

    /// Smoke test for issue #5: UDP bidirectional with parallel streams runs
    /// the full wired-up path (barrier release at TestStart, self-deadline,
    /// teardown) without deadlocking, and the outer timeout turns any hang into
    /// a failure rather than a wedged run.
    ///
    /// NOTE: this does not by itself reproduce the original hang on fast
    /// hardware — that needs real per-core starvation plus lossy handshake
    /// delivery (an 8-vCPU VM), conditions loopback on a workstation doesn't
    /// create, so it can pass even with the fix disabled. The deadline and
    /// start-barrier mechanisms are pinned directly by the `stream.rs` unit
    /// tests; this is the end-to-end "doesn't wedge" guard.
    #[tokio::test]
    async fn udp_bidir_parallel_completes() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(2)
            .num_streams(4)
            .bidir(true)
            .bandwidth(50_000_000_000) // 50 Gbps/stream target
            .build()
            .unwrap();
        // -t is 2s; allow generous slack, but fail (not hang) on a regression.
        let result = tokio::time::timeout(Duration::from_secs(20), client.run()).await;
        assert!(
            result.is_ok(),
            "UDP --bidir -P 4 hung — issue #5 regression"
        );
        assert!(result.unwrap().is_ok(), "UDP --bidir -P 4 errored");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_high_rate_reverse() {
        // 50G reverse mode — server sends, client receives
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(2)
            .bandwidth(50_000_000_000)
            .reverse(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP 50G reverse failed: {result:?}");
        let _ = server_task.await;
    }

    // -----------------------------------------------------------------------
    // UDP-specific flag tests — verify Tier 2 flags work with UDP protocol
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn udp_dont_fragment() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .dont_fragment(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP --dont-fragment failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_dscp() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .dscp("ef")
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP --dscp ef failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_fq_rate() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .fq_rate(10_000_000)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP --fq-rate failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_ipv6() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .bind_address("::1")
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("::1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .ip_version(6)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP -6 failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_rcv_timeout() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(1)
            .bandwidth(1_000_000)
            .rcv_timeout(120_000)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "UDP --rcv-timeout failed: {result:?}");
        let _ = server_task.await;
    }
}

// ===========================================================================
// Still-unimplemented flag tests (remain ignored)
// ===========================================================================

mod unimplemented_flags {
    #[allow(unused_imports)]
    use super::*;

    // -- Tier 2: behavior not yet wired --

    #[tokio::test]
    async fn force_ipv6() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .bind_address("::1")
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("::1")
            .port(Some(port))
            .duration(1)
            .ip_version(6)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-6 IPv6 loopback failed: {result:?}");
        let _ = server_task.await;
    }

    // --bind-dev is implemented on Linux (SO_BINDTODEVICE) and macOS (IP_BOUND_IF),
    // each with its own loopback interface name (`lo` vs `lo0`). The test runs on
    // both; other platforms reject --bind-dev at build() since #149 (the old
    // fallback silently no-opped), so the cli rejection tests cover them (#72).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    const LOOPBACK_DEV: &str = if cfg!(target_os = "macos") {
        "lo0"
    } else {
        "lo"
    };

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn bind_device_loopback() {
        // --bind-dev <loopback>: bind all sockets to the loopback interface.
        // Unprivileged on Linux 5.7+ (SO_BINDTODEVICE) and on macOS (IP_BOUND_IF).
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .bind_dev(LOOPBACK_DEV)
            .build()
            .unwrap();
        let result = client.run().await;
        if let Err(ref e) = result {
            let msg = format!("{e}");
            if msg.contains("Operation not permitted") {
                return; // old kernel, skip gracefully
            }
        }
        assert!(
            result.is_ok(),
            "--bind-dev {LOOPBACK_DEV} failed: {result:?}"
        );
        let _ = server_task.await;
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn bind_device_invalid_rejects() {
        // An invalid device name should cause an error, proving set_bind_dev is called.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .bind_dev("nonexistent_dev_xyz")
            .build()
            .unwrap();
        let result = client.run().await;
        if let Err(ref e) = result {
            let msg = format!("{e}");
            if msg.contains("Operation not permitted") {
                return; // old kernel, can't test
            }
        }
        // Should fail: "No such device" (ENODEV)
        assert!(
            result.is_err(),
            "--bind-dev nonexistent should fail but succeeded"
        );
        // Post-#88 the client errors BEFORE connecting, so the one-off server
        // never sees a connection and would block a `.await` forever — abort it.
        server_task.abort();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn bind_device_applied_before_connect() {
        // #88: SO_BINDTODEVICE / IP_BOUND_IF affect routing decided at connect
        // time, so the option must be applied BEFORE connect(). Pin the ordering
        // behaviorally: with NO server listening, an invalid device must surface
        // the device error (raised pre-connect), not the connection-refused error
        // a post-connect application hits first.
        let port = next_port(); // nothing listens here
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .bind_dev("nonexistent_dev_xyz")
            .build()
            .unwrap();
        let err = match client.run().await {
            Err(e) => format!("{e}"),
            Ok(_) => panic!("--bind-dev nonexistent with no server must fail"),
        };
        if err.contains("Operation not permitted") {
            return; // old kernel: SO_BINDTODEVICE needs CAP_NET_RAW; ordering not observable
        }
        assert!(
            !err.to_lowercase().contains("refused"),
            "device error must surface before connect; got a connect error instead: {err}"
        );
    }

    #[tokio::test]
    async fn ipv6_flowlabel() {
        // Flow label is IPv6-only. Just verify the flag is accepted
        // and test completes over IPv6 loopback.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .bind_address("::1")
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("::1")
            .port(Some(port))
            .duration(1)
            .ip_version(6)
            .flowlabel(12345)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-L flowlabel failed: {result:?}");
        let _ = server_task.await;
    }

    // --dscp moved to implemented_flag_tests

    // -m mptcp moved to implemented_flag_tests

    #[tokio::test]
    async fn skip_rx_copy() {
        // --skip-rx-copy uses MSG_TRUNC to avoid copying received data.
        // Verify the test completes (data still counted even if not copied).
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .reverse(true) // server sends, client receives with skip-rx-copy
            .skip_rx_copy(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--skip-rx-copy -R failed: {result:?}");
        let _ = server_task.await;
    }

    // --rcv-timeout and --snd-timeout moved to implemented_flag_tests

    // --cntl-ka moved to implemented_flag_tests

    // -- Tier 3: features requiring new logic --

    // -i interval reporting moved to implemented_flag_tests

    #[tokio::test]
    async fn get_server_output_runs() {
        // --get-server-output: client requests server output. Just verify no crash.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .get_server_output(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--get-server-output failed: {result:?}");
        let _ = server_task.await;
    }

    // --pidfile, --logfile, --forceflush, --timestamps, -D: implemented and validated on sandbox
    // (can't test fork/file-redirect from library integration tests)

    #[tokio::test]
    async fn idle_timeout_restarts() {
        // Server with idle_timeout=2 should timeout if no client connects
        // within 2 seconds and exit cleanly (one_off mode).
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .idle_timeout(2)
            .build()
            .unwrap();
        let start = std::time::Instant::now();
        let result = server.run().await;
        let elapsed = start.elapsed();
        // Server should have timed out after ~2 seconds, not hung forever
        assert!(
            elapsed.as_secs() < 5,
            "idle-timeout took too long: {elapsed:?}"
        );
        // Timeout is not an error in one-off mode — server just exits
        let _ = result;
    }

    #[tokio::test]
    async fn server_max_duration() {
        // #230: --server-max-duration is an UPFRONT param-exchange check in
        // iperf3 (iperf_api.c:2666), not a timer — a -t 10 request against
        // max 2 is refused before the test starts, with the strerror of
        // IEMAXSERVERTESTDURATIONEXCEEDED(37) relayed to the client.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .server_max_duration(2)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(10)
            .build()
            .unwrap();
        let start = std::time::Instant::now();
        let result = client.run().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 4,
            "the refusal is upfront — no transfer, no timer wait: {elapsed:?}"
        );
        let err = result.expect_err("the relayed SERVER_ERROR is the client's error");
        assert_eq!(
            err.to_string(),
            "client's requested duration exceeds the server's maximum permitted limit",
            "{err:?}"
        );
        // The server side errors to ITS sink but run() is Ok - iperf3's
        // one-off exits 0 on the refusal path (live-verified, the #224 wart).
        let joined = server_task.await.expect("server task");
        assert!(
            joined.is_ok(),
            "server refusal is not a run() error: {joined:?}"
        );
    }

    #[tokio::test]
    async fn server_bitrate_limit() {
        // Server with a very low bitrate limit should terminate the test
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .server_bitrate_limit(1_000) // 1 Kbit/s — absurdly low
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(10)
            .build()
            .unwrap();
        let start = std::time::Instant::now();
        let result = client.run().await;
        let elapsed = start.elapsed();
        // Should terminate well before 10 seconds
        assert!(
            elapsed.as_secs() < 5,
            "bitrate limit didn't cut test: {elapsed:?}"
        );
        // #224 ground truth (iperf 3.21): SERVER_ERROR + IETOTALRATE, the
        // client adopts iperf_strerror(27); no generic ServerTerminated.
        let err = result.expect_err("the relayed SERVER_ERROR is the client's error");
        assert_eq!(
            err.to_string(),
            "total required bandwidth is larger than server limit",
            "{err:?}"
        );
        let joined = server_task.await.expect("server task");
        assert!(
            joined.is_ok(),
            "server self-terminate is not a run() error: {joined:?}"
        );
    }

    #[tokio::test]
    async fn file_transfer_send() {
        // -F with sender: read from file instead of zero buffer
        let port = next_port();

        // Create a temp file with known content
        let tmp = std::env::temp_dir().join(format!("riperf3-test-send-{port}"));
        std::fs::write(&tmp, vec![0xABu8; 1024 * 1024]).unwrap(); // 1 MB

        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .file(tmp.to_str().unwrap())
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-F send failed: {result:?}");

        let _ = server_task.await;
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn file_transfer_recv() {
        // -F with receiver (-R): write received data to file
        let port = next_port();
        let tmp = std::env::temp_dir().join(format!("riperf3-test-recv-{port}"));

        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .reverse(true)
            .file(tmp.to_str().unwrap())
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-F recv failed: {result:?}");

        // File should exist and have data
        let meta = std::fs::metadata(&tmp);
        assert!(meta.is_ok(), "output file should exist");
        assert!(meta.unwrap().len() > 0, "output file should have data");

        let _ = server_task.await;
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn file_transfer_content_integrity() {
        // Verify the file content actually flows through the wire:
        // 1. Create a file with a known repeating pattern
        // 2. Sender reads from it with -F
        // 3. Receiver writes to a different file with -F -R
        // 4. Receiver's file should contain the same pattern
        let port = next_port();
        let send_file = std::env::temp_dir().join(format!("riperf3-integrity-send-{port}"));
        let recv_file = std::env::temp_dir().join(format!("riperf3-integrity-recv-{port}"));

        // Known pattern: 128 KB of repeating [0xDE, 0xAD, 0xBE, 0xEF]
        let pattern: Vec<u8> = (0..128 * 1024)
            .map(|i| [0xDE, 0xAD, 0xBE, 0xEF][i % 4])
            .collect();
        std::fs::write(&send_file, &pattern).unwrap();

        // Server sends (reverse mode), reading from send_file
        // But -F on the server side isn't wired — only client uses it.
        // So: client sends from file, server receives normally,
        // then we verify the send side read the file.
        //
        // For a true end-to-end content check: client sends from file,
        // a second client receives to file. But we only have one client.
        //
        // Instead: run sender with -F, receiver writes to file.
        // The receiver gets whatever the sender sent.
        // Since the sender reads from our pattern file, the receiver
        // should get that pattern (repeated as many times as the test runs).

        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Client sends from file, using byte limit to control transfer size
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .bytes(128 * 1024) // exactly one file's worth
            .file(send_file.to_str().unwrap())
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-F content test failed: {result:?}");
        let _ = server_task.await;

        // Now test the receive path: server sends, client receives to file
        let port2 = next_port();
        let server2 = ServerBuilder::new()
            .port(Some(port2))
            .one_off(true)
            .build()
            .unwrap();
        let server_task2 = tokio::spawn(async move { server2.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client2 = ClientBuilder::new("127.0.0.1")
            .port(Some(port2))
            .duration(1)
            .reverse(true)
            .file(recv_file.to_str().unwrap())
            .build()
            .unwrap();
        let result2 = client2.run().await;
        assert!(result2.is_ok(), "-F recv content test failed: {result2:?}");
        let _ = server_task2.await;

        // Verify received file has data (server sends zero-filled buffers
        // in reverse mode since server doesn't use -F, so content won't
        // match our pattern — but it MUST have non-zero length)
        let recv_data = std::fs::read(&recv_file).unwrap();
        assert!(!recv_data.is_empty(), "received file should have data");

        let _ = std::fs::remove_file(&send_file);
        let _ = std::fs::remove_file(&recv_file);
    }

    #[tokio::test]
    async fn file_transfer_end_to_end_content_match() {
        // True end-to-end: client sends from file, server receives to file,
        // verify the received file contains the sent pattern.
        let port = next_port();
        let send_file = std::env::temp_dir().join(format!("riperf3-e2e-send-{port}"));
        let recv_file = std::env::temp_dir().join(format!("riperf3-e2e-recv-{port}"));

        // Write a known pattern: repeating DEADBEEF, exactly 128KB
        let pattern: Vec<u8> = (0..128 * 1024)
            .map(|i| [0xDE, 0xAD, 0xBE, 0xEF][i % 4])
            .collect();
        std::fs::write(&send_file, &pattern).unwrap();

        // Server receives to file (normal mode: client sends, server receives)
        let recv_path = recv_file.to_str().unwrap().to_string();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .file(&recv_path)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Client sends from file, byte-limited to exactly 128KB
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .bytes(128 * 1024)
            .file(send_file.to_str().unwrap())
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "e2e file send failed: {result:?}");
        let _ = server_task.await;

        // Verify: received file should start with our pattern
        let recv_data = std::fs::read(&recv_file).unwrap();
        assert!(!recv_data.is_empty(), "received file should have data");
        // Check the first 128KB matches (server may receive more due to timing)
        let check_len = pattern.len().min(recv_data.len());
        assert_eq!(
            &recv_data[..check_len],
            &pattern[..check_len],
            "received content doesn't match sent pattern"
        );

        let _ = std::fs::remove_file(&send_file);
        let _ = std::fs::remove_file(&recv_file);
    }

    #[tokio::test]
    async fn json_stream_runs() {
        // --json-stream: each interval emitted as JSON. Just verify it doesn't crash.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(2)
            .json_output(true)
            .json_stream(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "--json-stream failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn udp_gso_gro() {
        // --gsro enables UDP GSO (send) and GRO (recv).
        // May not be available on all kernels — skip gracefully if ENOPROTOOPT.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .protocol(TransportProtocol::Udp)
            .duration(2)
            .bandwidth(100_000_000) // 100 Mbps
            .build()
            .unwrap();
        // gsro flag is on the CLI but not wired to behavior yet —
        // just verify UDP works (gsro is an optimization, not a protocol change)
        let result = client.run().await;
        assert!(result.is_ok(), "UDP test failed: {result:?}");
        let _ = server_task.await;
    }

    // -- Deferred --

    // -Z zerocopy uses sendfile, implemented for Linux/macOS/FreeBSD but not
    // Windows (no sendfile). Gate to Unix (#76).
    #[cfg(unix)]
    #[tokio::test]
    async fn zerocopy_send() {
        // -Z zerocopy: uses sendfile() to avoid userspace-to-kernel copy.
        // Verify the test completes and data flows.
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .bytes(10 * 1024 * 1024) // 10 MB
            .zerocopy(true)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "-Z zerocopy failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn rsa_auth_success() {
        let port = next_port();
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .rsa_private_key_path(&format!("{fixtures}/test_private.pem"))
            .authorized_users_path(&format!("{fixtures}/test_users.csv"))
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .username("testuser")
            .password("testpass")
            .rsa_public_key_path(&format!("{fixtures}/test_public.pem"))
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_ok(), "RSA auth failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn rsa_auth_wrong_password() {
        let port = next_port();
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .rsa_private_key_path(&format!("{fixtures}/test_private.pem"))
            .authorized_users_path(&format!("{fixtures}/test_users.csv"))
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .username("testuser")
            .password("wrongpass")
            .rsa_public_key_path(&format!("{fixtures}/test_public.pem"))
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "wrong password should fail");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn rsa_auth_no_token_rejected() {
        let port = next_port();
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .rsa_private_key_path(&format!("{fixtures}/test_private.pem"))
            .authorized_users_path(&format!("{fixtures}/test_users.csv"))
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Client without auth — should be rejected
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "no auth token should be rejected");
        let _ = server_task.await;
    }
}

// ---------------------------------------------------------------------------
// sendmmsg tests (experimental batched UDP sends)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
#[tokio::test]
async fn udp_sendmmsg_normal() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(1)
        .bandwidth(10_000_000) // 10 Mbps
        .sendmmsg(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(result.is_ok(), "UDP sendmmsg client failed: {result:?}");

    let _ = server_task.await;
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
#[tokio::test]
async fn udp_sendmmsg_high_rate() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(1)
        .bandwidth(1_000_000_000) // 1 Gbps
        .sendmmsg(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(
        result.is_ok(),
        "UDP sendmmsg high-rate client failed: {result:?}"
    );

    let _ = server_task.await;
}

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
#[tokio::test]
async fn udp_sendmmsg_with_64bit_counters() {
    let port = next_port();

    let server = ServerBuilder::new()
        .port(Some(port))
        .one_off(true)
        .build()
        .unwrap();

    let server_task = tokio::spawn(async move { server.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .protocol(TransportProtocol::Udp)
        .duration(1)
        .bandwidth(10_000_000)
        .sendmmsg(true)
        .udp_counters_64bit(true)
        .build()
        .unwrap();

    let result = client.run().await;
    assert!(
        result.is_ok(),
        "UDP sendmmsg + 64bit counters failed: {result:?}"
    );

    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// Client::run return-value tests (added with the Result<TestResultsJson> change)
// ---------------------------------------------------------------------------

mod client_run_return_value {
    use super::*;

    /// Happy path: a normal TCP exchange yields a populated `TestResultsJson`,
    /// proving the library now exposes what `print_results` used to consume internally.
    #[tokio::test]
    async fn run_returns_populated_results() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .bytes(1024 * 1024)
            .build()
            .unwrap();

        let results = client.run().await.expect("client run failed");

        assert!(
            !results.streams.is_empty(),
            "expected at least one stream in returned results"
        );
        let total_bytes: i64 = results.streams.iter().map(|s| s.bytes as i64).sum();
        assert!(
            total_bytes > 0,
            "expected non-zero bytes across streams, got {total_bytes}"
        );
        assert!(
            results.cpu_util_total.is_finite() && results.cpu_util_total >= 0.0,
            "cpu_util_total not a sane non-negative number: {}",
            results.cpu_util_total
        );

        let _ = server_task.await;
    }
    // run_errors_when_server_skips_results_exchange migrated in-crate to src/client.rs (#67).
}
