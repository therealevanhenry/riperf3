use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpStream;

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, RiperfError, Result};
use crate::net;
use crate::protocol::{
    self, TestParams, TestResultsJson, TestState, TransportProtocol,
};
use crate::stream::{
    self, DataStream, RateLimiter, StreamCounters, UdpRecvStats,
};
use crate::utils::*;

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Client {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) protocol: TransportProtocol,
    pub(crate) duration: u32,
    pub(crate) num_streams: u32,
    pub(crate) blksize: usize,
    pub(crate) reverse: bool,
    pub(crate) bidir: bool,
    pub(crate) omit: u32,
    pub(crate) no_delay: bool,
    pub(crate) mss: Option<i32>,
    pub(crate) window: Option<i32>,
    pub(crate) bandwidth: u64,
    pub(crate) tos: i32,
    pub(crate) congestion: Option<String>,
    pub(crate) udp_counters_64bit: bool,
    pub(crate) connect_timeout: Option<Duration>,
    pub(crate) title: Option<String>,
    pub(crate) extra_data: Option<String>,
    pub(crate) verbose: bool,
    pub(crate) bytes_to_send: Option<u64>,
    pub(crate) blocks_to_send: Option<u64>,
}

impl Client {
    pub async fn run(&self) -> Result<()> {
        // ---- Generate cookie and connect ----
        let cookie = protocol::make_cookie();
        let mut ctrl = net::tcp_connect(&self.host, self.port, self.connect_timeout).await?;
        net::configure_tcp_stream(&ctrl, true)?;
        protocol::send_cookie(&mut ctrl, &cookie).await?;

        if self.verbose {
            vprintln!("Connecting to host {}, port {}", self.host, self.port);
        }

        let done = Arc::new(AtomicBool::new(false));
        let mut streams: Vec<DataStream> = Vec::new();
        let mut cpu_start: Option<CpuSnapshot> = None;

        // ---- State machine: react to server-driven transitions ----
        loop {
            let state = protocol::recv_state(&mut ctrl).await?;

            match state {
                TestState::ParamExchange => {
                    let params = self.build_params();
                    protocol::send_params(&mut ctrl, &params).await?;
                }

                TestState::CreateStreams => {
                    streams = self.create_streams(&cookie, &done).await?;
                }

                TestState::TestStart => {
                    cpu_start = Some(CpuSnapshot::now());
                }

                TestState::TestRunning => {
                    self.run_test(&mut ctrl, &streams, &done).await?;
                    // Test finished — send TestEnd
                    protocol::send_state(&mut ctrl, TestState::TestEnd).await?;
                }

                TestState::ExchangeResults => {
                    let results = self.build_results(&streams, cpu_start.as_ref());
                    // Client sends first, then reads server's results
                    protocol::send_results(&mut ctrl, &results).await?;
                    let _server_results = protocol::recv_results(&mut ctrl).await?;
                }

                TestState::DisplayResults => {
                    self.print_results(&streams);
                    protocol::send_state(&mut ctrl, TestState::IperfDone).await?;
                    break; // test complete — server will close the connection
                }

                TestState::IperfDone => break,

                TestState::AccessDenied => {
                    return Err(RiperfError::AccessDenied);
                }
                TestState::ServerError => {
                    return Err(RiperfError::Protocol("server error".into()));
                }

                other => {
                    if self.verbose {
                        vprintln!("Unexpected state: {other:?}");
                    }
                }
            }
        }

        // ---- Clean up ----
        done.store(true, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        for s in streams {
            let _ = s.task.await;
        }

        Ok(())
    }

    fn build_params(&self) -> TestParams {
        let mut p = TestParams::default();
        match self.protocol {
            TransportProtocol::Tcp => p.tcp = Some(true),
            TransportProtocol::Udp => p.udp = Some(true),
        }
        p.time = Some(self.duration as i32);
        p.omit = Some(self.omit as i32);
        p.parallel = Some(self.num_streams as i32);
        p.len = Some(self.blksize as i32);
        if self.reverse {
            p.reverse = Some(true);
        }
        if self.bidir {
            p.bidirectional = Some(true);
        }
        if self.no_delay {
            p.nodelay = Some(true);
        }
        p.mss = self.mss;
        p.window = self.window;
        if self.bandwidth > 0 {
            p.bandwidth = Some(self.bandwidth);
        }
        if self.tos != 0 {
            p.tos = Some(self.tos);
        }
        p.congestion = self.congestion.clone();
        p.title = self.title.clone();
        p.extra_data = self.extra_data.clone();
        if self.udp_counters_64bit {
            p.udp_counters_64bit = Some(1);
        }
        p.client_version = Some(format!("riperf3 {}", env!("CARGO_PKG_VERSION")));
        if let Some(bytes) = self.bytes_to_send {
            p.num = Some(bytes);
        }
        if let Some(blocks) = self.blocks_to_send {
            p.blockcount = Some(blocks);
        }
        p
    }

    async fn create_streams(
        &self,
        cookie: &[u8; protocol::COOKIE_SIZE],
        done: &Arc<AtomicBool>,
    ) -> Result<Vec<DataStream>> {
        let mut streams = Vec::new();

        // In normal mode: client sends. Reverse: client receives. Bidir: both.
        let send_count = if self.reverse && !self.bidir { 0 } else { self.num_streams };
        let recv_count = if self.reverse || self.bidir { self.num_streams } else { 0 };
        let total = send_count + recv_count;
        let mut stream_id = 0i32;

        match self.protocol {
            TransportProtocol::Tcp => {
                for i in 0..total {
                    let mut data_stream =
                        net::tcp_connect(&self.host, self.port, self.connect_timeout).await?;
                    protocol::send_cookie(&mut data_stream, cookie).await?;
                    net::configure_tcp_stream(&data_stream, self.no_delay)?;

                    stream_id += 1;
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());
                    let raw_fd = data_stream.as_raw_fd();

                    let task = if is_sender {
                        let buf = vec![0u8; self.blksize];
                        let c = counters.clone();
                        let d = done.clone();
                        tokio::spawn(async move {
                            stream::run_tcp_sender(data_stream, c, buf, d).await
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = self.blksize;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d).await
                        })
                    };

                    streams.push(DataStream {
                        id: stream_id,
                        is_sender,
                        counters,
                        udp_recv_stats: None,
                        task,
                        raw_fd: Some(raw_fd),
                    });
                }
            }
            TransportProtocol::Udp => {
                for i in 0..total {
                    let udp_sock = net::udp_bind(None, 0).await?;
                    udp_sock
                        .connect(format!("{}:{}", self.host, self.port))
                        .await?;
                    protocol::udp_connect_client(&udp_sock).await?;

                    stream_id += 1;
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());

                    let task = if is_sender {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = self.blksize;
                        let rate = if self.bandwidth > 0 {
                            self.bandwidth
                        } else {
                            DEFAULT_UDP_RATE
                        };
                        let limiter = Some(RateLimiter::new(rate, 0, bs));
                        let u64bit = self.udp_counters_64bit;
                        tokio::spawn(async move {
                            stream::run_udp_sender(udp_sock, c, bs, d, limiter, u64bit).await
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = self.blksize;
                        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                        let sc = stats.clone();
                        let u64bit = self.udp_counters_64bit;
                        let task = tokio::spawn(async move {
                            stream::run_udp_receiver(udp_sock, c, sc, bs, d, u64bit).await
                        });
                        streams.push(DataStream {
                            id: stream_id,
                            is_sender,
                            counters,
                            udp_recv_stats: Some(stats),
                            task,
                            raw_fd: None,
                        });
                        continue;
                    };

                    streams.push(DataStream {
                        id: stream_id,
                        is_sender,
                        counters,
                        udp_recv_stats: None,
                        task,
                        raw_fd: None,
                    });
                }
            }
        }

        Ok(streams)
    }

    async fn run_test(
        &self,
        ctrl: &mut TcpStream,
        streams: &[DataStream],
        done: &Arc<AtomicBool>,
    ) -> Result<()> {
        // Determine test end condition
        let end_condition = if let Some(bytes) = self.bytes_to_send {
            EndCondition::Bytes(bytes)
        } else if let Some(blocks) = self.blocks_to_send {
            EndCondition::Blocks(blocks)
        } else {
            EndCondition::Duration(Duration::from_secs(self.duration as u64))
        };

        match end_condition {
            EndCondition::Duration(dur) => {
                // Use select to handle both timer and control socket
                tokio::select! {
                    _ = tokio::time::sleep(dur) => {}
                    state = protocol::recv_state(ctrl) => {
                        // Server sent something unexpected during the test
                        if let Ok(TestState::ServerTerminate) = state {
                            return Err(RiperfError::Aborted("server terminated".into()));
                        }
                    }
                }
            }
            EndCondition::Bytes(target) => {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let total: u64 = streams
                        .iter()
                        .filter(|s| s.is_sender)
                        .map(|s| s.counters.bytes_sent())
                        .sum();
                    if total >= target {
                        break;
                    }
                }
            }
            EndCondition::Blocks(target) => {
                // For block-based, approximate by dividing bytes by blksize
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let total_bytes: u64 = streams
                        .iter()
                        .filter(|s| s.is_sender)
                        .map(|s| s.counters.bytes_sent())
                        .sum();
                    let blocks = total_bytes / self.blksize as u64;
                    if blocks >= target {
                        break;
                    }
                }
            }
        }

        done.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn build_results(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
    ) -> TestResultsJson {
        let cpu_end = CpuSnapshot::now();
        let cpu_util = cpu_start
            .map(|start| cpu_end.utilization_since(start))
            .unwrap_or_default();

        let test_duration = self.duration as f64;

        let stream_results: Vec<_> = streams
            .iter()
            .map(|s| {
                let bytes = if s.is_sender {
                    s.counters.bytes_sent()
                } else {
                    s.counters.bytes_received()
                };

                let (jitter, errors, packets) = if let Some(ref udp_stats) = s.udp_recv_stats {
                    udp_stats
                        .lock()
                        .map(|stats| (stats.jitter, stats.cnt_error, stats.packet_count))
                        .unwrap_or((0.0, 0, 0))
                } else {
                    (0.0, 0, 0)
                };

                protocol::StreamResultJson {
                    id: s.id,
                    bytes,
                    retransmits: -1,
                    jitter,
                    errors,
                    omitted_errors: 0,
                    packets,
                    omitted_packets: 0,
                    start_time: 0.0,
                    end_time: test_duration,
                }
            })
            .collect();

        TestResultsJson {
            cpu_util_total: cpu_util.host_total,
            cpu_util_user: cpu_util.host_user,
            cpu_util_system: cpu_util.host_system,
            sender_has_retransmits: if streams.iter().any(|s| s.is_sender) {
                0
            } else {
                -1
            },
            congestion_used: None,
            streams: stream_results,
        }
    }

    fn print_results(&self, streams: &[DataStream]) {
        let test_duration = self.duration as f64;
        crate::reporter::print_separator();

        for s in streams {
            let bytes = if s.is_sender {
                s.counters.bytes_sent()
            } else {
                s.counters.bytes_received()
            };

            let (jitter, lost, total) = if let Some(ref udp_stats) = s.udp_recv_stats {
                udp_stats
                    .lock()
                    .map(|st| (Some(st.jitter), Some(st.cnt_error), Some(st.packet_count)))
                    .unwrap_or((None, None, None))
            } else {
                (None, None, None)
            };

            crate::reporter::print_summary(
                &crate::reporter::StreamSummary {
                    stream_id: s.id,
                    start: 0.0,
                    end: test_duration,
                    bytes,
                    is_sender: s.is_sender,
                    retransmits: None,
                    jitter,
                    lost,
                    total_packets: total,
                },
                'a', // adaptive format
            );
        }
    }
}

enum EndCondition {
    Duration(Duration),
    Bytes(u64),
    Blocks(u64),
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct ClientBuilder {
    host: Option<String>,
    port: Option<u16>,
    protocol: TransportProtocol,
    duration: u32,
    num_streams: u32,
    blksize: Option<usize>,
    reverse: bool,
    bidir: bool,
    omit: u32,
    no_delay: bool,
    mss: Option<i32>,
    window: Option<i32>,
    bandwidth: u64,
    tos: i32,
    congestion: Option<String>,
    udp_counters_64bit: bool,
    connect_timeout: Option<Duration>,
    title: Option<String>,
    extra_data: Option<String>,
    verbose: bool,
    bytes_to_send: Option<u64>,
    blocks_to_send: Option<u64>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            host: None,
            port: Some(DEFAULT_PORT),
            protocol: TransportProtocol::Tcp,
            duration: DEFAULT_DURATION,
            num_streams: DEFAULT_NUM_STREAMS,
            blksize: None,
            reverse: false,
            bidir: false,
            omit: DEFAULT_OMIT,
            no_delay: false,
            mss: None,
            window: None,
            bandwidth: 0,
            tos: 0,
            congestion: None,
            udp_counters_64bit: false,
            connect_timeout: None,
            title: None,
            extra_data: None,
            verbose: false,
            bytes_to_send: None,
            blocks_to_send: None,
        }
    }
}

impl ClientBuilder {
    pub fn new(host: &str) -> Self {
        Self::default().host(host)
    }

    pub fn host(mut self, host: &str) -> Self {
        self.host = Some(host.to_string());
        self
    }

    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    pub fn protocol(mut self, protocol: TransportProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn duration(mut self, secs: u32) -> Self {
        self.duration = secs;
        self
    }

    pub fn num_streams(mut self, n: u32) -> Self {
        self.num_streams = n;
        self
    }

    pub fn blksize(mut self, size: usize) -> Self {
        self.blksize = Some(size);
        self
    }

    pub fn reverse(mut self, reverse: bool) -> Self {
        self.reverse = reverse;
        self
    }

    pub fn bidir(mut self, bidir: bool) -> Self {
        self.bidir = bidir;
        self
    }

    pub fn omit(mut self, secs: u32) -> Self {
        self.omit = secs;
        self
    }

    pub fn no_delay(mut self, no_delay: bool) -> Self {
        self.no_delay = no_delay;
        self
    }

    pub fn mss(mut self, mss: i32) -> Self {
        self.mss = Some(mss);
        self
    }

    pub fn window(mut self, window: i32) -> Self {
        self.window = Some(window);
        self
    }

    pub fn bandwidth(mut self, bps: u64) -> Self {
        self.bandwidth = bps;
        self
    }

    pub fn tos(mut self, tos: i32) -> Self {
        self.tos = tos;
        self
    }

    pub fn congestion(mut self, algo: &str) -> Self {
        self.congestion = Some(algo.to_string());
        self
    }

    pub fn udp_counters_64bit(mut self, enabled: bool) -> Self {
        self.udp_counters_64bit = enabled;
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    pub fn extra_data(mut self, data: &str) -> Self {
        self.extra_data = Some(data.to_string());
        self
    }

    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn bytes(mut self, bytes: u64) -> Self {
        self.bytes_to_send = Some(bytes);
        self
    }

    pub fn blocks(mut self, blocks: u64) -> Self {
        self.blocks_to_send = Some(blocks);
        self
    }

    pub fn build(self) -> std::result::Result<Client, ConfigError> {
        let host = self.host.ok_or(ConfigError::MissingField("host"))?;

        let default_blksize = match self.protocol {
            TransportProtocol::Tcp => DEFAULT_TCP_BLKSIZE,
            TransportProtocol::Udp => DEFAULT_UDP_BLKSIZE,
        };

        Ok(Client {
            host,
            port: self.port.unwrap_or(DEFAULT_PORT),
            protocol: self.protocol,
            duration: self.duration,
            num_streams: self.num_streams,
            blksize: self.blksize.unwrap_or(default_blksize),
            reverse: self.reverse,
            bidir: self.bidir,
            omit: self.omit,
            no_delay: self.no_delay,
            mss: self.mss,
            window: self.window,
            bandwidth: self.bandwidth,
            tos: self.tos,
            congestion: self.congestion,
            udp_counters_64bit: self.udp_counters_64bit,
            connect_timeout: self.connect_timeout,
            title: self.title,
            extra_data: self.extra_data,
            verbose: self.verbose,
            bytes_to_send: self.bytes_to_send,
            blocks_to_send: self.blocks_to_send,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod client_builder_tests {
        use super::*;

        #[test]
        fn test_client_builder_default() {
            let b = ClientBuilder::default();
            assert_eq!(b.host, None);
            assert_eq!(b.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_new() {
            let b = ClientBuilder::new("localhost");
            assert_eq!(b.host, Some("localhost".to_string()));
            assert_eq!(b.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_host() {
            let b = ClientBuilder::new("localhost").host("otherhost");
            assert_eq!(b.host, Some("otherhost".to_string()));
        }

        #[test]
        fn test_client_builder_port() {
            let b = ClientBuilder::new("localhost").port(Some(1234));
            assert_eq!(b.port, Some(1234));
        }

        #[test]
        fn test_client_builder_build() {
            let r = ClientBuilder::default().build();
            assert!(r.is_err());
            assert_eq!(r.unwrap_err(), ConfigError::MissingField("host"));

            let c = ClientBuilder::new("localhost").build().unwrap();
            assert_eq!(c.host, "localhost");
            assert_eq!(c.port, DEFAULT_PORT);

            let c = ClientBuilder::new("localhost")
                .host("otherhost")
                .port(Some(1234))
                .build()
                .unwrap();
            assert_eq!(c.host, "otherhost");
            assert_eq!(c.port, 1234);
        }

        #[test]
        fn test_client_builder_all_fields() {
            let c = ClientBuilder::new("10.0.0.1")
                .protocol(TransportProtocol::Udp)
                .duration(30)
                .num_streams(4)
                .blksize(1460)
                .reverse(true)
                .bidir(false)
                .no_delay(true)
                .bandwidth(100_000_000)
                .tos(0x10)
                .verbose(true)
                .build()
                .unwrap();

            assert_eq!(c.protocol, TransportProtocol::Udp);
            assert_eq!(c.duration, 30);
            assert_eq!(c.num_streams, 4);
            assert_eq!(c.blksize, 1460);
            assert!(c.reverse);
            assert!(!c.bidir);
            assert!(c.no_delay);
            assert_eq!(c.bandwidth, 100_000_000);
            assert_eq!(c.tos, 0x10);
        }
    }
}
