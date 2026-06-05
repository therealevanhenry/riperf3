use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpStream;

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, Result, RiperfError};
use crate::net;
use crate::protocol::{self, TestParams, TestResultsJson, TestState, TransportProtocol};
use crate::stream::{self, DataStream, StreamCounters, UdpRecvStats};
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
    /// Whether `blksize` came from an explicit `-l`. When false for UDP, the
    /// datagram size is derived from the control-socket MSS at run time
    /// (iperf3 parity, issue #6) rather than using the `blksize` default.
    /// Internal: set by the builder from whether `.blksize()` was called.
    blksize_explicit: bool,
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
    pub zerocopy: bool,
    pub gsro: bool,
    pub sendmmsg: bool,
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
    pub username: Option<String>,
    pub password: Option<String>,
    pub rsa_public_key_path: Option<String>,
    pub use_pkcs1_padding: bool,
}

/// Build receiver-perspective summaries from the server's results. In a forward
/// test the local streams are senders, so the receiver's view — most importantly
/// UDP loss — lives only in the results the server returned. Surfacing it as a
/// `receiver` line matches iperf3; without it, forward UDP looks loss-free even
/// when the link is dropping packets (issue #25). `is_udp` gates the datagram
/// loss/jitter columns so a forward TCP run shows a plain receiver byte line.
fn server_receiver_summaries(
    server: &TestResultsJson,
    end: f64,
    is_udp: bool,
) -> Vec<crate::reporter::StreamSummary> {
    server
        .streams
        .iter()
        .map(|s| crate::reporter::StreamSummary {
            stream_id: s.id,
            start: 0.0,
            end,
            bytes: s.bytes,
            is_sender: false,
            retransmits: None,
            jitter: is_udp.then_some(s.jitter),
            lost: is_udp.then_some(s.errors),
            total_packets: is_udp.then_some(s.packets),
        })
        .collect()
}

impl Client {
    pub async fn run(&self) -> Result<TestResultsJson> {
        // -T/--title: prefix every client text line with "<title>:  " (#34),
        // matching iperf3. Run-scoped (cleared on drop) and only in plain-text
        // mode — `-J` and `--json-stream` emit machine JSON, which iperf3 never
        // titles. Held for the whole run so the reporter task and the preamble
        // both see it.
        let _title_guard = (!self.json_output && !self.json_stream)
            .then(|| crate::macros::OutputTitleGuard::set(self.title.clone()));

        // ---- Generate cookie and connect ----
        let cookie = protocol::make_cookie();
        let mut ctrl = net::tcp_connect(
            &self.host,
            self.port,
            self.connect_timeout,
            None,
            self.bind_address.as_deref(),
            self.mptcp,
            self.ip_version,
        )
        .await?;
        net::configure_tcp_stream(&ctrl, true)?;

        // The control connection's MSS sizes UDP datagrams (issue #6) and feeds
        // the `-J` start.tcp_mss_default field (#36 PR3).
        let control_mss_opt = net::tcp_maxseg(&ctrl);
        let control_mss = control_mss_opt.unwrap_or(0);

        // Resolve the UDP datagram size now that the control connection exists:
        // when `-l` wasn't given, derive it from the control-socket MSS so a
        // jumbo-frame path uses large datagrams instead of the 1460 floor
        // (iperf3 parity, issue #6). TCP keeps its own block size unchanged.
        let blksize = if self.protocol == TransportProtocol::Udp && !self.blksize_explicit {
            resolve_udp_blksize(None, control_mss_opt)
        } else {
            self.blksize
        };

        // Apply control connection options
        if let Some(ref dev) = self.bind_dev {
            net::set_bind_dev(&ctrl, dev)?;
        }
        if let Some(ref spec) = self.cntl_ka {
            let (idle, intv, cnt) = parse_keepalive(spec);
            net::set_tcp_keepalive(&ctrl, idle, intv, cnt)?;
        }

        protocol::send_cookie(&mut ctrl, &cookie).await?;

        if self.verbose {
            vprintln!("Connecting to host {}, port {}", self.host, self.port);
        }

        let done = Arc::new(AtomicBool::new(false));
        // Signal `done` on every exit path (incl. early `?` returns) so a UDP
        // sender parked on the start barrier can't leak if setup fails (#5).
        let _done_guard = stream::DoneOnDrop(done.clone());
        // Released at TestStart so UDP senders don't transmit during stream
        // setup (issue #5): the create-streams handshake is lost under a flood.
        let start = Arc::new(AtomicBool::new(false));
        // Interval samples + TCP_INFO extremes the reporter collects during the
        // run, read back at DisplayResults for the `-J` blob (#36 PR2).
        let interval_data = Arc::new(Mutex::new(crate::reporter::CollectedIntervals::default()));
        let mut streams: Vec<DataStream> = Vec::new();
        let mut cpu_start: Option<CpuSnapshot> = None;
        let mut server_results: Option<TestResultsJson> = None;
        // Wall-clock at TestStart, for the `-J` start.timestamp (#36 PR3).
        let mut test_start_millis = 0u64;

        // ---- State machine: react to server-driven transitions ----
        loop {
            let state = protocol::recv_state(&mut ctrl).await?;

            match state {
                TestState::ParamExchange => {
                    let params = self.build_params(blksize);
                    protocol::send_params(&mut ctrl, &params).await?;
                }

                TestState::CreateStreams => {
                    streams = self.create_streams(&cookie, &done, &start, blksize).await?;
                }

                TestState::TestStart => {
                    // All streams are set up — release the UDP senders.
                    start.store(true, Ordering::Relaxed);
                    cpu_start = Some(CpuSnapshot::now());
                    test_start_millis = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);

                    // --json-stream: emit the `start` event now, before the reporter
                    // streams any `interval` events (faithful event order, #62).
                    if self.json_stream {
                        self.emit_json_stream_start(
                            &streams,
                            cpu_start.as_ref(),
                            blksize,
                            &interval_data,
                            &StartMeta {
                                cookie: String::from_utf8_lossy(
                                    &cookie[..protocol::COOKIE_SIZE - 1],
                                )
                                .into_owned(),
                                tcp_mss_default: control_mss,
                                start_time_millis: test_start_millis,
                            },
                        );
                    }
                }

                TestState::TestRunning => {
                    self.run_test(&mut ctrl, &streams, &done, blksize, interval_data.clone())
                        .await?;
                    // Test finished — send TestEnd
                    protocol::send_state(&mut ctrl, TestState::TestEnd).await?;
                }

                TestState::ExchangeResults => {
                    let results = self.build_results(&streams, cpu_start.as_ref());
                    protocol::send_results(&mut ctrl, &results).await?;
                    server_results = Some(protocol::recv_results(&mut ctrl).await?);
                }

                TestState::DisplayResults => {
                    self.print_results(
                        &streams,
                        cpu_start.as_ref(),
                        server_results.as_ref(),
                        blksize,
                        &interval_data,
                        &StartMeta {
                            cookie: String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1])
                                .into_owned(),
                            tcp_mss_default: control_mss,
                            start_time_millis: test_start_millis,
                        },
                    );
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

        server_results.ok_or_else(|| {
            RiperfError::Protocol("missing server results in control exchange".into())
        })
    }

    fn build_params(&self, blksize: usize) -> TestParams {
        let mut p = TestParams::default();
        match self.protocol {
            TransportProtocol::Tcp => p.tcp = Some(true),
            TransportProtocol::Udp => p.udp = Some(true),
        }
        p.time = Some(self.duration as i32);
        p.omit = Some(self.omit as i32);
        p.parallel = Some(self.num_streams as i32);
        p.len = Some(blksize as i32);
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
        // Always carry the resolved rate (incl. 0 = unlimited) so the server
        // paces correctly in reverse/bidir; matches iperf3, which always sends
        // it. `bandwidth` is the effective rate after the build-time default
        // (UDP unset → 1 Mbit/s), so 0 here unambiguously means unlimited (#17).
        p.bandwidth = Some(self.bandwidth);
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

        // Auth: encrypt credentials if username and public key are set
        if let (Some(ref username), Some(ref pubkey_path)) =
            (&self.username, &self.rsa_public_key_path)
        {
            let pubkey_pem = std::fs::read(pubkey_path).unwrap_or_default();
            let password = self
                .password
                .clone()
                .or_else(|| crate::auth::read_password().ok())
                .unwrap_or_default();
            if let Ok(token) = crate::auth::encode_auth_token(
                username,
                &password,
                &pubkey_pem,
                self.use_pkcs1_padding,
            ) {
                p.authtoken = Some(token);
            }
        }

        p
    }

    async fn create_streams(
        &self,
        cookie: &[u8; protocol::COOKIE_SIZE],
        done: &Arc<AtomicBool>,
        start: &Arc<AtomicBool>,
        blksize: usize,
    ) -> Result<Vec<DataStream>> {
        let mut streams = Vec::new();

        // In normal mode: client sends. Reverse: client receives. Bidir: both.
        let send_count = if self.reverse && !self.bidir {
            0
        } else {
            self.num_streams
        };
        let recv_count = if self.reverse || self.bidir {
            self.num_streams
        } else {
            0
        };
        let total = send_count + recv_count;

        // Max send duration the UDP senders self-enforce (issue #5): the
        // sender stops itself at `-t` so termination never depends on `done`
        // being set by a CPU-starved runtime. Only in duration mode;
        // byte/block-limited tests stop on `done`.
        let max_duration = (self.bytes_to_send.is_none() && self.blocks_to_send.is_none())
            .then(|| Duration::from_secs(self.duration as u64));

        match self.protocol {
            TransportProtocol::Tcp => {
                for i in 0..total {
                    let mut data_stream = net::tcp_connect(
                        &self.host,
                        self.port,
                        self.connect_timeout,
                        self.cport,
                        self.bind_address.as_deref(),
                        self.mptcp,
                        self.ip_version,
                    )
                    .await?;
                    protocol::send_cookie(&mut data_stream, cookie).await?;
                    net::configure_tcp_stream_full(
                        &data_stream,
                        self.no_delay,
                        self.mss,
                        self.window,
                        self.congestion.as_deref(),
                    )?;
                    // Apply socket options (no-ops on non-Linux)
                    if self.dont_fragment {
                        net::set_dont_fragment(&data_stream)?;
                    }
                    if let Some(rate) = self.fq_rate {
                        net::set_fq_rate(&data_stream, rate)?;
                    }
                    if let Some(ms) = self.rcv_timeout {
                        net::set_rcv_timeout(&data_stream, ms)?;
                    }
                    if let Some(ms) = self.snd_timeout {
                        net::set_snd_timeout(&data_stream, ms)?;
                    }
                    if let Some(label) = self.flowlabel {
                        net::set_ipv6_flowlabel(&data_stream, label)?;
                    }
                    if let Some(ref dev) = self.bind_dev {
                        net::set_bind_dev(&data_stream, dev)?;
                    }
                    if self.tos != 0 {
                        net::set_tos(&data_stream, self.tos as u32)?;
                    }

                    // Extract raw fd for TCP_INFO (Unix only)
                    #[cfg(unix)]
                    let raw_fd = {
                        use std::os::unix::io::AsRawFd;
                        Some(data_stream.as_raw_fd())
                    };
                    #[cfg(not(unix))]
                    let raw_fd: Option<i32> = None;

                    // Capture the real socket addresses before the stream moves
                    // into its task, for the `-J` start.connected block (#36).
                    let local_addr = data_stream.local_addr().ok();
                    let peer_addr = data_stream.peer_addr().ok();
                    let sock = socket2::SockRef::from(&data_stream);
                    let sndbuf_actual = sock.send_buffer_size().ok().map(|v| v as u64);
                    let rcvbuf_actual = sock.recv_buffer_size().ok().map(|v| v as u64);

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());
                    let fp = self.file.as_ref().map(std::path::PathBuf::from);

                    let task = if is_sender {
                        let buf = make_send_buffer(blksize, self.repeating_payload);
                        let c = counters.clone();
                        let d = done.clone();
                        let zc = self.zerocopy;
                        tokio::spawn(async move {
                            if zc {
                                // Zerocopy senders exist only for these targets
                                // (stream.rs). The gate must match the impls, not
                                // `unix`: other-Unix (NetBSD/OpenBSD/illumos) is
                                // `unix` with no zerocopy impl, so `#[cfg(unix)]`
                                // referenced a nonexistent fn and failed to
                                // compile there (#78). Elsewhere `-Z` cleanly
                                // falls back to the normal sender.
                                #[cfg(any(
                                    target_os = "linux",
                                    target_os = "macos",
                                    target_os = "freebsd"
                                ))]
                                {
                                    stream::run_tcp_sender_zerocopy(data_stream, c, buf, d).await
                                }
                                #[cfg(not(any(
                                    target_os = "linux",
                                    target_os = "macos",
                                    target_os = "freebsd"
                                )))]
                                {
                                    stream::run_tcp_sender(data_stream, c, buf, d, fp).await
                                }
                            } else {
                                stream::run_tcp_sender(data_stream, c, buf, d, fp).await
                            }
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        let srxc = self.skip_rx_copy;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d, srxc, fp).await
                        })
                    };

                    streams.push(DataStream {
                        id: stream_id,
                        is_sender,
                        counters,
                        udp_recv_stats: None,
                        task,
                        raw_fd,
                        local_addr,
                        peer_addr,
                        sndbuf_actual,
                        rcvbuf_actual,
                    });
                }
            }
            TransportProtocol::Udp => {
                // Resolve once, honoring -4/-6, so the bind family matches the
                // peer and the connection respects the version preference (#10).
                let remote = net::resolve_host(&self.host, self.port, self.ip_version).await?;
                // Honor -B: resolve the UDP source address once (family-validated
                // against the target), then bind every stream's socket to it,
                // mirroring the TCP path (#15).
                let bind_ip = match self.bind_address.as_deref() {
                    Some(b) => Some(
                        net::resolve_bind_ip(b, remote.is_ipv6(), &self.host)
                            .await?
                            .to_string(),
                    ),
                    None => None,
                };
                for i in 0..total {
                    let udp_sock = net::udp_bind(bind_ip.as_deref(), 0, remote.is_ipv6()).await?;
                    if let Some(ref dev) = self.bind_dev {
                        net::set_bind_dev(&udp_sock, dev)?;
                    }
                    udp_sock.connect(remote).await?;
                    protocol::udp_connect_client(&udp_sock).await?;

                    // Apply GSO/GRO if requested (no-ops on non-Linux)
                    if self.gsro {
                        let _ = net::set_udp_gso(&udp_sock, blksize as u16);
                        let _ = net::set_udp_gro(&udp_sock);
                    }
                    if self.tos != 0 {
                        let _ = net::set_tos(&udp_sock, self.tos as u32);
                    }

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i < send_count;
                    let counters = Arc::new(StreamCounters::new());

                    // Capture real addresses for the `-J` start.connected block
                    // (#36) before the socket moves into its task.
                    let local_addr = udp_sock.local_addr().ok();
                    let peer_addr = udp_sock.peer_addr().ok();
                    let sock = socket2::SockRef::from(&udp_sock);
                    // Honor -w/--window on the UDP socket too (#59); iperf3 applies
                    // it to UDP via iperf_udp_buffercheck. Set before the read-back
                    // so sndbuf_actual/rcvbuf_actual report the realized size.
                    net::apply_socket_window(&sock, self.window);
                    let sndbuf_actual = sock.send_buffer_size().ok().map(|v| v as u64);
                    let rcvbuf_actual = sock.recv_buffer_size().ok().map(|v| v as u64);

                    // Convert tokio UdpSocket to std for blocking I/O
                    let std_sock = udp_sock.into_std().map_err(RiperfError::Io)?;

                    let task = if is_sender {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        // Effective rate is resolved at build time (UDP unset →
                        // 1 Mbit/s); 0 means unlimited — no pacing (#17).
                        let rate = self.bandwidth;
                        let u64bit = self.udp_counters_64bit;
                        let use_sendmmsg = self.sendmmsg;
                        let st = start.clone();
                        let md = max_duration;
                        tokio::task::spawn_blocking(move || {
                            if use_sendmmsg {
                                stream::run_udp_sender_sendmmsg(
                                    std_sock, c, bs, d, rate, u64bit, st, md,
                                )
                            } else {
                                stream::run_udp_sender_blocking(
                                    std_sock, c, bs, d, rate, u64bit, st, md,
                                )
                            }
                        })
                    } else {
                        let c = counters.clone();
                        let d = done.clone();
                        let bs = blksize;
                        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                        let sc = stats.clone();
                        let u64bit = self.udp_counters_64bit;
                        let task = tokio::task::spawn_blocking(move || {
                            stream::run_udp_receiver_blocking(std_sock, c, sc, bs, d, u64bit)
                        });
                        streams.push(DataStream {
                            id: stream_id,
                            is_sender,
                            counters,
                            udp_recv_stats: Some(stats),
                            task,
                            raw_fd: None,
                            local_addr,
                            peer_addr,
                            sndbuf_actual,
                            rcvbuf_actual,
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
                        local_addr,
                        peer_addr,
                        sndbuf_actual,
                        rcvbuf_actual,
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
        blksize: usize,
        collector: Arc<Mutex<crate::reporter::CollectedIntervals>>,
    ) -> Result<()> {
        // Run the interval reporter whenever intervals are enabled. It prints
        // live for text / json-stream; for plain -J it runs silently to collect
        // intervals for the final blob (#36 PR2).
        let interval_secs = self.interval.unwrap_or(1.0);
        let print_intervals = !self.json_output || self.json_stream;
        let collect_intervals = self.json_output && !self.json_stream;
        // The reporter needs the collector whenever we emit JSON: `-J` collects the
        // typed intervals for the final blob; `--json-stream` streams them live but
        // still needs the per-stream TCP_INFO extremes (max cwnd/rtt) handed back
        // for the `end` event (#62).
        let want_collector = collect_intervals || self.json_stream;
        // Clock origin shared with the reporter: `report_start` is captured right
        // before spawning it, so `report_start.elapsed()` at end-of-test is the
        // authoritative final-interval boundary (#55).
        let reporter_end = Arc::new(crate::reporter::ReporterEnd::new());
        let report_start = std::time::Instant::now();
        let interval_handle = if interval_secs > 0.0 {
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
                    print: print_intervals,
                    blksize,
                },
                stream_refs,
                done.clone(),
                reporter_end.clone(),
                want_collector.then(|| collector.clone()),
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

        // The authoritative end time for the reporter's final interval (#55). A
        // duration run ends at exactly `-t`, so pass that value (not the measured
        // elapsed, which trails the deadline by a variable scheduling slack and
        // would smear a boundary-aligned end into a spurious sliver). A
        // byte/block run ends at an arbitrary instant, so use the measured
        // elapsed.
        let end_secs = match end_condition {
            EndCondition::Duration(dur) => {
                // Use select to handle both timer and control socket.
                //
                // The UDP senders also enforce this deadline themselves inside
                // their loop (see the `deadline` passed at stream creation):
                // at a high `-b` the CPU-bound senders can saturate every core
                // and starve this async timer, so they must not depend on it to
                // stop (issue #5). Once they self-terminate, CPU frees and this
                // timer fires normally to drive the rest of the shutdown.
                tokio::select! {
                    _ = tokio::time::sleep(dur) => {}
                    state = protocol::recv_state(ctrl) => {
                        // Server sent something unexpected during the test
                        if let Ok(TestState::ServerTerminate) = state {
                            return Err(RiperfError::Aborted("server terminated".into()));
                        }
                    }
                }
                dur.as_secs_f64()
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
                report_start.elapsed().as_secs_f64()
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
                    let blocks = total_bytes / blksize as u64;
                    if blocks >= target {
                        break;
                    }
                }
                report_start.elapsed().as_secs_f64()
            }
        };

        // End of test: hand the reporter the authoritative end time, then stop
        // the senders immediately (`done`) so no bytes leak past the deadline into
        // the final interval or the summary (#55). The reporter prioritises this
        // `finish` over `done` (see its select), so the final interval still
        // flushes; we then wait for it before tearing the streams down.
        reporter_end.finish(end_secs);
        done.store(true, Ordering::Relaxed);
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
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) {
        if self.json_output {
            self.print_results_json(
                streams,
                cpu_start,
                remote_cpu,
                blksize,
                interval_data,
                start_meta,
            );
        } else if self.json_stream {
            // --json-stream: emit the `end` event. (Previously this fell through
            // to print_results_text, printing text banners into the NDJSON — #62.)
            self.emit_json_stream_end(
                streams,
                cpu_start,
                remote_cpu,
                blksize,
                interval_data,
                start_meta,
            );
        } else {
            self.print_results_text(streams, remote_cpu);
        }
    }

    fn print_results_text(&self, streams: &[DataStream], server_results: Option<&TestResultsJson>) {
        let test_duration = self.duration as f64;
        crate::reporter::print_separator();

        let mut summaries: Vec<crate::reporter::StreamSummary> = streams
            .iter()
            .map(|s| {
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

                crate::reporter::StreamSummary {
                    stream_id: s.id,
                    start: 0.0,
                    end: test_duration,
                    bytes,
                    is_sender: s.is_sender,
                    retransmits: None,
                    jitter,
                    lost,
                    total_packets: total,
                }
            })
            .collect();

        // Forward: the server is the receiver, so its loss/throughput lives only
        // in the results it returned — surface it as the receiver line iperf3
        // prints, otherwise forward UDP looks loss-free even when the link drops
        // packets (issue #25). Reverse already reports the receiver locally;
        // bidir's server-receive half isn't split out of the aggregate results.
        if !self.reverse && !self.bidir {
            if let Some(server) = server_results {
                let is_udp = matches!(self.protocol, TransportProtocol::Udp);
                summaries.extend(server_receiver_summaries(server, test_duration, is_udp));
            }
        }

        // Per-stream lines plus aggregate [SUM] row(s) for parallel streams
        // (issue #4), via the shared path the server also uses.
        crate::reporter::print_final_summaries(&summaries, self.format_char);
    }

    /// Assemble the typed iperf3-schema report input from the finished test.
    /// Shared by `-J` (build + pretty-print) and `--json-stream` (build + emit the
    /// `end` event; and at TestStart, the `start` event from a partial input where
    /// only the start fields are meaningful — see `emit_json_stream_start`).
    fn build_report_input(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) -> crate::json_report::ReportInput {
        use crate::json_report::{
            CpuUtilization, ReportInput, StreamReport, TcpEndExtras, UdpStreamStats,
        };

        // Take the interval samples + per-stream extremes the reporter collected.
        // Its task has been joined by now (run_test awaits it), so this is final.
        let (collected_intervals, extremes) = match interval_data.lock() {
            Ok(mut g) => (
                std::mem::take(&mut g.intervals),
                std::mem::take(&mut g.extremes),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };

        let test_duration = self.duration as f64;
        let cpu_end = CpuSnapshot::now();
        let cpu_util = cpu_start
            .map(|start| cpu_end.utilization_since(start))
            .unwrap_or_default();
        let (remote_total, remote_user, remote_system) = remote_cpu
            .map(|r| (r.cpu_util_total, r.cpu_util_user, r.cpu_util_system))
            .unwrap_or((0.0, 0.0, 0.0));

        let is_udp = matches!(self.protocol, TransportProtocol::Udp);
        // Forward UDP: the server is the receiver, so its loss lives only in the
        // results it returned — attach it to the (sender) streams (#25).
        let is_forward_udp = is_udp && !self.reverse && !self.bidir;

        let stream_reports: Vec<StreamReport> = streams
            .iter()
            .map(|s| {
                let local_bytes = if s.is_sender {
                    s.counters.bytes_sent()
                } else {
                    s.counters.bytes_received()
                };
                // The peer's per-stream result is the opposite side of this stream.
                let server_stream =
                    remote_cpu.and_then(|r| r.streams.iter().find(|x| x.id == s.id));

                // UDP datagram stats: from our local receiver if we measured them,
                // else (forward UDP) from the server's results for this stream (#25).
                let udp = if let Some(ref lock) = s.udp_recv_stats {
                    lock.lock().ok().map(|st| UdpStreamStats {
                        jitter_secs: st.jitter,
                        lost_packets: st.cnt_error,
                        packets: st.packet_count,
                        out_of_order: st.outoforder_packets,
                    })
                } else if is_forward_udp && s.is_sender {
                    server_stream.map(|x| UdpStreamStats {
                        jitter_secs: x.jitter,
                        lost_packets: x.errors,
                        packets: x.packets,
                        out_of_order: 0,
                    })
                } else {
                    None
                };

                // to_canonical(): unwrap an IPv4-mapped IPv6 address to plain IPv4
                // (matches iperf3); a no-op for the client's usual canonical
                // addresses, correct if the client is bound to a dual-stack socket.
                let (local_host, local_port) = s
                    .local_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_else(|| (self.host.clone(), 0));
                let (remote_host, remote_port) = s
                    .peer_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_else(|| (self.host.clone(), self.port));

                // Sender-side TCP_INFO extremes + real retransmit total collected
                // across intervals (#36 PR2); only present for streams we sent.
                let ext = extremes
                    .iter()
                    .find(|e| e.stream_id == s.id && e.has_samples());
                let tcp_end = ext.map(|e| TcpEndExtras {
                    max_snd_cwnd: e.max_snd_cwnd,
                    max_rtt: e.max_rtt,
                    min_rtt: e.min_rtt,
                    mean_rtt: e.mean_rtt(),
                    reorder: e.reorder,
                });
                let retransmits = if is_udp {
                    None
                } else {
                    // Real cumulative total when TCP_INFO gave us one (forward
                    // sender). Otherwise (reverse, or a stream we didn't send) use
                    // iperf3's defaults: 0 on a platform that supports retransmit
                    // info, -1 where it doesn't.
                    ext.and_then(|e| e.total_retransmits)
                        .map(|r| r as i64)
                        .or(Some(if crate::tcp_info::has_retransmit_info() {
                            0
                        } else {
                            -1
                        }))
                };

                StreamReport {
                    id: s.id,
                    local_host,
                    local_port,
                    remote_host,
                    remote_port,
                    is_sender: s.is_sender,
                    local_bytes,
                    remote_bytes: server_stream.map(|x| x.bytes),
                    retransmits,
                    tcp_end,
                    udp,
                }
            })
            .collect();

        let input = ReportInput {
            protocol: self.protocol,
            reverse: self.reverse,
            bidir: self.bidir,
            duration: test_duration,
            num_streams: self.num_streams as i32,
            blksize: blksize as i64,
            omit: self.omit as i32,
            tos: self.tos,
            target_bitrate: self.bandwidth,
            bytes: self.bytes_to_send.unwrap_or(0),
            blocks: self.blocks_to_send.unwrap_or(0),
            connecting_host: self.host.clone(),
            connecting_port: self.port,
            is_server: false,
            accepted_host: String::new(),
            accepted_port: 0,
            version: format!("riperf3 {}", env!("CARGO_PKG_VERSION")),
            system_info: system_info(),
            cpu: CpuUtilization {
                host_total: cpu_util.host_total,
                host_user: cpu_util.host_user,
                host_system: cpu_util.host_system,
                remote_total,
                remote_user,
                remote_system,
            },
            // Reporting the *actually-applied* congestion algorithm is #37; omit
            // the field until that read-back lands rather than emit the requested
            // value (which may differ from what the kernel used).
            congestion_used: None,
            cookie: start_meta.cookie.clone(),
            tcp_mss_default: start_meta.tcp_mss_default,
            // -M/--set-mss request: emitted as start.tcp_mss (TCP only), which
            // suppresses tcp_mss_default. build() does the TCP/UDP gating.
            mss: self.mss.filter(|&m| m > 0).map(|m| m as u32),
            fq_rate: self.fq_rate.unwrap_or(0),
            // iperf3's start.sock_bufsize is the requested -w value (0 if unset);
            // sndbuf/rcvbuf_actual are the kernel's actual sizes on a data socket.
            // .max(0): the public builder accepts an i32 window; clamp so a
            // negative can't wrap to a huge u64 (the CLI path is already >= 0).
            sock_bufsize: self.window.map(|w| w.max(0) as u64).unwrap_or(0),
            sndbuf_actual: streams.first().and_then(|s| s.sndbuf_actual).unwrap_or(0),
            rcvbuf_actual: streams.first().and_then(|s| s.rcvbuf_actual).unwrap_or(0),
            interval: self.interval.unwrap_or(1.0),
            // riperf3's single --gsro flag drives both GSO and GRO.
            gso: i32::from(self.gsro),
            gro: i32::from(self.gsro),
            start_time_millis: start_meta.start_time_millis,
            extra_data: self.extra_data.clone(),
            intervals: collected_intervals,
            streams: stream_reports,
        };

        input
    }

    /// `-J`: build and pretty-print the single batched report blob.
    fn print_results_json(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) {
        let input = self.build_report_input(
            streams,
            cpu_start,
            remote_cpu,
            blksize,
            interval_data,
            start_meta,
        );
        println!("{}", serde_json::to_string_pretty(&input.build()).unwrap());
    }

    /// `--json-stream`: emit the `start` event (#62). Called at TestStart, before
    /// any interval event. Only the `start` block is meaningful at this point; the
    /// rest of the report input is placeholder (no bytes/cpu/intervals collected
    /// yet) and is discarded.
    fn emit_json_stream_start(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) {
        let input =
            self.build_report_input(streams, cpu_start, None, blksize, interval_data, start_meta);
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "start",
            &input.build().start,
        ));
    }

    /// `--json-stream`: emit the `end` event (#62) at DisplayResults. The interval
    /// events were already streamed live by the reporter.
    fn emit_json_stream_end(
        &self,
        streams: &[DataStream],
        cpu_start: Option<&CpuSnapshot>,
        remote_cpu: Option<&TestResultsJson>,
        blksize: usize,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
        start_meta: &StartMeta,
    ) {
        let input = self.build_report_input(
            streams,
            cpu_start,
            remote_cpu,
            blksize,
            interval_data,
            start_meta,
        );
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "end",
            &input.build().end,
        ));
    }
}

enum EndCondition {
    Duration(Duration),
    Bytes(u64),
    Blocks(u64),
}

/// Start-of-test metadata for the `-J` `start` block (#36 PR3), captured in
/// `run()` where the cookie / control-MSS / start wall-clock are known.
struct StartMeta {
    cookie: String,
    tcp_mss_default: u32,
    start_time_millis: u64,
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
    bandwidth: Option<u64>,
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
    zerocopy: bool,
    gsro: bool,
    sendmmsg: bool,
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
    username: Option<String>,
    password: Option<String>,
    rsa_public_key_path: Option<String>,
    use_pkcs1_padding: bool,
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
            bandwidth: None,
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
            zerocopy: false,
            gsro: false,
            sendmmsg: false,
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
            username: None,
            password: None,
            rsa_public_key_path: None,
            use_pkcs1_padding: false,
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
        // `Some` even for 0: an explicit `-b 0` means unlimited and must be
        // distinguishable from "unset" (which resolves to the UDP default) (#17).
        self.bandwidth = Some(bps);
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

    /// Prefix every client text-output line with `<title>:  ` (`-T/--title`),
    /// matching iperf3. Applies only to plain-text output, not `-J`/`--json-stream`.
    ///
    /// Note: the prefix is tracked in a process-global for the duration of the
    /// run, so two `Client::run` calls executing concurrently in the same process
    /// are not isolated for `-T` (their titled lines can interleave). This does
    /// not affect the CLI (one run per process) or sequential library use.
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

    pub fn zerocopy(mut self, enabled: bool) -> Self {
        self.zerocopy = enabled;
        self
    }

    pub fn gsro(mut self, enabled: bool) -> Self {
        self.gsro = enabled;
        self
    }

    pub fn sendmmsg(mut self, enabled: bool) -> Self {
        self.sendmmsg = enabled;
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
        debug_assert!(
            matches!(version, 4 | 6),
            "ip_version must be 4 or 6, got {version}"
        );
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

    pub fn username(mut self, name: &str) -> Self {
        self.username = Some(name.to_string());
        self
    }

    pub fn password(mut self, pass: &str) -> Self {
        self.password = Some(pass.to_string());
        self
    }

    pub fn rsa_public_key_path(mut self, path: &str) -> Self {
        self.rsa_public_key_path = Some(path.to_string());
        self
    }

    pub fn use_pkcs1_padding(mut self, enabled: bool) -> Self {
        self.use_pkcs1_padding = enabled;
        self
    }

    // String-accepting variants — parse KMG suffixes (e.g., "1M", "512K", "10G")
    // so callers don't need to import parse_kmg/parse_bitrate.

    pub fn bytes_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.bytes(parse_kmg(s)?))
    }

    pub fn blocks_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.blocks(parse_kmg(s)?))
    }

    pub fn blksize_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.blksize(parse_kmg(s)? as usize))
    }

    pub fn window_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        Ok(self.window(parse_kmg(s)? as i32))
    }

    pub fn bandwidth_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        let (rate, _burst) = parse_bitrate(s)?;
        Ok(self.bandwidth(rate))
    }

    pub fn fq_rate_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        // --fq-rate is a rate: decimal (1000-based) suffixes, like iperf3 (#56).
        Ok(self.fq_rate(crate::utils::parse_rate(s)?))
    }

    pub fn build(self) -> std::result::Result<Client, ConfigError> {
        let host = self.host.ok_or(ConfigError::MissingField("host"))?;

        // Reject a -B literal whose family contradicts -4/-6 at config time,
        // mirroring the server-side check (#12); a bind hostname is validated
        // against the target family at connect time instead (#15).
        if let (Some(v), Some(addr)) = (self.ip_version, self.bind_address.as_deref()) {
            let addr = addr.split('%').next().unwrap_or(addr);
            if let Ok(ip) = addr.parse::<std::net::IpAddr>() {
                if (v == 4 && ip.is_ipv6()) || (v == 6 && ip.is_ipv4()) {
                    return Err(ConfigError::InvalidValue(
                        "bind_address",
                        format!("-{v} conflicts with bind address {addr}"),
                    ));
                }
            }
        }

        // Reject flags that require OS support not available on this platform.
        // Matches iperf3 behavior: error at build/parse time, not at runtime.
        #[cfg(not(unix))]
        {
            if self.zerocopy {
                return Err(ConfigError::Unsupported(
                    "this OS does not support sendfile".into(),
                ));
            }
            if self.affinity.is_some() {
                return Err(ConfigError::Unsupported(
                    "CPU affinity is not supported on this platform".into(),
                ));
            }
            if self.bind_dev.is_some() {
                return Err(ConfigError::Unsupported(
                    "SO_BINDTODEVICE is not supported on this platform".into(),
                ));
            }
            if self.congestion.is_some() {
                return Err(ConfigError::Unsupported(
                    "TCP congestion control is not supported on this platform".into(),
                ));
            }
            if self.gsro {
                return Err(ConfigError::Unsupported(
                    "UDP GSO/GRO is not supported on this platform".into(),
                ));
            }
        }

        // sendmmsg's real implementation is Linux/FreeBSD/NetBSD only; elsewhere
        // (incl. macOS, which is `unix` but unsupported) it would silently fall
        // back to the per-packet sender, so reject it at build time instead (#18).
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
        if self.sendmmsg {
            return Err(ConfigError::Unsupported(
                "sendmmsg is only supported on Linux, FreeBSD, and NetBSD".into(),
            ));
        }

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
            blksize_explicit: self.blksize.is_some(),
            reverse: self.reverse,
            bidir: self.bidir,
            omit: self.omit,
            no_delay: self.no_delay,
            mss: self.mss,
            window: self.window,
            // Resolve the rate default now (UDP unset → 1 Mbit/s, like iperf3);
            // an explicit -b (incl. 0 = unlimited) is honored. TCP default is
            // unlimited (0). After this, bandwidth==0 unambiguously = unlimited.
            bandwidth: self.bandwidth.unwrap_or(match self.protocol {
                TransportProtocol::Udp => DEFAULT_UDP_RATE,
                TransportProtocol::Tcp => 0,
            }),
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
            zerocopy: self.zerocopy,
            gsro: self.gsro,
            sendmmsg: self.sendmmsg,
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
            username: self.username,
            password: self.password,
            rsa_public_key_path: self.rsa_public_key_path,
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

        // -- UDP -b 0 = unlimited (issue #17) --

        #[test]
        fn udp_unset_bandwidth_defaults_to_1m() {
            // No -b on UDP resolves to the 1 Mbit/s default (iperf3 parity),
            // now resolved at build time rather than in the sender.
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, DEFAULT_UDP_RATE);
        }

        #[test]
        fn udp_explicit_zero_bandwidth_is_unlimited() {
            // -b 0 means unlimited (0), NOT the 1 Mbit/s default (#17).
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .bandwidth(0)
                .build()
                .unwrap();
            assert_eq!(c.bandwidth, 0);
        }

        #[test]
        fn tcp_unset_bandwidth_is_unlimited() {
            // TCP default stays unlimited (0).
            let c = ClientBuilder::new("h").build().unwrap();
            assert_eq!(c.protocol, TransportProtocol::Tcp);
            assert_eq!(c.bandwidth, 0);
        }

        #[test]
        fn udp_build_params_carries_bandwidth_including_zero() {
            // The negotiated rate must reach the server (in `len`/`bandwidth`)
            // so reverse-mode -b 0 is unlimited server-side, not throttled.
            let c = ClientBuilder::new("h")
                .protocol(TransportProtocol::Udp)
                .bandwidth(0)
                .build()
                .unwrap();
            assert_eq!(c.build_params(1460).bandwidth, Some(0));
        }

        // -- forward UDP receiver-loss reporting (issue #25) --

        #[test]
        fn forward_udp_surfaces_server_receiver_loss() {
            // Forward UDP: the client is the sender, so the receiver's loss lives
            // only in the server's results. riperf3 must surface it as a receiver
            // line, like iperf3 — otherwise forward looks artificially loss-free
            // even when the link drops packets (issue #25).
            let server = TestResultsJson {
                cpu_util_total: 0.0,
                cpu_util_user: 0.0,
                cpu_util_system: 0.0,
                sender_has_retransmits: -1,
                congestion_used: None,
                streams: vec![protocol::StreamResultJson {
                    id: 1,
                    bytes: 2_000_000,
                    retransmits: -1,
                    jitter: 0.000_03,
                    errors: 4258,
                    omitted_errors: 0,
                    packets: 267_190,
                    omitted_packets: 0,
                    start_time: 0.0,
                    end_time: 5.0,
                }],
            };

            let recv = server_receiver_summaries(&server, 5.0, true);
            assert_eq!(recv.len(), 1);
            assert!(!recv[0].is_sender, "server is the receiver in forward mode");
            assert_eq!(recv[0].lost, Some(4258));
            assert_eq!(recv[0].total_packets, Some(267_190));
            assert_eq!(recv[0].jitter, Some(0.000_03));

            // Renders as a receiver line carrying the loss iperf3 would print.
            let line = crate::reporter::format_summary_line(&recv[0], 'a');
            assert!(line.contains("receiver"), "{line}");
            assert!(line.contains("4258/267190"), "{line}");

            // TCP forward: no datagram-loss columns, just a receiver byte line.
            let tcp = server_receiver_summaries(&server, 5.0, false);
            assert_eq!(tcp[0].lost, None);
            assert_eq!(tcp[0].total_packets, None);
            assert_eq!(tcp[0].jitter, None);
        }

        // -- client -B vs -4/-6 build-time validation (issue #15) --

        #[test]
        fn bind_address_family_conflict_rejected_at_build() {
            // A -B literal contradicting -4/-6 is rejected at config time,
            // mirroring the server (#12); matching families build fine (#15).
            assert!(ClientBuilder::new("h")
                .ip_version(6)
                .bind_address("10.0.0.1")
                .build()
                .is_err());
            assert!(ClientBuilder::new("h")
                .ip_version(4)
                .bind_address("::1")
                .build()
                .is_err());
            assert!(ClientBuilder::new("h")
                .ip_version(4)
                .bind_address("10.0.0.1")
                .build()
                .is_ok());
        }
    }
}
