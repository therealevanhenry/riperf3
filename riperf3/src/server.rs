use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, RiperfError, Result};
use crate::net;
use crate::protocol::{
    self, TestParams, TestResultsJson, TestState, TransportProtocol,
};
use crate::stream::{
    self, DataStream, StreamCounters, UdpRecvStats,
};
use crate::utils::*;

/// Shared test configuration derived from the client's parameter JSON.
pub struct TestConfig {
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
}

impl TestConfig {
    pub fn from_params(params: &TestParams) -> Self {
        let is_udp = params.udp.unwrap_or(false);
        let protocol = if is_udp {
            TransportProtocol::Udp
        } else {
            TransportProtocol::Tcp
        };

        let default_blksize = if is_udp {
            DEFAULT_UDP_BLKSIZE
        } else {
            DEFAULT_TCP_BLKSIZE
        };

        Self {
            protocol,
            duration: params.time.unwrap_or(DEFAULT_DURATION as i32) as u32,
            num_streams: params.parallel.unwrap_or(1) as u32,
            blksize: params.len.unwrap_or(default_blksize as i32) as usize,
            reverse: params.reverse.unwrap_or(false),
            bidir: params.bidirectional.unwrap_or(false),
            omit: params.omit.unwrap_or(0) as u32,
            no_delay: params.nodelay.unwrap_or(false),
            mss: params.mss,
            window: params.window,
            bandwidth: params.bandwidth.unwrap_or(0),
            tos: params.tos.unwrap_or(0),
            congestion: params.congestion.clone(),
            udp_counters_64bit: params.udp_counters_64bit.unwrap_or(0) != 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub struct Server {
    pub port: u16,
    pub one_off: bool,
    pub verbose: bool,
    pub daemon: bool,
    pub idle_timeout: Option<u32>,
    pub server_bitrate_limit: Option<u64>,
    pub server_max_duration: Option<u32>,
    pub pidfile: Option<String>,
    pub logfile: Option<String>,
    pub forceflush: bool,
    pub bind_address: Option<String>,
    pub timestamps: Option<String>,
    pub file: Option<String>,
    pub rsa_private_key_path: Option<String>,
    pub authorized_users_path: Option<String>,
    pub time_skew_threshold: u32,
    pub use_pkcs1_padding: bool,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        if self.daemon {
            #[cfg(unix)]
            unsafe {
                if libc::daemon(0, 0) != 0 {
                    return Err(RiperfError::Io(std::io::Error::last_os_error()));
                }
            }
        }

        let listener = net::tcp_listen(self.bind_address.as_deref(), self.port, None).await?;
        let sep = "-----------------------------------------------------------";
        println!("{sep}");
        println!("Server listening on {}", self.port);
        println!("{sep}");

        loop {
            match self.handle_one_test(&listener).await {
                Ok(()) => {}
                Err(RiperfError::PeerDisconnected) => {
                    if self.verbose {
                        vprintln!("Client disconnected.");
                    }
                }
                Err(e) => {
                    eprintln!("iperf3: error - {e}");
                }
            }

            if self.one_off {
                break;
            }
            println!("{sep}");
            println!("Server listening on {}", self.port);
            println!("{sep}");
        }
        Ok(())
    }

    async fn handle_one_test(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<()> {
        // ---- Accept control connection (with optional idle timeout) ----
        let (mut ctrl, peer_addr) = if let Some(secs) = self.idle_timeout {
            match tokio::time::timeout(
                std::time::Duration::from_secs(secs as u64),
                listener.accept(),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    return Err(RiperfError::Aborted("idle timeout".into()));
                }
            }
        } else {
            listener.accept().await?
        };
        if self.verbose {
            vprintln!("Accepted connection from {peer_addr}");
        }
        net::configure_tcp_stream(&ctrl, true)?;

        // ---- Cookie ----
        let cookie = protocol::recv_cookie(&mut ctrl).await?;

        // ---- ParamExchange ----
        protocol::send_state(&mut ctrl, TestState::ParamExchange).await?;
        let params = protocol::recv_params(&mut ctrl).await?;
        let cfg = TestConfig::from_params(&params);

        // ---- Auth validation (after params, before streams) ----
        if let (Some(ref privkey_path), Some(ref users_path)) =
            (&self.rsa_private_key_path, &self.authorized_users_path)
        {
            if let Some(ref token) = params.authtoken {
                let privkey_pem = std::fs::read(privkey_path).map_err(|e| {
                    RiperfError::Protocol(format!("cannot read RSA private key: {e}"))
                })?;
                match crate::auth::decode_auth_token(token, &privkey_pem, self.use_pkcs1_padding) {
                    Ok((username, password, ts)) => {
                        crate::auth::check_credentials(
                            &username,
                            &password,
                            ts,
                            users_path,
                            self.time_skew_threshold,
                        )?;
                        if self.verbose {
                            vprintln!("Authenticated user: {username}");
                        }
                    }
                    Err(e) => {
                        protocol::send_state(&mut ctrl, TestState::AccessDenied).await?;
                        return Err(e);
                    }
                }
            } else {
                // Server requires auth but client didn't send token
                protocol::send_state(&mut ctrl, TestState::AccessDenied).await?;
                return Err(RiperfError::AccessDenied);
            }
        }

        if self.verbose {
            vprintln!(
                "Test: {:?} {} stream(s) blksize={} duration={}s",
                cfg.protocol, cfg.num_streams, cfg.blksize, cfg.duration
            );
        }

        // ---- CreateStreams ----
        let done = Arc::new(AtomicBool::new(false));
        let mut streams: Vec<DataStream> = Vec::new();

        // Determine how many streams to accept and their roles.
        // Normal: server receives. Reverse: server sends. Bidir: both.
        let recv_count = if cfg.reverse && !cfg.bidir { 0 } else { cfg.num_streams };
        let send_count = if cfg.reverse || cfg.bidir { cfg.num_streams } else { 0 };
        let total = recv_count + send_count;

        match cfg.protocol {
            TransportProtocol::Tcp => {
                protocol::send_state(&mut ctrl, TestState::CreateStreams).await?;

                for i in 0..total {
                    let (mut data_stream, _) = listener.accept().await?;
                    let stream_cookie = protocol::recv_cookie(&mut data_stream).await?;
                    if stream_cookie != cookie {
                        return Err(RiperfError::CookieMismatch);
                    }
                    // Apply socket options (nodelay, MSS, window, congestion) to each stream
                    net::configure_tcp_stream_full(
                        &data_stream,
                        cfg.no_delay,
                        cfg.mss,
                        cfg.window,
                        cfg.congestion.as_deref(),
                    )?;

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i >= recv_count;
                    let counters = Arc::new(StreamCounters::new());
                    let raw_fd = data_stream.as_raw_fd();
                    let fp = self.file.as_ref().map(std::path::PathBuf::from);

                    let task = if is_sender {
                        let buf = make_send_buffer(cfg.blksize, false);
                        let c = counters.clone();
                        let d = done.clone();
                        tokio::spawn(async move {
                            stream::run_tcp_sender(data_stream, c, buf, d, fp).await
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = cfg.blksize;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d, false, fp).await
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
                // Create the initial UDP listener with SO_REUSEADDR.
                // For each stream: accept the connect handshake on the listener,
                // which locks that socket to the client. Then create a fresh
                // listener for the next stream (iperf3's recycling pattern).
                let mut udp_listener = net::udp_bind_reusable(self.bind_address.as_deref(), self.port, self.bind_address.as_ref().is_some_and(|a| a.contains(':'))).await?;

                protocol::send_state(&mut ctrl, TestState::CreateStreams).await?;

                for i in 0..total {
                    // Accept: recv magic, connect() to client, send reply
                    let _client_addr =
                        protocol::udp_connect_server(&udp_listener).await?;
                    // The listener is now locked to this client — use it as the data socket
                    let data_sock = udp_listener;

                    // Create a fresh listener for the next stream (if any)
                    if i + 1 < total {
                        udp_listener = net::udp_bind_reusable(self.bind_address.as_deref(), self.port, self.bind_address.as_ref().is_some_and(|a| a.contains(':'))).await?;
                    } else {
                        // Last stream — create a dummy that won't be used
                        udp_listener = net::udp_bind(None, 0, false).await?;
                    }

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i >= recv_count;
                    let counters = Arc::new(StreamCounters::new());

                    let std_sock = data_sock.into_std()
                        .map_err(RiperfError::Io)?;

                    if is_sender {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = cfg.blksize;
                        let rate = if cfg.bandwidth > 0 {
                            cfg.bandwidth
                        } else {
                            DEFAULT_UDP_RATE
                        };
                        let u64bit = cfg.udp_counters_64bit;
                        let task = tokio::task::spawn_blocking(move || {
                            stream::run_udp_sender_blocking(std_sock, c, bs, d, rate, u64bit)
                        });
                        streams.push(DataStream {
                            id: stream_id,
                            is_sender,
                            counters,
                            udp_recv_stats: None,
                            task,
                            raw_fd: None,
                        });
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = cfg.blksize;
                        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                        let stats_clone = stats.clone();
                        let u64bit = cfg.udp_counters_64bit;
                        let task = tokio::task::spawn_blocking(move || {
                            stream::run_udp_receiver_blocking(std_sock, c, stats_clone, bs, d, u64bit)
                        });
                        streams.push(DataStream {
                            id: stream_id,
                            is_sender,
                            counters,
                            udp_recv_stats: Some(stats),
                            task,
                            raw_fd: None,
                        });
                    }
                }
            }
        }

        // ---- TestStart / TestRunning ----
        protocol::send_state(&mut ctrl, TestState::TestStart).await?;
        let cpu_start = CpuSnapshot::now();
        protocol::send_state(&mut ctrl, TestState::TestRunning).await?;

        // Spawn interval reporter (server uses 1.0s default)
        let interval_handle = {
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
                    interval_secs: 1.0,
                    protocol: cfg.protocol,
                    format_char: 'a',
                    omit_secs: cfg.omit,
                    num_streams: streams.len(),
                    forceflush: self.forceflush,
                    timestamp_format: self.timestamps.clone(),
                    json_stream: false, // server doesn't stream JSON
                },
                stream_refs,
                done.clone(),
            )
        };

        // ---- Wait for TEST_END (with optional max duration and bitrate limit) ----
        let bitrate_limit = self.server_bitrate_limit;
        let test_start = std::time::Instant::now();
        let max_dur_secs = self.server_max_duration.unwrap_or(0) as u64;

        let mut rate_check = tokio::time::interval(std::time::Duration::from_secs(1));
        rate_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        rate_check.tick().await; // skip immediate tick

        let mut server_terminated = false;

        loop {
            tokio::select! {
                state = protocol::recv_state(&mut ctrl) => {
                    match state? {
                        TestState::TestEnd => break,
                        TestState::ClientTerminate => {
                            return Err(RiperfError::Aborted("client terminated".into()));
                        }
                        _ => {}
                    }
                }
                _ = rate_check.tick(), if bitrate_limit.is_some() => {
                    let elapsed = test_start.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        let total_bytes: u64 = streams.iter().map(|s| {
                            s.counters.bytes_sent() + s.counters.bytes_received()
                        }).sum();
                        let bits_per_sec = total_bytes as f64 * 8.0 / elapsed;
                        if let Some(limit) = bitrate_limit {
                            if bits_per_sec > limit as f64 {
                                protocol::send_state(&mut ctrl, TestState::ServerTerminate).await?;
                                server_terminated = true;
                                break;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(max_dur_secs)), if max_dur_secs > 0 => {
                    protocol::send_state(&mut ctrl, TestState::ServerTerminate).await?;
                    server_terminated = true;
                    break;
                }
            }
        }

        let _ = server_terminated;

        // ---- Shut down streams ----
        done.store(true, Ordering::Relaxed);

        if let Some(handle) = interval_handle {
            let _ = handle.await;
        }
        let cpu_end = CpuSnapshot::now();

        // Wait briefly then join tasks (senders may be blocked on write)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut result_streams = Vec::new();
        let test_duration = cfg.duration as f64;

        for s in &streams {
            let bytes = if s.is_sender {
                s.counters.bytes_sent()
            } else {
                s.counters.bytes_received()
            };

            let (jitter, errors, packets) = if let Some(ref udp_stats) = s.udp_recv_stats {
                if let Ok(stats) = udp_stats.lock() {
                    (stats.jitter, stats.cnt_error, stats.packet_count)
                } else {
                    (0.0, 0, 0)
                }
            } else {
                (0.0, 0, 0)
            };

            result_streams.push(protocol::StreamResultJson {
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
            });
        }

        // ---- ExchangeResults ----
        let cpu_util = cpu_end.utilization_since(&cpu_start);
        let server_results = TestResultsJson {
            cpu_util_total: cpu_util.host_total,
            cpu_util_user: cpu_util.host_user,
            cpu_util_system: cpu_util.host_system,
            sender_has_retransmits: if streams.iter().any(|s| s.is_sender) {
                0
            } else {
                -1
            },
            congestion_used: None,
            streams: result_streams,
        };

        protocol::send_state(&mut ctrl, TestState::ExchangeResults).await?;
        // iperf3 protocol: server reads client results first, then sends its own
        let _client_results = protocol::recv_results(&mut ctrl).await?;
        protocol::send_results(&mut ctrl, &server_results).await?;

        // ---- DisplayResults / IperfDone ----
        protocol::send_state(&mut ctrl, TestState::DisplayResults).await?;

        // Wait for client to send IperfDone
        loop {
            match protocol::recv_state(&mut ctrl).await {
                Ok(TestState::IperfDone) => break,
                Ok(_) => continue,
                Err(RiperfError::PeerDisconnected) => break,
                Err(e) => return Err(e),
            }
        }

        // Print summary
        for s in &streams {
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
                'a',
            );
        }

        // Join stream tasks (best-effort, they should be done)
        for s in streams {
            let _ = s.task.await;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct ServerBuilder {
    port: Option<u16>,
    one_off: bool,
    verbose: bool,
    daemon: bool,
    idle_timeout: Option<u32>,
    server_bitrate_limit: Option<u64>,
    server_max_duration: Option<u32>,
    pidfile: Option<String>,
    logfile: Option<String>,
    forceflush: bool,
    bind_address: Option<String>,
    timestamps: Option<String>,
    file: Option<String>,
    rsa_private_key_path: Option<String>,
    authorized_users_path: Option<String>,
    time_skew_threshold: u32,
    use_pkcs1_padding: bool,
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self {
            port: Some(DEFAULT_PORT),
            one_off: false,
            verbose: false,
            daemon: false,
            idle_timeout: None,
            server_bitrate_limit: None,
            server_max_duration: None,
            pidfile: None,
            logfile: None,
            forceflush: false,
            bind_address: None,
            timestamps: None,
            file: None,
            rsa_private_key_path: None,
            authorized_users_path: None,
            time_skew_threshold: 10,
            use_pkcs1_padding: false,
        }
    }
}

impl ServerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    pub fn one_off(mut self, one_off: bool) -> Self {
        self.one_off = one_off;
        self
    }

    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn daemon(mut self, daemon: bool) -> Self {
        self.daemon = daemon;
        self
    }

    pub fn idle_timeout(mut self, secs: u32) -> Self {
        self.idle_timeout = Some(secs);
        self
    }

    pub fn server_bitrate_limit(mut self, rate: u64) -> Self {
        self.server_bitrate_limit = Some(rate);
        self
    }

    pub fn server_max_duration(mut self, secs: u32) -> Self {
        self.server_max_duration = Some(secs);
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

    pub fn forceflush(mut self, enabled: bool) -> Self {
        self.forceflush = enabled;
        self
    }

    pub fn bind_address(mut self, addr: &str) -> Self {
        self.bind_address = Some(addr.to_string());
        self
    }

    pub fn timestamps(mut self, fmt: &str) -> Self {
        self.timestamps = Some(fmt.to_string());
        self
    }

    pub fn file(mut self, path: &str) -> Self {
        self.file = Some(path.to_string());
        self
    }

    pub fn rsa_private_key_path(mut self, path: &str) -> Self {
        self.rsa_private_key_path = Some(path.to_string());
        self
    }

    pub fn authorized_users_path(mut self, path: &str) -> Self {
        self.authorized_users_path = Some(path.to_string());
        self
    }

    pub fn time_skew_threshold(mut self, secs: u32) -> Self {
        self.time_skew_threshold = secs;
        self
    }

    pub fn use_pkcs1_padding(mut self, enabled: bool) -> Self {
        self.use_pkcs1_padding = enabled;
        self
    }

    pub fn build(self) -> std::result::Result<Server, ConfigError> {
        Ok(Server {
            port: self.port.unwrap_or(DEFAULT_PORT),
            one_off: self.one_off,
            verbose: self.verbose,
            daemon: self.daemon,
            idle_timeout: self.idle_timeout,
            server_bitrate_limit: self.server_bitrate_limit,
            server_max_duration: self.server_max_duration,
            pidfile: self.pidfile,
            logfile: self.logfile,
            forceflush: self.forceflush,
            bind_address: self.bind_address,
            timestamps: self.timestamps,
            file: self.file,
            rsa_private_key_path: self.rsa_private_key_path,
            authorized_users_path: self.authorized_users_path,
            time_skew_threshold: self.time_skew_threshold,
            use_pkcs1_padding: self.use_pkcs1_padding,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod server_builder_tests {
        use super::*;

        #[test]
        fn test_server_builder_default() {
            let b = ServerBuilder::default();
            assert_eq!(b.port, Some(DEFAULT_PORT));
            assert!(!b.one_off);
        }

        #[test]
        fn test_server_builder_new() {
            let b = ServerBuilder::new();
            assert_eq!(b.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_server_builder_port() {
            let b = ServerBuilder::new().port(Some(1234));
            assert_eq!(b.port, Some(1234));
        }

        #[test]
        fn test_server_builder_build() {
            let s = ServerBuilder::default().build().unwrap();
            assert_eq!(s.port, DEFAULT_PORT);

            let s = ServerBuilder::new().build().unwrap();
            assert_eq!(s.port, DEFAULT_PORT);

            let s = ServerBuilder::new().port(Some(1234)).build().unwrap();
            assert_eq!(s.port, 1234);
        }
    }

    mod server_tests {
        use super::*;

        #[test]
        fn test_server_default() {
            let s = ServerBuilder::default().build().unwrap();
            assert_eq!(s.port, DEFAULT_PORT);
        }

        #[test]
        fn test_server_port() {
            let s = ServerBuilder::new().port(Some(1234)).build().unwrap();
            assert_eq!(s.port, 1234);
        }
    }
}
