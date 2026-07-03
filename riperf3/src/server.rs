use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use crate::cpu::CpuSnapshot;
use crate::error::{ConfigError, Result, RiperfError};
use crate::net;
use crate::protocol::{self, TestParams, TestResultsJson, TestState, TransportProtocol};
use crate::stream::{self, DataStream, StreamCounters, StreamMeta, UdpRecvStats};
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
    /// The client's `--fq-rate` (0 = unset): GT paces its ACCEPTED data
    /// sockets with it too (iperf_tcp.c:138-153, #302).
    pub fq_rate: u64,
    /// The client's GSO/GRO request (#316, GT iperf_api.c:2599-2619): the
    /// server enables UDP_SEGMENT/UDP_GRO on its UDP sockets when asked —
    /// best-effort like the client's #45 posture.
    pub gso: bool,
    pub gso_dg_size: i64,
    pub gro: bool,
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

        // Bound a negotiated `burst` like the client parse (IEBURST,
        // 1..=MAX_BURST): no real iperf3 client can send more (its own parse
        // enforces the cap), and an unbounded value drives the sender's
        // per-batch loop and green-light debt for hours — the same
        // hostile-peer posture as `len` below (#160 review r2). Absent or
        // non-positive (iperf3 gates on nonzero before sending) = unset.
        let burst = match params.burst {
            Some(b) if b > MAX_BURST as i32 => {
                return Err(ConfigError::InvalidValue(
                    "burst count",
                    format!("invalid burst count (maximum = {MAX_BURST}): {b}"),
                ));
            }
            Some(b) if b > 0 => b as u32,
            _ => 0,
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
            fq_rate: params.fqrate.unwrap_or(0),
            gso: params.gso.unwrap_or(0) != 0,
            // GT recomputes a zero dg_size from the negotiated blksize
            // (iperf_api.c:2607-2613); DEFAULT_UDP_BLKSIZE guards blksize 0.
            gso_dg_size: match params.gso_dg_size.unwrap_or(0) {
                0 if params.gso.unwrap_or(0) != 0 => match params.len.unwrap_or(0) {
                    blk if blk > 0 => i64::from(blk),
                    _ => 1460,
                },
                v => v,
            },
            gro: params.gro.unwrap_or(0) != 0,
            burst,
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
// Server test-run pipeline state (#289)
// ---------------------------------------------------------------------------

/// The per-test state `handle_one_test`'s phases thread through the run
/// (#289): the control connection, the negotiated test, and everything the
/// test accumulates as it advances. One instance per served test; the
/// phase-local variables the old monolithic `handle_one_test` mutated in
/// place are now named fields with one owner.
/// INVARIANT (#296): `build_result_streams` runs before
/// `finish_server_output` — both read the same live stream counters, and
/// the pipeline preserves the monolith's read order (the exchange figures
/// are captured first; with live counters either order carries a small
/// freshness skew — GT sidesteps it by snapshotting at TEST_END — so the
/// order itself is the contract). No suite can pin an inversion
/// deterministically (post-flush counters are settled); this doc and the
/// pipeline comment at the call site are the guard.
struct TestRunCtx {
    ctrl: tokio::net::TcpStream,
    /// The control-socket peer, for `start.accepted_connection` (#50):
    /// iperf3 uses getpeername(ctrl_sck) — distinct from the data-stream
    /// addresses in `connected[]` — with v4-mapped v6 unmapped like
    /// mapped_v4_to_regular_v4.
    accepted_host: String,
    accepted_port: u16,
    cookie: [u8; protocol::COOKIE_SIZE],
    params: TestParams,
    cfg: TestConfig,
    /// --get-server-output (#33): the client asked for this server's output.
    want_server_output: bool,
    /// The text-mode TEE into the exchange buffer (#33): iperf3's
    /// iperf_printf dual-write — the console stays live. `None` for JSON-mode
    /// servers (they attach their full report instead) and when unrequested.
    capture: Option<crate::macros::OutputCaptureGuard>,
    done: Arc<AtomicBool>,
    /// Released at TestStart so UDP senders don't transmit during stream
    /// setup (issue #5): the create-streams handshake is lost under a flood.
    start: Arc<AtomicBool>,
    streams: Vec<DataStream>,
    /// `-n`/`-k` shared byte budget for the server's TCP senders
    /// (reverse/bidir) — see `make_byte_budget` for the 0-is-unlimited and
    /// overflow-clamp rules.
    byte_budget: Option<Arc<AtomicI64>>,
    /// The boundary-refill target, captured BEFORE any sender can consume:
    /// loading it at reporter-spawn time read `N − early_consumed` on fast
    /// links (senders start in the TestStart→spawn gap), silently
    /// shrinking the refill (review r4).
    budget_target: Option<i64>,
    /// Single-socket UDP server demux (#80): one demux receiver thread serves
    /// every receiving stream, so its handle lives outside the per-stream
    /// `DataStream`s and is joined alongside them at teardown. `None` on the
    /// recycling path and on pure-reverse demux tests (no receivers).
    udp_demux_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    /// Interval samples + TCP_INFO extremes the reporter collects (#50/#62).
    interval_data: Arc<Mutex<crate::reporter::CollectedIntervals>>,
    /// Stamped at construction, RE-stamped by `start_test` at the real
    /// TestStart — only the re-stamped value is ever read.
    cpu_start: CpuSnapshot,
    /// Wall-clock at TestStart, for the `-J` start.timestamp (#50). 0 until
    /// `start_test` stamps it.
    test_start_millis: u64,
    /// Captured right before the reporter spawn so its elapsed at TEST_END is
    /// the authoritative final-interval boundary handed to the reporter
    /// (#55). Stamped at construction, RE-stamped by `start_test`.
    report_start: std::time::Instant,
    reporter_end: Arc<crate::reporter::ReporterEnd>,
    interval_handle: Option<tokio::task::JoinHandle<()>>,
    /// #224: a SELF-terminated test (bitrate limit / max duration) relays
    /// SERVER_ERROR + i_errno and skips BOTH the exchange and the local
    /// summary dump; the message lands on the server's own error sink.
    server_error: Option<&'static str>,
    /// #210: a peer-terminated or interrupted test skips the results
    /// exchange (the peer is gone) but still dumps local results.
    client_terminated: bool,
    interrupted: Option<String>,
}

/// What `shutdown_and_flush` distills for the finalize phases (#289): the
/// end-of-test CPU figure, the report-error message every sink shares
/// (#210 r1 f1), and the summary window (#103/#31).
struct EndState {
    cpu_util: crate::cpu::CpuUtilization,
    report_error: Option<String>,
    test_duration: f64,
}

/// Where the test's rich report stands after `finish_server_output` (#287):
/// either already built (the JSON-mode --get-server-output pre-exchange build,
/// #33 — the drained collections are inside) or still pending, carrying the
/// drained collections to the single post-exchange build. Exactly one build
/// happens either way, by construction.
enum ReportSource {
    Built(Box<crate::json_report::Report>),
    Pending(crate::reporter::CollectedIntervals),
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
    /// `--bind-dev`: bind the listener (and UDP server sockets) to a device,
    /// like iperf3's netannounce — Linux/SO_BINDTODEVICE only (#149).
    pub(crate) bind_dev: Option<String>,
    pub(crate) ip_version: Option<u8>,
    pub(crate) timestamps: Option<String>,
    pub(crate) file: Option<String>,
    pub(crate) rsa_private_key_path: Option<String>,
    pub(crate) authorized_users_path: Option<String>,
    pub(crate) time_skew_threshold: u32,
    pub(crate) use_pkcs1_padding: bool,
    /// Emit the test results as iperf3-schema JSON on stdout instead of text (#50).
    pub(crate) json_output: bool,
    /// #290: console output enabled (default). When false, `run`/`run_once`
    /// write nothing to stdout/stderr; reports flow via the return value and
    /// the wire (--get-server-output still relays).
    pub(crate) emit_output: bool,
    /// Stream line-delimited interval JSON during the test (`--json-stream`).
    pub(crate) json_stream: bool,
    /// #210: fired by the consumer (the CLI's first signal); a running test
    /// dumps stats, sends SERVER_TERMINATE, and `run()` returns.
    pub(crate) interrupt: Option<crate::client::InterruptWatch>,
    pub(crate) json_stream_full_output: bool,
    /// `-f` unit format for the text report (#242): the server-side flag was
    /// silently ignored — every render site hardcoded the adaptive default.
    pub(crate) format_char: char,
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
    /// Chainable form of [`ServerBuilder::interrupt`] for an already-built
    /// server (#210).
    pub fn with_interrupt(mut self, rx: tokio::sync::watch::Receiver<Option<String>>) -> Self {
        self.interrupt = Some(crate::client::InterruptWatch(rx));
        self
    }

    /// A non-report stdout line (the listening banner): iperf3 routes these
    /// through iperf_printf too, so they carry the `--timestamps` prefix
    /// (#216). Not tee'd into the --get-server-output capture — iperf3's
    /// banner prints before the test's JSON/capture exists.
    fn banner_line(line: &str) {
        // #290 (r1 finding 1): the quiet gate must live at the PRINT SITE —
        // arming the flag in run() silences nothing here by itself.
        if !crate::macros::output_quiet() {
            println!("{}{line}", crate::macros::output_timestamp_prefix());
        }
    }

    pub async fn run(&self) -> Result<()> {
        // #290: run-scoped console silence, armed FIRST so the listening
        // banner honors it. Construct-only-when-quiet (see the guard doc).
        let _quiet_guard = (!self.emit_output).then(crate::macros::OutputQuietGuard::set);
        // #262: GT's per-test banner counter (iperf_server_api.c:137's
        // server_test_number) — #1 on the first listen, incremented for each
        // serve round so the re-printed banner numbers the UPCOMING test.
        let mut server_test_number: u64 = 1;
        // Daemonizing (`-s -D`) is a process-level concern handled by the binary
        // *before* the tokio runtime is built — `daemon()` forks, and a fork from
        // inside a multi-threaded runtime would leave the child with no worker
        // threads (#81). The library must not fork here.
        let listener = net::tcp_listen(
            self.bind_address.as_deref(),
            self.port,
            self.ip_version,
            self.bind_dev.as_deref(),
        )
        .await?;
        // Under -J / --json-stream iperf3's server stdout is pure JSON (the
        // "Server listening" banners are suppressed) so the document parses
        // cleanly; match that.
        let json = self.json_output || self.json_stream;
        // Run-scoped, BEFORE the banner: iperf3 prefixes every iperf_printf
        // line, the listening banner included (#216). --timestamps is the
        // server's own flag (not exchanged), so run scope is its natural
        // home; the per-test guard this replaces missed the banner.
        let _ts_guard = (!json)
            .then_some(self.timestamps.as_deref())
            .flatten()
            .map(crate::macros::OutputTimestampGuard::set);
        let sep = "-----------------------------------------------------------";
        if !json {
            // -V reprints version/uname EVERY accept round, like GT's
            // iperf_run_server loop (r1 item 12; #222).
            if self.verbose {
                vprintln!("riperf3 {}", env!("CARGO_PKG_VERSION"));
                vprintln!("{}", crate::utils::system_info());
            }
            Self::banner_line(sep);
            Self::banner_line(&format!(
                "Server listening on {} (test #{server_test_number})",
                self.port
            ));
            Self::banner_line(sep);
        }

        let mut interrupt_fired = self.interrupt.clone().map(|w| w.0);
        loop {
            // #262 r1 F3: an idle-timeout expiry is GT's silent rc==2 restart
            // (iperf_server_api.c:133-135) — no stderr line, no banner
            // re-print, no counter increment; the accept simply re-arms.
            let mut idle_restart = false;
            match self.handle_one_test(&listener).await {
                // #137: the daemon loop discards each test's Report; library
                // users who want it call `run_once`.
                Ok(_) => {}
                Err(RiperfError::Aborted(msg)) if msg == "idle timeout" => {
                    idle_restart = true;
                }
                Err(RiperfError::PeerDisconnected) => {
                    if self.verbose {
                        vprintln!("Client disconnected.");
                    }
                }
                Err(RiperfError::ClientTerminated) => {
                    // iperf3 prints IECLIENTTERM WITHOUT the "error - "
                    // prefix ("iperf3: the client has terminated",
                    // live-captured) and keeps serving (#210). In the JSON
                    // modes the doc/event carried it — stderr stays silent
                    // like iperf_err (review r1 f1/f3).
                    if !json && !crate::macros::output_quiet() {
                        eprintln!("riperf3: {}", RiperfError::ClientTerminated);
                    }
                }
                Err(e) => {
                    if !crate::macros::output_quiet() {
                        eprintln!("riperf3: error - {e}");
                    }
                }
            }

            // #210: an interrupted run stops serving — handle_one_test
            // already dumped its stats and told the client; the caller owns
            // the signal-normal exit.
            if interrupt_fired
                .as_mut()
                .is_some_and(|rx| rx.borrow_and_update().is_some())
            {
                return Ok(());
            }
            if self.one_off {
                break;
            }
            if !idle_restart {
                // #262: the served round is over — the next banner numbers
                // the upcoming test.
                server_test_number += 1;
                if !json {
                    if self.verbose {
                        vprintln!("riperf3 {}", env!("CARGO_PKG_VERSION"));
                        vprintln!("{}", crate::utils::system_info());
                    }
                    Self::banner_line(sep);
                    Self::banner_line(&format!(
                        "Server listening on {} (test #{server_test_number})",
                        self.port
                    ));
                    Self::banner_line(sep);
                }
            }
        }
        Ok(())
    }

    /// Serve exactly one test and return its rich JSON [`Report`](crate::Report) — the same
    /// object [`Client::run`](crate::Client::run) returns and `-s -J` prints.
    ///
    /// Binds its own listener, accepts one client, runs the test to completion,
    /// and returns. This is the one-shot building block, mirroring
    /// `tokio::net::TcpListener::accept`; use [`Server::run`] for the long-lived
    /// accept loop. Unlike `run`, it does not print the "Server listening"
    /// banner — it is a library entry point, not the daemon. The test report is
    /// still printed to stdout in `-J` / text mode, like `Client::run`.
    pub async fn run_once(&self) -> Result<crate::json_report::Report> {
        let listener = self.listen().await?;
        self.serve_once(&listener).await
    }

    /// Bind the configured listener ONCE and return an owned handle that can
    /// serve tests on it (#291) — the accept()-style building block
    /// `run_once`'s per-call rebind couldn't be: sequential tests keep the
    /// port (no steal window, no re-listen race), and a `port(Some(0))`
    /// ephemeral bind is learnable via [`BoundServer::local_addr`] before any
    /// client connects. Consumes the `Server` (like a socket `bind`
    /// constructor), so the handle is `'static` and moves freely into
    /// spawned tasks.
    pub async fn bind(self) -> Result<BoundServer> {
        let listener = self.listen().await?;
        Ok(BoundServer {
            server: self,
            listener,
        })
    }

    /// The configured control listener — shared by `run_once` and `bind`.
    async fn listen(&self) -> Result<tokio::net::TcpListener> {
        net::tcp_listen(
            self.bind_address.as_deref(),
            self.port,
            self.ip_version,
            self.bind_dev.as_deref(),
        )
        .await
    }

    /// One test on an already-bound listener: the shared body of
    /// [`Server::run_once`] and [`BoundServer::run_once`] (#291), with the
    /// #290 console-quiet scope.
    async fn serve_once(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<crate::json_report::Report> {
        // #290: run-scoped console silence for this test.
        let _quiet_guard = (!self.emit_output).then(crate::macros::OutputQuietGuard::set);
        match self.handle_one_test(listener).await? {
            Some(report) => Ok(report),
            // Reachable only with an interrupt watch set (e.g. the CLI's signal
            // handling): interrupted while idle, before any client connected.
            None => Err(RiperfError::Aborted(
                "interrupted before a test started".into(),
            )),
        }
    }

    async fn handle_one_test(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<Option<crate::json_report::Report>> {
        // ---- Accept control connection (with optional idle timeout) ----
        let (mut ctrl, peer_addr) = match self.accept_control(listener).await? {
            Some(accepted) => accepted,
            // Interrupted while idle (no client): no test ran, so no report.
            None => return Ok(None),
        };
        // (#222 r1 item 6: the Time/banner/Cookie/MSS block prints AFTER the
        // param exchange — GT's iperf_on_connect fires there — so a
        // --get-server-output capture relays it; see print_connect_block.)
        net::configure_tcp_stream(&ctrl, true)?;

        // The control-socket peer address feeds the server's `start.accepted_connection`
        // (iperf_api.c uses getpeername(ctrl_sck) — distinct from the data-stream
        // addresses in `connected[]`). Captured for the `-J` blob (#50).
        // `to_canonical()` unwraps an IPv4-mapped IPv6 address (`::ffff:127.0.0.1`)
        // from the dual-stack listener back to plain `127.0.0.1`, as iperf3 does
        // (mapped_v4_to_regular_v4).
        let (accepted_host, accepted_port) =
            (peer_addr.ip().to_canonical().to_string(), peer_addr.port());

        // ---- Cookie + ParamExchange (+ the #230 upfront max-duration refusal) ----
        let (cookie, params, cfg) = match self.negotiate_test(&mut ctrl).await? {
            Some(negotiated) => negotiated,
            // Refused before any test ran → no report.
            None => return Ok(None),
        };

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
        // The timestamp guard is run-scoped — set in `run()` before the
        // banner (#216) — so the capture above tees PREFIXED lines like
        // iperf3's linebuffer (#168) with nothing to do per test.

        self.print_connect_block(peer_addr, &cookie, &params, &cfg);

        // ---- Auth validation (after params, before streams) ----
        self.authenticate(&mut ctrl, &params).await?;

        // The test's accumulated state, threaded through the pipeline phases
        // (#289) — field docs on TestRunCtx.
        let mut ctx = TestRunCtx {
            ctrl,
            accepted_host,
            accepted_port,
            cookie,
            params,
            cfg,
            want_server_output,
            capture,
            done: Arc::new(AtomicBool::new(false)),
            start: Arc::new(AtomicBool::new(false)),
            streams: Vec::new(),
            byte_budget: None,
            budget_target: None,
            udp_demux_handle: None,
            interval_data: Arc::new(Mutex::new(crate::reporter::CollectedIntervals::default())),
            cpu_start: CpuSnapshot::now(),
            test_start_millis: 0,
            report_start: std::time::Instant::now(),
            reporter_end: Arc::new(crate::reporter::ReporterEnd::new()),
            interval_handle: None,
            server_error: None,
            client_terminated: false,
            interrupted: None,
        };
        // Signal `done` on every exit path (incl. early `?` returns) so a UDP
        // sender parked on the start barrier can't leak if setup fails (#5).
        // Declared AFTER ctx so an early return drops the guard FIRST —
        // `done` is set before ctx's fields (the control socket, the capture
        // guard) drop, the monolith's drop order (r1 F1).
        let _done_guard = stream::DoneOnDrop(ctx.done.clone());

        // ---- CreateStreams ----
        self.setup_data_streams(&mut ctx, listener).await?;

        // ---- TestStart / TestRunning ----
        self.start_test(&mut ctx).await?;

        // ---- Wait for TEST_END (watchdog + bitrate limit + interrupt) ----
        self.await_test_end(&mut ctx).await?;

        // ---- Shut down streams + flush the reporter ----
        let mut end = self.shutdown_and_flush(&mut ctx).await;

        // ORDER CONSTRAINT (#296): built BEFORE the --get-server-output
        // finish. Both read live stream counters; the exchange figures must
        // be captured before the capture-finish renders text summaries, or
        // a still-draining receiver could send the peer fresher numbers
        // than its own rendered rows. Post-flush the counters are usually
        // settled, so no deterministic pin can catch an inversion — this
        // comment (and TestRunCtx's docs) are the guard.
        let result_streams = self.build_result_streams(&ctx, &end);

        // The ONE drain of the reporter's collections (#287): the reporter was
        // joined in shutdown_and_flush, so the take is final. From here the
        // collections move by value — into the pre-exchange build (JSON-mode
        // --get-server-output) or through `ReportSource::Pending` to the
        // single post-exchange build below.
        let collected = crate::reporter::CollectedIntervals::drain(&ctx.interval_data);

        // ---- --get-server-output finish + ExchangeResults / IperfDone ----
        let (server_output_text, server_output_json, report_source) =
            self.finish_server_output(&mut ctx, &end, collected);
        let was_captured = server_output_text.is_some();
        self.exchange_results_phase(
            &mut ctx,
            &end,
            result_streams,
            server_output_text,
            server_output_json,
        )
        .await?;

        // #319: an interrupt landing INSIDE the exchange phase fires after
        // EndState froze report_error — re-resolve so the sigend dump
        // carries the interrupt key like the mid-test arms. (A pre-exchange
        // --get-server-output capture keeps its frozen shape: it was built
        // before the signal by definition.)
        if end.report_error.is_none() {
            if let Some(msg) = &ctx.interrupted {
                end.report_error = Some(msg.clone());
            }
        }

        // #137/#287: the report is built exactly ONCE per test, by
        // construction — the collections moved either into the pre-exchange
        // --get-server-output build (#33) or arrive here for the single
        // post-exchange build. handle_one_test returns it (run discards it;
        // run_once hands it back to the library caller).
        let report = match report_source {
            ReportSource::Built(mut report) => {
                // #322 r1 F4: a mid-exchange interrupt postdates the
                // pre-exchange --get-server-output build — carry the key
                // like GT's exit-time json_finish.
                if report.error.is_none() {
                    report.error = end.report_error.clone();
                }
                *report
            }
            ReportSource::Pending(collected) => self.build_ctx_report(&ctx, &end, collected),
        };

        self.emit_final_output(&ctx, &end, &report, was_captured);

        // Join stream tasks (best-effort, they should be done).
        // #322 r1 F1: a mid-EXCHANGE interrupt postdates shutdown_and_flush's
        // abort gate — abort here so a wedged peer can't park the joins.
        if ctx.interrupted.is_some() {
            for s in &ctx.streams {
                s.task.abort();
            }
            if let Some(h) = &ctx.udp_demux_handle {
                h.abort();
            }
        }
        for s in ctx.streams {
            let _ = s.task.await;
        }
        // The single-socket UDP demux receiver (#80) serves all receiving streams
        // and lives outside `streams`; join it too. `None` on the recycling path.
        if let Some(h) = ctx.udp_demux_handle {
            let _ = h.await;
        }

        if ctx.client_terminated {
            // The dump above already rendered; the caller prints iperf3's
            // "the client has terminated" (no "error - " prefix) (#210).
            return Err(RiperfError::ClientTerminated);
        }
        Ok(Some(report))
    }

    // -----------------------------------------------------------------------
    // handle_one_test phases (#289). Each is one segment of the old monolith,
    // moved verbatim; handle_one_test owns only the pipeline.
    // -----------------------------------------------------------------------

    /// Accept the control connection (with optional idle timeout). The IDLE
    /// wait races the interrupt watch (#210 follow-through): an idle server's
    /// first signal must exit promptly — without this arm it burned the CLI's
    /// full 5 s dump window (the post-merge macOS CI red: systemd-style
    /// SIGTERM-while-listening took ~5 s). There is nothing to dump while
    /// idle; returning `None` lets the run loop's interrupt check exit the
    /// serve loop. The post-accept phases (cookie/param reads) stay
    /// interrupt-blind by design — the #158 second-signal wedge test depends
    /// on that window.
    async fn accept_control(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<Option<(tokio::net::TcpStream, std::net::SocketAddr)>> {
        let mut accept_interrupt = self.interrupt.clone().map(|w| w.0);
        let accepted = tokio::select! {
            r = async {
                if let Some(secs) = self.idle_timeout {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(secs as u64),
                        listener.accept(),
                    )
                    .await
                    {
                        Ok(result) => result.map_err(RiperfError::from),
                        Err(_) => Err(RiperfError::Aborted("idle timeout".into())),
                    }
                } else {
                    listener.accept().await.map_err(RiperfError::from)
                }
            } => Some(r),
            _ = crate::client::wait_interrupt(accept_interrupt.as_mut()) => None,
        };
        match accepted {
            Some(r) => r.map(Some),
            // Interrupted while idle (no client): no test ran, so no report.
            None => Ok(None),
        }
    }

    /// Cookie read + ParamExchange + config derivation, plus the #230 upfront
    /// max-duration check. `Ok(None)` = refused (no test ran, no report).
    async fn negotiate_test(
        &self,
        ctrl: &mut tokio::net::TcpStream,
    ) -> Result<Option<([u8; protocol::COOKIE_SIZE], TestParams, TestConfig)>> {
        // ---- Cookie ----
        let cookie = protocol::recv_cookie(ctrl).await?;

        // ---- ParamExchange ----
        protocol::send_state(ctrl, TestState::ParamExchange).await?;
        let mut params = protocol::recv_params(ctrl).await?;
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

        // #230: GT's upfront requested-duration check — the tail of
        // get_parameters (iperf_api.c:2666): with --server-max-duration set,
        // refuse at param exchange when (time + omit) > max, or when the
        // request is unbounded (time == 0 — which is every -n/-k run, since
        // the client zeroes the wire duration, iperf_api.c:1981, and -t 0).
        // GT runs this BEFORE auth and skips on_connect entirely (the goto
        // error_handling path), so it sits ahead of the connect block and
        // the capture guard. cfg.duration already mirrors GT's server-side
        // default (absent time → 10). The flag arms NO timer — the in-flight
        // watchdog is duration-anchored and flag-independent, like
        // GT's create_server_timers.
        let duration_violated = self
            .server_max_duration
            .filter(|&m| m > 0)
            .is_some_and(|max| cfg.duration.saturating_add(cfg.omit) > max || cfg.duration == 0);

        // #260: GT's upfront total-rate check, immediately beside the
        // duration check (iperf_api.c:2672-2684): total = num_streams * rate
        // * (bidir ? 2 : 1), evaluated for BOTH the requested bitrate and the
        // fq rate; refused with SERVER_ERROR + IETOTALRATE(27). Distinct from
        // the in-flight 1 Hz breach check in await_test_end, which stays.
        let rate_violated = self
            .server_bitrate_limit
            .filter(|&l| l > 0)
            .is_some_and(|limit| {
                let mult = u64::from(cfg.num_streams) * if cfg.bidir { 2 } else { 1 };
                let over = |rate: u64| rate.saturating_mul(mult) > limit;
                over(cfg.bandwidth) || over(params.fqrate.unwrap_or(0))
            });

        // r1 F3: GT runs the duration check first but has NO early return —
        // the rate check's later i_errno assignment wins, so a doubly-
        // violating client is refused with IETOTALRATE (live-verified).
        // GT stamps json_start.target_bitrate before the checks run
        // (iperf_api.c:2662), so both refusal docs carry the client's -b.
        let refused_rate = params.bandwidth.filter(|&b| b > 0);
        if rate_violated {
            // Refused before any test ran → no report.
            self.refuse_total_rate(ctrl, refused_rate).await?;
            return Ok(None);
        }
        if duration_violated {
            self.refuse_max_duration(ctrl, refused_rate).await?;
            return Ok(None);
        }
        Ok(Some((cookie, params, cfg)))
    }

    /// #222: the connect text block, in GT's order and GT's TIMING —
    /// iperf_on_connect fires post-param-exchange, which also puts these
    /// lines inside the --get-server-output capture (r1 item 6). The
    /// banner is unconditional in text mode; the rest is -V. The server's
    /// control MSS is 0 "(default)" (ctrl_sck_mss, r1 item 2).
    fn print_connect_block(
        &self,
        peer_addr: std::net::SocketAddr,
        cookie: &[u8; protocol::COOKIE_SIZE],
        params: &TestParams,
        cfg: &TestConfig,
    ) {
        if !self.json_output && !self.json_stream {
            if self.verbose {
                vprintln!(
                    "Time: {}",
                    crate::json_report::http_date(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    )
                );
            }
            // iperf3's shape: "host, port N", with v4-mapped v6 addresses
            // unmapped for display (mapped_v4_to_regular_v4).
            vprintln!(
                "Accepted connection from {}, port {}",
                peer_addr.ip().to_canonical(),
                peer_addr.port()
            );
            if self.verbose {
                vprintln!(
                    "      Cookie: {}",
                    String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1])
                );
                if matches!(cfg.protocol, TransportProtocol::Tcp) {
                    // The client's -M arrives in params (r3): GT prints the
                    // SET value suffix-free, "0 (default)" only when unset —
                    // the server mirror of the client-side rule.
                    match params.mss.filter(|&m| m != 0) {
                        Some(m) => vprintln!("      TCP MSS: {m}"),
                        None => vprintln!("      TCP MSS: 0 (default)"),
                    }
                }
                // GT's on_connect verbose tail is role-independent (r2
                // item 3): the client's requested rate arrives in params.
                if let Some(b) = params.bandwidth {
                    if b != 0 {
                        vprintln!("      Target Bitrate: {b}");
                    }
                }
            }
        }
    }

    /// Auth validation (after params, before streams).
    async fn authenticate(
        &self,
        ctrl: &mut tokio::net::TcpStream,
        params: &TestParams,
    ) -> Result<()> {
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
                        protocol::send_state(ctrl, TestState::AccessDenied).await?;
                        return Err(e);
                    }
                }
            } else {
                // Server requires auth but client didn't send token
                protocol::send_state(ctrl, TestState::AccessDenied).await?;
                return Err(RiperfError::AccessDenied);
            }
        }
        Ok(())
    }

    /// CreateStreams: the `-n`/`-k` byte budget and the per-protocol data
    /// stream setup, filling `ctx.streams` (+ the UDP demux handle).
    async fn setup_data_streams(
        &self,
        ctx: &mut TestRunCtx,
        listener: &tokio::net::TcpListener,
    ) -> Result<()> {
        // Determine how many streams to accept and their roles.
        // Normal: server receives. Reverse: server sends. Bidir: both.
        let recv_count = if ctx.cfg.reverse && !ctx.cfg.bidir {
            0
        } else {
            ctx.cfg.num_streams
        };
        let send_count = if ctx.cfg.reverse || ctx.cfg.bidir {
            ctx.cfg.num_streams
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
        ctx.byte_budget = (matches!(ctx.cfg.protocol, TransportProtocol::Tcp) && send_count > 0)
            .then(|| {
                stream::make_byte_budget(ctx.params.num, ctx.params.blockcount, ctx.cfg.blksize)
            })
            .flatten();
        // The boundary-refill target, captured BEFORE any sender can consume:
        // loading it at reporter-spawn time read `N − early_consumed` on fast
        // links (senders start in the TestStart→spawn gap), silently
        // shrinking the refill (review r4).
        ctx.budget_target = ctx.byte_budget.as_ref().map(|b| b.load(Ordering::Relaxed));

        match ctx.cfg.protocol {
            TransportProtocol::Tcp => {
                protocol::send_state(&mut ctx.ctrl, TestState::CreateStreams).await?;

                for i in 0..total {
                    let (mut data_stream, _) = listener.accept().await?;
                    let stream_cookie = protocol::recv_cookie(&mut data_stream).await?;
                    if stream_cookie != ctx.cookie {
                        return Err(RiperfError::CookieMismatch);
                    }
                    // Apply socket options (nodelay, MSS, window, congestion) to each stream
                    net::configure_tcp_stream_full(
                        &data_stream,
                        ctx.cfg.no_delay,
                        ctx.cfg.mss,
                        ctx.cfg.window,
                        ctx.cfg.congestion.as_deref(),
                    )?;
                    if ctx.cfg.tos != 0 {
                        // Fatal like every other set_tos site (#45): iperf3's
                        // iperf_common_sockopts errors (IESETTOS) when IP_TOS
                        // can't be applied, on both roles and both protocols.
                        net::set_tos(&data_stream, ctx.cfg.tos as u32)?;
                    }
                    // #302: GT enables fair-queue pacing on the server's
                    // ACCEPTED data sockets too (iperf_tcp.c:138-153) — the
                    // exchanged --fq-rate paces the reverse/bidir send path.
                    // Warn-only like GT's four sites; Linux sockopt.
                    net::apply_fq_rate(&data_stream, ctx.cfg.fq_rate);

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

                    // Real socket addresses + kernel buffer sizes + the #97
                    // window-clamp check for the server's `-J` `connected[]` /
                    // `sndbuf_actual` / `rcvbuf_actual` (#50), captured before the
                    // stream moves into its task. TCP applied -w earlier in
                    // configure_tcp_stream_full, so apply_window is false (#144).
                    let sock = net::capture_stream_meta(
                        socket2::SockRef::from(&data_stream),
                        ctx.cfg.window,
                        false,
                    )?;
                    // #37: congestion algorithm actually in effect on this stream.
                    let congestion_used = net::tcp_congestion_used(&data_stream);

                    let task = if is_sender {
                        let buf = make_send_buffer(ctx.cfg.blksize, false);
                        let c = counters.clone();
                        let d = ctx.done.clone();
                        // `-b` paces the sender in reverse/bidir too (negotiated
                        // rate; 0 = unlimited), on the client's pacing-timer
                        // quantum. #102/#32
                        let rate = ctx.cfg.bandwidth;
                        let pt = ctx.cfg.pacing_timer;
                        let bu = ctx.cfg.burst;
                        let bb = ctx.byte_budget.clone();
                        tokio::spawn(async move {
                            stream::run_tcp_sender(data_stream, c, buf, d, fp, rate, pt, bu, bb)
                                .await
                        })
                    } else {
                        let c = counters.clone();
                        let d = ctx.done.clone();
                        let bs = ctx.cfg.blksize;
                        tokio::spawn(async move {
                            stream::run_tcp_receiver(data_stream, c, bs, d, false, fp).await
                        })
                    };

                    ctx.streams.push(DataStream {
                        meta: StreamMeta {
                            id: stream_id,
                            is_sender,
                            counters,
                            raw_fd,
                            sock,
                            congestion_used,
                        },
                        task,
                        udp_recv_stats: None,
                    });
                }
            }
            TransportProtocol::Udp => {
                // Max send duration the server's UDP senders self-enforce
                // (issue #5): in bidir/reverse the server sends too, and at a
                // high `-b` those CPU-bound senders can starve this side's
                // runtime so it never processes the client's TestEnd. Only in
                // duration mode; byte/block-limited tests stop on `done`.
                let max_duration = (ctx.params.num.is_none() && ctx.params.blockcount.is_none())
                    .then(|| {
                        std::time::Duration::from_secs((ctx.cfg.duration + ctx.cfg.omit) as u64)
                    });

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
                        &mut ctx.ctrl,
                        &ctx.cfg,
                        recv_count,
                        total,
                        max_duration,
                        &ctx.done,
                        &ctx.start,
                        &mut ctx.streams,
                        &mut ctx.udp_demux_handle,
                    )
                    .await?;
                } else {
                    self.setup_udp_recycling_streams(
                        &mut ctx.ctrl,
                        &ctx.cfg,
                        recv_count,
                        total,
                        max_duration,
                        &ctx.done,
                        &ctx.start,
                        &mut ctx.streams,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// TestStart / TestRunning: the #222 preamble prints, the start-barrier
    /// release, the state sends + clock stamps, the --json-stream `start`
    /// event, and the interval-reporter spawn.
    /// (The legacy "Test: Tcp N stream(s)..." -V line is gone — its GT
    /// replacement is the Starting Test line here; r1 item 14.)
    async fn start_test(&self, ctx: &mut TestRunCtx) -> Result<()> {
        // #222: the per-stream preamble (unconditional, text) and the -V
        // Starting Test parameter line, like the client side.
        if !self.json_output && !self.json_stream {
            for s in &ctx.streams {
                if let (Some(l), Some(p)) = (s.meta.sock.local_addr, s.meta.sock.peer_addr) {
                    vprintln!(
                        "[{:3}] local {} port {} connected to {} port {}",
                        s.meta.id,
                        l.ip().to_canonical(),
                        l.port(),
                        p.ip().to_canonical(),
                        p.port()
                    );
                }
            }
            if self.verbose {
                // The bytes/blocks/time variants, like the client side (r2
                // item 1): a -n/-k client's server printed a phantom
                // duration before.
                let proto = match ctx.cfg.protocol {
                    TransportProtocol::Tcp => "TCP",
                    TransportProtocol::Udp => "UDP",
                };
                let head = format!(
                    "Starting Test: protocol: {proto}, {} streams, {} byte blocks, \
                     omitting {} seconds",
                    ctx.cfg.num_streams, ctx.cfg.blksize, ctx.cfg.omit
                );
                if let Some(bytes) = ctx.params.num.filter(|&n| n > 0) {
                    vprintln!("{head}, {bytes} bytes to send, tos {}", ctx.cfg.tos);
                } else if let Some(blocks) = ctx.params.blockcount.filter(|&n| n > 0) {
                    vprintln!("{head}, {blocks} blocks to send, tos {}", ctx.cfg.tos);
                } else {
                    vprintln!(
                        "{head}, {} second test, tos {}",
                        ctx.cfg.duration,
                        ctx.cfg.tos
                    );
                }
            }
        }
        // All streams are set up — release the UDP senders.
        ctx.start.store(true, Ordering::Relaxed);
        protocol::send_state(&mut ctx.ctrl, TestState::TestStart).await?;
        ctx.cpu_start = CpuSnapshot::now();
        // Wall-clock at TestStart, for the `-J` start.timestamp (#50).
        ctx.test_start_millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        protocol::send_state(&mut ctx.ctrl, TestState::TestRunning).await?;

        // For plain -J the reporter runs silently to collect intervals for the
        // final blob; for text or --json-stream it prints/streams live, matching
        // the client's gating (#50).
        let json = self.json_output || self.json_stream;
        let print_intervals = !json || self.json_stream;
        let collect_intervals = json && !self.json_stream;
        // Like the client: `--json-stream` streams intervals live but still needs
        // the per-stream TCP_INFO extremes handed back for the `end` event (#62).
        let want_collector = collect_intervals || self.json_stream;

        // --json-stream: emit the `start` event now — before the reporter is
        // spawned, so it is guaranteed to precede every `interval` event (#62).
        if self.json_stream {
            self.emit_json_stream_start(
                &ctx.streams,
                &ctx.cfg,
                &ctx.params,
                &ctx.cookie,
                &ctx.accepted_host,
                ctx.accepted_port,
                ctx.test_start_millis,
            );
        }

        // Spawn interval reporter (server uses 1.0s default). `report_start` is
        // captured right before the spawn so its elapsed at TEST_END is the
        // authoritative final-interval boundary handed to the reporter (#55).
        ctx.report_start = std::time::Instant::now();
        ctx.interval_handle = {
            let stream_refs: Vec<_> = ctx
                .streams
                .iter()
                .map(|s| crate::reporter::IntervalStreamRef {
                    id: s.meta.id,
                    is_sender: s.meta.is_sender,
                    counters: s.meta.counters.clone(),
                    udp_recv_stats: s.udp_recv_stats.clone(),
                    raw_fd: s.meta.raw_fd,
                })
                .collect();
            crate::reporter::spawn_interval_reporter(
                crate::reporter::IntervalReporterConfig {
                    interval_secs: 1.0,
                    protocol: ctx.cfg.protocol,
                    // #242: the wired -f (was a hardcoded adaptive default).
                    format_char: self.format_char,
                    omit_secs: ctx.cfg.omit,
                    forceflush: self.forceflush,
                    json_stream: self.json_stream,
                    print: print_intervals,
                    blksize: ctx.cfg.blksize,
                    // iperf3's discard_json: a json-stream run RETAINS the
                    // interval objects when the client asked for output OR
                    // under --json-stream-full-output (#168, #213).
                    keep_intervals: self.json_stream
                        && (ctx.want_server_output || self.json_stream_full_output),
                    bidir: ctx.cfg.bidir,
                    is_server: true,
                },
                stream_refs,
                ctx.done.clone(),
                ctx.reporter_end.clone(),
                want_collector.then(|| ctx.interval_data.clone()),
                ctx.byte_budget.clone().zip(ctx.budget_target),
                // The server has no -n/-k end-check driver — the client ends
                // the test — so it never waits on the boundary signal.
                None,
            )
        };
        Ok(())
    }

    /// Wait for TEST_END, racing the duration watchdog, the bitrate limit,
    /// and the interrupt watch; records how the test ended in the context
    /// flags (`server_error` / `client_terminated` / `interrupted`).
    async fn await_test_end(&self, ctx: &mut TestRunCtx) -> Result<()> {
        let bitrate_limit = self.server_bitrate_limit;
        let test_start = ctx.report_start;
        // #230: GT's in-flight 160-watchdog (create_server_timers,
        // iperf_server_api.c:380-395) arms for EVERY test with a nonzero
        // requested duration, at (time + omit + grace) where grace =
        // max_rtt(4) × state_transitions(10) = 40 s — flag-independent.
        // --server-max-duration arms nothing here; it only drives the
        // upfront param-exchange check. Unbounded (-n/-k/-t 0) requests get
        // no watchdog, exactly like GT's `if (test->duration != 0)` gate.
        const WATCHDOG_GRACE_SECS: u64 = 40;
        let watchdog_secs = match ctx.cfg.duration {
            0 => 0,
            d => (d as u64).saturating_add(ctx.cfg.omit as u64) + WATCHDOG_GRACE_SECS,
        };

        let mut rate_check = tokio::time::interval(std::time::Duration::from_secs(1));
        rate_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        rate_check.tick().await; // skip immediate tick

        // #237: ONE absolute deadline, pinned before the loop. A sleep()
        // recreated inside select! restarts from zero every time another arm
        // (the 1 Hz rate ticks) re-enters the loop, so a deadline > ~1 s
        // could never fire. Guarded below, so the 0-when-unset deadline
        // (already in the past) is never polled.
        let watchdog_deadline = tokio::time::sleep_until(tokio::time::Instant::from_std(
            test_start + std::time::Duration::from_secs(watchdog_secs),
        ));
        tokio::pin!(watchdog_deadline);

        // #224 (iperf 3.21 ground truth): a SELF-terminated test (bitrate
        // limit / max duration) relays SERVER_ERROR + i_errno and skips BOTH
        // the exchange and the local summary dump; the message lands on the
        // server's own error sink. iperf_strerror(IETOTALRATE) for the rate
        // arm; the duration arm prints server_timer_proc's LITERAL line
        // (the client side shows strerror(160) instead).
        const SELF_TERM_RATE_MSG: &str = "total required bandwidth is larger than server limit";
        const SELF_TERM_DURATION_MSG: &str =
            "server test duration expired - test is terminated by the server";
        let mut interrupt_rx = self.interrupt.clone().map(|w| w.0);

        loop {
            tokio::select! {
                state = protocol::recv_state(&mut ctx.ctrl) => {
                    match state? {
                        TestState::TestEnd => break,
                        TestState::ClientTerminate => {
                            // iperf_got_sigend's peer half (#210): dump the
                            // partial results in the finalize phases (the old
                            // early return leaked the reporter — the #147
                            // class — and skipped the dump iperf3 performs).
                            ctx.client_terminated = true;
                            break;
                        }
                        // #145: AUDITABILITY ONLY — log an out-of-sequence
                        // control byte during the data phase, then STILL
                        // swallow it (behavior unchanged, default-tolerant).
                        other => {
                            if !protocol::is_legal_next(
                                TestState::TestRunning,
                                other,
                                protocol::Role::Server,
                            ) {
                                log::debug!(
                                    "server: out-of-sequence control state {other:?} \
                                     during the data phase (ignored)"
                                );
                            }
                        }
                    }
                }
                msg = crate::client::wait_interrupt(interrupt_rx.as_mut()) => {
                    // iperf_got_sigend, server role mid-TEST_RUNNING (#210):
                    // tell the client, then dump local results in the
                    // finalize phases.
                    let _ = protocol::send_state(&mut ctx.ctrl, TestState::ServerTerminate).await;
                    ctx.interrupted = Some(msg);
                    break;
                }
                _ = rate_check.tick(), if bitrate_limit.is_some() => {
                    let elapsed = test_start.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        let total_bytes: u64 = ctx.streams.iter().map(|s| {
                            s.meta.counters.bytes_sent() + s.meta.counters.bytes_received()
                        }).sum();
                        let bits_per_sec = total_bytes as f64 * 8.0 / elapsed;
                        if let Some(limit) = bitrate_limit {
                            if bits_per_sec > limit as f64 {
                                // #224: SERVER_ERROR + IETOTALRATE(27), not
                                // SERVER_TERMINATE (iperf 3.21 GT).
                                protocol::send_server_error(&mut ctx.ctrl, 27).await?;
                                ctx.server_error = Some(SELF_TERM_RATE_MSG);
                                break;
                            }
                        }
                    }
                }
                _ = &mut watchdog_deadline, if watchdog_secs > 0 => {
                    // #224: iperf3's server_timer_proc — SERVER_ERROR +
                    // IESERVERTESTDURATIONEXPIRED(160) on the wire.
                    protocol::send_server_error(&mut ctx.ctrl, 160).await?;
                    ctx.server_error = Some(SELF_TERM_DURATION_MSG);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Shut down the streams, flush the reporter, and distill the end state
    /// (the CPU figure, the shared report-error message, and the summary
    /// window) for the finalize phases.
    async fn shutdown_and_flush(&self, ctx: &mut TestRunCtx) -> EndState {
        // GT mirror (#230): a self-terminated test CLOSES its data sockets at
        // once — server_timer_proc frees every stream + ctrl_sck on the 160
        // path, cleanup_server pthread_cancels on the rate path — so a
        // wedged/silent peer cannot hold the post-loop joins hostage (a
        // receiver parked in stream.read() never re-checks `done`; the
        // PR #247 r1 probe measured a 169 s hang against a SIGSTOP'd client).
        // The real work is on the TCP async tasks (abort drops the socket at
        // the next await); the UDP runners are spawn_blocking, where abort
        // is a no-op once running — their joins stay bounded anyway by the
        // 500 ms read-timeout + `done` polling in the blocking loops.
        if ctx.server_error.is_some() || ctx.interrupted.is_some() {
            // #322 r1 F1: interrupts take the same abort — a wedged peer
            // holding sockets open must not park the joins (GT closes its
            // data sockets at TEST_END and sigend exits in milliseconds).
            for s in &ctx.streams {
                s.task.abort();
            }
            if let Some(h) = &ctx.udp_demux_handle {
                h.abort();
            }
        }
        // #55 window, #159 order: stop the streams, let the catch-up land,
        // then hand the reporter the authoritative end time for the flush.
        let measured_elapsed = ctx.report_start.elapsed().as_secs_f64();
        // The reporter's timeline restarted at the omit boundary (#31), so its
        // authoritative end time is post-omit; clamp for runs that died inside
        // the warm-up.
        // #159: senders stop first, the catch-up grace runs, THEN the flush
        // is signalled (see the client-side twin for the full rationale).
        ctx.done.store(true, Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        ctx.reporter_end
            .finish((measured_elapsed - ctx.cfg.omit as f64).max(0.0));
        if let Some(handle) = ctx.interval_handle.take() {
            let _ = handle.await;
        }
        let cpu_end = CpuSnapshot::now();
        // The terminate/interrupt message every report sink shares (#210 r1
        // f1): iperf3's iperf_exit/iperf_err put it in the -J doc's error
        // key (and suppress stderr) on the server role too.
        let report_error: Option<String> = if let Some(msg) = &ctx.interrupted {
            Some(msg.clone())
        } else if ctx.client_terminated {
            Some("the client has terminated".to_string())
        } else {
            // iperf_err's in-doc wart, mirrored exactly: on SELF-terminate
            // the -J error key carries an "error - " prefix (GT iperf 3.21:
            // "error - total required bandwidth is larger than server
            // limit") where the peer-terminate keys carry none.
            ctx.server_error.map(|msg| format!("error - {msg}"))
        };
        // The self-terminate line on the server's own stderr (iperf_err; the
        // JSON sinks carry it via report_error instead). iperf3's one-off
        // still EXITS 0 after this — live-verified wart, mirrored by NOT
        // returning an error from this path (#224).
        if let Some(msg) = ctx.server_error {
            if !self.json_output && !self.json_stream && !crate::macros::output_quiet() {
                eprintln!("riperf3: error - {msg}");
            }
        }

        // (The pre-results grace moved up to the #159 stop-then-flush
        // sequence; the counters are already settled here.)

        // Summary window + bitrate: the measured elapsed for a byte/block-limited
        // run, exactly `-t` otherwise (#103, mirrors the client). The requested
        // `-t` is reported separately as the test_start `duration` parameter.
        let test_duration = if ctx.params.num.is_some()
            || ctx.params.blockcount.is_some()
            || ctx.client_terminated
            || ctx.interrupted.is_some()
            || ctx.server_error.is_some()
        {
            // Rebase to the post-omit window (#31): the measured elapsed
            // includes the warm-up the summary must exclude. A terminated
            // run (#210) reports the window it actually ran — iperf3's
            // sigend dump stamps the partial elapsed, not `-t` (live:
            // "[  5] 0.00-2.00 sec" for a -t 10 run killed at 2 s).
            (measured_elapsed - ctx.cfg.omit as f64).max(0.0)
        } else {
            ctx.cfg.duration as f64
        };

        EndState {
            cpu_util: cpu_end.utilization_since(&ctx.cpu_start),
            report_error,
            test_duration,
        }
    }

    /// The per-stream wire results for the exchange: net (post-omit) bytes;
    /// packets/errors stay GROSS with the omitted_* baselines alongside,
    /// like iperf3's exchange (#31).
    fn build_result_streams(
        &self,
        ctx: &TestRunCtx,
        end: &EndState,
    ) -> Vec<protocol::StreamResultJson> {
        let mut result_streams = Vec::new();
        for s in &ctx.streams {
            let bytes = if s.meta.is_sender {
                s.meta.counters.bytes_sent_net()
            } else {
                s.meta.counters.bytes_received_net()
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
                } else if s.meta.is_sender && matches!(ctx.cfg.protocol, TransportProtocol::Udp) {
                    // iperf3's UDP sender counts every datagram it sends
                    // (iperf_udp.c `++sp->packet_count`) and exchanges that
                    // count unconditionally (iperf_api.c `"packets"`); the
                    // peer's sender line renders it. Fill the equivalent from
                    // sent bytes, keeping the gross+baseline convention (#184
                    // — a zero here made an iperf3 client print `0/0` for a
                    // riperf3 server's reverse stream).
                    // #256: the authoritative per-datagram send counter (an
                    // exact `++sp->packet_count`), not the old `bytes/blksize`
                    // derivation. Full-block-only senders keep this == the old
                    // value bit-for-bit (no compat-matrix drift); making it
                    // authoritative just protects against a future short-send.
                    let gross = s.meta.counters.datagrams_sent() as i64;
                    let net = s.meta.counters.datagrams_sent_net() as i64;
                    (0.0, 0, gross, 0, gross - net)
                } else {
                    (0.0, 0, 0, 0, 0)
                };

            let is_udp_stream = matches!(ctx.cfg.protocol, TransportProtocol::Udp);
            let retransmits = s.sender_retransmits(is_udp_stream).unwrap_or(-1);

            result_streams.push(protocol::StreamResultJson {
                id: s.meta.id,
                bytes,
                retransmits,
                jitter,
                errors,
                omitted_errors: Some(omitted_errors),
                packets,
                omitted_packets: Some(omitted_packets),
                start_time: 0.0,
                end_time: end.test_duration,
            });
        }
        result_streams
    }

    /// --get-server-output (#33): finish the diverted text (render the final
    /// summaries into the capture first, so the client sees the complete
    /// report), or attach the full -J report for a JSON-mode server. Returns
    /// `(server_output_text, server_output_json, report_source)` — the drained
    /// collections go in and come back either inside the pre-built report or
    /// untouched for the single post-exchange build (#287).
    fn finish_server_output(
        &self,
        ctx: &mut TestRunCtx,
        end: &EndState,
        collected: crate::reporter::CollectedIntervals,
    ) -> (Option<String>, Option<serde_json::Value>, ReportSource) {
        let mut report_source = ReportSource::Pending(collected);
        let (server_output_text, server_output_json) = if let Some(capture) = ctx.capture.take() {
            let summaries = Self::text_summaries(&ctx.streams, end.test_duration, &ctx.cfg);
            let with_retr = summaries.iter().any(|s| s.retransmits.is_some());
            crate::reporter::print_separator();
            // The -V additions render here too (r1 item 7): the captured
            // pre-exchange path is the console output AND the relay.
            if self.verbose {
                vprintln!("Test Complete. Summary Results:");
            }
            crate::reporter::print_final_header(ctx.cfg.protocol, ctx.cfg.bidir, with_retr);
            crate::reporter::print_final_summaries_server(
                &summaries,
                self.format_char,
                self.verbose,
                ctx.cfg.protocol,
            );
            if self.verbose && ctx.streams.iter().any(|s| s.meta.is_sender) {
                // GT gates the CPU line on the SENDING side (iperf_api.c:
                // 4563): a -R server prints it, with ZERO remote figures —
                // the peer's CPU is never exchanged to the server (#50).
                vprintln!(
                    "CPU Utilization: local/sender {:.1}% ({:.1}%u/{:.1}%s), \
                     remote/receiver 0.0% (0.0%u/0.0%s)",
                    end.cpu_util.host_total,
                    end.cpu_util.host_user,
                    end.cpu_util.host_system
                );
            }
            if self.verbose && matches!(ctx.cfg.protocol, TransportProtocol::Tcp) {
                if let Some(c) = ctx
                    .streams
                    .iter()
                    .find_map(|s| s.meta.congestion_used.clone())
                {
                    if ctx.streams.iter().any(|s| s.meta.is_sender) {
                        vprintln!("snd_tcp_congestion {c}");
                    } else {
                        vprintln!("rcv_tcp_congestion {c}");
                    }
                }
            }
            (Some(capture.take()), None)
        } else if ctx.want_server_output && (self.json_output || self.json_stream) {
            // A --json-stream server attaches its JSON report too: iperf3
            // keeps json_top alive specifically for this flag
            // (discard_json = json_stream && ... && !(server && get_server_output),
            // iperf_api.c:3900) — without this a real iperf3 client
            // requesting output silently got none (#168).
            let ReportSource::Pending(collected) = report_source else {
                unreachable!("report_source is Pending until this single build");
            };
            let report = self.build_ctx_report(ctx, end, collected);
            let value = serde_json::to_value(&report).ok();
            report_source = ReportSource::Built(Box::new(report));
            (None, value)
        } else {
            (None, None)
        };
        (server_output_text, server_output_json, report_source)
    }

    /// The one `build_report` plumbing site for the pipeline (#289). The
    /// drained collections arrive BY VALUE and move into the report, so the
    /// old "must be built exactly once" comment-invariant is structural
    /// (#287): the pre-exchange --get-server-output build consumes them into
    /// `ReportSource::Built` (#33/#137), else they ride `Pending` to the
    /// single post-exchange build.
    fn build_ctx_report(
        &self,
        ctx: &TestRunCtx,
        end: &EndState,
        collected: crate::reporter::CollectedIntervals,
    ) -> crate::json_report::Report {
        self.build_report(
            &ctx.streams,
            &ctx.cfg,
            &ctx.params,
            &end.cpu_util,
            end.test_duration,
            &ctx.cookie,
            &ctx.accepted_host,
            ctx.accepted_port,
            ctx.test_start_millis,
            collected,
            end.report_error.as_deref(),
        )
    }

    /// ExchangeResults + the DisplayResults / IperfDone end-of-test loop.
    /// Skipped entirely on the terminate paths — the peer is gone (#210) or
    /// the self-terminate relay already went out (#224).
    async fn exchange_results_phase(
        &self,
        ctx: &mut TestRunCtx,
        end: &EndState,
        result_streams: Vec<protocol::StreamResultJson>,
        server_output_text: Option<String>,
        server_output_json: Option<serde_json::Value>,
    ) -> Result<()> {
        if !ctx.client_terminated && ctx.interrupted.is_none() && ctx.server_error.is_none() {
            // Built only when the exchange actually runs — it was dead work
            // on every terminate path (#224).
            let server_results = TestResultsJson {
                cpu_util_total: end.cpu_util.host_total,
                cpu_util_user: end.cpu_util.host_user,
                cpu_util_system: end.cpu_util.host_system,
                // #156: 1 when this side is a retransmit-capable TCP sender
                // (reverse/bidir), like iperf3's check_sender_has_retransmits.
                sender_has_retransmits: if ctx.streams.iter().any(|s| s.meta.is_sender) {
                    i64::from(
                        matches!(ctx.cfg.protocol, TransportProtocol::Tcp)
                            && crate::tcp_info::has_retransmit_info(),
                    )
                } else {
                    -1
                },
                // #37: the congestion algorithm actually in effect (read back at stream
                // creation); None for UDP / unsupported platforms.
                congestion_used: ctx
                    .streams
                    .first()
                    .and_then(|s| s.meta.congestion_used.clone()),
                server_output_text,
                server_output_json,
                streams: result_streams,
            };

            protocol::send_state(&mut ctx.ctrl, TestState::ExchangeResults).await?;
            // #319 (sibling of #268): both post-test reads race the
            // interrupt watch — a client wedging mid-results (or never
            // sending IperfDone) must not hold the server past a signal.
            // GT's server sigend longjmps out of the same reads. On fire:
            // tell the client (best-effort), record the interrupt, and let
            // the finalize phases render the sigend dump.
            let mut exchange_interrupt = self.interrupt.clone().map(|w| w.0);
            // iperf3 protocol: server reads client results first, then sends its
            // own. The client's results are not used in the server's own report —
            // iperf3's server reports only its own measured bytes and a 0 remote
            // CPU (#50).
            let _client_results = tokio::select! {
                r = protocol::recv_results(&mut ctx.ctrl) => r?,
                msg = crate::client::wait_interrupt(exchange_interrupt.as_mut()) => {
                    let _ = protocol::send_state(&mut ctx.ctrl, TestState::ServerTerminate).await;
                    ctx.interrupted = Some(msg);
                    return Ok(());
                }
            };
            protocol::send_results(&mut ctx.ctrl, &server_results).await?;

            // ---- DisplayResults / IperfDone ----
            protocol::send_state(&mut ctx.ctrl, TestState::DisplayResults).await?;

            // Wait for client to send IperfDone
            loop {
                let next = tokio::select! {
                    r = protocol::recv_state(&mut ctx.ctrl) => r,
                    msg = crate::client::wait_interrupt(exchange_interrupt.as_mut()) => {
                        let _ = protocol::send_state(&mut ctx.ctrl, TestState::ServerTerminate).await;
                        ctx.interrupted = Some(msg);
                        return Ok(());
                    }
                };
                match next {
                    Ok(TestState::IperfDone) => break,
                    // #145: AUDITABILITY ONLY — log an out-of-table control
                    // byte in the end-of-test loop, then STILL continue
                    // (behavior unchanged, default-tolerant; iperf3's
                    // end-loop tolerates intervening bytes).
                    Ok(other) => {
                        if !protocol::is_legal_next(
                            TestState::DisplayResults,
                            other,
                            protocol::Role::Server,
                        ) {
                            log::debug!(
                                "server: out-of-sequence control state {other:?} \
                                 in the end-of-test loop (ignored)"
                            );
                        }
                        continue;
                    }
                    Err(RiperfError::PeerDisconnected) => break,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }

    /// The final output dispatch. #220: stream mode WINS when both flags are
    /// set — iperf3's OPT_JSON_STREAM implies -J, so `-s -J --json-stream` IS
    /// stream mode (the client-side dispatch and the CLI error sinks already
    /// follow this rule).
    fn emit_final_output(
        &self,
        ctx: &TestRunCtx,
        end: &EndState,
        report: &crate::json_report::Report,
        was_captured: bool,
    ) {
        if self.json_stream {
            // A terminated/interrupted run emits the discrete `error` event
            // BEFORE `end`, like iperf_json_finish on both roles
            // (iperf_api.c:5310-5323) — without this the r1 stderr gating
            // left the message nowhere in server json-stream mode (#210
            // review r2 d).
            if let Some(e) = &end.report_error {
                crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
                    "error", e,
                ));
            }
            // --json-stream: emit the `end` event (intervals already streamed; #62).
            self.emit_json_stream_end(report);
        } else if self.json_output {
            // Emit the iperf3-schema JSON report on stdout (#50).
            self.print_results_json(report);
        } else if !was_captured && ctx.server_error.is_none() {
            // Print summary: per-stream lines plus aggregate [SUM] row(s) for
            // parallel streams (issue #4), via the shared path the client uses.
            // Also skipped on self-terminate: iperf3 prints NO summary there
            // (#224 GT — text gets only the stderr line, -J still gets the
            // full doc via the branch above).
            // Skipped when --get-server-output already rendered them (#33):
            // the pre-exchange render TEE'd to console + capture (iperf3 also
            // prints at TEST_END, before its exchange), so printing here again
            // would duplicate the lines.
            let summaries = Self::text_summaries(&ctx.streams, end.test_duration, &ctx.cfg);
            let with_retr = summaries.iter().any(|s| s.retransmits.is_some());
            crate::reporter::print_separator();
            // #222 (-V): iperf3 captions the final block; the server closes
            // with its measured side's congestion line (no CPU line — GT).
            if self.verbose {
                vprintln!("Test Complete. Summary Results:");
            }
            crate::reporter::print_final_header(ctx.cfg.protocol, ctx.cfg.bidir, with_retr);
            crate::reporter::print_final_summaries_server(
                &summaries,
                self.format_char,
                self.verbose,
                ctx.cfg.protocol,
            );
            if self.verbose && ctx.streams.iter().any(|s| s.meta.is_sender) {
                // GT gates the CPU line on the SENDING side (iperf_api.c:
                // 4563): a -R server prints it, with ZERO remote figures —
                // the peer's CPU is never exchanged to the server (#50).
                vprintln!(
                    "CPU Utilization: local/sender {:.1}% ({:.1}%u/{:.1}%s), \
                     remote/receiver 0.0% (0.0%u/0.0%s)",
                    end.cpu_util.host_total,
                    end.cpu_util.host_user,
                    end.cpu_util.host_system
                );
            }
            if self.verbose && matches!(ctx.cfg.protocol, TransportProtocol::Tcp) {
                if let Some(c) = ctx
                    .streams
                    .iter()
                    .find_map(|s| s.meta.congestion_used.clone())
                {
                    let is_sender = ctx.streams.iter().any(|s| s.meta.is_sender);
                    if is_sender {
                        vprintln!("snd_tcp_congestion {c}");
                    } else {
                        vprintln!("rcv_tcp_congestion {c}");
                    }
                }
            }
        }
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
        let mut udp_listener = net::udp_bind_reusable(
            self.bind_address.as_deref(),
            self.port,
            self.ip_version,
            self.bind_dev.as_deref(),
        )
        .await?;
        // #316: honor the client's GSO/GRO request on the server's UDP
        // sockets — best-effort like the client's #45 posture (a kernel
        // without UDP_SEGMENT/UDP_GRO degrades to plain sends).
        if cfg.gso {
            let _ = net::set_udp_gso(&udp_listener, cfg.gso_dg_size.clamp(1, 65507) as u16);
        }
        if cfg.gro {
            let _ = net::set_udp_gro(&udp_listener);
        }

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
            // #302 r2: pace EVERY accepted stream — GT's block lives in
            // iperf_udp_accept (iperf_udp.c:581-595), once per stream; the
            // pre-loop listener call covered stream 0 only.
            net::apply_fq_rate(&data_sock, cfg.fq_rate);

            // Create a fresh listener for the next stream (if any)
            if i + 1 < total {
                udp_listener = net::udp_bind_reusable(
                    self.bind_address.as_deref(),
                    self.port,
                    self.ip_version,
                    self.bind_dev.as_deref(),
                )
                .await?;
            } else {
                // Last stream — create a dummy that won't be used
                udp_listener = net::udp_bind(None, 0, false).await?;
            }

            let stream_id = iperf3_stream_id(i);
            let is_sender = i >= recv_count;
            let counters = Arc::new(StreamCounters::new());

            // iperf3 runs iperf_common_sockopts (IP_TOS/IPV6_TCLASS) on UDP
            // stream sockets on BOTH roles — it matters for reverse/bidir,
            // where the server marks egress. Fatal like every other set_tos
            // site (#45). The TCP accept loop has had this since #45; the
            // UDP paths never did (#154). IP_TOS is independent of SO_SND/RCVBUF,
            // so setting it before the window-apply/capture below is equivalent.
            if cfg.tos != 0 {
                net::set_tos(&data_sock, cfg.tos as u32)?;
            }
            // Socket addresses + buffer sizes + the #97 window-clamp check for
            // the `-J` blob (#50), captured before the socket is converted to
            // std + moved. apply_window=true: honor -w/--window on the server's
            // UDP data socket too (#59) so reverse/bidir UDP matches iperf3,
            // before the read-back (#144).
            let sock =
                net::capture_stream_meta(socket2::SockRef::from(&data_sock), cfg.window, true)?;

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
                let uw = cfg.window.is_some();
                let st = start.clone();
                let md = max_duration;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_sender_blocking(
                        std_sock, c, bs, d, rate, pt, bu, uw, u64bit, st, md,
                    )
                });
                streams.push(DataStream {
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock,
                        congestion_used: None,
                    },
                    task,
                    udp_recv_stats: None,
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
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock,
                        congestion_used: None,
                    },
                    task,
                    udp_recv_stats: Some(stats),
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
        let udp_sock = net::udp_bind_reusable(
            self.bind_address.as_deref(),
            self.port,
            self.ip_version,
            self.bind_dev.as_deref(),
        )
        .await?;
        // #302: the demux shared socket IS the data socket — pace it too.
        // NOTE (r2): one shared socket means aggregate pacing = R for -P N
        // (per-stream sockets pace N×R); inherent to the demux design,
        // which is Windows-default (where the sockopt no-ops) and Linux
        // opt-in via RIPERF3_UDP_SERVER_DEMUX.
        net::apply_fq_rate(&udp_sock, cfg.fq_rate);
        // #316: the demux shared socket IS the data socket — same GSO/GRO
        // honor, best-effort.
        if cfg.gso {
            let _ = net::set_udp_gso(&udp_sock, cfg.gso_dg_size.clamp(1, 65507) as u16);
        }
        if cfg.gro {
            let _ = net::set_udp_gro(&udp_sock);
        }

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
                    Ok(Ok(r)) => r,
                    // Reset-class noise: our own UDP_CONNECT_REPLY to a client
                    // port that just closed (e.g. a retry on a fresh socket)
                    // queues WSAECONNRESET on Windows — it must not abort setup
                    // for EVERY stream; skip like the data-phase receivers (#180).
                    Ok(Err(e)) if crate::stream::is_reset_class(&e) => continue,
                    Ok(Err(e)) => return Err(e.into()),
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
        // iperf3 applies IP_TOS/IPV6_TCLASS per UDP stream socket on both
        // roles; every stream here shares this one socket and one cfg.tos,
        // so once-per-socket is semantically identical (#154). Fatal per #45.
        if cfg.tos != 0 {
            net::set_tos(&udp_std, cfg.tos as u32)?;
        }
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
                let uw = cfg.window.is_some();
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
                        uw,
                        u64bit,
                        st,
                        md,
                    )
                });
                streams.push(DataStream {
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock: crate::net::SocketMeta {
                            local_addr,
                            peer_addr: Some(client_addr),
                            sndbuf_actual,
                            rcvbuf_actual,
                        },
                        congestion_used: None,
                    },
                    task,
                    udp_recv_stats: None,
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
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock: crate::net::SocketMeta {
                            local_addr,
                            peer_addr: Some(client_addr),
                            sndbuf_actual,
                            rcvbuf_actual,
                        },
                        congestion_used: None,
                    },
                    task,
                    udp_recv_stats: Some(stats),
                });
            }
        }

        // Spawn the single demux receiver over the shared socket (only if there
        // is anything to receive — a pure-reverse test has senders only).
        if !routes.is_empty() {
            let s = shared.clone();
            let d = done.clone();
            let bs = cfg.blksize;
            *demux_handle = Some(thread_gate.spawn(move || {
                stream::run_udp_server_demux_receiver(s, routes, bs, d, u64bit)
            }));
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
                let bytes = if s.meta.is_sender {
                    s.meta.counters.bytes_sent_net()
                } else {
                    s.meta.counters.bytes_received_net()
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
                } else if is_udp && s.meta.is_sender {
                    // iperf3's sender line shows zero jitter/loss over the
                    // sent datagram count, not blank columns (#184). #256: the
                    // authoritative post-omit datagram count, not bytes/blksize
                    // (== the old value for full-block-only senders).
                    (
                        Some(0.0),
                        Some(0),
                        Some(s.meta.counters.datagrams_sent_net() as i64),
                    )
                } else {
                    (None, None, None)
                };

                crate::reporter::StreamSummary {
                    stream_id: s.meta.id,
                    start: 0.0,
                    end: test_duration,
                    bytes,
                    is_sender: s.meta.is_sender,
                    // TCP sender lines carry the retransmit total (#184).
                    retransmits: s.sender_retransmits(is_udp),
                    jitter,
                    lost,
                    total_packets: total,
                    // Bidir tags every line with the stream's direction (#184).
                    role_tag: cfg
                        .bidir
                        .then_some(crate::reporter::bidir_role_tag(true, s.meta.is_sender)),
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
        collected: crate::reporter::CollectedIntervals,
        error: Option<&str>,
    ) -> crate::json_report::ReportInput {
        use crate::json_report::{
            CpuUtilization, ReportInput, StreamReport, TcpEndExtras, UdpStreamStats,
        };

        // The interval samples + per-stream TCP_INFO extremes the reporter
        // collected, handed in BY VALUE from the single drain point in
        // handle_one_test (#287) — a second build has nothing to drain,
        // structurally.
        let (collected_intervals, extremes) = (collected.intervals, collected.extremes);

        let is_udp = matches!(cfg.protocol, TransportProtocol::Udp);

        let stream_reports: Vec<StreamReport> = streams
            .iter()
            .map(|s| {
                let local_bytes = if s.meta.is_sender {
                    s.meta.counters.bytes_sent_net()
                } else {
                    s.meta.counters.bytes_received_net()
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
                    .meta
                    .sock
                    .local_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_default();
                let (remote_host, remote_port) = s
                    .meta
                    .sock
                    .peer_addr
                    .map(|a| (a.ip().to_canonical().to_string(), a.port()))
                    .unwrap_or_default();

                // Sender-side TCP_INFO extremes + retransmit total, present only
                // for streams the server sent (reverse / bidir).
                let ext = extremes
                    .iter()
                    .find(|e| e.stream_id == s.meta.id && e.has_samples());
                let tcp_end = ext.map(|e| TcpEndExtras {
                    max_snd_cwnd: e.max_snd_cwnd,
                    max_snd_wnd: e.max_snd_wnd,
                    max_rtt: e.max_rtt,
                    min_rtt: e.min_rtt,
                    mean_rtt: e.mean_rtt(),
                    reorder: e.reorder,
                });
                // Retransmits are a sender-side metric. The server only sends on
                // reverse/bidir streams; a stream it received has no retransmit
                // count (None → omitted), so it can't leak a 0 into sum_sent on a
                // forward test (where iperf3's server emits no retransmits).
                let retransmits = if is_udp || !s.meta.is_sender {
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
                    id: s.meta.id,
                    local_host,
                    local_port,
                    remote_host,
                    remote_port,
                    is_sender: s.meta.is_sender,
                    local_bytes,
                    // #256/#283: the authoritative per-stream SENT datagram
                    // count, net of the `-O` omit baseline — the SAME source
                    // #256 feeds to the wire/text per-stream figure
                    // (datagrams_sent_net). Only on UDP streams THIS HOST sent
                    // (reverse/bidir); None for received streams. == local_bytes
                    // / blksize bit-for-bit for a full-block-only sender, so the
                    // -J stays byte-identical.
                    datagrams_sent: (is_udp && s.meta.is_sender)
                        .then(|| s.meta.counters.datagrams_sent_net()),
                    // The server never learns the peer's per-stream bytes; build()
                    // zeroes the un-measured side for is_server reports.
                    remote_bytes: None,
                    // #235: never available server-side — the server prints
                    // before the exchange (iperf_server_api.c:277 vs :280).
                    remote_packets: None,
                    retransmits,
                    tcp_end,
                    udp,
                }
            })
            .collect();

        let input = ReportInput {
            // iperf_exit puts the terminate/interrupt message in the doc's
            // error key on the server too (#210 review r1 f1).
            error: error.map(str::to_string),
            protocol: cfg.protocol,
            reverse: cfg.reverse,
            bidir: cfg.bidir,
            // #265: never consulted server-side (the server gates on its own
            // capability; its received streams are bare regardless).
            peer_sender_has_retransmits: None,
            // #310: the server renders before its exchange — never present.
            peer_congestion_used: None,
            local_has_retransmit_info: crate::tcp_info::has_retransmit_info(),
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
            congestion_used: streams.first().and_then(|s| s.meta.congestion_used.clone()),
            cookie: String::from_utf8_lossy(&cookie[..protocol::COOKIE_SIZE - 1]).to_string(),
            // iperf3's server emits tcp_mss_default = 0 (it never reads the control
            // socket MSS); the requested -M (via params) still surfaces as tcp_mss.
            tcp_mss_default: 0,
            mss: cfg.mss.filter(|&m| m > 0).map(|m| m as u32),
            fq_rate: params.fqrate.unwrap_or(0),
            sock_bufsize: Some(cfg.window.map(|w| w.max(0) as u64).unwrap_or(0)),
            sndbuf_actual: Some(
                streams
                    .first()
                    .and_then(|s| s.meta.sock.sndbuf_actual)
                    .unwrap_or(0),
            ),
            rcvbuf_actual: Some(
                streams
                    .first()
                    .and_then(|s| s.meta.sock.rcvbuf_actual)
                    .unwrap_or(0),
            ),
            // #261/#281: the server only ever assembles a full report on a run
            // that reached TestStart — its upfront refusal renders the skeleton
            // via json_report::error_document, not build(). Always Started/full.
            start_stage: crate::json_report::StartStage::Started,
            bare_end: false,
            // The server reports at its 1s default; it has no -i.
            interval: 1.0,
            // #316: the GSO/GRO request IS exchanged now — GT's server
            // test_start carries the adopted values (live-probed gso:1).
            gso: i32::from(cfg.gso),
            gro: i32::from(cfg.gro),
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
        collected: crate::reporter::CollectedIntervals,
        error: Option<&str>,
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
            collected,
            error,
        )
        .build()
    }

    /// `-J`: pretty-print the server's single batched report blob (#50), or a
    /// prebuilt one (#33).
    fn print_results_json(&self, report: &crate::json_report::Report) {
        // #290: a quiet run returns the report without printing the document.
        if crate::macros::output_quiet() {
            return;
        }
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("riperf3: error - failed to serialize JSON: {e}"),
        }
    }

    /// #260: the upfront IETOTALRATE(27) refusal — GT's get_parameters
    /// total-rate check. Same sink shape as the max-duration refusal below;
    /// iperf_strerror(27) is perr=0, so the message carries no trailing ': '.
    async fn refuse_total_rate(
        &self,
        ctrl: &mut tokio::net::TcpStream,
        target_bitrate: Option<u64>,
    ) -> Result<()> {
        const MSG: &str = "total required bandwidth is larger than server limit";
        protocol::send_server_error(ctrl, 27).await?;
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::error_stream_events(
                &format!("error - {MSG}"),
            ));
        } else if self.json_output {
            if !crate::macros::output_quiet() {
                println!(
                    "{}",
                    crate::json_report::refusal_document(&format!("error - {MSG}"), target_bitrate)
                );
            }
        } else if !crate::macros::output_quiet() {
            eprintln!("riperf3: error - {MSG}");
        }
        Ok(())
    }

    /// #230: refuse a test at param exchange (GT's upfront requested-duration
    /// check). Sends cleanup_server's relay — SERVER_ERROR + the
    /// (IEMAXSERVERTESTDURATIONEXCEEDED=37, errno) pair — then renders GT's
    /// refusal shapes per output mode (live-captured, iperf 3.21): text gets
    /// the one stderr line; -J gets the skeleton error document (no
    /// accepted_connection/cookie — GT skips on_connect on this path); a
    /// --json-stream server gets the error + empty-end event pair with no
    /// start event. Returns Ok: iperf3's one-off exits 0 here, and a
    /// persistent server goes on to serve the next test.
    async fn refuse_max_duration(
        &self,
        ctrl: &mut tokio::net::TcpStream,
        target_bitrate: Option<u64>,
    ) -> Result<()> {
        const MSG: &str =
            "client's requested duration exceeds the server's maximum permitted limit";
        protocol::send_server_error(ctrl, 37).await?;
        if self.json_stream {
            // #198's pre-test error tail (error event + empty end event) is
            // byte-identical to GT's refusal events — reuse it (r1 item 8),
            // routed through the stream emitter for its flush.
            crate::reporter::emit_json_stream_line(&crate::json_report::error_stream_events(
                &format!("error - {MSG}"),
            ));
        } else if self.json_output {
            if !crate::macros::output_quiet() {
                println!(
                    "{}",
                    crate::json_report::refusal_document(&format!("error - {MSG}"), target_bitrate)
                );
            }
        } else if !crate::macros::output_quiet() {
            eprintln!("riperf3: error - {MSG}");
        }
        Ok(())
    }

    /// `--json-stream`: emit the server's `end` event (#62). The interval events
    /// were already streamed live by the reporter.
    // Takes the already-built `Report` (#137: handle_one_test builds it once
    // and returns it) so the report isn't reassembled — the collected
    // intervals moved into it by value at build time (#287).
    fn emit_json_stream_end(&self, report: &crate::json_report::Report) {
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "end",
            &report.end,
        ));
        // --json-stream-full-output: the monolithic document follows the
        // stream, like iperf_json_finish under the flag (#213).
        if self.json_stream_full_output && !crate::macros::output_quiet() {
            println!("{}", serde_json::to_string_pretty(report).unwrap());
        }
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
    ) {
        // Nothing is collected yet at TestStart (the reporter spawns after
        // this event), so the builder gets explicit empty collections (#287).
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
            crate::reporter::CollectedIntervals::default(),
            None,
        );
        crate::reporter::emit_json_stream_line(&crate::json_report::json_stream_event(
            "start",
            &input.build().start,
        ));
    }
}

// ---------------------------------------------------------------------------
// BoundServer (#291)
// ---------------------------------------------------------------------------

/// A [`Server`] bound to its listener (#291) — the accept()-style building
/// block. Obtained from [`Server::bind`]; holds the port across sequential
/// [`run_once`](BoundServer::run_once) calls, so a library caller serving N
/// tests has no rebind gap and can learn a `port(Some(0))` ephemeral
/// assignment up front via [`local_addr`](BoundServer::local_addr).
pub struct BoundServer {
    server: Server,
    listener: tokio::net::TcpListener,
}

impl BoundServer {
    /// The bound listener's local address — the resolved port for a
    /// `port(Some(0))` ephemeral bind.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        self.listener.local_addr().map_err(RiperfError::Io)
    }

    /// Serve exactly one test on the held listener and return its rich
    /// [`Report`](crate::Report) — the same contract as
    /// [`Server::run_once`], minus the per-call rebind. No "Server
    /// listening" banner (a library entry point, not the daemon); the test
    /// report still prints in `-J` / text mode unless the server was built
    /// with `emit_output(false)` (#290).
    pub async fn run_once(&self) -> Result<crate::json_report::Report> {
        self.server.serve_once(&self.listener).await
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
    bind_dev: Option<String>,
    ip_version: Option<u8>,
    timestamps: Option<String>,
    file: Option<String>,
    rsa_private_key_path: Option<String>,
    authorized_users_path: Option<String>,
    time_skew_threshold: u32,
    use_pkcs1_padding: bool,
    json_output: bool,
    emit_output: bool,
    json_stream: bool,
    interrupt: Option<crate::client::InterruptWatch>,
    json_stream_full_output: bool,
    format_char: char,
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
            bind_dev: None,
            ip_version: None,
            timestamps: None,
            file: None,
            rsa_private_key_path: None,
            authorized_users_path: None,
            time_skew_threshold: 10,
            use_pkcs1_padding: false,
            json_output: false,
            emit_output: true,
            json_stream: false,
            interrupt: None,
            json_stream_full_output: false,
            // iperf3 has NO default -f: every figure auto-scales (#221).
            format_char: 'a',
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
    /// When combined with [`Self::json_stream`], stream mode wins (#220).
    pub fn json_output(mut self, enabled: bool) -> Self {
        self.json_output = enabled;
        self
    }

    /// Console output from `run`/`run_once` (#290). `true` (the default)
    /// keeps today's behavior; `false` runs silently — reports flow only via
    /// the return value and the wire (`--get-server-output` still relays the
    /// text report to the requesting client). See
    /// [`ClientBuilder::emit_output`](crate::ClientBuilder::emit_output).
    pub fn emit_output(mut self, enabled: bool) -> Self {
        self.emit_output = enabled;
        self
    }

    /// Stream line-delimited interval JSON during the test (`--json-stream`).
    /// Combined with [`Self::json_output`], stream mode WINS — iperf3's
    /// OPT_JSON_STREAM implies -J (#220), same rule as the client builder.
    pub fn json_stream(mut self, enabled: bool) -> Self {
        self.json_stream = enabled;
        self
    }

    /// Wire an interrupt watch (#210): when the consumer sends a message, a
    /// running test dumps its accumulated stats like iperf_got_sigend, sends
    /// SERVER_TERMINATE to the client, and `run()` returns — the caller owns
    /// the signal-normal exit.
    pub fn interrupt(mut self, rx: tokio::sync::watch::Receiver<Option<String>>) -> Self {
        self.interrupt = Some(crate::client::InterruptWatch(rx));
        self
    }

    /// With json-stream, also print the complete monolithic JSON document
    /// after the stream ends — iperf3's `--json-stream-full-output`, the
    /// third leg of its discard_json condition (#213).
    pub fn json_stream_full_output(mut self, enabled: bool) -> Self {
        self.json_stream_full_output = enabled;
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

    /// `--server-max-duration`: refuse, at param exchange, any test whose
    /// requested duration + omit exceeds `secs` — or whose duration is
    /// unbounded (`-n`/`-k`/`-t 0`) — exactly like iperf3's upfront check
    /// (#230). It arms no timer; the in-flight watchdog is duration-anchored
    /// and independent of this flag. Unset (or 0): no limit.
    pub fn server_max_duration(mut self, secs: u32) -> Self {
        self.server_max_duration = Some(secs);
        self
    }

    /// `-f` unit format for the text report (#242), unit_snprintf chars:
    /// lowercase `kmgt` = bit-rates, UPPERCASE `KMGT` = byte-rates (#241),
    /// `'a'`/`'A'` adaptive. The Transfer column is always adaptive bytes,
    /// like iperf3 (#221); this drives the Bitrate column.
    pub fn format_char(mut self, c: char) -> Self {
        self.format_char = c;
        self
    }

    /// `--forceflush`: force flushing output at every interval.
    pub fn forceflush(mut self, enabled: bool) -> Self {
        self.forceflush = enabled;
        self
    }

    /// `--bind-dev`: bind the listening socket (and the UDP server sockets)
    /// to a network device, like iperf3's netannounce (#149). Linux only:
    /// netannounce applies SO_BINDTODEVICE exclusively (iperf3's macOS
    /// IP_BOUND_IF covers only the CLIENT path, so its macOS server fails
    /// the listen); rejected at `build()` elsewhere.
    pub fn bind_dev(mut self, dev: &str) -> Self {
        self.bind_dev = Some(dev.to_string());
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
        // The SERVER honors --bind-dev only on Linux: iperf3's netannounce
        // applies SO_BINDTODEVICE exclusively (its macOS IP_BOUND_IF support
        // covers only the client's bind_to_device/create_socket path, so a
        // macOS `iperf3 -s --bind-dev` FAILS at listener creation — review
        // r1 ground truth). Rejecting at config time everywhere else matches
        // both that and the no-CAN_BIND_TO_DEVICE unrecognized-option case;
        // a silent no-op bind would be the worst behavior (#149).
        #[cfg(not(target_os = "linux"))]
        if self.bind_dev.is_some() {
            return Err(ConfigError::Unsupported(
                "--bind-dev on the server requires SO_BINDTODEVICE, which this platform lacks"
                    .into(),
            ));
        }

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
            bind_dev: self.bind_dev,
            ip_version: self.ip_version,
            timestamps: self.timestamps,
            file: self.file,
            rsa_private_key_path: self.rsa_private_key_path,
            authorized_users_path: self.authorized_users_path,
            time_skew_threshold: self.time_skew_threshold,
            use_pkcs1_padding: self.use_pkcs1_padding,
            json_output: self.json_output,
            emit_output: self.emit_output,
            json_stream: self.json_stream,
            interrupt: self.interrupt.clone(),
            json_stream_full_output: self.json_stream_full_output,
            format_char: self.format_char,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {

    /// #316: the server adopts the client's exchanged GSO/GRO request
    /// (GT iperf_api.c:2599-2619), including the zero-dg_size recompute
    /// from the negotiated blksize (:2607-2613).
    #[test]
    fn test_config_adopts_the_gsro_block() {
        let params = crate::protocol::TestParams {
            udp: Some(true),
            len: Some(1200),
            gso: Some(1),
            gso_dg_size: Some(0), // old/odd peer: recompute from blksize
            gro: Some(1),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&params).unwrap();
        assert!(cfg.gso && cfg.gro);
        assert_eq!(cfg.gso_dg_size, 1200, "zero dg_size recomputes from len");

        // Absent block (old peer): everything off.
        let params = crate::protocol::TestParams {
            udp: Some(true),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&params).unwrap();
        assert!(!cfg.gso && !cfg.gro);
    }

    use super::*;

    /// Receive one datagram and return the IP_TOS control message byte, via
    /// raw libc recvmsg (nix 0.29's UnknownCmsg fields are private). The
    /// socket must have IP_RECVTOS enabled; with it on, the kernel delivers
    /// the cmsg for every datagram (value 0 included), so asserting the byte
    /// distinguishes "TOS applied" from "TOS defaulted" (#154).
    #[cfg(target_os = "linux")]
    fn recv_udp_tos(sock: &std::net::UdpSocket) -> Option<u8> {
        use std::os::fd::AsRawFd;
        let mut data = [0u8; 2048];
        // u64-aligned backing store: CMSG_FIRSTHDR/NXTHDR deref cmsghdr
        // fields, and a bare [u8] array has alignment 1 (review r1 n6).
        let mut cmsg = [0u64; 8];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr() as *mut _,
            iov_len: data.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        // `as _`: msg_iovlen/msg_controllen are usize on glibc but
        // c_int/socklen_t on musl.
        msg.msg_iovlen = 1 as _;
        msg.msg_controllen = std::mem::size_of_val(&cmsg) as _;
        msg.msg_control = cmsg.as_mut_ptr() as *mut _;
        // SAFETY: valid fd, valid buffers sized above; CMSG_* walk the buffer
        // the kernel just filled within msg_controllen.
        unsafe {
            if libc::recvmsg(sock.as_raw_fd(), &mut msg, 0) < 0 {
                return None;
            }
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::IPPROTO_IP && (*c).cmsg_type == libc::IP_TOS {
                    return Some(*libc::CMSG_DATA(c));
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        None
    }

    /// Sub-ephemeral, PID-windowed UDP port pick for these in-module tests —
    /// the lib unit tests can't reach tests/common's allocator, and a
    /// bind-:0-then-drop probe hands back an ephemeral port a concurrent test
    /// binary's client socket can land on (review r1 n5; the #176 scheme).
    #[cfg(target_os = "linux")]
    fn free_udp_port() -> u16 {
        use std::sync::atomic::AtomicU16;
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let window = 7000 + (std::process::id() % 250) as u16 * 100;
        for _ in 0..100 {
            let port = window + NEXT.fetch_add(1, Ordering::Relaxed) % 100;
            if std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
                return port;
            }
        }
        panic!("no free UDP port in test window {window}-{}", window + 99);
    }

    /// Test client half of the UDP connect handshake + first-data capture:
    /// enables IP_RECVTOS, retries the connect magic until the reply lands,
    /// then reads the first DATA datagram's TOS byte.
    #[cfg(target_os = "linux")]
    fn udp_tos_probe_client(server: std::net::SocketAddr) -> std::thread::JoinHandle<Option<u8>> {
        use std::os::fd::AsRawFd;
        std::thread::spawn(move || {
            let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let on: libc::c_int = 1;
            // SAFETY: plain setsockopt(IP_RECVTOS) on a valid fd.
            let rc = unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_RECVTOS,
                    &on as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                )
            };
            // A silent failure here would burn the recv window and fail with
            // a misleading "left: None" (review r1 n7).
            assert_eq!(rc, 0, "setsockopt(IP_RECVTOS)");
            sock.set_read_timeout(Some(std::time::Duration::from_millis(250)))
                .unwrap();
            let mut reply = [0u8; 4];
            // Handshake: retry the magic until the reply arrives (the server
            // task may bind after our first send).
            for _ in 0..40 {
                let _ = sock.send_to(&protocol::UDP_CONNECT_MSG.to_ne_bytes(), server);
                match sock.recv_from(&mut reply) {
                    Ok((4, _)) => break,
                    _ => continue,
                }
            }
            // First data datagram after TestStart carries the socket's TOS.
            for _ in 0..40 {
                if let Some(tos) = recv_udp_tos(&sock) {
                    return Some(tos);
                }
            }
            None
        })
    }

    /// #154: the server's UDP data sockets must carry IP_TOS — iperf3 runs
    /// iperf_common_sockopts on UDP stream sockets on both roles (matters
    /// for reverse/bidir egress marking). Reverse, one stream: the server is
    /// the sender. Recycling path.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn udp_recycling_server_sender_carries_tos() {
        let port = free_udp_port();

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _ctrl_client = tokio::net::TcpStream::connect(l.local_addr().unwrap())
            .await
            .unwrap();
        let (mut ctrl_srv, _) = l.accept().await.unwrap();

        let srv = ServerBuilder::new()
            .port(Some(port))
            .bind_address("127.0.0.1")
            .build()
            .unwrap();
        let params = TestParams {
            udp: Some(true),
            reverse: Some(true),
            parallel: Some(1),
            tos: Some(0x48),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&params).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(AtomicBool::new(false));
        let mut streams = Vec::new();

        let client = udp_tos_probe_client(format!("127.0.0.1:{port}").parse().unwrap());

        srv.setup_udp_recycling_streams(
            &mut ctrl_srv,
            &cfg,
            0,
            1,
            None,
            &done,
            &start,
            &mut streams,
        )
        .await
        .unwrap();
        start.store(true, Ordering::Relaxed);

        let tos = client.join().unwrap();
        done.store(true, Ordering::Relaxed);
        for s in streams {
            let _ = s.task.await;
        }
        assert_eq!(tos, Some(0x48), "server UDP egress must carry cfg.tos");
    }

    /// #154, demux flavor: one shared socket for every stream — TOS applied
    /// once to it covers all (single cfg.tos). Calling the setup fn directly
    /// avoids the RIPERF3_UDP_SERVER_DEMUX env-var gate (process-global).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn udp_demux_server_sender_carries_tos() {
        let port = free_udp_port();

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _ctrl_client = tokio::net::TcpStream::connect(l.local_addr().unwrap())
            .await
            .unwrap();
        let (mut ctrl_srv, _) = l.accept().await.unwrap();

        let srv = ServerBuilder::new()
            .port(Some(port))
            .bind_address("127.0.0.1")
            .build()
            .unwrap();
        let params = TestParams {
            udp: Some(true),
            reverse: Some(true),
            parallel: Some(1),
            tos: Some(0x48),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&params).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(AtomicBool::new(false));
        let mut streams = Vec::new();
        let mut demux_handle = None;

        let client = udp_tos_probe_client(format!("127.0.0.1:{port}").parse().unwrap());

        srv.setup_udp_demux_streams(
            &mut ctrl_srv,
            &cfg,
            0,
            1,
            None,
            &done,
            &start,
            &mut streams,
            &mut demux_handle,
        )
        .await
        .unwrap();
        start.store(true, Ordering::Relaxed);

        let tos = client.join().unwrap();
        done.store(true, Ordering::Relaxed);
        for s in streams {
            let _ = s.task.await;
        }
        if let Some(h) = demux_handle {
            h.abort();
        }
        assert_eq!(tos, Some(0x48), "demux UDP egress must carry cfg.tos");
    }

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
        fn server_builder_format_char() {
            // #242: -f is wired to Server.format_char (the render sites used
            // to hardcode 'a'); uppercase byte-rate chars survive (#241).
            let s = ServerBuilder::new().format_char('K').build().unwrap();
            assert_eq!(s.format_char, 'K');
            // iperf3 has no default -f: adaptive.
            let s = ServerBuilder::new().build().unwrap();
            assert_eq!(s.format_char, 'a');
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
        // Hostile/broken peers: above MAX_BURST is rejected (no real iperf3
        // client can produce it), non-positive is unset (#160 review r2).
        let p = TestParams {
            tcp: Some(true),
            burst: Some(1001),
            ..Default::default()
        };
        assert!(TestConfig::from_params(&p).is_err());
        let p = TestParams {
            tcp: Some(true),
            burst: Some(-5),
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
