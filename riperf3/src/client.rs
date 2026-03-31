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
    pub host: String,
    pub port: u16,
    pub protocol: TransportProtocol,
    pub duration: u32,
    pub num_streams: u32,
    pub blksize: usize,
    pub reverse: bool,
    pub bidir: bool,
    pub omit: u32,
    pub no_delay: bool,
    pub mss: Option<i32>,
    pub window: Option<i32>,
    pub bandwidth: u64,
    pub tos: i32,
    pub congestion: Option<String>,
    pub udp_counters_64bit: bool,
    pub connect_timeout: Option<Duration>,
    pub title: Option<String>,
    pub extra_data: Option<String>,
    pub verbose: bool,
    pub json_output: bool,
    pub json_stream: bool,
    pub bytes_to_send: Option<u64>,
    pub blocks_to_send: Option<u64>,
    pub repeating_payload: bool,
    pub dont_fragment: bool,
    pub cport: Option<u16>,
    pub get_server_output: bool,
    pub forceflush: bool,
    pub timestamps: Option<String>,
    pub bind_address: Option<String>,
    pub bind_dev: Option<String>,
    pub fq_rate: Option<u64>,
    pub flowlabel: Option<i32>,
    pub ip_version: Option<u8>,
    pub mptcp: bool,
    pub skip_rx_copy: bool,
    pub rcv_timeout: Option<u64>,
    pub snd_timeout: Option<u64>,
    pub file: Option<String>,
    pub affinity: Option<String>,
    pub dscp: Option<String>,
    pub format_char: char,
    pub interval: Option<f64>,
    pub cntl_ka: Option<String>,
    pub pidfile: Option<String>,
    pub logfile: Option<String>,
}

impl Client {
    pub async fn run(&self) -> Result<()> {
        // ---- Generate cookie and connect ----
        let cookie = protocol::make_cookie();
        let mut ctrl = net::tcp_connect(&self.host, self.port, self.connect_timeout, None, self.mptcp).await?;
        net::configure_tcp_stream(&ctrl, true)?;

        // Apply control connection options
        {
            use std::os::unix::io::AsRawFd;
            if let Some(ref dev) = self.bind_dev {
                net::set_bind_dev(ctrl.as_raw_fd(), dev)?;
            }
            if let Some(ref spec) = self.cntl_ka {
                let (idle, intv, cnt) = parse_keepalive(spec);
                net::set_tcp_keepalive(ctrl.as_raw_fd(), idle, intv, cnt)?;
            }
        }

        protocol::send_cookie(&mut ctrl, &cookie).await?;

        if self.verbose {
            vprintln!("Connecting to host {}, port {}", self.host, self.port);
        }

        let done = Arc::new(AtomicBool::new(false));
        let mut streams: Vec<DataStream> = Vec::new();
        let mut cpu_start: Option<CpuSnapshot> = None;
        let mut server_results: Option<TestResultsJson> = None;

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
                    protocol::send_results(&mut ctrl, &results).await?;
                    server_results = Some(protocol::recv_results(&mut ctrl).await?);
                }

                TestState::DisplayResults => {
                    self.print_results(&streams, cpu_start.as_ref(), server_results.as_ref());
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
        match self.protocol {
            TransportProtocol::Tcp => {
                for i in 0..total {
                    let mut data_stream =
                        net::tcp_connect(&self.host, self.port, self.connect_timeout, self.cport, self.mptcp).await?;
                    protocol::send_cookie(&mut data_stream, cookie).await?;
                    net::configure_tcp_stream_full(
                        &data_stream,
                        self.no_delay,
                        self.mss,
                        self.window,
                        self.congestion.as_deref(),
                    )?;
                    let raw_fd = data_stream.as_raw_fd();
                    if self.dont_fragment {
                        net::set_dont_fragment(raw_fd)?;
                    }
                    if let Some(rate) = self.fq_rate {
                        net::set_fq_rate(raw_fd, rate)?;
                    }
                    if let Some(ms) = self.rcv_timeout {
                        net::set_rcv_timeout(raw_fd, ms)?;
                    }
                    if let Some(ms) = self.snd_timeout {
                        net::set_snd_timeout(raw_fd, ms)?;
                    }
                    if let Some(label) = self.flowlabel {
                        net::set_ipv6_flowlabel(raw_fd, label)?;
                    }
                    if let Some(ref dev) = self.bind_dev {
                        net::set_bind_dev(raw_fd, dev)?;
                    }

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());

                    let task = if is_sender {
                        let buf = make_send_buffer(self.blksize, self.repeating_payload);
                        let c = counters.clone();
                        let d = done.clone();
                        tokio::spawn(async move {
                            stream::run_tcp_sender(data_stream, c, buf, d).await
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = self.blksize;
                        let srxc = self.skip_rx_copy;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d, srxc).await
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
                    let is_ipv6 = self.host.contains(':');
                    let udp_sock = net::udp_bind(None, 0, is_ipv6).await?;
                    if let Some(ref dev) = self.bind_dev {
                        use std::os::unix::io::AsRawFd;
                        net::set_bind_dev(udp_sock.as_raw_fd(), dev)?;
                    }
                    udp_sock
                        .connect(net::format_addr(&self.host, self.port))
                        .await?;
                    protocol::udp_connect_client(&udp_sock).await?;

                    let stream_id = iperf3_stream_id(i);
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
        // Spawn interval reporter (unless plain JSON output without streaming)
        let interval_secs = self.interval.unwrap_or(1.0);
        let use_intervals = interval_secs > 0.0 && (!self.json_output || self.json_stream);
        let interval_handle = if use_intervals {
            let stream_refs: Vec<_> = streams
                .iter()
                .map(|s| crate::reporter::IntervalStreamRef {
                    id: s.id,
                    is_sender: s.is_sender,
                    counters: s.counters.clone(),
                    udp_recv_stats: s.udp_recv_stats.clone(),
                    raw_fd: s.raw_fd,
                })
                .collect();
            crate::reporter::spawn_interval_reporter(
                crate::reporter::IntervalReporterConfig {
                    interval_secs,
                    protocol: self.protocol,
                    format_char: self.format_char,
                    omit_secs: self.omit,
                    num_streams: streams.len(),
                    forceflush: self.forceflush,
                    timestamp_format: self.timestamps.clone(),
                    json_stream: self.json_stream,
                },
                stream_refs,
                done.clone(),
            )
        } else {
            None
        };

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

        // Wait for interval reporter to finish its last tick
        if let Some(handle) = interval_handle {
            let _ = handle.await;
        }

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

    fn print_results(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
    ) {
        if self.json_output {
            self.print_results_json(streams, cpu_start, remote_cpu);
        } else {
            self.print_results_text(streams);
        }
    }

    fn print_results_text(&self, streams: &[DataStream]) {
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
                self.format_char,
            );
        }
    }

    fn print_results_json(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
    ) {
        let test_duration = self.duration as f64;
        let cpu_end = CpuSnapshot::now();
        let cpu_util = cpu_start
            .map(|start| cpu_end.utilization_since(start))
            .unwrap_or_default();

        let protocol_str = match self.protocol {
            TransportProtocol::Tcp => "TCP",
            TransportProtocol::Udp => "UDP",
        };

        // Build per-stream end results
        let mut j_streams = Vec::new();
        let mut sum_sent_bytes: u64 = 0;
        let mut sum_recv_bytes: u64 = 0;
        let sum_retransmits: i64 = 0;

        for s in streams {
            let sent = s.counters.bytes_sent();
            let recv = s.counters.bytes_received();
            let bytes = if s.is_sender { sent } else { recv };
            let bits_per_sec = bytes as f64 * 8.0 / test_duration;

            if s.is_sender {
                sum_sent_bytes += sent;
            } else {
                sum_recv_bytes += recv;
            }

            let mut stream_obj = serde_json::json!({
                "socket": s.id,
                "start": 0.0,
                "end": test_duration,
                "seconds": test_duration,
                "bytes": bytes,
                "bits_per_second": bits_per_sec,
                "sender": s.is_sender
            });

            if let (Some(ref udp_stats_lock), false) = (&s.udp_recv_stats, s.is_sender) {
                if let Ok(stats) = udp_stats_lock.lock() {
                    stream_obj["jitter_ms"] = serde_json::json!(stats.jitter * 1000.0);
                    stream_obj["lost_packets"] = serde_json::json!(stats.cnt_error);
                    stream_obj["packets"] = serde_json::json!(stats.packet_count);
                    let pct = if stats.packet_count > 0 {
                        stats.cnt_error as f64 / stats.packet_count as f64 * 100.0
                    } else {
                        0.0
                    };
                    stream_obj["lost_percent"] = serde_json::json!(pct);
                }
            }

            j_streams.push(stream_obj);
        }

        // If we only have senders, use sent bytes for both
        if sum_recv_bytes == 0 {
            sum_recv_bytes = sum_sent_bytes;
        }
        if sum_sent_bytes == 0 {
            sum_sent_bytes = sum_recv_bytes;
        }

        let (remote_total, remote_user, remote_system) = remote_cpu
            .map(|r| (r.cpu_util_total, r.cpu_util_user, r.cpu_util_system))
            .unwrap_or((0.0, 0.0, 0.0));

        let output = serde_json::json!({
            "start": {
                "connected": streams.iter().map(|s| {
                    serde_json::json!({
                        "socket": s.id,
                        "local_host": self.host,
                        "local_port": self.port,
                        "remote_host": self.host,
                        "remote_port": self.port
                    })
                }).collect::<Vec<_>>(),
                "version": format!("riperf3 {}", env!("CARGO_PKG_VERSION")),
                "system_info": "",
                "connecting_to": {
                    "host": self.host,
                    "port": self.port
                },
                "test_start": {
                    "protocol": protocol_str,
                    "num_streams": self.num_streams,
                    "blksize": self.blksize,
                    "omit": self.omit,
                    "duration": self.duration,
                    "bytes": 0,
                    "blocks": 0,
                    "reverse": if self.reverse { 1 } else { 0 },
                    "tos": self.tos,
                    "target_bitrate": self.bandwidth,
                    "bidir": if self.bidir { 1 } else { 0 }
                }
            },
            "intervals": [],
            "end": {
                "streams": j_streams,
                "sum_sent": {
                    "start": 0.0,
                    "end": test_duration,
                    "seconds": test_duration,
                    "bytes": sum_sent_bytes,
                    "bits_per_second": sum_sent_bytes as f64 * 8.0 / test_duration,
                    "retransmits": sum_retransmits,
                    "sender": true
                },
                "sum_received": {
                    "start": 0.0,
                    "end": test_duration,
                    "seconds": test_duration,
                    "bytes": sum_recv_bytes,
                    "bits_per_second": sum_recv_bytes as f64 * 8.0 / test_duration,
                    "sender": true
                },
                "cpu_utilization_percent": {
                    "host_total": cpu_util.host_total,
                    "host_user": cpu_util.host_user,
                    "host_system": cpu_util.host_system,
                    "remote_total": remote_total,
                    "remote_user": remote_user,
                    "remote_system": remote_system
                }
            }
        });

        println!("{}", serde_json::to_string_pretty(&output).unwrap());
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
    json_output: bool,
    json_stream: bool,
    bytes_to_send: Option<u64>,
    blocks_to_send: Option<u64>,
    repeating_payload: bool,
    dont_fragment: bool,
    cport: Option<u16>,
    get_server_output: bool,
    forceflush: bool,
    timestamps: Option<String>,
    bind_address: Option<String>,
    bind_dev: Option<String>,
    fq_rate: Option<u64>,
    flowlabel: Option<i32>,
    ip_version: Option<u8>,
    mptcp: bool,
    skip_rx_copy: bool,
    rcv_timeout: Option<u64>,
    snd_timeout: Option<u64>,
    file: Option<String>,
    affinity: Option<String>,
    dscp: Option<String>,
    format_char: char,
    interval: Option<f64>,
    cntl_ka: Option<String>,
    pidfile: Option<String>,
    logfile: Option<String>,
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
            json_output: false,
            json_stream: false,
            bytes_to_send: None,
            blocks_to_send: None,
            repeating_payload: false,
            dont_fragment: false,
            cport: None,
            get_server_output: false,
            forceflush: false,
            timestamps: None,
            bind_address: None,
            bind_dev: None,
            fq_rate: None,
            flowlabel: None,
            ip_version: None,
            mptcp: false,
            skip_rx_copy: false,
            rcv_timeout: None,
            snd_timeout: None,
            file: None,
            affinity: None,
            dscp: None,
            format_char: 'a',
            interval: None,
            cntl_ka: None,
            pidfile: None,
            logfile: None,
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

    pub fn json_output(mut self, enabled: bool) -> Self {
        self.json_output = enabled;
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

    pub fn json_stream(mut self, enabled: bool) -> Self {
        self.json_stream = enabled;
        self
    }

    pub fn repeating_payload(mut self, enabled: bool) -> Self {
        self.repeating_payload = enabled;
        self
    }

    pub fn dont_fragment(mut self, enabled: bool) -> Self {
        self.dont_fragment = enabled;
        self
    }

    pub fn cport(mut self, port: u16) -> Self {
        self.cport = Some(port);
        self
    }

    pub fn get_server_output(mut self, enabled: bool) -> Self {
        self.get_server_output = enabled;
        self
    }

    pub fn forceflush(mut self, enabled: bool) -> Self {
        self.forceflush = enabled;
        self
    }

    pub fn timestamps(mut self, fmt: &str) -> Self {
        self.timestamps = Some(fmt.to_string());
        self
    }

    pub fn bind_address(mut self, addr: &str) -> Self {
        self.bind_address = Some(addr.to_string());
        self
    }

    pub fn bind_dev(mut self, dev: &str) -> Self {
        self.bind_dev = Some(dev.to_string());
        self
    }

    pub fn fq_rate(mut self, rate: u64) -> Self {
        self.fq_rate = Some(rate);
        self
    }

    pub fn flowlabel(mut self, label: i32) -> Self {
        self.flowlabel = Some(label);
        self
    }

    pub fn ip_version(mut self, version: u8) -> Self {
        self.ip_version = Some(version);
        self
    }

    pub fn mptcp(mut self, enabled: bool) -> Self {
        self.mptcp = enabled;
        self
    }

    pub fn skip_rx_copy(mut self, enabled: bool) -> Self {
        self.skip_rx_copy = enabled;
        self
    }

    pub fn rcv_timeout(mut self, ms: u64) -> Self {
        self.rcv_timeout = Some(ms);
        self
    }

    pub fn snd_timeout(mut self, ms: u64) -> Self {
        self.snd_timeout = Some(ms);
        self
    }

    pub fn file(mut self, path: &str) -> Self {
        self.file = Some(path.to_string());
        self
    }

    pub fn affinity(mut self, spec: &str) -> Self {
        self.affinity = Some(spec.to_string());
        self
    }

    pub fn dscp(mut self, val: &str) -> Self {
        self.dscp = Some(val.to_string());
        self
    }

    pub fn format_char(mut self, c: char) -> Self {
        self.format_char = c;
        self
    }

    pub fn interval(mut self, secs: f64) -> Self {
        self.interval = Some(secs);
        self
    }

    pub fn cntl_ka(mut self, spec: &str) -> Self {
        self.cntl_ka = Some(spec.to_string());
        self
    }

    pub fn pidfile(mut self, path: &str) -> Self {
        self.pidfile = Some(path.to_string());
        self
    }

    pub fn logfile(mut self, path: &str) -> Self {
        self.logfile = Some(path.to_string());
        self
    }

    pub fn build(self) -> std::result::Result<Client, ConfigError> {
        let host = self.host.ok_or(ConfigError::MissingField("host"))?;

        let default_blksize = match self.protocol {
            TransportProtocol::Tcp => DEFAULT_TCP_BLKSIZE,
            TransportProtocol::Udp => DEFAULT_UDP_BLKSIZE,
        };

        // If --dscp is set, convert to TOS and override
        let tos = if let Some(ref dscp) = self.dscp {
            parse_dscp(dscp)?
        } else {
            self.tos
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
            tos,
            congestion: self.congestion,
            udp_counters_64bit: self.udp_counters_64bit,
            connect_timeout: self.connect_timeout,
            title: self.title,
            extra_data: self.extra_data,
            verbose: self.verbose,
            json_output: self.json_output,
            json_stream: self.json_stream,
            bytes_to_send: self.bytes_to_send,
            blocks_to_send: self.blocks_to_send,
            repeating_payload: self.repeating_payload,
            dont_fragment: self.dont_fragment,
            cport: self.cport,
            get_server_output: self.get_server_output,
            forceflush: self.forceflush,
            timestamps: self.timestamps,
            bind_address: self.bind_address,
            bind_dev: self.bind_dev,
            fq_rate: self.fq_rate,
            flowlabel: self.flowlabel,
            ip_version: self.ip_version,
            mptcp: self.mptcp,
            skip_rx_copy: self.skip_rx_copy,
            rcv_timeout: self.rcv_timeout,
            snd_timeout: self.snd_timeout,
            file: self.file,
            affinity: self.affinity,
            dscp: self.dscp,
            format_char: self.format_char,
            interval: self.interval,
            cntl_ka: self.cntl_ka,
            pidfile: self.pidfile,
            logfile: self.logfile,
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
