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
}

// ---------------------------------------------------------------------------
// Stream ID assignment
// ---------------------------------------------------------------------------

mod stream_id_tests {
    use riperf3::utils::iperf3_stream_id;

    #[test]
    fn matches_iperf3_pattern() {
        // iperf3's iperf_add_stream assigns: 1, 3, 4, 5, 6, ...
        assert_eq!(iperf3_stream_id(0), 1);
        assert_eq!(iperf3_stream_id(1), 3);
        assert_eq!(iperf3_stream_id(2), 4);
        assert_eq!(iperf3_stream_id(3), 5);
        assert_eq!(iperf3_stream_id(4), 6);
        assert_eq!(iperf3_stream_id(9), 11);
    }
}
