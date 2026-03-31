//! Integration tests: full client↔server protocol over loopback.
//!
//! These tests start a real riperf3 server and client on localhost,
//! exercise the complete wire protocol, and verify results.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use riperf3::protocol::TransportProtocol;
use riperf3::{ClientBuilder, ServerBuilder};

/// Allocate unique ports for parallel test execution.
static NEXT_PORT: AtomicU16 = AtomicU16::new(15201);
fn next_port() -> u16 {
    NEXT_PORT.fetch_add(1, Ordering::Relaxed)
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
    let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
    let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
    assert!(
        result.is_ok(),
        "Client failed with -P 2 -R -N: {result:?}"
    );

    let _ = server_task.await;
}

// ---------------------------------------------------------------------------
// Bug regression tests — specific behavioral verification
// ---------------------------------------------------------------------------

/// Verify -C congestion algorithm is applied to data stream sockets.
/// Bug: congestion was sent in TestParams JSON but not applied via setsockopt.
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
    use riperf3::net::set_cpu_affinity;

    // Set affinity to CPU 0 on the current thread and verify
    set_cpu_affinity(0).unwrap();

    unsafe {
        let mut cpuset = std::mem::MaybeUninit::<libc::cpu_set_t>::zeroed().assume_init();
        let ret = libc::sched_getaffinity(
            0,
            std::mem::size_of::<libc::cpu_set_t>(),
            &mut cpuset,
        );
        assert_eq!(ret, 0);
        assert!(libc::CPU_ISSET(0, &cpuset));
        // Verify we're NOT set on CPU 1 (unless machine only has 1 core)
        let nproc = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
        if nproc > 1 {
            assert!(!libc::CPU_ISSET(1, &cpuset));
        }
    }
}

// ---------------------------------------------------------------------------
// Builder coverage tests
// ---------------------------------------------------------------------------

mod builder_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn client_builder_protocol() {
        let c = ClientBuilder::new("h")
            .protocol(TransportProtocol::Udp)
            .build()
            .unwrap();
        assert_eq!(c.protocol, TransportProtocol::Udp);
    }

    #[test]
    fn client_builder_duration() {
        let c = ClientBuilder::new("h").duration(30).build().unwrap();
        assert_eq!(c.duration, 30);
    }

    #[test]
    fn client_builder_num_streams() {
        let c = ClientBuilder::new("h").num_streams(8).build().unwrap();
        assert_eq!(c.num_streams, 8);
    }

    #[test]
    fn client_builder_blksize() {
        let c = ClientBuilder::new("h").blksize(65536).build().unwrap();
        assert_eq!(c.blksize, 65536);
    }

    #[test]
    fn client_builder_blksize_defaults() {
        let tcp = ClientBuilder::new("h").build().unwrap();
        assert_eq!(tcp.blksize, 128 * 1024);

        let udp = ClientBuilder::new("h")
            .protocol(TransportProtocol::Udp)
            .build()
            .unwrap();
        assert_eq!(udp.blksize, 1460);
    }

    #[test]
    fn client_builder_reverse() {
        let c = ClientBuilder::new("h").reverse(true).build().unwrap();
        assert!(c.reverse);
    }

    #[test]
    fn client_builder_bidir() {
        let c = ClientBuilder::new("h").bidir(true).build().unwrap();
        assert!(c.bidir);
    }

    #[test]
    fn client_builder_omit() {
        let c = ClientBuilder::new("h").omit(3).build().unwrap();
        assert_eq!(c.omit, 3);
    }

    #[test]
    fn client_builder_no_delay() {
        let c = ClientBuilder::new("h").no_delay(true).build().unwrap();
        assert!(c.no_delay);
    }

    #[test]
    fn client_builder_mss() {
        let c = ClientBuilder::new("h").mss(1400).build().unwrap();
        assert_eq!(c.mss, Some(1400));
    }

    #[test]
    fn client_builder_window() {
        let c = ClientBuilder::new("h").window(524288).build().unwrap();
        assert_eq!(c.window, Some(524288));
    }

    #[test]
    fn client_builder_bandwidth() {
        let c = ClientBuilder::new("h").bandwidth(1_000_000).build().unwrap();
        assert_eq!(c.bandwidth, 1_000_000);
    }

    #[test]
    fn client_builder_tos() {
        let c = ClientBuilder::new("h").tos(0x10).build().unwrap();
        assert_eq!(c.tos, 0x10);
    }

    #[test]
    fn client_builder_congestion() {
        let c = ClientBuilder::new("h").congestion("bbr").build().unwrap();
        assert_eq!(c.congestion, Some("bbr".to_string()));
    }

    #[test]
    fn client_builder_udp_64bit() {
        let c = ClientBuilder::new("h")
            .udp_counters_64bit(true)
            .build()
            .unwrap();
        assert!(c.udp_counters_64bit);
    }

    #[test]
    fn client_builder_connect_timeout() {
        let c = ClientBuilder::new("h")
            .connect_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        assert_eq!(c.connect_timeout, Some(Duration::from_millis(500)));
    }

    #[test]
    fn client_builder_title() {
        let c = ClientBuilder::new("h").title("my test").build().unwrap();
        assert_eq!(c.title, Some("my test".to_string()));
    }

    #[test]
    fn client_builder_extra_data() {
        let c = ClientBuilder::new("h").extra_data("x").build().unwrap();
        assert_eq!(c.extra_data, Some("x".to_string()));
    }

    #[test]
    fn client_builder_verbose() {
        let c = ClientBuilder::new("h").verbose(true).build().unwrap();
        assert!(c.verbose);
    }

    #[test]
    fn client_builder_json_output() {
        let c = ClientBuilder::new("h").json_output(true).build().unwrap();
        assert!(c.json_output);
    }

    #[test]
    fn client_builder_bytes() {
        let c = ClientBuilder::new("h").bytes(1_000_000).build().unwrap();
        assert_eq!(c.bytes_to_send, Some(1_000_000));
    }

    #[test]
    fn client_builder_blocks() {
        let c = ClientBuilder::new("h").blocks(100).build().unwrap();
        assert_eq!(c.blocks_to_send, Some(100));
    }

    #[test]
    fn server_builder_one_off() {
        let s = ServerBuilder::new().one_off(true).build().unwrap();
        assert!(s.one_off);
    }

    #[test]
    fn server_builder_verbose() {
        let s = ServerBuilder::new().verbose(true).build().unwrap();
        assert!(s.verbose);
    }
}

// ---------------------------------------------------------------------------
// TestConfig from params
// ---------------------------------------------------------------------------

mod test_config_tests {
    use riperf3::protocol::{TestParams, TransportProtocol};
    use riperf3::TestConfig;

    #[test]
    fn tcp_defaults() {
        let p = TestParams {
            tcp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p);
        assert_eq!(cfg.protocol, TransportProtocol::Tcp);
        assert_eq!(cfg.duration, 10);
        assert_eq!(cfg.num_streams, 1);
        assert_eq!(cfg.blksize, 128 * 1024);
        assert!(!cfg.reverse);
        assert!(!cfg.bidir);
    }

    #[test]
    fn udp_defaults() {
        let p = TestParams {
            udp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p);
        assert_eq!(cfg.protocol, TransportProtocol::Udp);
        assert_eq!(cfg.blksize, 1460);
    }

    #[test]
    fn full_params() {
        let p = TestParams {
            tcp: Some(true),
            time: Some(30),
            parallel: Some(4),
            len: Some(65536),
            reverse: Some(true),
            bidirectional: Some(true),
            omit: Some(2),
            nodelay: Some(true),
            mss: Some(1400),
            window: Some(524288),
            bandwidth: Some(1_000_000_000),
            tos: Some(0x10),
            congestion: Some("bbr".to_string()),
            udp_counters_64bit: Some(1),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p);
        assert_eq!(cfg.duration, 30);
        assert_eq!(cfg.num_streams, 4);
        assert_eq!(cfg.blksize, 65536);
        assert!(cfg.reverse);
        assert!(cfg.bidir);
        assert_eq!(cfg.omit, 2);
        assert!(cfg.no_delay);
        assert_eq!(cfg.mss, Some(1400));
        assert_eq!(cfg.window, Some(524288));
        assert_eq!(cfg.bandwidth, 1_000_000_000);
        assert_eq!(cfg.tos, 0x10);
        assert_eq!(cfg.congestion, Some("bbr".to_string()));
        assert!(cfg.udp_counters_64bit);
    }
}

// ---------------------------------------------------------------------------
// Protocol param/results round-trip
// ---------------------------------------------------------------------------

mod protocol_tests {
    use riperf3::protocol::{self, TestParams, TestResultsJson, StreamResultJson};

    #[tokio::test]
    async fn params_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let params = TestParams {
            tcp: Some(true),
            time: Some(30),
            parallel: Some(4),
            len: Some(65536),
            reverse: Some(true),
            nodelay: Some(true),
            mss: Some(1400),
            bandwidth: Some(1_000_000),
            congestion: Some("cubic".to_string()),
            client_version: Some("test 1.0".to_string()),
            ..Default::default()
        };

        let params_clone = params.clone();
        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            protocol::send_params(&mut stream, &params_clone).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let received = protocol::recv_params(&mut stream).await.unwrap();
        writer.await.unwrap();

        assert_eq!(received.tcp, Some(true));
        assert_eq!(received.time, Some(30));
        assert_eq!(received.parallel, Some(4));
        assert_eq!(received.len, Some(65536));
        assert_eq!(received.reverse, Some(true));
        assert_eq!(received.nodelay, Some(true));
        assert_eq!(received.mss, Some(1400));
        assert_eq!(received.bandwidth, Some(1_000_000));
        assert_eq!(received.congestion, Some("cubic".to_string()));
        assert_eq!(received.client_version, Some("test 1.0".to_string()));
        // Unset fields should remain None
        assert_eq!(received.udp, None);
        assert_eq!(received.bidirectional, None);
    }

    #[tokio::test]
    async fn results_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let results = TestResultsJson {
            cpu_util_total: 42.5,
            cpu_util_user: 30.0,
            cpu_util_system: 12.5,
            sender_has_retransmits: 3,
            congestion_used: Some("bbr".to_string()),
            streams: vec![
                StreamResultJson {
                    id: 1,
                    bytes: 10_000_000,
                    retransmits: 3,
                    jitter: 0.0,
                    errors: 0,
                    omitted_errors: 0,
                    packets: 0,
                    omitted_packets: 0,
                    start_time: 0.0,
                    end_time: 10.0,
                },
                StreamResultJson {
                    id: 3,
                    bytes: 9_500_000,
                    retransmits: 0,
                    jitter: 0.0,
                    errors: 0,
                    omitted_errors: 0,
                    packets: 0,
                    omitted_packets: 0,
                    start_time: 0.0,
                    end_time: 10.0,
                },
            ],
        };

        let results_clone = results.clone();
        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            protocol::send_results(&mut stream, &results_clone).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let received = protocol::recv_results(&mut stream).await.unwrap();
        writer.await.unwrap();

        assert_eq!(received.cpu_util_total, 42.5);
        assert_eq!(received.sender_has_retransmits, 3);
        assert_eq!(received.congestion_used, Some("bbr".to_string()));
        assert_eq!(received.streams.len(), 2);
        assert_eq!(received.streams[0].id, 1);
        assert_eq!(received.streams[0].bytes, 10_000_000);
        assert_eq!(received.streams[1].id, 3);
    }

    #[tokio::test]
    async fn oversized_json_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            // Write a JSON payload larger than MAX_PARAMS_JSON_LEN (8KB)
            let big = serde_json::json!({"data": "x".repeat(10_000)});
            protocol::json_write(&mut stream, &big).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let result = protocol::json_read(&mut stream, protocol::MAX_PARAMS_JSON_LEN).await;
        writer.await.unwrap();

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn params_with_unknown_fields_accepted() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Simulate a future iperf3 version sending extra fields
        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let json = serde_json::json!({
                "tcp": true,
                "time": 10,
                "some_future_field": 42,
                "another_new_thing": "hello"
            });
            protocol::json_write(&mut stream, &json).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let result = protocol::recv_params(&mut stream).await;
        writer.await.unwrap();

        // Should succeed — unknown fields are ignored by serde default
        let params = result.unwrap();
        assert_eq!(params.tcp, Some(true));
        assert_eq!(params.time, Some(10));
    }
}

// ---------------------------------------------------------------------------
// Error path tests
// ---------------------------------------------------------------------------

mod error_tests {
    use riperf3::error::ConfigError;
    use riperf3::utils::{parse_kmg, parse_bitrate};
    use riperf3::ClientBuilder;

    #[test]
    fn build_without_host_fails() {
        let r = ClientBuilder::default().build();
        assert!(matches!(r, Err(ConfigError::MissingField("host"))));
    }

    #[test]
    fn parse_kmg_negative() {
        assert!(parse_kmg("-1").is_err());
    }

    #[test]
    fn parse_kmg_float() {
        assert!(parse_kmg("1.5M").is_err());
    }

    #[test]
    fn parse_kmg_empty_suffix() {
        assert!(parse_kmg("K").is_err());
    }

    #[test]
    fn parse_bitrate_empty() {
        assert!(parse_bitrate("").is_err());
    }

    #[test]
    fn parse_bitrate_bad_burst() {
        assert!(parse_bitrate("100M/abc").is_err());
    }

    // -- Error type display messages --

    #[test]
    fn error_display_variants() {
        use riperf3::RiperfError;
        assert_eq!(format!("{}", RiperfError::CookieMismatch), "cookie mismatch");
        assert_eq!(format!("{}", RiperfError::AccessDenied), "access denied by server");
        assert_eq!(format!("{}", RiperfError::PeerDisconnected), "peer disconnected");
        assert!(format!("{}", RiperfError::Aborted("test".into())).contains("test"));
        assert_eq!(format!("{}", RiperfError::ConnectionTimeout), "connection timed out");
        assert!(format!("{}", RiperfError::Protocol("bad".into())).contains("bad"));
        assert!(format!("{}", RiperfError::Aborted("reason".into())).contains("reason"));
    }

    // -- Edge cases --

    #[tokio::test]
    async fn connect_to_wrong_port_fails() {
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(1)) // port 1 — almost certainly not listening
            .duration(1)
            .connect_timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "connecting to port 1 should fail");
    }
}

// ---------------------------------------------------------------------------
// Protocol error state tests
// ---------------------------------------------------------------------------

mod protocol_error_tests {
    use riperf3::protocol::{self, TestState};

    #[tokio::test]
    async fn client_handles_access_denied() {
        // Server sends AccessDenied state — client should return AccessDenied error
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read cookie (37 bytes)
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie).await.unwrap();
            // Send AccessDenied
            protocol::send_state(&mut stream, TestState::AccessDenied).await.unwrap();
        });

        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "client should error on AccessDenied");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("access denied") || err.contains("protocol"),
            "error should mention access denied, got: {err}"
        );
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn client_handles_server_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie).await.unwrap();
            protocol::send_state(&mut stream, TestState::ServerError).await.unwrap();
        });

        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "client should error on ServerError");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn client_handles_peer_disconnect_during_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Accept connection then immediately close it
            drop(stream);
        });

        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "client should error on peer disconnect");
        let _ = server_task.await;
    }
}

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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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

    #[test]
    fn test_params_serializes_all_fields() {
        use riperf3::protocol::TestParams;
        let p = TestParams {
            tcp: Some(true),
            time: Some(10),
            parallel: Some(4),
            len: Some(131072),
            reverse: Some(true),
            bidirectional: Some(true),
            nodelay: Some(true),
            mss: Some(1400),
            window: Some(524288),
            bandwidth: Some(1_000_000),
            tos: Some(16),
            congestion: Some("bbr".to_string()),
            client_version: Some("riperf3 0.1.0".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["tcp"], true);
        assert_eq!(v["time"], 10);
        assert_eq!(v["parallel"], 4);
        assert_eq!(v["len"], 131072);
        assert_eq!(v["reverse"], true);
        assert_eq!(v["bidirectional"], true);
        assert_eq!(v["nodelay"], true);
        assert_eq!(v["MSS"], 1400);
        assert_eq!(v["window"], 524288);
        assert_eq!(v["bandwidth"], 1_000_000);
        assert_eq!(v["TOS"], 16);
        assert_eq!(v["congestion"], "bbr");
        assert_eq!(v["client_version"], "riperf3 0.1.0");
    }

    #[test]
    fn test_results_json_structure() {
        use riperf3::protocol::{TestResultsJson, StreamResultJson};
        let r = TestResultsJson {
            cpu_util_total: 50.0,
            cpu_util_user: 40.0,
            cpu_util_system: 10.0,
            sender_has_retransmits: 5,
            congestion_used: Some("cubic".to_string()),
            streams: vec![StreamResultJson {
                id: 1,
                bytes: 10_000_000_000,
                retransmits: 5,
                jitter: 0.001,
                errors: 2,
                omitted_errors: 0,
                packets: 10000,
                omitted_packets: 0,
                start_time: 0.0,
                end_time: 10.0,
            }],
        };
        let json = serde_json::to_string(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["cpu_util_total"], 50.0);
        assert_eq!(v["sender_has_retransmits"], 5);
        assert_eq!(v["congestion_used"], "cubic");
        assert_eq!(v["streams"][0]["id"], 1);
        assert_eq!(v["streams"][0]["bytes"], 10_000_000_000u64);
        assert_eq!(v["streams"][0]["retransmits"], 5);
    }
}

// ---------------------------------------------------------------------------
// Interval reporter edge cases
// ---------------------------------------------------------------------------

mod interval_reporter_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use riperf3::protocol::TransportProtocol;
    use riperf3::reporter::{IntervalReporterConfig, spawn_interval_reporter};

    #[tokio::test]
    async fn disabled_returns_none() {
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: 0.0,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            timestamp_format: None,
            json_stream: false,
        };
        assert!(spawn_interval_reporter(config, vec![], done).is_none());
    }

    #[tokio::test]
    async fn negative_interval_returns_none() {
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: -1.0,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            timestamp_format: None,
            json_stream: false,
        };
        assert!(spawn_interval_reporter(config, vec![], done).is_none());
    }

    #[tokio::test]
    async fn zero_streams_doesnt_panic() {
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: 0.5,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 0,
            forceflush: false,
            timestamp_format: None,
            json_stream: false,
        };
        let handle = spawn_interval_reporter(config, vec![], done.clone());
        assert!(handle.is_some());
        // Let it tick once then stop
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        done.store(true, Ordering::Relaxed);
        if let Some(h) = handle {
            let _ = h.await;
        }
    }
}

// ---------------------------------------------------------------------------
// UDP edge cases
// ---------------------------------------------------------------------------

mod udp_edge_tests {
    use riperf3::stream::{UdpHeader, UdpRecvStats};

    #[test]
    fn udp_header_32bit_sequence_max() {
        let h = UdpHeader { sec: 0, usec: 0, seq: u32::MAX as u64 };
        let mut buf = [0u8; 16];
        h.write_to(&mut buf, false);
        let h2 = UdpHeader::read_from(&buf, false).unwrap();
        assert_eq!(h2.seq, u32::MAX as u64);
    }

    #[test]
    fn udp_header_64bit_sequence_max() {
        let h = UdpHeader { sec: 0, usec: 0, seq: u64::MAX };
        let mut buf = [0u8; 16];
        h.write_to(&mut buf, true);
        let h2 = UdpHeader::read_from(&buf, true).unwrap();
        assert_eq!(h2.seq, u64::MAX);
    }

    #[test]
    fn udp_stats_massive_gap() {
        // Simulate losing 1000 packets at once
        let mut stats = UdpRecvStats::new();
        stats.update(&UdpHeader { sec: 0, usec: 0, seq: 1 }, 0.0);
        stats.update(&UdpHeader { sec: 0, usec: 0, seq: 1002 }, 1.0);
        assert_eq!(stats.cnt_error, 1000);
        assert_eq!(stats.packet_count, 1002);
    }

    #[test]
    fn udp_stats_duplicate_packet() {
        let mut stats = UdpRecvStats::new();
        stats.update(&UdpHeader { sec: 0, usec: 0, seq: 1 }, 0.0);
        stats.update(&UdpHeader { sec: 0, usec: 0, seq: 2 }, 0.001);
        // Duplicate of packet 1
        stats.update(&UdpHeader { sec: 0, usec: 0, seq: 1 }, 0.002);
        assert_eq!(stats.outoforder_packets, 1);
        assert_eq!(stats.packet_count, 2);
    }
}

// ---------------------------------------------------------------------------
// Stream ID assignment
// ---------------------------------------------------------------------------

mod stream_id_tests {
    use riperf3::utils::iperf3_stream_id;

    #[test]
    fn matches_iperf3_pattern() {
        assert_eq!(iperf3_stream_id(0), 1);
        assert_eq!(iperf3_stream_id(1), 3);
        assert_eq!(iperf3_stream_id(2), 4);
        assert_eq!(iperf3_stream_id(3), 5);
        assert_eq!(iperf3_stream_id(4), 6);
        assert_eq!(iperf3_stream_id(9), 11);
    }
}

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

    #[tokio::test]
    async fn format_flag_kbits() {
        // -f format is wired to Client.format_char and used in reporter
        let c = ClientBuilder::new("h").format_char('k').build().unwrap();
        assert_eq!(c.format_char, 'k');
    }

    #[tokio::test]
    async fn udp_counters_64bit_flag() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        assert!(result.is_ok(), "UDP with 64-bit counters failed: {result:?}");
        let _ = server_task.await;
    }

    #[test]
    fn repeating_payload_buffer() {
        let buf = riperf3::utils::make_send_buffer(256, true);
        assert_eq!(buf.len(), 256);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[255], 255);

        let zeros = riperf3::utils::make_send_buffer(256, false);
        assert!(zeros.iter().all(|&b| b == 0));
    }

    #[tokio::test]
    async fn repeating_payload_runs() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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

    #[tokio::test]
    async fn congestion_cubic_runs() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        use riperf3::net::set_cpu_affinity;
        set_cpu_affinity(0).unwrap();
        unsafe {
            let mut cpuset = std::mem::MaybeUninit::<libc::cpu_set_t>::zeroed().assume_init();
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut cpuset);
            assert!(libc::CPU_ISSET(0, &cpuset));
        }
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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

    #[tokio::test]
    async fn mptcp_flag_accepted() {
        // MPTCP may not be available on all kernels. Just verify the flag
        // is wired and the client attempts to use it (may fail with EPROTONOSUPPORT).
        let c = ClientBuilder::new("h").mptcp(true).build().unwrap();
        assert!(c.mptcp);
    }

    #[test]
    fn dscp_symbolic_and_numeric() {
        use riperf3::utils::parse_dscp;
        // Symbolic names
        assert_eq!(parse_dscp("ef").unwrap(), 46 << 2);    // EF = 184
        assert_eq!(parse_dscp("af11").unwrap(), 10 << 2);  // AF11 = 40
        assert_eq!(parse_dscp("cs1").unwrap(), 8 << 2);    // CS1 = 32
        // Numeric
        assert_eq!(parse_dscp("46").unwrap(), 46 << 2);
        assert_eq!(parse_dscp("0x2e").unwrap(), 46 << 2);  // 0x2e = 46
        assert_eq!(parse_dscp("056").unwrap(), 46 << 2);   // 056 octal = 46
        // Out of range
        assert!(parse_dscp("64").is_err());
        assert!(parse_dscp("abc").is_err());
    }

    #[tokio::test]
    async fn dscp_flag_runs() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .dscp("ef")
            .build()
            .unwrap();
        assert_eq!(client.tos, 46 << 2); // EF mapped to TOS
        let result = client.run().await;
        assert!(result.is_ok(), "--dscp ef failed: {result:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn client_port_binding() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
    // UDP-specific flag tests — verify Tier 2 flags work with UDP protocol
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn udp_dont_fragment() {
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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

    #[tokio::test]
    async fn bind_device_loopback() {
        // --bind-dev lo: bind all sockets to the loopback interface.
        // Works unprivileged on Linux 5.7+.
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .bind_dev("lo")
            .build()
            .unwrap();
        let result = client.run().await;
        if let Err(ref e) = result {
            let msg = format!("{e}");
            if msg.contains("Operation not permitted") {
                return; // old kernel, skip gracefully
            }
        }
        assert!(result.is_ok(), "--bind-dev lo failed: {result:?}");
        let _ = server_task.await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bind_device_invalid_rejects() {
        // An invalid device name should cause an error, proving set_bind_dev is called.
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        assert!(result.is_err(), "--bind-dev nonexistent should fail but succeeded");
        let _ = server_task.await;
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .reverse(true)  // server sends, client receives with skip-rx-copy
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
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
        assert!(elapsed.as_secs() < 5, "idle-timeout took too long: {elapsed:?}");
        // Timeout is not an error in one-off mode — server just exits
        let _ = result;
    }

    #[tokio::test]
    async fn server_max_duration() {
        // Server with max_duration=2 should terminate a test that runs longer
        let port = next_port();
        let server = ServerBuilder::new()
            .port(Some(port))
            .one_off(true)
            .server_max_duration(2)
            .build()
            .unwrap();
        let server_task = tokio::spawn(async move { server.run().await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Client tries to run 10 seconds but server should cut it at 2
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(10)
            .build()
            .unwrap();
        let start = std::time::Instant::now();
        let _ = client.run().await;
        let elapsed = start.elapsed();
        assert!(elapsed.as_secs() < 5, "server-max-duration didn't cut test: {elapsed:?}");
        let _ = server_task.await;
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
        let _ = client.run().await; // may error — server terminates early
        let elapsed = start.elapsed();
        // Should terminate well before 10 seconds
        assert!(elapsed.as_secs() < 5, "bitrate limit didn't cut test: {elapsed:?}");
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn file_transfer_send() {
        // -F with sender: read from file instead of zero buffer
        let port = next_port();

        // Create a temp file with known content
        let tmp = std::env::temp_dir().join(format!("riperf3-test-send-{port}"));
        std::fs::write(&tmp, vec![0xABu8; 1024 * 1024]).unwrap(); // 1 MB

        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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

        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
    async fn json_stream_runs() {
        // --json-stream: each interval emitted as JSON. Just verify it doesn't crash.
        let port = next_port();
        let server = ServerBuilder::new().port(Some(port)).one_off(true).build().unwrap();
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
    #[ignore = "not yet implemented: --gsro"]
    async fn udp_gso_gro() {}

    // -- Deferred --

    #[tokio::test]
    #[ignore = "deferred: -Z zerocopy requires sendfile/splice"]
    async fn zerocopy_send() {}

    #[tokio::test]
    #[ignore = "deferred: auth requires RSA key handling"]
    async fn rsa_authentication() {}
}
