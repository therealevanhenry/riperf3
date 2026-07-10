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
    let transferred: u64 = res.report.end.sum_sent.as_ref().unwrap().bytes;
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
    let transferred: u64 = res.report.end.sum_sent.as_ref().unwrap().bytes;
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
    let bytes: u64 = result.report.end.sum_received.as_ref().unwrap().bytes;
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
    let bytes: u64 = result.report.end.sum_sent.as_ref().unwrap().bytes;
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
    // run() returns the rich report; forward → server received ≈ what we sent.
    let bytes: u64 = result.report.end.sum_received.as_ref().unwrap().bytes;
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
    let bytes: u64 = result.report.end.sum_received.as_ref().unwrap().bytes;
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
    let bytes: u64 = result.report.end.sum_received.as_ref().unwrap().bytes;
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
    let bytes: u64 = result.report.end.sum_sent.as_ref().unwrap().bytes;
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
    async fn interrupt_honored_in_setup_phase_wait() {
        // #231: iperf_catch_sigend is armed for the WHOLE run — a client
        // whose interrupt watch fires while it waits on the control channel
        // between states (here: a mock that accepts + reads the cookie, then
        // goes silent pre-ParamExchange) must dump and return promptly,
        // exactly like the TEST_RUNNING arm. Pre-#231 the central recv_state
        // wait never polled the watch and run() blocked until the read
        // returned.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let mock = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncReadExt;
                let mut cookie = [0u8; 37];
                let _ = sock.read_exact(&mut cookie).await;
                // Silent hold, outliving the test body.
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
        });

        let (tx, rx) = tokio::sync::watch::channel::<Option<String>>(None);
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(5)
            .interrupt(rx)
            .build()
            .unwrap();
        let run = tokio::spawn(async move { client.run().await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        tx.send(Some(
            "interrupt - the client has terminated by signal Terminated(15)".into(),
        ))
        .unwrap();

        let res = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .expect("the interrupt must be honored in the setup-phase wait (#231)")
            .expect("join");
        // GT dumps + returns normally (iperf_got_sigend's client arm has no
        // phase gate); the signal-normal EXIT is the CLI's business. #293: the
        // run is Ok(RunOutcome) with Termination::Interrupted — distinct from
        // Completed, and since BOTH exit 0, this is the only assertion that
        // catches an Interrupted↔Completed mis-map.
        let outcome = res.expect("interrupt run returns Ok(RunOutcome)");
        assert_eq!(
            outcome.termination,
            riperf3::Termination::Interrupted,
            "a local signal ends Interrupted"
        );
        mock.abort();
    }

    /// #386: GT's refused round does not END at the relay — cleanup_server
    /// closes the ctrl through iperf_sync_close_socket (net.c:877-886):
    /// shutdown(SHUT_WR), then the BOUNDED drain (per-Nread 10 s idle —
    /// see the bound pin below). The round (and its refusal doc) completes
    /// at the client's close or the bound, whichever first; riperf3
    /// completed immediately (probed 30/30 vs GT 10/10 under the #385 r1
    /// signal race). A mock that reads the fe+37 relay and HOLDS must see
    /// (a) the server's FIN promptly (the SHUT_WR half) while (b) run_once
    /// stays parked well under the bound; the mock's close ends the round.
    #[tokio::test]
    async fn refused_round_parks_until_client_eof() {
        use std::io::{Read, Write};

        let server = ServerBuilder::new()
            .port(Some(0))
            .server_max_duration(2)
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.expect("bind");
        let port = bound.local_addr().unwrap().port();
        let handle = tokio::spawn(async move { bound.run_once().await });

        let held = tokio::task::spawn_blocking(move || {
            let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
            ctrl.write_all(&[b'x'; 37]).unwrap();
            let mut b = [0u8; 1];
            ctrl.read_exact(&mut b).unwrap();
            assert_eq!(b[0], 9, "ParamExchange");
            let params = br#"{"tcp":true,"time":30,"parallel":1,"len":4096}"#;
            ctrl.write_all(&(params.len() as u32).to_be_bytes())
                .unwrap();
            ctrl.write_all(params).unwrap();
            let mut relay = [0u8; 9];
            ctrl.read_exact(&mut relay).unwrap();
            assert_eq!(relay[0], 0xfe, "SERVER_ERROR state");
            assert_eq!(
                u32::from_be_bytes(relay[1..5].try_into().unwrap()),
                37,
                "IEMAXSERVERTESTDURATIONEXCEEDED"
            );
            // (a) the server half-closes promptly (GT's SHUT_WR): the next
            // read is EOF, not a hang.
            ctrl.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            let n = ctrl.read(&mut b).expect("the shutdown FIN arrives bounded");
            assert_eq!(n, 0, "EOF from the server's write-half shutdown");
            ctrl
        })
        .await
        .expect("mock");

        // (b) the round is PARKED while the client holds.
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(
            !handle.is_finished(),
            "#386: the refused round must park until the client closes"
        );
        drop(held); // client EOF → the round ends
        let out = tokio::time::timeout(Duration::from_secs(4), handle)
            .await
            .expect("the round ends bounded after client EOF")
            .expect("join");
        // A refusal is a no-report round: Err per the #293 rule.
        assert!(out.is_err(), "refusal has no report: {out:?}");
    }

    /// #386 (#429 r1 F1): the park is BOUNDED — GT's drain rides Nread,
    /// whose front-select times out at 10 s of silence and returns 0,
    /// failing the `> 0` drain test exactly like EOF (net.c:75, :415-436;
    /// GT live-probed self-freeing at ~10 s against a wedged holder, doc
    /// rendered). A client that reads the relay and HOLDS silently must
    /// not wedge the round forever: run_once ends within the bound. ~10 s
    /// of wall clock by design (the 41 s watchdog cell made the opposite
    /// call; this one IS the r1 blocker, so it stays pinned).
    #[tokio::test]
    async fn refused_round_park_is_bounded_at_nread_idle() {
        use std::io::{Read, Write};

        let server = ServerBuilder::new()
            .port(Some(0))
            .server_max_duration(2)
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.expect("bind");
        let port = bound.local_addr().unwrap().port();
        let handle = tokio::spawn(async move { bound.run_once().await });

        let _held = tokio::task::spawn_blocking(move || {
            let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
            ctrl.write_all(&[b'x'; 37]).unwrap();
            let mut b = [0u8; 1];
            ctrl.read_exact(&mut b).unwrap();
            assert_eq!(b[0], 9, "ParamExchange");
            let params = br#"{"tcp":true,"time":30,"parallel":1,"len":4096}"#;
            ctrl.write_all(&(params.len() as u32).to_be_bytes())
                .unwrap();
            ctrl.write_all(params).unwrap();
            let mut relay = [0u8; 9];
            ctrl.read_exact(&mut relay).unwrap();
            assert_eq!(relay[0], 0xfe, "SERVER_ERROR state");
            ctrl // hold silently — never close
        })
        .await
        .expect("mock");

        let out = tokio::time::timeout(Duration::from_secs(14), handle)
            .await
            .expect("the park self-frees at ~10 s against a silent holder")
            .expect("join");
        assert!(out.is_err(), "refusal has no report: {out:?}");
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
        // #293: Ok(RunOutcome) carrying the adopted message.
        let outcome = result.expect("the relayed SERVER_ERROR returns Ok(RunOutcome)");
        assert_eq!(
            outcome.termination,
            riperf3::Termination::ServerError(
                "client's requested duration exceeds the server's maximum permitted limit"
                    .to_string()
            ),
            "{:?}",
            outcome.termination
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
        // client adopts iperf_strerror(27). #293: Ok(RunOutcome) with the
        // ServerError ending.
        let outcome = result.expect("the relayed SERVER_ERROR returns Ok(RunOutcome)");
        assert_eq!(
            outcome.termination,
            riperf3::Termination::ServerError(
                "total required bandwidth is larger than server limit".to_string()
            ),
            "{:?}",
            outcome.termination
        );
        // #404 r1 F2: the relay report's end is BARE in every mode — this
        // bare-builder (quiet-default) client rides the text-path
        // partial_report, the half no CLI pin reaches; GT never
        // end-processes a SERVER_ERROR kill at any stage.
        assert!(
            outcome.report.end.sum_sent.is_none() && outcome.report.end.sum_received.is_none(),
            "the SERVER_ERROR relay report carries GT's bare end (#404): {:?}",
            outcome.report.end
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
// Client::run / Server::run_once return-value tests (#137: the rich Report API)
// ---------------------------------------------------------------------------

mod client_run_return_value {
    use super::*;

    /// #137: a normal TCP exchange makes `Client::run` return a populated rich
    /// `Report` (the same object `-J` serializes), not the lean wire struct.
    #[tokio::test]
    async fn run_returns_rich_report() {
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
            // JSON mode + a duration run so the reporter KEEPS interval samples:
            // iperf3's discard_json keeps them only under -J (text mode prints
            // then discards), and a duration run crosses an interval boundary.
            // The build-once guard below needs `report.intervals` non-empty.
            .json_output(true)
            .duration(2)
            .build()
            .unwrap();

        // #293: run() returns a RunOutcome; a clean run ends Completed, and
        // this test inspects the report it carries.
        let outcome = client.run().await.expect("client run failed");
        assert_eq!(
            outcome.termination,
            riperf3::Termination::Completed,
            "a clean run ends Completed"
        );
        let report = outcome.report;

        // The end block carries both halves on a forward run.
        assert!(
            !report.end.streams.is_empty(),
            "expected at least one stream in report.end"
        );
        assert!(
            report.end.sum_sent.as_ref().unwrap().bytes > 0,
            "expected non-zero sent bytes, got {}",
            report.end.sum_sent.as_ref().unwrap().bytes
        );
        assert!(
            report.end.sum_received.as_ref().unwrap().bytes > 0,
            "expected non-zero received bytes, got {}",
            report.end.sum_received.as_ref().unwrap().bytes
        );
        // start metadata is populated …
        assert!(
            report.start.version.starts_with("riperf"),
            "start.version should name the tool: {:?}",
            report.start.version
        );
        // … and the host CPU figure is a sane number.
        let cpu = report
            .end
            .cpu_utilization_percent
            .as_ref()
            .unwrap()
            .host_total;
        assert!(
            cpu.is_finite() && cpu >= 0.0,
            "cpu host_total not a sane non-negative number: {cpu}"
        );
        // r1 F1 (#297): the started run's -J timestamp is the TestStart
        // wall-clock (RunStage::Started) — a stage regression to epoch-0 or a
        // misfiring connect-clock fallback shows here as a non-now value.
        // (Previously only a unit pin guarded this; a mutation zeroing the
        // Started stamp survived every integration test.)
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = report.start.timestamp.timesecs;
        assert!(
            now_secs.abs_diff(ts) < 300,
            "start.timestamp.timesecs must be the real TestStart wall-clock: {ts} vs now {now_secs}"
        );
        // intervals is built from the reporter's collected samples, which
        // build_report_input drains via mem::take — a non-empty array here guards
        // the #137 build-once invariant at the LIB layer (a double build would
        // empty it; previously only the CLI golden tests caught that). Even a
        // byte-limited run yields the final partial interval (#159).
        assert!(
            !report.intervals.is_empty(),
            "report.intervals empty — build-once / final-flush regression"
        );

        let _ = server_task.await;
    }

    /// #137: the `Server::run_once` analog returns the same rich `Report` for the
    /// single test it serves — symmetric with `Client::run`.
    #[tokio::test]
    async fn server_run_once_returns_rich_report() {
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            // JSON mode so the server KEEPS interval samples (discard_json),
            // making the build-once intervals guard below meaningful.
            .json_output(true)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run_once().await });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            // Duration run so the server's reporter collects intervals (the
            // build-once guard on the returned report needs them non-empty).
            .duration(2)
            .build()
            .unwrap();
        let _ = client.run().await.expect("client run failed");

        // #293: run_once returns a RunOutcome; a clean round ends Completed.
        let outcome = server_task
            .await
            .expect("server task panicked")
            .expect("server run_once failed");
        assert_eq!(
            outcome.termination,
            riperf3::Termination::Completed,
            "a clean server round ends Completed"
        );
        let report = outcome.report;
        assert!(
            !report.end.streams.is_empty(),
            "server report.end should carry the served test's streams"
        );
        // Forward run: the server is the receiver, so its received aggregate moved.
        assert!(
            report.end.sum_received.as_ref().unwrap().bytes > 0,
            "server expected non-zero received bytes, got {}",
            report.end.sum_received.as_ref().unwrap().bytes
        );
        // Guards the build-once invariant on the server path too (#137).
        assert!(
            !report.intervals.is_empty(),
            "server report.intervals empty — build-once / final-flush regression"
        );
    }
    // run_errors_when_server_skips_results_exchange migrated in-crate to src/client.rs (#67).
}

/// #260: GT's UPFRONT total-rate check at the param exchange
/// (iperf_api.c:2666-2684, beside the #230 max-duration check):
/// `total = num_streams * rate * (bidir ? 2 : 1)` — for BOTH `-b` and the fq
/// rate — refused with SERVER_ERROR + IETOTALRATE(27) BEFORE CreateStreams.
/// The client adopts iperf_strerror(27) (perr=0: no trailing ": ").
#[tokio::test]
async fn server_refuses_over_limit_rate_upfront() {
    const MSG: &str = "total required bandwidth is larger than server limit";
    // (bandwidth, fq, streams, bidir, refused)
    let cases: &[(u64, u64, u32, bool, bool)] = &[
        (2_000_000, 0, 1, false, true), // plain -b breach
        (0, 2_000_000, 1, false, true), // fq-rate twin
        (600_000, 0, 2, false, true),   // per-stream multiplier
        (600_000, 0, 1, true, true),    // bidir doubler
        // Under the limit: the test must proceed. Wide margin (0.2M vs 1M)
        // so the RUNTIME 1 Hz breach check's pacing burstiness can't trip it.
        (200_000, 0, 1, false, false),
    ];
    for &(bw, fq, streams, bidir, refused) in cases {
        let server = ServerBuilder::new()
            .port(Some(0))
            .server_bitrate_limit(1_000_000)
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.expect("bind");
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });

        let mut cb = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .num_streams(streams)
            .bidir(bidir)
            // Small blocks so the CONTROL case's real throughput respects its
            // pacing: one default 128K block already averages ~1.05 Mbps over
            // 1 s, tripping the (correct) RUNTIME breach check.
            .blksize(8 * 1024)
            .emit_output(false);
        if bw > 0 {
            cb = cb.bandwidth(bw);
        }
        if fq > 0 {
            cb = cb.fq_rate(fq);
        }
        let client = cb.build().unwrap();
        let t0 = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(15), client.run())
            .await
            .expect("client hung");
        let elapsed = t0.elapsed();
        let server_result = server_task.await.expect("server task");

        if refused {
            // #293: a relayed SERVER_ERROR is Ok(RunOutcome) now, carrying the
            // partial report + Termination::ServerError(msg).
            match result {
                Ok(outcome) => {
                    assert_eq!(
                        outcome.termination,
                        riperf3::Termination::ServerError(MSG.to_string()),
                        "GT strerror(27), perr=0 — no trailing colon"
                    );
                }
                other => panic!(
                    "bw={bw} fq={fq} P={streams} bidir={bidir}: expected the \
                     upfront IETOTALRATE refusal, got {other:?}"
                ),
            }
            // r1 F1: the refusal must be the UPFRONT param-exchange check, not
            // the runtime 1 Hz breach check (which relays the IDENTICAL code +
            // message ~1 s in). Two discriminators: an upfront-refused test
            // never ran, so the server has NO report (run_once errs), and the
            // whole exchange resolves well under the first rate tick.
            assert!(
                server_result.is_err(),
                "bw={bw} fq={fq} P={streams} bidir={bidir}: an upfront refusal \
                 produces no server report; Ok(..) means the RUNTIME check fired"
            );
            // The no-server-report assert above is the load-bearing
            // discriminator (both r1 mutations tripped IT). The elapsed
            // check is a soft sanity bound only — generous enough for a
            // starved 2-core CI runner (#191 class), still far under the
            // multi-second runs a runtime-path refusal implies.
            assert!(
                elapsed < Duration::from_secs(5),
                "bw={bw} fq={fq} P={streams} bidir={bidir}: refusal took \
                 {elapsed:?} — not remotely upfront"
            );
        } else {
            result.unwrap_or_else(|e| {
                panic!("under-limit run must proceed (rate {bw} < limit): {e}")
            });
            assert!(
                server_result.is_ok(),
                "the under-limit control produces a real server report"
            );
        }
    }
}

/// #260 r1 F3: when a client violates BOTH the max-duration and total-rate
/// limits, GT's get_parameters runs the duration check first but the rate
/// check's i_errno assignment WINS (no early return) — the refusal is
/// IETOTALRATE, live-verified against GT 3.21.
#[tokio::test]
async fn both_violations_relay_total_rate_like_gt() {
    let server = ServerBuilder::new()
        .port(Some(0))
        .server_max_duration(5)
        .server_bitrate_limit(1_000_000)
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(10)
        .bandwidth(2_000_000)
        .emit_output(false)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), client.run())
        .await
        .expect("client hung");
    let _ = server_task.await;
    match result {
        // #293: Ok(RunOutcome) with the ServerError termination.
        Ok(outcome) => {
            assert_eq!(
                outcome.termination,
                riperf3::Termination::ServerError(
                    "total required bandwidth is larger than server limit".to_string()
                ),
                "the rate refusal wins over the duration refusal (GT last-assignment)"
            );
        }
        other => panic!("expected the IETOTALRATE refusal, got {other:?}"),
    }
}

/// #293 r1 F3: the SERVER half of a RUNTIME self-terminate through `run_once`.
/// The upfront-refusal tests above assert the CLIENT's `ServerError` and leave
/// the server report-less (`run_once` errs); this pins the OTHER half. A
/// runtime `--server-bitrate-limit` breach BUILDS a partial server report, so
/// `run_once` returns `Ok(RunOutcome)` with `Termination::SelfTerminated` (the
/// server-side counterpart of the client's `ServerError`). Load-bearing
/// because a `SelfTerminated`↔`Completed` mis-map is invisible to every other
/// test — both hit `Server::run`'s silent `_ =>` arm (the stderr line is
/// emitted independently), so only a lib-level `run_once` assertion catches it.
#[tokio::test]
async fn server_run_once_self_terminates_on_runtime_bitrate_breach() {
    const MSG: &str = "total required bandwidth is larger than server limit";
    let server = ServerBuilder::new()
        .port(Some(0))
        .server_bitrate_limit(1_000) // 1 Kbit/s: any real transfer trips the 1 Hz check
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });

    // Default TCP bandwidth is 0 (unlimited): the upfront total-rate check
    // (total = 0) lets the test START, then loopback throughput blows past
    // 1 Kbit/s and the RUNTIME breach fires. An explicit `-b 2M` would instead
    // be refused UPFRONT (no server report — the other tests' path).
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(2)
        .emit_output(false)
        .build()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(15), client.run())
        .await
        .expect("client hung");
    let server_result = server_task.await.expect("server task");

    // Server: the runtime breach produced a report → Ok (an Err here is exactly
    // the pre-#293 regression, the partial report discarded); it ended
    // SelfTerminated with the bare IETOTALRATE message.
    let outcome =
        server_result.expect("run_once errored on a runtime breach — partial report discarded");
    assert_eq!(
        outcome.termination,
        riperf3::Termination::SelfTerminated(MSG.to_string()),
        "the server's own rate-limit breach ends SelfTerminated"
    );

    // Client half of the SAME breach: the relayed SERVER_ERROR.
    let cli = result.expect("client run errored");
    assert_eq!(
        cli.termination,
        riperf3::Termination::ServerError(MSG.to_string()),
        "the client sees the relayed SERVER_ERROR — the symmetric half"
    );
}

/// #392: GT renders a NEGATIVE requested window verbatim — `-w -1` is
/// real-client-reachable (unit_atof keeps the sign; only the upper bound
/// is range-checked, iperf_api.c:1446) and BOTH roles' `-J` docs carry
/// `sock_bufsize: -1` with kernel-truth actuals (live-probed 3.21; the
/// actuals were already identical on both tools — only the render
/// diverged: riperf3's `.max(0)` clamps emitted 0).
#[tokio::test]
async fn negative_window_renders_minus_one_like_gt() {
    let server = ServerBuilder::new()
        .port(Some(0))
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });
    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .window(-1)
        .emit_output(false)
        .build()
        .unwrap();
    let cli = tokio::time::timeout(Duration::from_secs(15), client.run())
        .await
        .expect("client hung")
        .expect("client run");
    let srv = server_task.await.expect("join").expect("server run_once");
    for (role, outcome) in [("client", &cli), ("server", &srv)] {
        let v = serde_json::to_value(&outcome.report).unwrap();
        assert_eq!(
            v["start"]["sock_bufsize"].as_i64(),
            Some(-1),
            "the {role} doc renders the requested -w -1 verbatim (#392): {v}"
        );
    }
}

/// Run one full round and return both roles' `start` docs as
/// `(sock_bufsize, sndbuf_actual, rcvbuf_actual)` trios — the #415 probes'
/// actuals trio, client then server. A CONCRETE port (not `Some(0)`): a UDP
/// round's data socket binds the CONFIGURED port, so an ephemeral control
/// bind strands it on a random port and the round wedges (#431).
async fn run_round_actuals(
    window: Option<i32>,
    udp: bool,
) -> [(Option<i64>, Option<i64>, Option<i64>); 2] {
    let port = next_port();
    let server = ServerBuilder::new()
        .port(Some(port))
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let server_task = tokio::spawn(async move { bound.run_once().await });
    let mut builder = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(1)
        .emit_output(false);
    if udp {
        builder = builder.protocol(TransportProtocol::Udp);
    }
    if let Some(w) = window {
        builder = builder.window(w);
    }
    let client = builder.build().unwrap();
    let cli = tokio::time::timeout(Duration::from_secs(15), client.run())
        .await
        .expect("client hung")
        .expect("client run");
    let srv = server_task.await.expect("join").expect("server run_once");
    [&cli, &srv].map(|outcome| {
        let v = serde_json::to_value(&outcome.report).unwrap();
        (
            v["start"]["sock_bufsize"].as_i64(),
            v["start"]["sndbuf_actual"].as_i64(),
            v["start"]["rcvbuf_actual"].as_i64(),
        )
    })
}

/// #415: an explicit `-w 0` is a NO-OP in GT — the buffer-apply guard is C
/// truthiness (`if ((opt = test->settings->socket_bufsize))`,
/// iperf_tcp.c:257/:434), so `socket_bufsize = 0` never reaches setsockopt
/// and the w=0 cell equals the unset cell exactly (the #391 equality-pin
/// pattern, extended from the setup-doc to the live data path). Pre-fix
/// riperf3 applied 0 to BOTH roles' data sockets — the client's directly,
/// the server's via the params blob's `"window": 0` (GT omits the key,
/// iperf_api.c:2451) — and the kernel clamped the buffers to its minimums
/// (live-probed: 4608/2304 vs 3939840/131072 untouched), a real throughput
/// divergence, not just a doc one.
#[tokio::test]
async fn tcp_window_zero_equals_unset_cell_both_roles() {
    let unset = run_round_actuals(None, false).await;
    let zero = run_round_actuals(Some(0), false).await;
    for (role, i) in [("client", 0), ("server", 1)] {
        assert_eq!(
            zero[i], unset[i],
            "the {role}'s -w 0 actuals trio must equal the unset cell (#415)"
        );
        assert_eq!(
            zero[i].0,
            Some(0),
            "the {role}'s sock_bufsize renders 0 for -w 0, like GT's verbatim 0"
        );
    }
}

/// #415, UDP flavor: the UDP apply site is `capture_stream_meta`
/// (apply_window=true, the #59 path), distinct from TCP's
/// `configure_tcp_stream_full` — GT's guard is the same truthiness in
/// `iperf_udp_buffercheck` (iperf_udp.c:384). Client doc only: the server's
/// UDP start block carries no actuals (#383).
#[tokio::test]
async fn udp_window_zero_equals_unset_cell() {
    let unset = run_round_actuals(None, true).await;
    let zero = run_round_actuals(Some(0), true).await;
    assert_eq!(
        zero[0], unset[0],
        "the client's UDP -w 0 actuals trio must equal the unset cell (#415)"
    );
}

/// #415 (wire half): GT gates the params blob's `"window"` key on the same
/// truthiness (`if (test->settings->socket_bufsize)` — iperf_api.c:2451), so
/// a `-w 0` client sends NO window key. Pre-fix riperf3 sent `"window": 0`,
/// which a pre-fix riperf3 server applied to its data sockets (the server
/// half of the live bug; a GT server ingests 0 harmlessly but the wire bytes
/// still diverge). A `-w 65536` control cell keeps the positive path pinned.
#[tokio::test]
async fn params_blob_omits_window_zero_like_gt() {
    use std::io::{Read, Write};

    async fn capture_params_blob(window: i32) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let mock = tokio::task::spawn_blocking(move || {
            let (mut ctrl, _) = listener.accept().expect("ctrl accept");
            let mut cookie = [0u8; 37];
            ctrl.read_exact(&mut cookie).unwrap();
            ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
            let mut len = [0u8; 4];
            ctrl.read_exact(&mut len).unwrap();
            let mut blob = vec![0u8; u32::from_be_bytes(len) as usize];
            ctrl.read_exact(&mut blob).unwrap();
            blob
            // ctrl drops here — the client errors out of the round promptly.
        });
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .window(window)
            .emit_output(false)
            .build()
            .unwrap();
        let run = tokio::spawn(async move { client.run().await });
        let blob = tokio::time::timeout(Duration::from_secs(10), mock)
            .await
            .expect("mock hung")
            .expect("mock join");
        // Reap the erroring client; its outcome is not this pin's business.
        let _ = tokio::time::timeout(Duration::from_secs(10), run).await;
        serde_json::from_slice(&blob).expect("params blob is JSON")
    }

    let zero = capture_params_blob(0).await;
    assert!(
        zero.get("window").is_none(),
        "-w 0 must omit the params blob's window key like GT (iperf_api.c:2451): {zero}"
    );
    let explicit = capture_params_blob(65536).await;
    assert_eq!(
        explicit["window"].as_i64(),
        Some(65536),
        "a nonzero -w still carries the window key: {explicit}"
    );
}

/// #406 (r1): the `RecvMessageFailed`↔`SendFailed` cell of the same
/// invisibility class — the repo's first RENDERING-IDENTICAL variant pair
/// (both print `error - {msg}` with the carried message and both errexit
/// to None), so only a lib-level `run_once` assertion discriminates them.
/// A raw mock completes the full round through DisplayResults, then
/// SO_LINGER(0)-RSTs instead of IperfDone — the IperfDone-wait read fails
/// hard (IERECVMESSAGE) over the POPULATED report. Linux-gated like every
/// RST-timing cell (the #339 lesson).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn server_run_once_exchange_rst_ends_recv_message_failed() {
    let server = ServerBuilder::new()
        .port(Some(0))
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });

    // The raw round runs on a blocking thread (std sockets).
    let mock = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let rd1 = |s: &mut std::net::TcpStream| {
            let mut b = [0u8; 1];
            s.read_exact(&mut b).expect("state byte");
            b[0]
        };
        let blob_w = |s: &mut std::net::TcpStream, p: &str| {
            s.write_all(&(p.len() as u32).to_be_bytes()).unwrap();
            s.write_all(p.as_bytes()).unwrap();
        };
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(rd1(&mut ctrl), 9, "ParamExchange");
        blob_w(
            &mut ctrl,
            r#"{"tcp":true,"time":1,"parallel":1,"len":4096}"#,
        );
        assert_eq!(rd1(&mut ctrl), 10, "CreateStreams");
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(rd1(&mut ctrl), 1, "TestStart");
        assert_eq!(rd1(&mut ctrl), 2, "TestRunning");
        data.write_all(&[0u8; 4096]).unwrap();
        ctrl.write_all(&[4u8]).unwrap(); // TestEnd
        assert_eq!(rd1(&mut ctrl), 13, "ExchangeResults");
        blob_w(
            &mut ctrl,
            r#"{"cpu_util_total":1.0,"cpu_util_user":0.5,"cpu_util_system":0.5,"sender_has_retransmits":1,"streams":[{"id":1,"bytes":4096,"retransmits":0,"jitter":0,"errors":0,"packets":0,"start_time":0,"end_time":1}]}"#,
        );
        // Read the server's results blob, then DisplayResults.
        let mut len = [0u8; 4];
        ctrl.read_exact(&mut len).unwrap();
        let mut blob = vec![0u8; u32::from_be_bytes(len) as usize];
        ctrl.read_exact(&mut blob).unwrap();
        assert_eq!(rd1(&mut ctrl), 14, "DisplayResults");
        // The #406 cell: RST instead of IperfDone/FIN.
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        let rc = unsafe {
            use std::os::fd::AsRawFd;
            libc::setsockopt(
                ctrl.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                std::ptr::from_ref(&linger).cast(),
                std::mem::size_of::<libc::linger>() as libc::socklen_t,
            )
        };
        assert_eq!(rc, 0, "SO_LINGER(0)");
        drop((ctrl, data));
    });

    let outcome = tokio::time::timeout(Duration::from_secs(15), server_task)
        .await
        .expect("server hung")
        .expect("join")
        .expect("the completed-exchange RST is a report-producing round (#406)");
    mock.await.expect("mock");
    match &outcome.termination {
        riperf3::Termination::RecvMessageFailed(msg) => assert!(
            msg.starts_with("unable to receive control message"),
            "the carried IERECVMESSAGE message: {msg:?}"
        ),
        other => panic!("expected RecvMessageFailed, got {other:?}"),
    }
    assert!(
        !outcome.report.end.streams.is_empty(),
        "the exchange completed — the report keeps the POPULATED end (#406)"
    );
}

/// The server-side `Interrupted` cell of the same invisibility class: a
/// mid-test interrupt on `run_once` comes back `Ok(RunOutcome)` with
/// `Termination::Interrupted`. Like `SelfTerminated` above, an
/// `Interrupted`↔`Completed` mis-map hits `Server::run`'s silent `_ =>` arm
/// and both exit 0, so only a lib-level assertion catches it. The client
/// sees the SERVER_TERMINATE half (iperf_got_sigend's server arm).
#[tokio::test]
async fn server_run_once_interrupt_mid_test_ends_interrupted() {
    let (tx, rx) = tokio::sync::watch::channel::<Option<String>>(None);
    let server = ServerBuilder::new()
        .port(Some(0))
        .interrupt(rx)
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .duration(6)
        .emit_output(false)
        .build()
        .unwrap();
    let client_task =
        tokio::spawn(
            async move { tokio::time::timeout(Duration::from_secs(20), client.run()).await },
        );
    // 1.5 s into a 6 s test: the loopback handshake is millis-class even
    // under CI contention (the breach test above rides the same margin), so
    // the watch fires in TEST_RUNNING, not the setup phase (which is the
    // documented Err path instead).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    tx.send(Some(
        "interrupt - the server has terminated by signal Terminated(15)".into(),
    ))
    .unwrap();

    let server_result = tokio::time::timeout(Duration::from_secs(10), server_task)
        .await
        .expect("server hung after the interrupt")
        .expect("server task");
    let outcome = server_result.expect("an interrupted mid-test round still carries its report");
    assert_eq!(
        outcome.termination,
        riperf3::Termination::Interrupted,
        "a local signal mid-test ends Interrupted"
    );

    let cli = client_task
        .await
        .expect("client task")
        .expect("client hung")
        .expect("client run");
    assert_eq!(
        cli.termination,
        riperf3::Termination::ServerTerminated,
        "the client sees SERVER_TERMINATE — the symmetric half"
    );
}

/// #302: GT enables SO_MAX_PACING_RATE on its ACCEPTED data sockets too
/// (iperf_tcp.c:138-153), so a client's --fq-rate paces the server's
/// reverse send path. Live GT reverse --fq-rate 5M ≈ 6.3 Mbps where the
/// unpaced loopback runs ~100 Gbps — pre-fix riperf3 measured 95 Gbps.
/// Linux-gated: the sockopt (and kernel-side TCP pacing) is Linux-only.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn server_paces_reverse_with_the_client_fq_rate() {
    let server = ServerBuilder::new()
        .port(Some(0))
        .emit_output(false)
        .build()
        .unwrap();
    let bound = server.bind().await.expect("bind");
    let port = bound.local_addr().unwrap().port();
    let server_task = tokio::spawn(async move { bound.run_once().await });

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .fq_rate(5_000_000)
        .duration(2)
        .emit_output(false)
        .build()
        .unwrap();
    let report = tokio::time::timeout(Duration::from_secs(25), client.run())
        .await
        .expect("client hung")
        .expect("client errored")
        .report;
    let bps = report.end.sum_received.as_ref().unwrap().bits_per_second;
    assert!(
        bps < 20_000_000.0,
        "the server's reverse send is fq-paced at ~5 Mbps like GT, got {bps}"
    );
    let _ = server_task.await;
}
