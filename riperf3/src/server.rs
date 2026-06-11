use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, Result, RiperfError};
use crate::net;
use crate::protocol::{self, TestParams, TestResultsJson, TestState, TransportProtocol};
use crate::stream::{self, DataStream, StreamCounters, UdpRecvStats};
use crate::utils::*;

/// Shared test configuration derived from the client's parameter JSON.
/// Crate-internal (#67): retracted from the public API, so `pub(crate)` keeps a
/// future stray `pub use` from silently re-leaking it.
pub(crate) struct TestConfig {
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
    /// `-b rate/burst` block count from the client (0 = unset) (#160).
    pub burst: u32,
    pub pacing_timer: u32,
    pub tos: i32,
    pub congestion: Option<String>,
    pub udp_counters_64bit: bool,
}

impl TestConfig {
    // Crate-internal: takes the wire `TestParams` (now `pub(crate)` per #67),
    // so it cannot be part of the public API.
    pub(crate) fn from_params(params: &TestParams) -> std::result::Result<Self, ConfigError> {
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

        // Bounds-check a negotiated `len` like the client builder (#188):
        // 0/absent → protocol default (iperf3 clients omit len 0); negative
        // would wrap `as usize` into a multi-EiB allocation; UDP below
        // MIN_UDP_BLKSIZE panicked the datagram-header write; oversized is an
        // allocation DoS. iperf3's server never sees these from real clients
        // (they validate locally), so rejection only fires for broken or
        // hostile peers — drop the test rather than run degenerate.
        let blksize = match params.len {
            None | Some(0) => default_blksize,
            Some(l) if l < 0 => {
                return Err(ConfigError::InvalidValue(
                    "len",
                    format!("negative block size: {l}"),
                ));
            }
            Some(l) => {
                let l = l as usize;
                if is_udp && !(MIN_UDP_BLKSIZE..=MAX_UDP_BLKSIZE).contains(&l) {
                    return Err(ConfigError::InvalidValue(
                        "len",
                        format!(
                            "block size invalid (minimum = {MIN_UDP_BLKSIZE} bytes, maximum = {MAX_UDP_BLKSIZE} bytes): {l}"
                        ),
                    ));
                }
                if !is_udp && l > MAX_BLOCKSIZE {
                    return Err(ConfigError::InvalidValue(
                        "len",
                        format!("block size too large (maximum = {MAX_BLOCKSIZE} bytes): {l}"),
                    ));
                }
                l
            }
        };

        Ok(Self {
            protocol,
            duration: params.time.unwrap_or(DEFAULT_DURATION as i32) as u32,
            num_streams: params.parallel.unwrap_or(1) as u32,
            blksize,
            reverse: params.reverse.unwrap_or(false),
            bidir: params.bidirectional.unwrap_or(false),
            omit: params.omit.unwrap_or(0) as u32,
            no_delay: params.nodelay.unwrap_or(false),
            mss: params.mss,
            window: params.window,
            // A present rate (incl. 0 = unlimited) is used verbatim. An ABSENT
            // rate means unlimited (0), matching iperf3: it omits the param only
            // for -b 0 and sends it explicitly otherwise (incl. its 1 Mbit/s UDP
            // default), as do riperf3 clients. Defaulting an absent UDP rate to
            // 1 Mbit/s throttled an iperf3 -b 0 reverse/bidir sender (#21). The
            // 1 Mbit/s UDP default is a client-side concern, resolved at build.
            bandwidth: params.bandwidth.unwrap_or(0),
            // Present only when the client set `-b rate/burst`, like iperf3
            // (it gates on nonzero before sending) (#160).
            burst: params
                .burst
                .filter(|&b| b > 0)
                .map(|b| b as u32)
                .unwrap_or(0),
            // The client's --pacing-timer quantum (#32); iperf3 always sends
            // it. Absent/non-positive (older peers) → iperf3's 1000 µs default.
            pacing_timer: params
                .pacing_timer
                .filter(|&us| us > 0)
                .map(|us| us as u32)
                .unwrap_or(crate::utils::DEFAULT_PACING_TIMER_US),
            tos: params.tos.unwrap_or(0),
            congestion: params.congestion.clone(),
            udp_counters_64bit: params.udp_counters_64bit.unwrap_or(0) != 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

// Constructed only via ServerBuilder; #[non_exhaustive] keeps future field
// additions (like json_output/json_stream in #50) from being breaking changes
// for downstream crates (#43 semver-proofing, the cheap half).
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub struct Server {
    pub(crate) port: u16,
    pub(crate) one_off: bool,
    pub(crate) verbose: bool,
    pub(crate) idle_timeout: Option<u32>,
    pub(crate) server_bitrate_limit: Option<u64>,
    pub(crate) server_max_duration: Option<u32>,
    pub(crate) forceflush: bool,
    pub(crate) bind_address: Option<String>,
    pub(crate) ip_version: Option<u8>,
    pub(crate) timestamps: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) rsa_private_key_path: Option<String>,
    pub(crate) authorized_users_path: Option<String>,
    pub(crate) time_skew_threshold: u32,
    pub(crate) use_pkcs1_padding: bool,
    /// Emit the test results as iperf3-schema JSON on stdout instead of text (#50).
    pub(crate) json_output: bool,
    /// Stream line-delimited interval JSON during the test (`--json-stream`).
    pub(crate) json_stream: bool,
}

/// Best-effort source IP the kernel would use to reach `client_addr`, paired
/// with `server_port` — the per-stream `local_host`/`local_port` for the `-J`
/// connected block on the single-socket UDP demux path (#80). The demux socket
/// is never `connect()`'d, so its own `local_addr` is the wildcard bind; the
/// recycling path reports the connected socket's source IP instead. Reproduce
/// that by connecting a throwaway socket to the client and reading its local IP.
/// Returns `None` on any error so the caller can fall back to the shared socket.
fn demux_local_addr_for(
    client_addr: std::net::SocketAddr,
    server_port: u16,
) -> Option<std::net::SocketAddr> {
    let bind: std::net::SocketAddr = if client_addr.is_ipv6() {
        (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
    } else {
        (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let probe = std::net::UdpSocket::bind(bind).ok()?;
    probe.connect(client_addr).ok()?;
    Some(std::net::SocketAddr::new(
        probe.local_addr().ok()?.ip(),
        server_port,
    ))
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        // Daemonizing (`-s -D`) is a process-level concern handled by the binary
        // *before* the tokio runtime is built — `daemon()` forks, and a fork from
        // inside a multi-threaded runtime would leave the child with no worker
        // threads (#81). The library must not fork here.
        let listener =
            net::tcp_listen(self.bind_address.as_deref(), self.port, self.ip_version).await?;
        // Under -J / --json-stream iperf3's server stdout is pure JSON (the
        // "Server listening" banners are suppressed) so the document parses
        // cleanly; match that.
        let json = self.json_output || self.json_stream;
        let sep = "-----------------------------------------------------------";
        if !json {
            println!("{sep}");
            println!("Server listening on {}", self.port);
            println!("{sep}");
        }

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
            if !json {
                println!("{sep}");
                println!("Server listening on {}", self.port);
                println!("{sep}");
            }
        }
        Ok(())
    }

    async fn handle_one_test(&self, listener: &tokio::net::TcpListener) -> Result<()> {
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

        let json = self.json_output || self.json_stream;
        // The control-socket peer address feeds the server's `start.accepted_connection`
        // (iperf_api.c uses getpeername(ctrl_sck) — distinct from the data-stream
        // addresses in `connected[]`). Captured for the `-J` blob (#50).
        // `to_canonical()` unwraps an IPv4-mapped IPv6 address (`::ffff:127.0.0.1`)
        // from the dual-stack listener back to plain `127.0.0.1`, as iperf3 does
        // (mapped_v4_to_regular_v4).
        let (accepted_host, accepted_port) =
            (peer_addr.ip().to_canonical().to_string(), peer_addr.port());

        // ---- Cookie ----
        let cookie = protocol::recv_cookie(&mut ctrl).await?;

        // ---- ParamExchange ----
        protocol::send_state(&mut ctrl, TestState::ParamExchange).await?;
        let mut params = protocol::recv_params(&mut ctrl).await?;
        // iperf3 sends num/blockcount = 0 for a plain `-t` run; treat 0 as
        // unlimited so the byte-limit checks below don't misread a duration test
        // as byte-limited (#119).
        params.normalize_unlimited();
        // Malformed negotiated params (e.g. len 0 < l < 16 on UDP, negative,
        // or oversized) refuse the test outright (#188). No SERVER_ERROR
        // state: iperf3's client follows that with an errno exchange we don't
        // speak, and real iperf3 clients never send these values — dropping
        // the control connection is the safe shape for a hostile peer.
        let cfg = TestConfig::from_params(&params)?;
        // --get-server-output (#33): when the client asks and this server is
        // in text mode, TEE the console report into the exchange buffer
        // (iperf3's iperf_printf dual-write — the console stays live).
        // JSON-mode servers attach their full report instead (built
        // pre-exchange below).
        let want_server_output = params.get_server_output == Some(1);
        // --json-stream x get-server-output divergences are tracked in #168
        // (iperf3's streaming server DOES attach; its streaming client emits a
        // server_output event).
        let capture = (want_server_output && !self.json_output && !self.json_stream)
            .then(crate::macros::OutputCaptureGuard::start);

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
                cfg.protocol,
                cfg.num_streams,
                cfg.blksize,
                cfg.duration
            );
        }

        // ---- CreateStreams ----
        let done = Arc::new(AtomicBool::new(false));
        // Signal `done` on every exit path (incl. early `?` returns) so a UDP
        // sender parked on the start barrier can't leak if setup fails (#5).
        let _done_guard = stream::DoneOnDrop(done.clone());
        // Released at TestStart so UDP senders don't transmit during stream
        // setup (issue #5): the create-streams handshake is lost under a flood.
        let start = Arc::new(AtomicBool::new(false));
        let mut streams: Vec<DataStream> = Vec::new();

        // Determine how many streams to accept and their roles.
        // Normal: server receives. Reverse: server sends. Bidir: both.
        let recv_count = if cfg.reverse && !cfg.bidir {
            0
        } else {
            cfg.num_streams
        };
        let send_count = if cfg.reverse || cfg.bidir {
            cfg.num_streams
        } else {
            0
        };
        let total = recv_count + send_count;

        // `-n`/`-k` shared byte budget for the server's sending streams (reverse /
        // bidir): they collectively stop at ~N bytes so the server self-limits at
        // the negotiated total instead of free-running to the client's TestEnd —
        // the byte-limit overshoot fix, mirroring the client.
        // Only the server's TCP senders (reverse/bidir) consume the budget, so
        // build it only for a TCP run that has senders. See `make_byte_budget`
        // for the 0-is-unlimited (iperf3 sends `num`/`blocks` = 0 for a plain
        // `-t` run) and overflow-clamp rules.
        // -O + -n/-k on the SERVER's senders (reverse/bidir, #31 review r2):
        // same pause-at-limit + reporter-boundary-refill design as the client.
        let byte_budget: Option<Arc<AtomicI64>> = (matches!(cfg.protocol, TransportProtocol::Tcp)
            && send_count > 0)
            .then(|| stream::make_byte_budget(params.num, params.blockcount, cfg.blksize))
            .flatten();
        // The boundary-refill target, captured BEFORE any sender can consume:
        // loading it at reporter-spawn time read `N − early_consumed` on fast
        // links (senders start in the TestStart→spawn gap), silently
        // shrinking the refill (review r4).
        let budget_target = byte_budget.as_ref().map(|b| b.load(Ordering::Relaxed));

        // Single-socket UDP server demux (#80): one demux receiver thread serves
        // every receiving stream, so its handle lives outside the per-stream
        // `DataStream`s and is joined alongside them at teardown. `None` on the
        // recycling path and on pure-reverse demux tests (no receivers).
        let mut udp_demux_handle: Option<tokio::task::JoinHandle<Result<()>>> = None;

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
                    if cfg.tos != 0 {
                        // Fatal like every other set_tos site (#45): iperf3's
                        // iperf_common_sockopts errors (IESETTOS) when IP_TOS
                        // can't be applied, on both roles and both protocols.
                        net::set_tos(&data_stream, cfg.tos as u32)?;
                    }

                    let stream_id = iperf3_stream_id(i);
                    let is_sender = i >= recv_count;
                    let counters = Arc::new(StreamCounters::new());
                    #[cfg(unix)]
                    let raw_fd = {
                        use std::os::unix::io::AsRawFd;
                        Some(data_stream.as_raw_fd())
                    };
                    #[cfg(not(unix))]
                    let raw_fd: Option<i32> = None;
                    let fp = self.file.as_ref().map(std::path::PathBuf::from);

                    // Real socket addresses + kernel buffer sizes for the server's
                    // `-J` `connected[]` / `sndbuf_actual` / `rcvbuf_actual` (#50),
                    // captured before the stream moves into its task.
                    let local_addr = data_stream.local_addr().ok();
                    let peer_addr_s = data_stream.peer_addr().ok();
                    let sock = socket2::SockRef::from(&data_stream);
                    let sndbuf_actual = sock.send_buffer_size().ok().map(|v| v as u64);
                    let rcvbuf_actual = sock.recv_buffer_size().ok().map(|v| v as u64);
                    // #97: abort if the kernel clamped -w below the request (iperf3 IESETBUF2).
                    net::check_socket_window(cfg.window, sndbuf_actual, rcvbuf_actual)?;
                    // #37: congestion algorithm actually in effect on this stream.
                    let congestion_used = net::tcp_congestion_used(&data_stream);

                    let task = if is_sender {
                        let buf = make_send_buffer(cfg.blksize, false);
                        let c = counters.clone();
                        let d = done.clone();
                        // `-b` paces the sender in reverse/bidir too (negotiated
                        // rate; 0 = unlimited), on the client's pacing-timer
                        // quantum. #102/#32
                        let rate = cfg.bandwidth;
                        let pt = cfg.pacing_timer;
                        let bu = cfg.burst;
                        let bb = byte_budget.clone();
                        tokio::spawn(async move {
                            stream::run_tcp_sender(data_stream, c, buf, d, fp, rate, pt, bu, bb)
                                .await
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
                        raw_fd,
                        local_addr,
                        peer_addr: peer_addr_s,
                        sndbuf_actual,
                        rcvbuf_actual,
                        congestion_used,
                    });
                }
            }
            TransportProtocol::Udp => {
                // Max send duration the server's UDP senders self-enforce
                // (issue #5): in bidir/reverse the server sends too, and at a
                // high `-b` those CPU-bound senders can starve this side's
                // runtime so it never processes the client's TestEnd. Only in
                // duration mode; byte/block-limited tests stop on `done`.
                let max_duration = (params.num.is_none() && params.blockcount.is_none())
                    .then(|| std::time::Duration::from_secs((cfg.duration + cfg.omit) as u64));

                // Two server UDP designs, same wire protocol. The default Unix
                // path mirrors iperf3: one connected data socket per stream, all
                // sharing the port via SO_REUSEADDR, with the kernel demuxing by
                // 4-tuple. Native winsock silently drops a new source's datagram
                // when a connected and a wildcard socket share a port, so that
                // design hangs `-P > 1` setup on Windows (#80). The demux path
                // binds ONE unconnected socket and routes by source address in
                // userspace — correct on every platform.
                //
                // Default: demux on Windows (recycling cannot work there),
                // recycling on Unix (faithful to iperf3, kernel-parallel
                // receive). RIPERF3_UDP_SERVER_DEMUX overrides either way —
                // `0`/`false`/`no`/empty force recycling, any other value forces
                // demux — so both paths are exercisable on one build (the Windows
                // red→green sets it to `0` to reproduce the old hang).
                let udp_use_demux = match std::env::var("RIPERF3_UDP_SERVER_DEMUX") {
                    Ok(v) => !matches!(v.as_str(), "" | "0" | "false" | "no"),
                    Err(_) => cfg!(windows),
                };

                if udp_use_demux {
                    self.setup_udp_demux_streams(
                        &mut ctrl,
                        &cfg,
                        recv_count,
                        total,
                        max_duration,
                        &done,
                        &start,
                        &mut streams,
                        &mut udp_demux_handle,
                    )
                    .await?;
                } else {
                    self.setup_udp_recycling_streams(
                        &mut ctrl,
                        &cfg,
                        recv_count,
                        total,
                        max_duration,
                        &done,
                        &start,
                        &mut streams,
                    )
                    .await?;
                }
            }
        }

        // ---- TestStart / TestRunning ----
        // All streams are set up — release the UDP senders.
        start.store(true, Ordering::Relaxed);
        protocol::send_state(&mut ctrl, TestState::TestStart).await?;
        let cpu_start = CpuSnapshot::now();
        // Wall-clock at TestStart, for the `-J` start.timestamp (#50).
        let test_start_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        protocol::send_state(&mut ctrl, TestState::TestRunning).await?;

        // For plain -J the reporter runs silently to collect intervals for the
        // final blob; for text or --json-stream it prints/streams live, matching
        // the client's gating (#50).
        let print_intervals = !json || self.json_stream;
        let collect_intervals = json && !self.json_stream;
        // Like the client: `--json-stream` streams intervals live but still needs
        // the per-stream TCP_INFO extremes handed back for the `end` event (#62).
        let want_collector = collect_intervals || self.json_stream;
        let interval_data = Arc::new(Mutex::new(crate::reporter::CollectedIntervals::default()));

        // --json-stream: emit the `start` event now — before the reporter is
        // spawned, so it is guaranteed to precede every `interval` event (#62).
        if self.json_stream {
            self.emit_json_stream_start(
                &streams,
                &cfg,
                &params,
                &cookie,
                &accepted_host,
                accepted_port,
                test_start_millis,
                &interval_data,
            );
        }

        // Spawn interval reporter (server uses 1.0s default). `report_start` is
        // captured right before the spawn so its elapsed at TEST_END is the
        // authoritative final-interval boundary handed to the reporter (#55).
        let reporter_end = Arc::new(crate::reporter::ReporterEnd::new());
        let report_start = std::time::Instant::now();
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
                    json_stream: self.json_stream,
                    print: print_intervals,
                    blksize: cfg.blksize,
                },
                stream_refs,
                done.clone(),
                reporter_end.clone(),
                want_collector.then(|| interval_data.clone()),
                byte_budget.clone().zip(budget_target),
                // The server has no -n/-k end-check driver — the client ends
                // the test — so it never waits on the boundary signal.
                None,
            )
        };

        // ---- Wait for TEST_END (with optional max duration and bitrate limit) ----
        let bitrate_limit = self.server_bitrate_limit;
        let test_start = report_start;
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
        // Hand the reporter the authoritative end time, then stop the streams
        // (`done`) so no received bytes leak past the deadline into the final
        // interval (#55). The reporter prioritises `finish` over `done`, so the
        // final interval still flushes; wait for it before tearing streams down.
        let measured_elapsed = report_start.elapsed().as_secs_f64();
        // The reporter's timeline restarted at the omit boundary (#31), so its
        // authoritative end time is post-omit; clamp for runs that died inside
        // the warm-up.
        reporter_end.finish((measured_elapsed - cfg.omit as f64).max(0.0));
        done.store(true, Ordering::Relaxed);
        if let Some(handle) = interval_handle {
            let _ = handle.await;
        }
        let cpu_end = CpuSnapshot::now();

        // Wait briefly then join tasks (senders may be blocked on write)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut result_streams = Vec::new();
        // Summary window + bitrate: the measured elapsed for a byte/block-limited
        // run, exactly `-t` otherwise (#103, mirrors the client). The requested
        // `-t` is reported separately as the test_start `duration` parameter.
        let test_duration = if params.num.is_some() || params.blockcount.is_some() {
            // Rebase to the post-omit window (#31): the measured elapsed
            // includes the warm-up the summary must exclude.
            (measured_elapsed - cfg.omit as f64).max(0.0)
        } else {
            cfg.duration as f64
        };

        for s in &streams {
            // Net (post-omit) bytes; packets/errors stay GROSS with the
            // omitted_* baselines alongside, like iperf3's exchange (#31).
            let bytes = if s.is_sender {
                s.counters.bytes_sent_net()
            } else {
                s.counters.bytes_received_net()
            };

            let (jitter, errors, packets, omitted_errors, omitted_packets) =
                if let Some(ref udp_stats) = s.udp_recv_stats {
                    if let Ok(st) = udp_stats.lock() {
                        (
                            st.jitter,
                            st.cnt_error,
                            st.packet_count,
                            st.omitted_cnt_error,
                            st.omitted_packet_count,
                        )
                    } else {
                        (0.0, 0, 0, 0, 0)
                    }
                } else if s.is_sender && matches!(cfg.protocol, TransportProtocol::Udp) {
                    // iperf3's UDP sender counts every datagram it sends
                    // (iperf_udp.c `++sp->packet_count`) and exchanges that
                    // count unconditionally (iperf_api.c `"packets"`); the
                    // peer's sender line renders it. Fill the equivalent from
                    // sent bytes, keeping the gross+baseline convention (#184
                    // — a zero here made an iperf3 client print `0/0` for a
                    // riperf3 server's reverse stream).
                    let blk = cfg.blksize.max(1) as u64;
                    let gross = (s.counters.bytes_sent() / blk) as i64;
                    let net = (bytes / blk) as i64;
                    (0.0, 0, gross, 0, gross - net)
                } else {
                    (0.0, 0, 0, 0, 0)
                };

            let is_udp_stream = matches!(cfg.protocol, TransportProtocol::Udp);
            let retransmits = s.sender_retransmits(is_udp_stream).unwrap_or(-1);

            result_streams.push(protocol::StreamResultJson {
                id: s.id,
                bytes,
                retransmits,
                jitter,
                errors,
                omitted_errors,
                packets,
                omitted_packets,
                start_time: 0.0,
                end_time: test_duration,
            });
        }

        // ---- ExchangeResults ----
        let cpu_util = cpu_end.utilization_since(&cpu_start);
        // --get-server-output (#33): finish the diverted text (render the final
        // summaries into the capture first, so the client sees the complete
        // report), or attach the full -J report for a JSON-mode server.
        let mut prebuilt_report: Option<crate::json_report::Report> = None;
        let (server_output_text, server_output_json) = if let Some(capture) = capture {
            let summaries = Self::text_summaries(&streams, test_duration, &cfg);
            let with_retr = summaries.iter().any(|s| s.retransmits.is_some());
            crate::reporter::print_separator();
            crate::reporter::print_final_header(cfg.protocol, cfg.bidir, with_retr);
            crate::reporter::print_final_summaries(&summaries, 'a');
            (Some(capture.take()), None)
        } else if want_server_output && self.json_output {
            let report = self.build_report(
                &streams,
                &cfg,
                &params,
                &cpu_util,
                test_duration,
                &cookie,
                &accepted_host,
                accepted_port,
                test_start_millis,
                &interval_data,
            );
            let value = serde_json::to_value(&report).ok();
            prebuilt_report = Some(report);
            (None, value)
        } else {
            (None, None)
        };
        let was_captured = server_output_text.is_some();
        let server_results = TestResultsJson {
            cpu_util_total: cpu_util.host_total,
            cpu_util_user: cpu_util.host_user,
            cpu_util_system: cpu_util.host_system,
            // #156: 1 when this side is a retransmit-capable TCP sender
            // (reverse/bidir), like iperf3's check_sender_has_retransmits.
            sender_has_retransmits: if streams.iter().any(|s| s.is_sender) {
                i64::from(
                    matches!(cfg.protocol, TransportProtocol::Tcp)
                        && crate::tcp_info::has_retransmit_info(),
                )
            } else {
                -1
            },
            // #37: the congestion algorithm actually in effect (read back at stream
            // creation); None for UDP / unsupported platforms.
            congestion_used: streams.first().and_then(|s| s.congestion_used.clone()),
            server_output_text,
            server_output_json,
            streams: result_streams,
        };

        protocol::send_state(&mut ctrl, TestState::ExchangeResults).await?;
        // iperf3 protocol: server reads client results first, then sends its own.
        // The client's results are not used in the server's own report — iperf3's
        // server reports only its own measured bytes and a 0 remote CPU (#50).
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

        if self.json_output {
            // Emit the iperf3-schema JSON report on stdout (#50); reuse the
            // pre-exchange build when --get-server-output attached it (#33).
            let report = prebuilt_report.unwrap_or_else(|| {
                self.build_report(
                    &streams,
                    &cfg,
                    &params,
                    &cpu_util,
                    test_duration,
                    &cookie,
                    &accepted_host,
                    accepted_port,
                    test_start_millis,
                    &interval_data,
                )
            });
            self.print_results_json(&report);
        } else if self.json_stream {
            // --json-stream: emit the `end` event (intervals already streamed; #62).
            self.emit_json_stream_end(
                &streams,
                &cfg,
                &params,
                &cpu_util,
                test_duration,
                &cookie,
                &accepted_host,
                accepted_port,
                test_start_millis,
                &interval_data,
            );
        } else if !was_captured {
            // Print summary: per-stream lines plus aggregate [SUM] row(s) for
            // parallel streams (issue #4), via the shared path the client uses.
            // Skipped when --get-server-output already rendered them (#33):
            // the pre-exchange render TEE'd to console + capture (iperf3 also
            // prints at TEST_END, before its exchange), so printing here again
            // would duplicate the lines.
            let summaries = Self::text_summaries(&streams, test_duration, &cfg);
            let with_retr = summaries.iter().any(|s| s.retransmits.is_some());
            crate::reporter::print_separator();
            crate::reporter::print_final_header(cfg.protocol, cfg.bidir, with_retr);
            crate::reporter::print_final_summaries(&summaries, 'a');
        }

        // Join stream tasks (best-effort, they should be done)
        for s in streams {
            let _ = s.task.await;
        }
        // The single-socket UDP demux receiver (#80) serves all receiving streams
        // and lives outside `streams`; join it too. `None` on the recycling path.
        if let Some(h) = udp_demux_handle {
            let _ = h.await;
        }

        Ok(())
    }

    /// iperf3's UDP server design (the default on Unix): one connected data
    /// socket per stream, recycled on the same port via SO_REUSEADDR, with the
    /// kernel demultiplexing incoming datagrams by 4-tuple. For each stream:
    /// accept the connect handshake on the listener (which locks that socket to
    /// the client), then bind a fresh listener on the same port for the next
    /// stream. This is faithful to iperf3 and gives kernel-parallel receive, but
    /// relies on kernel demux that native winsock doesn't provide (see
    /// [`Self::setup_udp_demux_streams`] / #80).
    #[allow(clippy::too_many_arguments)]
    async fn setup_udp_recycling_streams(
        &self,
        ctrl: &mut tokio::net::TcpStream,
        cfg: &TestConfig,
        recv_count: u32,
        total: u32,
        max_duration: Option<std::time::Duration>,
        done: &Arc<AtomicBool>,
        start: &Arc<AtomicBool>,
        streams: &mut Vec<DataStream>,
    ) -> Result<()> {
        let mut udp_listener =
            net::udp_bind_reusable(self.bind_address.as_deref(), self.port, self.ip_version)
                .await?;

        protocol::send_state(ctrl, TestState::CreateStreams).await?;

        // #178: each stream's spawn_blocking data thread is spawned through
        // the gate; the barrier below holds TestStart until the data plane
        // actually exists.
        let mut thread_gate = stream::StreamThreadGate::new();
        for i in 0..total {
            // Accept: recv magic, connect() to client, send reply.
            // Bounded so a client that never connects fails the test
            // instead of hanging setup forever (#11); uses the same
            // budget as the client's handshake so neither side aborts
            // while the other is still retrying.
            let _client_addr =
                protocol::udp_connect_server(&udp_listener, protocol::UDP_CONNECT_TOTAL_TIMEOUT)
                    .await?;
            // The listener is now locked to this client — use it as the data socket
            let data_sock = udp_listener;

            // Create a fresh listener for the next stream (if any)
            if i + 1 < total {
                udp_listener = net::udp_bind_reusable(
                    self.bind_address.as_deref(),
                    self.port,
                    self.ip_version,
                )
                .await?;
            } else {
                // Last stream — create a dummy that won't be used
                udp_listener = net::udp_bind(None, 0, false).await?;
            }

            let stream_id = iperf3_stream_id(i);
            let is_sender = i >= recv_count;
            let counters = Arc::new(StreamCounters::new());

            // Socket addresses + buffer sizes for the `-J` blob (#50),
            // captured before the socket is converted to std + moved.
            let local_addr = data_sock.local_addr().ok();
            let peer_addr_s = data_sock.peer_addr().ok();
            let (sndbuf_actual, rcvbuf_actual) = {
                let sock = socket2::SockRef::from(&data_sock);
                // Honor -w/--window on the server's UDP data socket too
                // (#59) so reverse/bidir UDP matches iperf3; set before the
                // read-back below.
                net::apply_socket_window(&sock, cfg.window);
                (
                    sock.send_buffer_size().ok().map(|v| v as u64),
                    sock.recv_buffer_size().ok().map(|v| v as u64),
                )
            };
            // #97: abort if -w was clamped below the request (iperf3 IESETBUF2).
            net::check_socket_window(cfg.window, sndbuf_actual, rcvbuf_actual)?;

            let std_sock = data_sock.into_std().map_err(RiperfError::Io)?;

            if is_sender {
                let c = counters.clone();
                let d = done.clone();
                let bs = cfg.blksize;
                // Already resolved in TestConfig (#17); 0 = unlimited.
                let rate = cfg.bandwidth;
                let pt = cfg.pacing_timer; // #185: pace the UDP batch too
                let u64bit = cfg.udp_counters_64bit;
                let bu = cfg.burst;
                let st = start.clone();
                let md = max_duration;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_sender_blocking(
                        std_sock, c, bs, d, rate, pt, bu, u64bit, st, md,
                    )
                });
                streams.push(DataStream {
                    id: stream_id,
                    is_sender,
                    counters,
                    udp_recv_stats: None,
                    task,
                    raw_fd: None,
                    local_addr,
                    peer_addr: peer_addr_s,
                    sndbuf_actual,
                    rcvbuf_actual,
                    congestion_used: None,
                });
            } else {
                let c = counters.clone();
                let d = done.clone();
                let bs = cfg.blksize;
                let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                let stats_clone = stats.clone();
                let u64bit = cfg.udp_counters_64bit;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_receiver_blocking(std_sock, c, stats_clone, bs, d, u64bit)
                });
                streams.push(DataStream {
                    id: stream_id,
                    is_sender,
                    counters,
                    udp_recv_stats: Some(stats),
                    task,
                    raw_fd: None,
                    local_addr,
                    peer_addr: peer_addr_s,
                    sndbuf_actual,
                    rcvbuf_actual,
                    congestion_used: None,
                });
            }
        }
        // #178: hold TestStart (sent by the caller right after this returns)
        // until every data thread is running — the test clock must not outrun
        // OS-thread creation, which stalls for seconds on loaded hosts. On
        // timeout proceed anyway (degraded = pre-fix behavior).
        thread_gate.wait(stream::STREAM_THREAD_START_TIMEOUT).await;
        Ok(())
    }

    /// Single-socket UDP server demux (#80; default on Windows). Bind ONE
    /// unconnected socket for the whole test and demultiplex streams by client
    /// source address in userspace, instead of one connected socket per stream.
    /// Native winsock silently drops a new source's datagram when a connected and
    /// a wildcard UDP socket share a port, which hangs `-P > 1` setup under the
    /// recycling design; one unconnected socket never forms that pair, so this is
    /// correct everywhere. The wire protocol is identical, so it interoperates
    /// with an iperf3 client.
    ///
    /// Slots are assigned in connect-arrival order, matching the recycling path's
    /// positional client/server stream pairing (the client creates streams
    /// sequentially by index). One demux thread serves every receiving stream;
    /// each sending stream `send_to`s its own client over the shared socket.
    #[allow(clippy::too_many_arguments)]
    async fn setup_udp_demux_streams(
        &self,
        ctrl: &mut tokio::net::TcpStream,
        cfg: &TestConfig,
        recv_count: u32,
        total: u32,
        max_duration: Option<std::time::Duration>,
        done: &Arc<AtomicBool>,
        start: &Arc<AtomicBool>,
        streams: &mut Vec<DataStream>,
        demux_handle: &mut Option<tokio::task::JoinHandle<Result<()>>>,
    ) -> Result<()> {
        use std::collections::{HashMap, HashSet};
        use std::net::SocketAddr;

        // One unconnected dual-stack socket for the whole test. Never connect()'d
        // and never sharing its port with a second socket, so there is no
        // connected+wildcard pair for winsock to drop a new source against (#80).
        let udp_sock =
            net::udp_bind_reusable(self.bind_address.as_deref(), self.port, self.ip_version)
                .await?;

        protocol::send_state(ctrl, TestState::CreateStreams).await?;

        // Accept the connect handshake from every client stream on the one
        // socket. Record the source address of each NEW client in arrival order
        // (slot i == stream i). Reply to every valid magic — including a
        // retransmit from an already-seen client whose reply was lost — but only
        // a new source claims a slot. Bounded by the same budget as the client
        // handshake so a client that never connects fails setup instead of
        // hanging (#11).
        let mut client_addrs: Vec<SocketAddr> = Vec::with_capacity(total as usize);
        let mut seen: HashSet<SocketAddr> = HashSet::new();
        let mut magic_buf = [0u8; 65536];
        // Each *new* stream gets a fresh budget — matching the recycling path,
        // which calls udp_connect_server(UDP_CONNECT_TOTAL_TIMEOUT) once per
        // stream, and the client, which retries each stream's connect for that
        // long independently. A single aggregate deadline would abort setup while
        // the client is still legitimately handshaking a later stream.
        let mut deadline = tokio::time::Instant::now() + protocol::UDP_CONNECT_TOTAL_TIMEOUT;
        while client_addrs.len() < total as usize {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(RiperfError::Aborted(
                    "timed out waiting for UDP stream connect".into(),
                ));
            }
            let (n, src) =
                match tokio::time::timeout(remaining, udp_sock.recv_from(&mut magic_buf)).await {
                    Ok(r) => r?,
                    Err(_) => {
                        return Err(RiperfError::Aborted(
                            "timed out waiting for UDP stream connect".into(),
                        ))
                    }
                };
            // Drop anything that isn't the connect magic (too short, or a stray
            // datagram) and keep waiting — matches udp_connect_server's check.
            if n < 4 {
                continue;
            }
            let msg = u32::from_ne_bytes(magic_buf[..4].try_into().unwrap());
            if msg != protocol::UDP_CONNECT_MSG {
                continue;
            }
            udp_sock
                .send_to(&protocol::UDP_CONNECT_REPLY.to_ne_bytes(), src)
                .await?;
            if seen.insert(src) {
                client_addrs.push(src);
                // Fresh per-stream budget for the next stream's handshake.
                deadline = tokio::time::Instant::now() + protocol::UDP_CONNECT_TOTAL_TIMEOUT;
            }
        }

        // Move to a blocking std socket for the data phase and share it across the
        // demux receiver thread and every sender thread. Set blocking here before
        // wrapping in `Arc` so no thread observes a nonblocking socket. (The
        // receiver thread and the senders' `configure_udp_sender` redundantly set
        // it again, but to the same value, so the result is deterministic.)
        let udp_std = udp_sock.into_std().map_err(RiperfError::Io)?;
        udp_std.set_nonblocking(false).map_err(RiperfError::Io)?;

        // `-J` metadata: the shared socket is the same for every stream, so the
        // buffer sizes are read once; honor -w/--window on it once too.
        let (sndbuf_actual, rcvbuf_actual) = {
            let sock = socket2::SockRef::from(&udp_std);
            net::apply_socket_window(&sock, cfg.window);
            // The recycling path gives each receiving stream its OWN socket (and
            // its own SO_RCVBUF), drained by its own thread. This path funnels
            // every receiving stream through ONE socket drained by a single
            // thread, so size its receive buffer to the aggregate the recycling
            // path spreads across `recv_count` sockets. Otherwise riperf3's
            // batched sender (32-packet bursts per stream) overflows the lone
            // default buffer and inflates measured UDP loss many-fold (#80 review).
            if recv_count > 1 {
                if let Ok(per_stream) = sock.recv_buffer_size() {
                    let _ =
                        sock.set_recv_buffer_size(per_stream.saturating_mul(recv_count as usize));
                }
            }
            (
                sock.send_buffer_size().ok().map(|v| v as u64),
                sock.recv_buffer_size().ok().map(|v| v as u64),
            )
        };
        // #97: abort if -w was clamped below the request (iperf3 IESETBUF2). On the
        // single-socket demux path the recv buffer is sized to the aggregate, but a
        // genuine wmem/rmem_max clamp still drives the readback below the request.
        net::check_socket_window(cfg.window, sndbuf_actual, rcvbuf_actual)?;
        // The connected recycling path reports each stream's local_host as the
        // kernel-selected source IP for that client. The demux socket is never
        // connect()'d, so its own local_addr is the wildcard bind — only on a
        // wildcard bind (no -B) do we probe the route per client to reproduce the
        // recycling/iperf3 local_host; with an explicit -B the bound IP is already
        // right for every stream. The port is always the shared socket's.
        let shared_local_addr = udp_std.local_addr().ok();
        let server_port = shared_local_addr.map_or(self.port, |a| a.port());
        let bound_wildcard = shared_local_addr.is_none_or(|a| a.ip().is_unspecified());
        let shared = Arc::new(udp_std);

        // Build the per-stream entries: senders get their own send_to task;
        // receivers register a route and share the single demux thread.
        let mut routes: HashMap<SocketAddr, stream::UdpDemuxRoute> = HashMap::new();
        let bs = cfg.blksize;
        let rate = cfg.bandwidth;
        let pt = cfg.pacing_timer; // #185: pace the UDP batch too
        let u64bit = cfg.udp_counters_64bit;
        // #178: every spawn_blocking data thread (each sender + the one demux
        // receiver) is spawned through the gate; the barrier below holds
        // TestStart until the data plane actually exists.
        let mut thread_gate = stream::StreamThreadGate::new();
        for i in 0..total {
            let stream_id = iperf3_stream_id(i);
            let is_sender = i >= recv_count;
            let client_addr = client_addrs[i as usize];
            let counters = Arc::new(StreamCounters::new());
            let local_addr = if bound_wildcard {
                demux_local_addr_for(client_addr, server_port).or(shared_local_addr)
            } else {
                shared_local_addr
            };

            if is_sender {
                let s = shared.clone();
                let c = counters.clone();
                let d = done.clone();
                let bu = cfg.burst;
                let st = start.clone();
                let md = max_duration;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_server_demux_sender(
                        s,
                        client_addr,
                        c,
                        bs,
                        d,
                        rate,
                        pt,
                        bu,
                        u64bit,
                        st,
                        md,
                    )
                });
                streams.push(DataStream {
                    id: stream_id,
                    is_sender,
                    counters,
                    udp_recv_stats: None,
                    task,
                    raw_fd: None,
                    local_addr,
                    peer_addr: Some(client_addr),
                    sndbuf_actual,
                    rcvbuf_actual,
                    congestion_used: None,
                });
            } else {
                let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                routes.insert(
                    client_addr,
                    stream::UdpDemuxRoute {
                        counters: counters.clone(),
                        stats: stats.clone(),
                    },
                );
                // Receiving streams are served by the one demux thread spawned
                // below; give each a resolved placeholder task so the per-stream
                // join at teardown stays uniform. The real handle is `demux_handle`.
                let task = tokio::spawn(async { Ok::<(), RiperfError>(()) });
                streams.push(DataStream {
                    id: stream_id,
                    is_sender,
                    counters,
                    udp_recv_stats: Some(stats),
                    task,
                    raw_fd: None,
                    local_addr,
                    peer_addr: Some(client_addr),
                    sndbuf_actual,
                    rcvbuf_actual,
                    congestion_used: None,
                });
            }
        }

        // Spawn the single demux receiver over the shared socket (only if there
        // is anything to receive — a pure-reverse test has senders only).
        if !routes.is_empty() {
            let s = shared.clone();
            let d = done.clone();
            *demux_handle = Some(
                thread_gate
                    .spawn(move || stream::run_udp_server_demux_receiver(s, routes, d, u64bit)),
            );
        }
        // #178: hold TestStart (sent by the caller right after this returns)
        // until every data thread is running — the test clock must not outrun
        // OS-thread creation, which stalls for seconds on loaded hosts. On
        // timeout proceed anyway (degraded = pre-fix behavior).
        thread_gate.wait(stream::STREAM_THREAD_START_TIMEOUT).await;
        Ok(())
    }

    /// Per-stream final summaries for the text report (shared by the normal
    /// print path and the --get-server-output pre-exchange render, #33).
    fn text_summaries(
        streams: &[DataStream],
        test_duration: f64,
        cfg: &TestConfig,
    ) -> Vec<crate::reporter::StreamSummary> {
        let is_udp = matches!(cfg.protocol, TransportProtocol::Udp);
        streams
            .iter()
            .map(|s| {
                let bytes = if s.is_sender {
                    s.counters.bytes_sent_net()
                } else {
                    s.counters.bytes_received_net()
                };

                let (jitter, lost, total) = if let Some(ref udp_stats) = s.udp_recv_stats {
                    udp_stats
                        .lock()
                        .map(|st| {
                            // Post-omit stats (#31, review r2 — this was the
                            // missed third site): gross minus the boundary
                            // baselines.
                            (
                                Some(st.jitter),
                                Some(st.cnt_error - st.omitted_cnt_error),
                                Some(st.packet_count - st.omitted_packet_count),
                            )
                        })
                        .unwrap_or((None, None, None))
                } else if is_udp && s.is_sender {
                    // iperf3's sender line shows zero jitter/loss over the
                    // sent datagram count, not blank columns (#184).
                    (
                        Some(0.0),
                        Some(0),
                        Some((bytes / cfg.blksize.max(1) as u64) as i64),
                    )
                } else {
                    (None, None, None)
                };

                crate::reporter::StreamSummary {
                    stream_id: s.id,
                    start: 0.0,
                    end: test_duration,
                    bytes,
                    is_sender: s.is_sender,
                    // TCP sender lines carry the retransmit total (#184).
                    retransmits: s.sender_retransmits(is_udp),
                    jitter,
                    lost,
                    total_packets: total,
                    // Bidir tags every line with the stream's direction (#184).
                    role_tag: cfg
                        .bidir
                        .then_some(if s.is_sender { "TX-S" } else { "RX-S" }),
                }
            })
            .collect()
    }

    /// Assemble the server's typed iperf3-schema report input (#50). Shared by
    /// `-J` (build + pretty-print, see `build_report`/`print_results_json`),
    /// `--json-stream` (the `start`/`end` events), and `--get-server-output`'s
    /// pre-exchange attachment (#33). The server's perspective is baked in via
    /// `is_server: true`: `accepted_connection` instead of `connecting_to`, no
    /// peer-byte graft (the un-measured side is 0), and `tcp_mss_default` of 0
    /// (iperf3's server never reads its control-socket MSS).
    #[allow(clippy::too_many_arguments)]
    fn build_report_input(
        &self,
        streams: &[DataStream],
        cfg: &TestConfig,
        params: &TestParams,
        cpu_util: &crate::cpu::CpuUtilization,
        test_duration: f64,
        cookie: &[u8; protocol::COOKIE_SIZE],
        accepted_host: &str,
        accepted_port: u16,
        start_time_millis: u64,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
    ) -> crate::json_report::ReportInput {
        use crate::json_report::{
            CpuUtilization, ReportInput, StreamReport, TcpEndExtras, UdpStreamStats,
        };

        // Take the interval samples + per-stream TCP_INFO extremes the reporter
        // collected (its task is joined by now, so this is final).
        let (collected_intervals, extremes) = match interval_data.lock() {
            Ok(mut g) => (
                std::mem::take(&mut g.intervals),
                std::mem::take(&mut g.extremes),
            ),
            Err(_) => (Vec::new(), Vec::new()),
        };

        let is_udp = matches!(cfg.protocol, TransportProtocol::Udp);

        let stream_reports: Vec<StreamReport> = streams
            .iter()
            .map(|s| {
                let local_bytes = if s.is_sender {
                    s.counters.bytes_sent_net()
                } else {
                    s.counters.bytes_received_net()
                };

                // The server measures UDP loss/jitter on the streams it
                // receives — post-omit stats (#31): gross minus baselines.
                let udp = s.udp_recv_stats.as_ref().and_then(|lock| {
                    lock.lock().ok().map(|st| UdpStreamStats {
                        jitter_secs: st.jitter,
                        lost_packets: st.cnt_error - st.omitted_cnt_error,
                        packets: st.packet_count - st.omitted_packet_count,
                        out_of_order: st.outoforder_packets - st.omitted_outoforder_packets,
                    })
                });

                // to_canonical(): unwrap IPv4-mapped IPv6 from the dual-stack
                // listener to plain IPv4 in connected[], matching iperf3.
                let (local_host, local_port) = s
                    .local_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_default();
                let (remote_host, remote_port) = s
                    .peer_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_default();

                // Sender-side TCP_INFO extremes + retransmit total, present only
                // for streams the server sent (reverse / bidir).
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
                // Retransmits are a sender-side metric. The server only sends on
                // reverse/bidir streams; a stream it received has no retransmit
                // count (None → omitted), so it can't leak a 0 into sum_sent on a
                // forward test (where iperf3's server emits no retransmits).
                let retransmits = if is_udp || !s.is_sender {
                    None
                } else {
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
                    // The server never learns the peer's per-stream bytes; build()
                    // zeroes the un-measured side for is_server reports.
                    remote_bytes: None,
                    retransmits,
                    tcp_end,
                    udp,
                }
            })
            .collect();

        let input = ReportInput {
            protocol: cfg.protocol,
            reverse: cfg.reverse,
            bidir: cfg.bidir,
            duration: cfg.duration as f64,
            elapsed: test_duration,
            num_streams: cfg.num_streams as i32,
            blksize: cfg.blksize as i64,
            omit: cfg.omit as i32,
            tos: cfg.tos,
            target_bitrate: cfg.bandwidth,
            bytes: params.num.unwrap_or(0),
            blocks: params.blockcount.unwrap_or(0),
            connecting_host: String::new(),
            connecting_port: 0,
            is_server: true,
            accepted_host: accepted_host.to_string(),
            accepted_port,
            version: format!("riperf3 {}", env!("CARGO_PKG_VERSION")),
            system_info: crate::utils::system_info(),
            cpu: CpuUtilization {
                // Only the server's own CPU. iperf3's server reports the remote
                // (client) CPU as 0 — it doesn't surface the client's figure even
                // though the client sends it — so match that rather than graft it.
                host_total: cpu_util.host_total,
                host_user: cpu_util.host_user,
                host_system: cpu_util.host_system,
                remote_total: 0.0,
                remote_user: 0.0,
                remote_system: 0.0,
            },
            // #37: the congestion algorithm actually in effect on the server's data
            // socket, read back via getsockopt(TCP_CONGESTION). None for UDP /
            // unsupported platforms.
            congestion_used: streams.first().and_then(|s| s.congestion_used.clone()),
            cookie: String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1]).to_string(),
            // iperf3's server emits tcp_mss_default = 0 (it never reads the control
            // socket MSS); the requested -M (via params) still surfaces as tcp_mss.
            tcp_mss_default: 0,
            mss: cfg.mss.filter(|&m| m > 0).map(|m| m as u32),
            fq_rate: params.fqrate.unwrap_or(0),
            sock_bufsize: cfg.window.map(|w| w.max(0) as u64).unwrap_or(0),
            sndbuf_actual: streams.first().and_then(|s| s.sndbuf_actual).unwrap_or(0),
            rcvbuf_actual: streams.first().and_then(|s| s.rcvbuf_actual).unwrap_or(0),
            // The server reports at its 1s default; it has no -i.
            interval: 1.0,
            // GSO/GRO are client-side knobs, not exchanged; iperf3's server emits 0.
            gso: 0,
            gro: 0,
            start_time_millis,
            // The client sends --extra-data via the parameter exchange; echo it
            // into the server's -J output too, like iperf3 (#35).
            extra_data: params.extra_data.clone(),
            server_output_text: None,
            server_output_json: None,
            intervals: collected_intervals,
            streams: stream_reports,
        };

        input
    }

    /// Build the server's full `-J` report. Split from the printer so
    /// `--get-server-output` (#33) can build it ONCE pre-exchange (attaching it
    /// to the results) and reuse it at print time — `build_report_input`
    /// drains the interval collector destructively, so it must run once.
    #[allow(clippy::too_many_arguments)]
    fn build_report(
        &self,
        streams: &[DataStream],
        cfg: &TestConfig,
        params: &TestParams,
        cpu_util: &crate::cpu::CpuUtilization,
        test_duration: f64,
        cookie: &[u8; protocol::COOKIE_SIZE],
        accepted_host: &str,
        accepted_port: u16,
        start_time_millis: u64,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
    ) -> crate::json_report::Report {
        self.build_report_input(
            streams,
            cfg,
            params,
            cpu_util,
            test_duration,
            cookie,
            accepted_host,
            accepted_port,
            start_time_millis,
            interval_data,
        )
        .build()
    }

    /// `-J`: pretty-print the server's single batched report blob (#50), or a
    /// prebuilt one (#33).
    fn print_results_json(&self, report: &crate::json_report::Report) {
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("iperf3: error - failed to serialize JSON: {e}"),
        }
    }

    /// `--json-stream`: emit the server's `end` event (#62). The interval events
    /// were already streamed live by the reporter.
    #[allow(clippy::too_many_arguments)]
    fn emit_json_stream_end(
        &self,
        streams: &[DataStream],
        cfg: &TestConfig,
        params: &TestParams,
        cpu_util: &crate::cpu::CpuUtilization,
        test_duration: f64,
        cookie: &[u8; protocol::COOKIE_SIZE],
        accepted_host: &str,
        accepted_port: u16,
        start_time_millis: u64,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
    ) {
        let input = self.build_report_input(
            streams,
            cfg,
            params,
            cpu_util,
            test_duration,
            cookie,
            accepted_host,
            accepted_port,
            start_time_millis,
            interval_data,
        );
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "end",
            &input.build().end,
        ));
    }

    /// `--json-stream`: emit the server's `start` event (#62), before any interval
    /// event. Only the `start` block is meaningful at this point; cpu/bytes are
    /// zero and intervals empty (the test hasn't run yet), and are discarded.
    #[allow(clippy::too_many_arguments)]
    fn emit_json_stream_start(
        &self,
        streams: &[DataStream],
        cfg: &TestConfig,
        params: &TestParams,
        cookie: &[u8; protocol::COOKIE_SIZE],
        accepted_host: &str,
        accepted_port: u16,
        start_time_millis: u64,
        interval_data: &Arc<Mutex<crate::reporter::CollectedIntervals>>,
    ) {
        let input = self.build_report_input(
            streams,
            cfg,
            params,
            &crate::cpu::CpuUtilization::default(),
            cfg.duration as f64,
            cookie,
            accepted_host,
            accepted_port,
            start_time_millis,
            interval_data,
        );
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "start",
            &input.build().start,
        ));
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct ServerBuilder {
    port: Option<u16>,
    one_off: bool,
    verbose: bool,
    idle_timeout: Option<u32>,
    server_bitrate_limit: Option<u64>,
    server_max_duration: Option<u32>,
    forceflush: bool,
    bind_address: Option<String>,
    ip_version: Option<u8>,
    timestamps: Option<String>,
    file: Option<String>,
    rsa_private_key_path: Option<String>,
    authorized_users_path: Option<String>,
    time_skew_threshold: u32,
    use_pkcs1_padding: bool,
    json_output: bool,
    json_stream: bool,
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self {
            port: Some(DEFAULT_PORT),
            one_off: false,
            verbose: false,
            idle_timeout: None,
            server_bitrate_limit: None,
            server_max_duration: None,
            forceflush: false,
            bind_address: None,
            ip_version: None,
            timestamps: None,
            file: None,
            rsa_private_key_path: None,
            authorized_users_path: None,
            time_skew_threshold: 10,
            use_pkcs1_padding: false,
            json_output: false,
            json_stream: false,
        }
    }
}

impl ServerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// `-p/--port`: server port to listen on (default 5201); `None` resolves
    /// back to the default at `build()`.
    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    /// `-1/--one-off`: handle one client connection, then exit.
    pub fn one_off(mut self, one_off: bool) -> Self {
        self.one_off = one_off;
        self
    }

    /// `-V/--verbose`: enable verbose output.
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Emit the results as iperf3-schema JSON on stdout instead of text (`-J`).
    pub fn json_output(mut self, enabled: bool) -> Self {
        self.json_output = enabled;
        self
    }

    /// Stream line-delimited interval JSON during the test (`--json-stream`).
    pub fn json_stream(mut self, enabled: bool) -> Self {
        self.json_stream = enabled;
        self
    }

    /// `--idle-timeout`: restart the listener if no client connects within
    /// `secs` seconds (with `-1/--one-off`, exit instead).
    pub fn idle_timeout(mut self, secs: u32) -> Self {
        self.idle_timeout = Some(secs);
        self
    }

    /// `--server-bitrate-limit`: abort a test whose aggregate throughput
    /// exceeds `rate` bits/sec (unset: no limit).
    pub fn server_bitrate_limit(mut self, rate: u64) -> Self {
        self.server_bitrate_limit = Some(rate);
        self
    }

    /// `--server-max-duration`: abort any test that runs longer than `secs`
    /// seconds (unset: no limit).
    pub fn server_max_duration(mut self, secs: u32) -> Self {
        self.server_max_duration = Some(secs);
        self
    }

    /// `--forceflush`: force flushing output at every interval.
    pub fn forceflush(mut self, enabled: bool) -> Self {
        self.forceflush = enabled;
        self
    }

    /// `-B/--bind`: bind the listener to a specific local address.
    pub fn bind_address(mut self, addr: &str) -> Self {
        self.bind_address = Some(addr.to_string());
        self
    }

    /// Restrict the listener to a specific IP version: `4` → IPv4 only, `6` →
    /// IPv6 only. Leave unset for dual-stack (the default). Signature matches
    /// `ClientBuilder::ip_version` for consistency.
    pub fn ip_version(mut self, version: u8) -> Self {
        debug_assert!(
            matches!(version, 4 | 6),
            "ip_version must be 4 or 6, got {version}"
        );
        self.ip_version = Some(version);
        self
    }

    /// `--timestamps`: prefix each output line with a timestamp in the given
    /// `strftime` format (the CLI defaults to `"%c "` when no format is given).
    pub fn timestamps(mut self, fmt: &str) -> Self {
        self.timestamps = Some(fmt.to_string());
        self
    }

    /// `-F/--file`: receiving streams write received data to this file;
    /// sending streams (reverse mode) read the payload from it.
    pub fn file(mut self, path: &str) -> Self {
        self.file = Some(path.to_string());
        self
    }

    /// `--rsa-private-key-path`: path to the RSA private key used to decrypt
    /// client authentication credentials.
    pub fn rsa_private_key_path(mut self, path: &str) -> Self {
        self.rsa_private_key_path = Some(path.to_string());
        self
    }

    /// `--authorized-users-path`: path to the file of users authorized to run
    /// authenticated tests.
    pub fn authorized_users_path(mut self, path: &str) -> Self {
        self.authorized_users_path = Some(path.to_string());
        self
    }

    /// `--time-skew-threshold`: allowed clock skew in seconds when validating
    /// an authentication token's timestamp (default 10).
    pub fn time_skew_threshold(mut self, secs: u32) -> Self {
        self.time_skew_threshold = secs;
        self
    }

    /// `--use-pkcs1-padding`: decrypt credentials with PKCS#1 v1.5 padding
    /// instead of OAEP (for tokens from pre-3.17 iperf3 clients).
    pub fn use_pkcs1_padding(mut self, enabled: bool) -> Self {
        self.use_pkcs1_padding = enabled;
        self
    }

    /// Like [`Self::server_bitrate_limit`], accepting an iperf3 rate string
    /// (`--server-bitrate-limit 1G`; decimal, 1000-based).
    pub fn server_bitrate_limit_str(self, s: &str) -> std::result::Result<Self, ConfigError> {
        // A bitrate limit is a rate: decimal (1000-based) suffixes, like iperf3 (#56).
        use crate::utils::parse_rate;
        Ok(self.server_bitrate_limit(parse_rate(s)?))
    }

    pub fn build(self) -> std::result::Result<Server, ConfigError> {
        // Reject -4/-6 contradicting an explicit -B of the opposite family,
        // instead of silently letting the bind address win (issue #12).
        if let (Some(v), Some(addr)) = (self.ip_version, self.bind_address.as_deref()) {
            // Strip any `%dev` suffix before parsing so e.g. `-B 10.0.0.1%eth0`
            // is still family-checked (consistent with the client, #15).
            let addr = addr.split('%').next().unwrap_or(addr);
            if let Ok(ip) = addr.parse::<std::net::IpAddr>() {
                let mismatch = (v == 4 && ip.is_ipv6()) || (v == 6 && ip.is_ipv4());
                if mismatch {
                    return Err(ConfigError::InvalidValue(
                        "bind_address",
                        format!("-{v} conflicts with bind address {addr}"),
                    ));
                }
            }
        }

        Ok(Server {
            port: self.port.unwrap_or(DEFAULT_PORT),
            one_off: self.one_off,
            verbose: self.verbose,
            idle_timeout: self.idle_timeout,
            server_bitrate_limit: self.server_bitrate_limit,
            server_max_duration: self.server_max_duration,
            forceflush: self.forceflush,
            bind_address: self.bind_address,
            ip_version: self.ip_version,
            timestamps: self.timestamps,
            file: self.file,
            rsa_private_key_path: self.rsa_private_key_path,
            authorized_users_path: self.authorized_users_path,
            time_skew_threshold: self.time_skew_threshold,
            use_pkcs1_padding: self.use_pkcs1_padding,
            json_output: self.json_output,
            json_stream: self.json_stream,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Per-setter builder tests migrated in-crate from `tests/integration.rs`
    // when `Server`'s fields became `pub(crate)` (#43): an external test crate
    // can no longer read `s.one_off`, `s.verbose`, etc.
    mod builder_setter_tests {
        use super::*;

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

        #[test]
        fn server_builder_rejects_version_bind_conflict() {
            // -4/-6 contradicting an explicit -B of the opposite family is an
            // error, not silently honored (issue #12).
            assert!(ServerBuilder::new()
                .ip_version(6)
                .bind_address("127.0.0.1")
                .build()
                .is_err());
            assert!(ServerBuilder::new()
                .ip_version(4)
                .bind_address("::")
                .build()
                .is_err());
            // Matching family, or a non-literal bind, is fine.
            assert!(ServerBuilder::new()
                .ip_version(6)
                .bind_address("::")
                .build()
                .is_ok());
            assert!(ServerBuilder::new()
                .ip_version(4)
                .bind_address("0.0.0.0")
                .build()
                .is_ok());
        }
    }

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

// ---------------------------------------------------------------------------
// TestConfig param resolution (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test_config_tests {
    use super::TestConfig;
    use crate::protocol::{TestParams, TransportProtocol};

    #[test]
    fn tcp_defaults() {
        let p = TestParams {
            tcp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p).unwrap();
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
        let cfg = TestConfig::from_params(&p).unwrap();
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
        let cfg = TestConfig::from_params(&p).unwrap();
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

    // #32: the server's reverse/bidir sender must pace on the CLIENT's
    // pacing-timer quantum — iperf3 always sends it and the server honors it.
    // Absent/zero (older peers) falls back to iperf3's 1000 µs default.
    #[test]
    fn from_params_honors_peer_pacing_timer() {
        let p = TestParams {
            pacing_timer: Some(250),
            ..Default::default()
        };
        assert_eq!(TestConfig::from_params(&p).unwrap().pacing_timer, 250);
        let p = TestParams::default();
        assert_eq!(TestConfig::from_params(&p).unwrap().pacing_timer, 1000);
        let p = TestParams {
            pacing_timer: Some(0),
            ..Default::default()
        };
        assert_eq!(TestConfig::from_params(&p).unwrap().pacing_timer, 1000);
    }

    // -- server mirrors the client's rate resolution (#17) --

    #[test]
    fn from_params_validates_len_bounds() {
        // A negotiated `len` is bounds-checked like the client builder (#188):
        // 0/absent → protocol default; negative would wrap `as usize` into a
        // multi-EiB buffer; UDP below MIN_UDP_BLKSIZE panicked the header
        // write; oversized is an allocation DoS. iperf3 clients never send
        // these (they validate locally and omit len 0), so rejection only
        // fires for broken/hostile peers.
        let udp = |len| TestParams {
            udp: Some(true),
            len,
            ..Default::default()
        };
        let tcp = |len| TestParams {
            tcp: Some(true),
            len,
            ..Default::default()
        };

        assert_eq!(
            TestConfig::from_params(&udp(None)).unwrap().blksize,
            crate::utils::DEFAULT_UDP_BLKSIZE
        );
        assert_eq!(
            TestConfig::from_params(&udp(Some(0))).unwrap().blksize,
            crate::utils::DEFAULT_UDP_BLKSIZE
        );
        assert_eq!(
            TestConfig::from_params(&tcp(Some(0))).unwrap().blksize,
            crate::utils::DEFAULT_TCP_BLKSIZE
        );
        assert_eq!(
            TestConfig::from_params(&udp(Some(1460))).unwrap().blksize,
            1460
        );

        assert!(TestConfig::from_params(&udp(Some(-1))).is_err());
        assert!(TestConfig::from_params(&tcp(Some(-1))).is_err());
        assert!(TestConfig::from_params(&udp(Some(8))).is_err());
        assert!(TestConfig::from_params(&udp(Some(70_000))).is_err());
        assert!(TestConfig::from_params(&tcp(Some(2 * 1024 * 1024))).is_err());
        assert!(TestConfig::from_params(&tcp(Some(1024 * 1024))).is_ok());
    }

    #[test]
    fn from_params_honors_burst() {
        // `-b rate/burst` arrives on the wire only when set (#160).
        let p = TestParams {
            tcp: Some(true),
            burst: Some(10),
            ..Default::default()
        };
        assert_eq!(TestConfig::from_params(&p).unwrap().burst, 10);
        let p = TestParams {
            tcp: Some(true),
            ..Default::default()
        };
        assert_eq!(TestConfig::from_params(&p).unwrap().burst, 0);
    }

    #[test]
    fn udp_absent_bandwidth_is_unlimited() {
        // iperf3 omits the `bandwidth` param only for -b 0 (unlimited) and sends
        // it explicitly otherwise (incl. its 1 Mbit/s default); riperf3 clients
        // always send it. So an ABSENT bandwidth means unlimited (0), matching
        // iperf3 — NOT the 1 Mbit/s default. Defaulting to 1M throttled an
        // iperf3 `-b 0` reverse/bidir client's server-side sender (#21).
        let p = TestParams {
            udp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p).unwrap();
        assert_eq!(cfg.bandwidth, 0);
    }

    #[test]
    fn udp_explicit_zero_bandwidth_is_unlimited() {
        // -b 0 carried in params → unlimited server-side, so reverse/bidir
        // runs flat-out instead of being throttled to 1 Mbit/s.
        let p = TestParams {
            udp: Some(true),
            bandwidth: Some(0),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p).unwrap();
        assert_eq!(cfg.bandwidth, 0);
    }

    #[test]
    fn tcp_absent_bandwidth_is_unlimited() {
        let p = TestParams {
            tcp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&p).unwrap();
        assert_eq!(cfg.bandwidth, 0);
    }
}
