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
    /// #414: the client asked for the repeating payload (GT's ASCII
    /// '0'..'9' pattern, iperf_util.c:85-99) — GT fills it in
    /// iperf_new_stream on BOTH roles (iperf_api.c:4891), so the server's
    /// reverse/bidir senders honor it too. PRESENCE-triggered on the wire
    /// (GT sets 1 whatever the value, iperf_api.c:2645-2646).
    pub repeating_payload: bool,
    /// #414: the client's --dont-fragment — GT sets DF in iperf_init_stream
    /// on BOTH roles, gated UDP && AF_INET (iperf_api.c:4964-4975), so the
    /// server's UDP v4 data sockets carry it on server-sent datagrams.
    /// (The wire `flowlabel` key is deliberately NOT plumbed here: GT's
    /// server ingests it but never applies it to any socket — the only
    /// apply site is the client's iperf_tcp_connect, iperf_tcp.c:521.)
    pub dont_fragment: bool,
}

/// GT's IECTRLCLOSE read-site sentence (iperf_server_api.c:249-254,
/// live-probed #330): any post-accept control EOF prints/docs this BARE
/// (direct iperf_err — no `error - ` prefix), sets IPERF_DONE, and the
/// round ends CLEAN (exit 0, persistent keeps serving).
const CTRL_CLOSED_MSG: &str = "the client has unexpectedly closed the connection";

/// #371: map a POST-TEST_END exchange control-write failure to the
/// IESENDMESSAGE class, preserving the live errno. `send_state`'s only
/// fallible op is `write_all` (→ `RiperfError::Io`); the fallthrough keeps
/// any already-typed error unchanged.
fn exchange_send_message_error(e: RiperfError) -> RiperfError {
    match e {
        RiperfError::Io(io) => RiperfError::ExchangeSendMessageFailed(io),
        other => other,
    }
}

/// #371: the `send_results` sibling → IESENDRESULTS. `send_results`
/// serializes before the socket write, so a (theoretical) serialize error
/// would be non-Io; the fallthrough leaves it unchanged rather than
/// mislabel it. In practice `TestResultsJson` never fails to serialize
/// (non-finite f64 → JSON null, not Err), so the input is always Io.
fn exchange_send_results_error(e: RiperfError) -> RiperfError {
    match e {
        RiperfError::Io(io) => RiperfError::ExchangeSendResultsFailed(io),
        other => other,
    }
}

/// i64→i32 for the adopted dg (#316 r2 F3 / r3 nit): GT's bundled cjson
/// carries `int64_t valueint` (cjson.h:119) and the narrowing happens at
/// the C `int` field assignment (iperf_api.c:2602) — an
/// implementation-defined mod-2^32 WRAP. riperf3 saturates instead;
/// divergence needs a wrap-aliased |dg| >= 2^31 (unreachable from any
/// real iperf3 build — int settings, blksize <= 65507). Either way the
/// value then goes RAW to setsockopt — the kernel is the validator.
fn saturate_i32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
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
            // #415 (r1 F1): a wire `"window": 0` (pre-0.9.0 riperf3 clients
            // sent the key; GT omits it, iperf_api.c:2451) is GT's unset
            // sentinel — normalize here so every consumer rides the unset
            // arm, the UDP senders' `uw` gates included. Nonzero values,
            // negatives included (#392), ingest verbatim.
            window: params.window.filter(|&w| w != 0),
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
            // (iperf_api.c:2607-2613). The len-absent arm (1460) is our
            // choice for a hand-rolled peer: GT's own :2612
            // DEFAULT_UDP_BLKSIZE arm is unreachable there — it would use
            // its stale server-default blksize and let the probe EINVAL
            // (r2 F5) — so 1460 is healthier and equally unreachable from
            // conforming peers.
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
            repeating_payload: params.repeating_payload.is_some(),
            dont_fragment: params.dont_fragment.unwrap_or(0) != 0,
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
    /// Wall-clock millis at ctx creation — GT's setup-doc `timestamp` is
    /// the on-connect stamp, not the emit time (#356 r1 F4): at the default
    /// 120 s rcv-timeout those differ by two minutes.
    accepted_millis: u64,
    /// #392: the setup-doc (sndbuf_actual, rcvbuf_actual) pair, computed
    /// ONCE at ctx construction — GT computes its trio at param ingest
    /// (protocol->listen right after get_parameters, iperf_api.c:2373-2382)
    /// and caches it, so fd exhaustion at EMIT time can't blank the keys.
    /// See [`Server::compute_setup_bufs`].
    setup_bufs: (Option<u64>, Option<u64>),
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
    /// #325: an unhandled control byte — GT's IEMESSAGE class
    /// (iperf_server_api.c:309-311; ONE switch serves every phase). The doc
    /// carries the `error - `-prefixed sentence and the run counts as a
    /// failed test (rc -1 in GT — which main.c does NOT errexit on, so
    /// one-off still exits 0).
    unknown_message: bool,
    /// #325 r2 F1: the final stats dump never ran (a mid-test IEMESSAGE
    /// fires before GT's TEST_END processing) — the -J doc keeps the
    /// accumulated intervals with a BARE `end: {}` (live-verified), and
    /// text prints no summary block, only the stderr line. The end-loop
    /// IEMESSAGE keeps its populated end: there the exchange completed.
    bare_end: bool,
    /// #330: a control-connection EOF mid-test or in the end loop — GT's
    /// IECTRLCLOSE read-site surface ([`CTRL_CLOSED_MSG`], clean round).
    /// Mid-test rides bare_end (no final dump); end-loop keeps the
    /// populated end (the exchange completed) — both live-probed.
    ctrl_closed: bool,
    /// #330: the exchange-phase results read failed — GT's IERECVRESULTS
    /// surface (live-probed): the Nread_json warning already printed to
    /// stderr, the doc keeps the POPULATED end (TEST_END processing ran)
    /// plus `error - unable to receive results: `, exit 0, persistent keeps
    /// serving.
    ///
    /// RECORDED DEVIATION (the dangling `: ` is the #248 perr form at
    /// errno 0): GT appends `strerror` of a leftover errno here — on Linux
    /// deterministically `Transport endpoint is not connected` (ENOTCONN)
    /// across every clean-close probe — while its own warning reports
    /// errno=0. The tail is a semantically-meaningless errno from the
    /// best-effort cleanup path, so we print the honest errno-0 form.
    ///
    /// RECORDED DEVIATION (r2 finding 2 — close-type message flip): GT's
    /// BASE message is not uniformly IERECVRESULTS. On a clean FIN the
    /// half-closed socket still accepts `cleanup_server`'s best-effort
    /// `SERVER_ERROR` write, so `i_errno` stays IERECVRESULTS ("unable to
    /// receive results"). On an RST that write fails and
    /// `iperf_set_send_state` overwrites `i_errno` to IESENDMESSAGE
    /// (iperf_server_api.c:466-472), flipping the rendered key to "unable
    /// to send control message - port may not be available...". That
    /// message MISDESCRIBES the failure (the receive is what failed, not a
    /// send), so per the faithful-ethos ruling we keep the honest
    /// IERECVRESULTS surface for every close-type and file upstream rather
    /// than reproduce the misleading flip.
    exchange_recv_failed: bool,
    /// #406: the IperfDone-wait read failed HARD (peer RST after the
    /// completed exchange) — GT's IERECVMESSAGE read-site class, the error
    /// sibling of `ctrl_closed`'s EOF. Holds the typed
    /// [`RiperfError::ExchangeRecvMessageFailed`] (the #371
    /// exchange_send_error pattern) so the doc key and the Termination
    /// share one rendering.
    exchange_recv_message_error: Option<RiperfError>,
    /// #371: a POST-TEST_END exchange-phase SEND failed (the state writes →
    /// IESENDMESSAGE, `send_results` → IESENDRESULTS). Set instead of
    /// propagating the raw io Err so the finalize renders the POPULATED doc
    /// (the reporter ran at TEST_END) + this typed key; the post-emit gate
    /// then returns it so the serve loop prints GT's line and keeps serving.
    exchange_send_error: Option<RiperfError>,
    /// #330 r1 F1: a mid-test IPERF_DONE — GT's switch has an EXPLICIT
    /// no-error arm for it (iperf_server_api.c:287-288) and Nread wrote
    /// the byte into test->state, so its run loop exits CLEAN: no error
    /// key, no stderr, bare end{}, exit 0 (live-probed). The exchange is
    /// skipped (the peer has left the protocol).
    early_done: bool,
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
    /// `--rcv-timeout` (ms): the no-progress bound on waits that GT caps at
    /// rcv_timeout (#338 CREATE_STREAMS; default 120000 = GT's
    /// DEFAULT_NO_MSG_RCVD_TIMEOUT, iperf_api.h:70).
    pub(crate) rcv_timeout: Option<u64>,
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

/// GT's DEFAULT_NO_MSG_RCVD_TIMEOUT (iperf_api.h:70): the server-side
/// no-progress bound applied when `--rcv-timeout` is unset (#338).
const DEFAULT_RCV_TIMEOUT_MS: u64 = 120_000;

/// How the #338 setup phase ended when it didn't produce data streams.
enum SetupFlow {
    /// All streams accepted — run the test.
    Proceed,
    /// The client sent IPERF_DONE mid-setup — GT's clean arm: the round
    /// ends with no error surface, like a refusal (#356 r1 F1).
    ClientDone,
}

/// GT's IETOTALRATE(27) strerror, wired here to the #260 upfront
/// total-rate refusal only (get_parameters, iperf_api.c:2672-2684); the
/// in-flight 1 Hz breach carries the same strerror through its own site.
const TOTAL_RATE_REFUSAL_MSG: &str = "total required bandwidth is larger than server limit";

/// GT's IEMAXSERVERTESTDURATIONEXCEEDED(37) strerror — the #230 upfront
/// requested-duration refusal (get_parameters, iperf_api.c:2666).
const MAX_DURATION_REFUSAL_MSG: &str =
    "client's requested duration exceeds the server's maximum permitted limit";

/// How the cookie/param phase ended (#386). A refusal has SENT its
/// SERVER_ERROR relay but NOT emitted any doc — the round first parks
/// until client EOF (GT cleanup_server's sync-close drain), and only the
/// park's outcome decides between the refusal doc (EOF) and the interrupt
/// skeleton (signal, doc abandoned).
enum NegotiateOutcome {
    /// Params accepted — run the test (boxed: the tuple dwarfs Refused).
    Proceed(Box<([u8; protocol::COOKIE_SIZE], TestParams, TestConfig)>),
    /// Refused upfront (#230/#260): relay sent, doc deferred to post-park.
    Refused {
        error_line: &'static str,
        refused_rate: Option<u64>,
    },
}

/// What the #338 setup-phase ctrl watch saw.
enum CtrlActivity {
    /// The peer closed the control connection (read-half EOF).
    Eof,
    /// A state byte is waiting; it stays OS-buffered for the mid-test loop.
    Data,
}

/// #356 r1 F7: GT's cleanup_server closes every accepted stream socket on
/// the setup-phase error paths (iperf_server_api.c:460-473); riperf3's
/// spawned tasks would otherwise detach into the runtime parked in read()
/// until the peer closes — an accumulating leak under a persistent server.
/// Abort drops each task's socket at its next await; no counts exist yet,
/// so there is nothing to freeze (the #352 join-site concern doesn't apply).
async fn abort_setup_streams(ctx: &mut TestRunCtx) {
    // #358: `done` first — the UDP paths hold spawn_blocking threads parked
    // on the start barrier / 500 ms-poll reads, where abort() is a no-op
    // and an unset `done` would hang the joins (the #372-gate lesson).
    // Every caller ends the round, so the store is round-terminal by
    // construction; the TCP tasks never needed it (abort suffices).
    ctx.done.store(true, Ordering::Relaxed);
    for s in &ctx.streams {
        s.task.abort();
    }
    for s in ctx.streams.drain(..) {
        let _ = s.task.await;
    }
}

/// #338: wait for control-socket activity without consuming it. This must
/// NOT be `TcpStream::peek` — tokio's poll_peek path leaves winsock
/// readiness corrupted so LATER reads on the same socket never wake (the
/// mid-test loop missed TEST_END forever; deterministic on Windows even
/// with the peek arm never resolving — reproduced natively on Windows 11,
/// see PR #356).
/// `ready()` performs no syscall on the socket, so in the common case
/// (setup completes with no ctrl activity) the socket is untouched; the
/// classifying peek goes straight to the OS via `SockRef`, with `try_io`
/// keeping tokio's WouldBlock/clear-readiness discipline. Cancel-safe: the
/// only await is `ready()`, which holds no I/O state.
async fn ctrl_activity(ctrl: &tokio::net::TcpStream) -> std::io::Result<CtrlActivity> {
    loop {
        ctrl.ready(tokio::io::Interest::READABLE).await?;
        let mut buf = [std::mem::MaybeUninit::<u8>::uninit()];
        match ctrl.try_io(tokio::io::Interest::READABLE, || {
            socket2::SockRef::from(ctrl).peek(&mut buf)
        }) {
            Ok(0) => return Ok(CtrlActivity::Eof),
            Ok(_) => return Ok(CtrlActivity::Data),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
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

    /// Run the persistent server: bind the configured listener, print the
    /// "Server listening" banner, and serve iperf3's accept loop — one test
    /// round per client, round after round — until a wired
    /// [`interrupt`](ServerBuilder::interrupt) fires or `-1/--one-off` ends
    /// it after the first round. This is the daemon-shaped entry the CLI
    /// drives; per-round reports are not returned — use [`Server::run_once`]
    /// or [`Server::bind`] + [`BoundServer::run_once`] to get each round's
    /// [`RunOutcome`](crate::RunOutcome).
    ///
    /// A failed round does not end the loop: like iperf3's server, the round's
    /// error line is printed and the next client is served (#224). `Err` from
    /// this method means a failed listener setup (the bind, a bad bind
    /// address, `--bind-dev`) — never a failed test (even the one-off
    /// `--idle-timeout` expiry ends the loop with `Ok(())`).
    ///
    /// Quiet by default like every lib run (#294): build with
    /// [`emit_output(true)`](ServerBuilder::emit_output) for iperf3's banners
    /// and reports.
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
                // users who want it call `run_once`. #293: a completed round
                // returns its Termination — the per-class stderr line that used
                // to hang off the returned Err now hangs off this match.
                Ok(Some((_report, termination))) => {
                    if !json && !crate::macros::output_quiet() {
                        use crate::outcome::Termination;
                        let prefix = crate::macros::output_timestamp_prefix();
                        match &termination {
                            // #210: IECLIENTTERM, no "error - " prefix.
                            Termination::ClientTerminated => {
                                eprintln!("{prefix}riperf3: {}", RiperfError::ClientTerminated)
                            }
                            // #330: the mid/end-loop control EOF read-site line.
                            Termination::ControlClosed => {
                                eprintln!("{prefix}riperf3: {CTRL_CLOSED_MSG}")
                            }
                            // #325: IEMESSAGE, one line.
                            Termination::UnknownMessage => eprintln!(
                                "{prefix}riperf3: error - {}",
                                RiperfError::UnknownControlMessage
                            ),
                            // #330: IERECVRESULTS, the #248 errno-0 dangling ": ".
                            Termination::RecvResultsFailed => eprintln!(
                                "{prefix}riperf3: error - {}: ",
                                RiperfError::RecvResultsFailed
                            ),
                            // #371: IESENDMESSAGE/IESENDRESULTS — the live errno
                            // rides the carried message (no dangling ": ").
                            Termination::SendFailed(msg) => {
                                eprintln!("{prefix}riperf3: error - {msg}")
                            }
                            // #406: IERECVMESSAGE — the completed-exchange RST
                            // read; live errno in the carried message.
                            Termination::RecvMessageFailed(msg) => {
                                eprintln!("{prefix}riperf3: error - {msg}")
                            }
                            // Completed / Interrupted / SelfTerminated print no
                            // line here: a clean round is silent, and the
                            // interrupt notice / self-terminate line are emitted
                            // elsewhere (the CLI signal path / shutdown_and_flush).
                            _ => {}
                        }
                    }
                }
                // A no-report round (idle-interrupt, refusal, mid-setup DONE):
                // keep serving, nothing to print here.
                Ok(None) => {}
                Err(RiperfError::Aborted(msg)) if msg == "idle timeout" => {
                    idle_restart = true;
                }
                // #293: the MID/END-loop versions of these classes (a report
                // exists) now arrive as `Ok(Some((_, termination)))` above and
                // print via that arm's Termination match. The arms below remain
                // for the PRE-REPORT / setup-phase versions, which still return
                // `Err` (no report) after emitting their own setup doc — the arm
                // matches (and, under -J, does nothing) so the generic `Err(e)`
                // sink below does NOT append a second document.
                Err(RiperfError::ControlSocketClosed) => {
                    // #330: a setup-phase control EOF. GT prints its read-site
                    // sentence once and keeps serving; under -J the setup doc
                    // already carried it. #344: --timestamps stamps the line.
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: {CTRL_CLOSED_MSG}",
                            crate::macros::output_timestamp_prefix()
                        );
                    }
                }
                Err(RiperfError::ClientTerminated) => {
                    // #210: a setup-phase CLIENT_TERMINATE. IECLIENTTERM, no
                    // "error - " prefix; -J carried it in the setup doc.
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: {}",
                            crate::macros::output_timestamp_prefix(),
                            RiperfError::ClientTerminated
                        );
                    }
                }
                Err(RiperfError::UnknownControlMessage) => {
                    // #325: a setup-phase unrecognized byte. IEMESSAGE, one
                    // line; -J carried it in the setup doc.
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - {}",
                            crate::macros::output_timestamp_prefix(),
                            RiperfError::UnknownControlMessage
                        );
                    }
                }
                Err(RiperfError::DataIdleTimeout) => {
                    // #338: GT's rcv_timeout no-progress bound at the
                    // CREATE_STREAMS wait — IENOMSG(144). The doc emitted at
                    // the setup site; text prints iperf_err's stamped line.
                    // Keep-serving, exit 0 (GT's rc -1 class, like IEMESSAGE).
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - {}",
                            crate::macros::output_timestamp_prefix(),
                            RiperfError::DataIdleTimeout
                        );
                    }
                }
                Err(e @ RiperfError::StreamConnectFailed(_)) => {
                    // #362: the data-accept kill — the populated setup doc
                    // emitted at the site; text prints the strerror'd line
                    // (live errno, no dangling). Keep-serving, exit 0.
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - {e}",
                            crate::macros::output_timestamp_prefix()
                        );
                    }
                }
                Err(RiperfError::RecvDataCookieFailed) => {
                    // #359 r1 F2: GT's hard-read-error kill at the DATA
                    // cookie gate (IERECVCOOKIE via cleanup_server). The
                    // populated setup doc emitted at the site; text prints
                    // the prefixed dangling perr line (#248 errno-0 form —
                    // GT appends the live/stale strerror, live "Bad file
                    // descriptor"). Keep-serving, exit 0.
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - {}: ",
                            crate::macros::output_timestamp_prefix(),
                            RiperfError::RecvDataCookieFailed
                        );
                    }
                }
                // #330: the pre-test control failures. Unlike the classes
                // above — which build a partial report inside handle_one_test
                // and carried the error in it — these error BEFORE any report
                // exists, so the serve loop emits GT's iperf_err sink shape
                // directly: silent stderr + a SKELETON accumulated doc under
                // -J, one text line otherwise.
                Err(e @ RiperfError::RecvCookieFailed)
                | Err(e @ RiperfError::RecvParamsFailed)
                | Err(e @ RiperfError::SendControlFailed(_))
                // #362: IEACCEPT / IESETNODELAY ride the same pre-test
                // sink (their Displays carry the live strerror, so no
                // dangling suffix — the SendControlFailed convention).
                | Err(e @ RiperfError::AcceptFailed(_))
                | Err(e @ RiperfError::SetNoDelayFailed(_)) => {
                    self.emit_pretest_error(&e);
                }
                Err(RiperfError::AccessDenied) => {
                    // #377 r2 F1: the -J denial doc (with the ingested -b)
                    // emitted at the auth site, where params is in scope;
                    // only the text line prints here. #395: GT's runtime
                    // auth deny never stamps i_errno, so the line renders
                    // iperf_strerror(0) — "no error" — not the lib error's
                    // Display. (Fresh-process string; the stale-i_errno
                    // multi-round wrinkle is recorded at the auth gate.)
                    if !json && !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - no error",
                            crate::macros::output_timestamp_prefix()
                        );
                    }
                }
                Err(e) => {
                    // Any residual pre-report error rides the same sink: under
                    // -J iperf_err stays silent and puts the message in a
                    // skeleton doc, rather than the raw stderr line GT never
                    // emits in JSON mode (#330 divergence 1).
                    if json {
                        self.emit_pretest_error_doc(&format!("error - {e}"), None);
                    } else if !crate::macros::output_quiet() {
                        eprintln!(
                            "{}riperf3: error - {e}",
                            crate::macros::output_timestamp_prefix()
                        );
                    }
                }
            }

            // #210: an interrupted run stops serving — handle_one_test
            // already dumped its stats and told the client (or, interrupted
            // while idle, emitted the #346 skeleton at the accept site);
            // the caller owns the signal-normal exit.
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

    /// Serve exactly one test and return its [`RunOutcome`](crate::RunOutcome)
    /// — the rich JSON [`Report`](crate::Report) the test measured (the same
    /// object [`Client::run`](crate::Client::run) returns and `-s -J` prints)
    /// plus how the round ended.
    ///
    /// Binds its own listener, accepts one client, runs the test to completion,
    /// and returns. This is the one-shot building block, mirroring
    /// `tokio::net::TcpListener::accept`; use [`Server::run`] for the long-lived
    /// accept loop. Unlike `run`, it does not print the "Server listening"
    /// banner — it is a library entry point, not the daemon. Like `Client::run`,
    /// it is quiet by default (#294): build with `emit_output(true)` to print
    /// iperf3's full `-J` / text output.
    ///
    /// #293: returns a [`RunOutcome`](crate::RunOutcome) — the measured
    /// [`Report`](crate::Report) plus a [`Termination`](crate::Termination)
    /// saying how the round ended. A peer-caused ending that still produced a
    /// report is `Ok` with the matching server-side `Termination`
    /// (`ClientTerminated`, `ControlClosed`, `UnknownMessage`,
    /// `RecvResultsFailed`, `SendFailed`), or `SelfTerminated` when the server
    /// ended the test on its own limit; the report carries the partial stats.
    /// `Err` is reserved for rounds that produced NO report — a pre-test
    /// cookie/param/accept failure, or a round interrupted before any test
    /// started.
    pub async fn run_once(&self) -> Result<crate::outcome::RunOutcome> {
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
    ) -> Result<crate::outcome::RunOutcome> {
        // #290: run-scoped console silence for this test.
        let _quiet_guard = (!self.emit_output).then(crate::macros::OutputQuietGuard::set);
        match self.handle_one_test(listener).await? {
            // #293: every report-producing round returns its Termination.
            Some((report, termination)) => Ok(crate::outcome::RunOutcome::new(report, termination)),
            // The no-report rounds: interrupted while idle (an interrupt
            // watch), a refused test, or IPERF_DONE mid-setup (#356) — no
            // report exists, so `Err` per the #293 rule.
            None => Err(RiperfError::Aborted(
                "interrupted before a test started".into(),
            )),
        }
    }

    async fn handle_one_test(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<Option<(crate::json_report::Report, crate::outcome::Termination)>> {
        // ---- Accept control connection (with optional idle timeout) ----
        let (mut ctrl, peer_addr) = match self.accept_control(listener).await? {
            Some(accepted) => accepted,
            // Interrupted while idle (no client): no test ran, so no report.
            // #346: THIS is the only doc-less None — refusals and
            // IPERF_DONE-at-setup rounds return None after emitting their
            // own docs — so the idle-interrupt skeleton emits here, not at
            // the serve loop (where it would double-emit in a
            // signal-during-round race). GT's signormalexit shape,
            // live-probed: skeleton doc / error+bare-end pair with the
            // interrupt-class key (no prefix), silent stderr, exit 0.
            None => {
                if let Some(w) = &self.interrupt {
                    if let Some(msg) = w.0.borrow().clone() {
                        self.emit_pretest_error_doc(&msg, None);
                    }
                }
                return Ok(None);
            }
        };
        // (#222 r1 item 6: the Time/banner/Cookie/MSS block prints AFTER the
        // param exchange — GT's iperf_on_connect fires there — so a
        // --get-server-output capture relays it; see print_connect_block.)
        // #362: GT classes a failed TCP_NODELAY on the just-accepted ctrl
        // as IESETNODELAY(122) (iperf_server_api.c:170-173) — the macOS
        // kind-only-InvalidInput cell's likeliest site. GT's cleanup_server
        // relays fe+122+errno on the live ctrl (ctrl_sck is set BEFORE the
        // sockopt, :169) — best-effort here like the sibling relays (r1 F5).
        if let Err(e) = net::configure_tcp_stream(&ctrl, true) {
            let err = match e {
                RiperfError::Io(io) => {
                    let errno = io.raw_os_error().unwrap_or(0) as u32;
                    let _ = protocol::send_server_error_errno(&mut ctrl, 122, errno).await;
                    RiperfError::SetNoDelayFailed(io)
                }
                other => other,
            };
            return Err(err);
        }

        // The control-socket peer address feeds the server's `start.accepted_connection`
        // (iperf_api.c uses getpeername(ctrl_sck) — distinct from the data-stream
        // addresses in `connected[]`). Captured for the `-J` blob (#50).
        // `to_canonical()` unwraps an IPv4-mapped IPv6 address (`::ffff:127.0.0.1`)
        // from the dual-stack listener back to plain `127.0.0.1`, as iperf3 does
        // (mapped_v4_to_regular_v4).
        let (accepted_host, accepted_port) =
            (peer_addr.ip().to_canonical().to_string(), peer_addr.port());

        // ---- Cookie + ParamExchange (+ the #230 upfront max-duration refusal) ----
        // #361: the cookie/params reads race the interrupt watch — GT's
        // sigend exits IMMEDIATELY from this window with the #346 skeleton
        // (live: exit 0, dt 0.00); the old interrupt-blind window rode the
        // CLI's 5 s dump wall with EMPTY -J stdout against a wedged
        // client. (The #158 second-signal test's deterministic wedge moved
        // one phase later, to the interrupt-blind setup wait.)
        let mut neg_interrupt = self.interrupt.clone().map(|w| w.0);
        let negotiated = tokio::select! {
            r = self.negotiate_test(&mut ctrl) => r?,
            msg = crate::client::wait_interrupt(neg_interrupt.as_mut()) => {
                self.emit_pretest_error_doc(&msg, None);
                return Ok(None);
            }
        };
        let (cookie, params, cfg) = match negotiated {
            NegotiateOutcome::Proceed(negotiated) => *negotiated,
            // Refused before any test ran → no report. #386: the round does
            // NOT end at the relay — it parks until the client closes OR
            // 10 s of ctrl silence (GT cleanup_server's sync-close drain,
            // which is BOUNDED by Nread's front-select — see
            // park_refused_round), and only then renders the refusal
            // sinks; a signal landing in the park ABANDONS the refusal doc
            // and emits the interrupt skeleton alone, carrying the parked
            // round's target_bitrate (GT stamps json_start at
            // get_parameters and sigend's json_finish renders it — cell B
            // live-probed). The park sits OUTSIDE the #361 select above so
            // that select's rate-less skeleton can't win the signal race.
            NegotiateOutcome::Refused {
                error_line,
                refused_rate,
            } => {
                match self.park_refused_round(&mut ctrl).await {
                    None => self.emit_refusal(error_line, refused_rate),
                    Some(msg) => self.emit_pretest_error_doc(&msg, refused_rate),
                }
                return Ok(None);
            }
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

        // ---- Auth validation (after params, before streams) ----
        if self.authenticate(&params).await.is_err() {
            // #377 r2 F1: GT's auth gate runs AFTER get_parameters
            // (iperf_api.c:2368 vs :2662), so the denial doc carries
            // the early target_bitrate like the #260 refusal twins.
            // Emitted HERE — params is out of scope at the serve
            // loop's arm, which prints only the text line.
            // #395: EVERY runtime auth failure (tokenless, undecodable
            // token, failed credential check) shares GT's deny surface —
            // test_is_authorized returns -1 WITHOUT stamping i_errno
            // (iperf_api.c:2313-2343), so a fresh GT process renders
            // iperf_strerror(0), "no error". RECORDED DEVIATION (r1 F1,
            // r2 F1): GT never RESETS the global i_errno between rounds,
            // so a multi-round GT server whose EARLIER round stamped an
            // errno renders that stale string on a later deny (live-
            // probed: cookie-EOF round, then deny → "unable to receive
            // cookie" twice) AND writes a 0xFE SERVER_ERROR block instead
            // of the bare FIN (cleanup_server's wire-back gates on
            // `i_errno != IENONE`, iperf_server_api.c:465-473). riperf3
            // always takes the fresh-process surface — bare close, "no
            // error". The underlying error never reaches a surface; the
            // lib normalizes to `AccessDenied`.
            self.emit_pretest_error_doc("error - no error", params.bandwidth.filter(|&b| b > 0));
            return Err(RiperfError::AccessDenied);
        }

        // #395: GT's on_connect fires only after iperf_exchange_parameters —
        // auth gate included — succeeds (iperf_server_api.c:207-214); a
        // denied round prints the listen banner and nothing else.
        self.print_connect_block(peer_addr, &cookie, &params, &cfg);

        // The test's accumulated state, threaded through the pipeline phases
        // (#289) — field docs on TestRunCtx.
        let mut ctx = TestRunCtx {
            ctrl,
            accepted_host,
            accepted_port,
            // #392: param ingest is GT's compute point for the setup trio.
            setup_bufs: Self::compute_setup_bufs(&cfg, listener),
            accepted_millis: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
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
            unknown_message: false,
            ctrl_closed: false,
            early_done: false,
            exchange_recv_failed: false,
            exchange_recv_message_error: None,
            exchange_send_error: None,
            bare_end: false,
            interrupted: None,
        };
        // #380: if THIS FUTURE is dropped (a run_once wrapped in
        // timeout/select) the abort/join gate below never runs and `done`
        // can't wake a parked read — this guard abort()s the stream tasks
        // instead. (The chained UDP demux handle only stops a queued-not-
        // yet-started runner: the demux is spawn_blocking, and abort() is
        // a no-op once it runs — it exits via `done` + its 500 ms poll.)
        // Armed after setup, disarmed after the gate's joins. Declared
        // BEFORE _done_guard so the cancel-drop stores `done` first, then
        // aborts — the gate's done-store-BEFORE-abort order (the recycled-
        // raw_fd record below).
        let mut abort_guard = stream::AbortStreamsOnDrop::new();
        // Signal `done` on every exit path (incl. early `?` returns) so a UDP
        // sender parked on the start barrier can't leak if setup fails (#5).
        // Declared AFTER ctx so an early return drops the guard FIRST —
        // `done` is set before ctx's fields (the control socket, the capture
        // guard) drop, the monolith's drop order (r1 F1).
        let _done_guard = stream::DoneOnDrop(ctx.done.clone());

        // #372: every phase from setup to the final output runs inside
        // this block, so every `?` — the setup-phase sites (#381),
        // start_test's sends, await_test_end's non-EOF recv errors, the
        // exchange phase (#353) — falls through to the ONE unconditional
        // abort/join gate below instead of skipping it. Per-arm teardown
        // wrappers were whack-a-mole: this is the 4th site in the family
        // (#331 gate, #353 exchange, #372 running phase, #381 setup). A
        // `return` inside the block exits the BLOCK, not handle_one_test.
        let outcome: Result<Option<crate::json_report::Report>> = async {
            // ---- CreateStreams ----
            // #381: setup runs INSIDE the block — its `?` sites (the
            // post-accept configure/tos/meta, dispatch propagations, the
            // UDP sub-setups) previously skipped the gate with earlier-
            // iteration tasks already spawned in ctx.streams. Every spawn
            // site (the TCP loop, both UDP sub-setups, the demux) also
            // pushes its task into the abort guard AS IT SPAWNS (the #426
            // r1 F2 mid-setup cancel window). The classified arms still
            // reap inline first — the gate below is a no-op on drained
            // streams.
            // Box::pin (#427: the Windows stack-overflow incident): nesting
            // setup INSIDE this block put its entire poll frame on top of
            // the block's own — MSVC debug frames tipped the CLI server
            // (block_on on Windows' 1 MB main-thread stack) into
            // "thread 'main' has overflowed its stack" at the first accept,
            // killing every bidir_intervals cell while Linux (8 MB main)
            // and worker/test threads (2 MB) never noticed. Boxing moves
            // setup's state out of the enclosing frame; behavior identical.
            if let SetupFlow::ClientDone =
                Box::pin(self.setup_data_streams(&mut ctx, listener, &mut abort_guard)).await?
            {
                // #356 r1 F1: GT's clean IPERF_DONE arm — the round ends
                // with no error surface, keep-serving like a refusal
                // (None maps to handle_one_test's Ok(None) after the gate).
                return Ok(None);
            }
            // #380: the full arm — every real task was already pushed at
            // its spawn site (arm replaces with the same set, plus the
            // demux arm's already-resolved placeholder dummies, a no-op
            // superset). Kept as the one canonical post-setup statement of
            // the guarded set.
            abort_guard.arm(
                ctx.streams
                    .iter()
                    .map(|s| s.task.abort_handle())
                    .chain(ctx.udp_demux_handle.iter().map(|h| h.abort_handle())),
            );

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
            // #353: an exchange Err would reach the gate through this `?` —
            // counts are frozen at build_result_streams, so the teardown is
            // safe on this path. Post-#405 the send failures are caught into
            // ctx.exchange_send_error, and post-#406 the IperfDone-loop hard
            // read error is caught into ctx.exchange_recv_message_error —
            // every exchange ending now returns Ok with its ctx flag, so no
            // LIVE Err remains through this `?` (defensive only).
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
                } else if ctx.client_terminated {
                    // #325: an end-loop CLIENT_TERMINATE postdates EndState's
                    // resolution exactly like the #322 mid-exchange interrupt.
                    end.report_error = Some("the client has terminated".to_string());
                } else if ctx.unknown_message {
                    // #325: GT's IEMESSAGE reaches the doc through main.c:174's
                    // `iperf_err(test, "error - %s", ...)` json sink — the
                    // in-doc value carries the prefix (live-captured), unlike
                    // IECLIENTTERM's bare sentence above.
                    end.report_error =
                        Some(format!("error - {}", RiperfError::UnknownControlMessage));
                } else if ctx.ctrl_closed {
                    // #330: IECTRLCLOSE's read-site line is a DIRECT iperf_err
                    // — bare, no prefix (live-probed both windows).
                    end.report_error = Some(CTRL_CLOSED_MSG.to_string());
                } else if ctx.exchange_recv_failed {
                    // #330: the perr dangling `: ` at errno 0 (#248 form; see
                    // the field's deviation record).
                    end.report_error =
                        Some(format!("error - {}: ", RiperfError::RecvResultsFailed));
                } else if let Some(err) = &ctx.exchange_send_error {
                    // #371: GT's IESENDMESSAGE/IESENDRESULTS key over the
                    // populated doc — prefixed like the self-terminate keys,
                    // with the live strerror the variant's Display carries.
                    end.report_error = Some(format!("error - {err}"));
                } else if let Some(err) = &ctx.exchange_recv_message_error {
                    // #406: GT's IERECVMESSAGE key over the populated doc —
                    // prefixed, live strerror (see the variant's deviation
                    // record for GT's clobbered loopback observable).
                    end.report_error = Some(format!("error - {err}"));
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
            Ok(Some(report))
        }
        .await;

        // Join stream tasks (best-effort, they should be done).
        // #322 r1 F1: a mid-EXCHANGE interrupt postdates shutdown_and_flush's
        // abort gate — abort here so a wedged peer can't park the joins.
        // #325 r3 F1: an END-LOOP IEMESSAGE postdates it the same way, and
        // r4 NF-1: so does an end-loop CLIENT_TERMINATE — GT's terminate arm
        // closes the stream sockets INLINE (iperf_server_api.c:301-305), so
        // a peer that sends 12 and then holds its sockets must not park the
        // joins (it held the process for the peer's whole hold; GT exits on
        // its own clock). Covers the pre-existing mid-test terminate too.
        // #330: ctrl_closed joins — a peer may EOF the control socket
        // while holding the DATA sockets open; GT's cleanup closes them.
        // #331: the SUCCESS path parks the same way — a completed peer that
        // holds its data sockets leaves receivers in read(), so the old
        // abnormal-end gate is now unconditional. GT closes every stream
        // socket at TEST_END (iperf_server_api.c:272-275); closing at
        // riperf3's literal TestEnd arm would race the #55/#159 catch-up
        // window and drift byte counts, so the safe equivalent is here:
        // counts are frozen at build_result_streams + the exchange, and
        // every sink has emitted. Abort drops each tokio task's socket at
        // its next await; the spawn_blocking UDP runners are bounded by the
        // 500 ms read-timeout + `done` polling either way.
        // #372: `done` must be set HERE, not only by the drop guard — on
        // the block's Err paths shutdown_and_flush never ran, and a
        // spawn_blocking UDP runner ignores abort() and exits via `done` +
        // its 500 ms read-timeout poll; joining it with `done` unset would
        // hang. Idempotent on the clean path (shutdown_and_flush set it).
        // The interval reporter (ctx.interval_handle) is deliberately NOT
        // reaped here (#379 r1 F2 record): on the Err paths it
        // self-terminates detached via `done` (bounded: its ~1 s tick +
        // 2 s wait), prints nothing post-done, and holds no peer-visible
        // fd — while an abort could kill a text-mode emit mid-line. The
        // done-store-BEFORE-abort order also keeps a post-close reporter
        // tick from sampling a recycled raw_fd.
        ctx.done.store(true, Ordering::Relaxed);
        for s in &ctx.streams {
            s.task.abort();
        }
        if let Some(h) = &ctx.udp_demux_handle {
            h.abort();
        }
        for s in ctx.streams.drain(..) {
            let _ = s.task.await;
        }
        // The single-socket UDP demux receiver (#80) serves all receiving streams
        // and lives outside `streams`; join it too. `None` on the recycling path.
        if let Some(h) = ctx.udp_demux_handle.take() {
            let _ = h.await;
        }
        // #380 (#426 r1 F1): disarmed only NOW — the guard stays armed
        // through the gate's own join awaits, where a cancel would
        // otherwise land disarmed-but-unaborted and leak. abort() is
        // idempotent, so the guard firing mid-join is free.
        abort_guard.disarm();

        let report = match outcome? {
            Some(report) => report,
            // #356 r1 F1 / #381: the clean IPERF_DONE round — keep-serving,
            // no error surface. The gate above was a no-op (dispatch's
            // IperfDone arm reaped inline via abort_setup_streams).
            None => return Ok(None),
        };

        // #293: the doc above already rendered on every abnormal path. Derive
        // how the round ended — this drives BOTH run_once's RunOutcome and
        // run()'s per-class stderr line (run() matches this Termination now,
        // where it used to match the returned Err). The comments on each class
        // (rc -1 keep-serving, GT's read-site line, the errno-0 dangling ": ",
        // etc.) live on the corresponding run() arm.
        //
        // r1 F4: the precedence matches report_error's derivation (the -J doc's
        // `error` key) at both its sites — interrupt first, then the peer
        // terminate, the self-terminate, then the exchange classes (server.rs
        // ~2247 and ~1037). The flags are mutually exclusive today (each
        // abnormal arm breaks the data loop and the exchange phase is gated off
        // once any is set), so ordering is behavior-neutral now; keeping the two
        // derivations in lockstep means run_once's Termination and the doc's key
        // stay consistent if a future change ever let two co-occur.
        let termination = if ctx.interrupted.is_some() {
            crate::outcome::Termination::Interrupted
        } else if ctx.client_terminated {
            crate::outcome::Termination::ClientTerminated
        } else if let Some(msg) = ctx.server_error {
            crate::outcome::Termination::SelfTerminated(msg.to_string())
        } else if ctx.unknown_message {
            crate::outcome::Termination::UnknownMessage
        } else if ctx.ctrl_closed {
            crate::outcome::Termination::ControlClosed
        } else if ctx.exchange_recv_failed {
            crate::outcome::Termination::RecvResultsFailed
        } else if let Some(err) = ctx.exchange_send_error.take() {
            crate::outcome::Termination::SendFailed(err.to_string())
        } else if let Some(err) = ctx.exchange_recv_message_error.take() {
            // #406: same slot order as report_error's site above.
            crate::outcome::Termination::RecvMessageFailed(err.to_string())
        } else {
            // Clean completion (incl. the #330 mid-test IPERF_DONE bare-end).
            crate::outcome::Termination::Completed
        };
        Ok(Some((report, termination)))
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
    /// serve loop. The cookie/param reads race the interrupt watch too
    /// (#361 — GT's sigend exits immediately from any phase); the remaining
    /// interrupt-blind window is the CREATE_STREAMS setup wait, where the
    /// #158 second-signal wedge test now parks (recorded on #361).
    async fn accept_control(
        &self,
        listener: &tokio::net::TcpListener,
    ) -> Result<Option<(tokio::net::TcpStream, std::net::SocketAddr)>> {
        let mut accept_interrupt = self.interrupt.clone().map(|w| w.0);
        let accepted = tokio::select! {
            r = async {
                // #362: an accept() failure is GT's IEACCEPT
                // (iperf_server_api.c:163) — previously the raw io line
                // on the generic arm (BSD ECONNABORTED / Linux EMFILE).
                if let Some(secs) = self.idle_timeout {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(secs as u64),
                        listener.accept(),
                    )
                    .await
                    {
                        Ok(result) => result.map_err(RiperfError::AcceptFailed),
                        Err(_) => Err(RiperfError::Aborted("idle timeout".into())),
                    }
                } else {
                    listener.accept().await.map_err(RiperfError::AcceptFailed)
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

    /// Cookie read + ParamExchange + config derivation, plus the #230/#260
    /// upfront refusal checks. A `Refused` outcome has SENT its relay but
    /// NOT emitted its doc: the round must first PARK until client EOF or
    /// GT's 10 s drain bound (#386 — cleanup_server's sync-close drain),
    /// owned by handle_one_test so the park is not raced by the #361
    /// negotiate-phase interrupt select (whose skeleton carries no
    /// target_bitrate — the parked round's must, GT cell B).
    async fn negotiate_test(&self, ctrl: &mut tokio::net::TcpStream) -> Result<NegotiateOutcome> {
        // ---- Cookie ----
        // #330: a failed cookie read is GT's IERECVCOOKIE(106) — iperf_accept
        // errors and cleanup_server relays SERVER_ERROR(-2) + the code before
        // the serve loop renders the exit-0 keep-serving surface (skeleton -J
        // doc / one text line). The wire-back is best-effort: a peer that
        // already closed (a port scan) just no-ops the send, like GT's Nwrite.
        let cookie = match protocol::recv_cookie(ctrl).await {
            Ok(cookie) => cookie,
            Err(_) => {
                let _ = protocol::send_server_error(ctrl, 106).await;
                return Err(RiperfError::RecvCookieFailed);
            }
        };

        // ---- ParamExchange ----
        // #345: a failed post-cookie state write is GT's IESENDMESSAGE(111)
        // — iperf_accept's iperf_set_send_state failure path; cleanup_server
        // relays SERVER_ERROR(-2)+111 (best-effort and usually unobservable:
        // the peer's RST is what broke the write — unpinned by design).
        if let Err(e) = protocol::send_state(ctrl, TestState::ParamExchange).await {
            let _ = protocol::send_server_error(ctrl, 111).await;
            return Err(match e {
                RiperfError::Io(io) => RiperfError::SendControlFailed(io),
                // send_state is one write_all, so only Io reaches here today
                // (r1 F3) — the arm fails safe if that ever changes.
                other => other,
            });
        }
        // #330: GT's get_parameters sets IERECVPARAMS(114) whenever JSON_read
        // returns NULL — a short/absent body OR a cJSON parse failure alike —
        // and relays SERVER_ERROR(-2) + the code through cleanup_server.
        let mut params = match protocol::recv_params(ctrl).await {
            Ok(params) => params,
            Err(_) => {
                let _ = protocol::send_server_error(ctrl, 114).await;
                return Err(RiperfError::RecvParamsFailed);
            }
        };
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
            // #260: cleanup_server's relay — SERVER_ERROR + (IETOTALRATE=27,
            // errno) — then the #386 park before any doc renders.
            protocol::send_server_error(ctrl, 27).await?;
            return Ok(NegotiateOutcome::Refused {
                error_line: TOTAL_RATE_REFUSAL_MSG,
                refused_rate,
            });
        }
        if duration_violated {
            // #230: the IEMAXSERVERTESTDURATIONEXCEEDED(37) relay, same shape.
            protocol::send_server_error(ctrl, 37).await?;
            return Ok(NegotiateOutcome::Refused {
                error_line: MAX_DURATION_REFUSAL_MSG,
                refused_rate,
            });
        }
        Ok(NegotiateOutcome::Proceed(Box::new((cookie, params, cfg))))
    }

    /// #222: the connect text block, in GT's order and GT's TIMING —
    /// iperf_on_connect fires post-param-exchange AND post-auth (#395:
    /// iperf_server_api.c:213 runs only after iperf_exchange_parameters,
    /// auth gate included, succeeds — a denied round prints no block),
    /// which also puts these lines inside the --get-server-output capture
    /// (r1 item 6). The banner is unconditional in text mode; the rest is
    /// -V. The server's control MSS is 0 "(default)" (ctrl_sck_mss, r1
    /// item 2).
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

    /// Auth validation (after params, before streams). #395: GT signals an
    /// auth deny with a bare control-socket close — test_is_authorized's -1
    /// aborts iperf_exchange_parameters before any state byte
    /// (iperf_api.c:2368); the 0xFF `ACCESS_DENIED` byte is exclusively the
    /// busy-server signal (iperf_server_api.c:222). No wire write here on
    /// ANY failure leg.
    async fn authenticate(&self, params: &TestParams) -> Result<()> {
        if let (Some(ref privkey_path), Some(ref users_path)) =
            (&self.rsa_private_key_path, &self.authorized_users_path)
        {
            if let Some(ref token) = params.authtoken {
                let privkey_pem = std::fs::read(privkey_path).map_err(|e| {
                    RiperfError::Protocol(format!("cannot read RSA private key: {e}"))
                })?;
                let (username, password, ts) =
                    crate::auth::decode_auth_token(token, &privkey_pem, self.use_pkcs1_padding)?;
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
            } else {
                // Server requires auth but client didn't send token — same
                // bare close.
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
        abort_guard: &mut stream::AbortStreamsOnDrop,
    ) -> Result<SetupFlow> {
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

                // #338: GT's CREATE_STREAMS wait runs inside its select()
                // event loop, so a ctrl-EOF is noticed at once (IECTRLCLOSE,
                // iperf_server_api.c:249-254) and a no-progress round is
                // bounded at rcv_timeout (IENOMSG=144, :663-678). The bare
                // accept parked unbounded on both. The ctrl watch is
                // readiness-only (see ctrl_activity for why it must not be
                // TcpStream::peek); a waiting state byte is then DISPATCHED
                // through GT's handle_message_server arms below (:236-311,
                // #356 r1 F1) — consuming it keeps the no-progress clock
                // honest (each byte is receive progress, GT's
                // last_receive_time semantics) and a byte can never wedge
                // the select in a ready-spin.
                let rcv_timeout = std::time::Duration::from_millis(
                    self.rcv_timeout.unwrap_or(DEFAULT_RCV_TIMEOUT_MS),
                );
                // #383 r1 F2: GT arms the CREATE_STREAMS no-progress bound
                // only when the server RECEIVES (test->mode != SENDER,
                // iperf_server_api.c:662-678, 720-733; the #351 idle_armed
                // precedent one phase later) — a pure-reverse round waits
                // patiently (GT live: >9 s at --rcv-timeout 3000).
                let idle_armed = !ctx.cfg.reverse || ctx.cfg.bidir;
                for i in 0..total {
                    let data_stream = loop {
                        tokio::select! {
                            accepted = listener.accept() => {
                                // #362 (the PR #384 r2 F4 cell): a failed
                                // data accept is GT's IESTREAMCONNECT
                                // through cleanup_server (iperf_tcp.c:
                                // 134-135) — the fe+203 wire-back, the
                                // populated setup doc, round dead,
                                // keep-serving. The raw io line rode the
                                // generic arm before (BSD ECONNABORTED).
                                let (mut candidate, _) = match accepted {
                                    Ok(v) => v,
                                    Err(e) => {
                                        abort_setup_streams(ctx).await;
                                        // r1 F2: GT wires the LIVE accept
                                        // errno (cleanup_server sends
                                        // htonl(errno), iperf_server_api.c:
                                        // 470-471) — the wire word carries
                                        // the strerror content of the GT
                                        // client's SERVER ERROR line (r2
                                        // F2: the line prints either way;
                                        // the word gates its errno tail).
                                        let errno =
                                            e.raw_os_error().unwrap_or(0) as u32;
                                        let _ = protocol::send_server_error_errno(
                                            &mut ctx.ctrl,
                                            203,
                                            errno,
                                        )
                                        .await;
                                        let err =
                                            RiperfError::StreamConnectFailed(e);
                                        self.emit_setup_phase_error(
                                            ctx,
                                            &format!("error - {err}"),
                                        );
                                        return Err(err);
                                    }
                                };
                                // #359: GT's deny-and-continue cookie gate —
                                // a wrong-cookie or silent/FIN'd connect
                                // gets ACCESS_DENIED on ITS socket
                                // (iperf_tcp.c:161-166; the closed-socket
                                // guard iperf_server_api.c:786) and the wait
                                // continues; only the no-progress clock ends
                                // a round with no real streams. The old gate
                                // killed the round (CookieMismatch /
                                // unexpected-EOF).
                                // PARITY NOTE (#384 r1 F1): this inline
                                // cookie read blinding the ctrl/timeout arms
                                // is GT-FAITHFUL — GT's data-cookie read is
                                // an inline blocking Nread inside
                                // iperf_tcp_accept (iperf_tcp.c:155-160),
                                // probed identical (silent conn + ctrl-EOF
                                // at t=2 → both tools notice at ~10 s); the
                                // bounds are Nread's (10 s idle / 30 s
                                // overall on a dripper), same as GT's
                                // net.c:75-76 modulo the recorded nread_step
                                // per-step-idle nuance.
                                match protocol::recv_cookie(&mut candidate).await {
                                    Ok(c) if c == ctx.cookie => break candidate,
                                    // Wrong cookie, the silent Nread bound,
                                    // or a clean FIN (nread collapses both
                                    // to UnexpectedEof): best-effort deny
                                    // like GT's Nwrite-then-close (the peer
                                    // may already be gone; GT prints a
                                    // "failed to send access denied" line
                                    // when ITS deny write fails — riperf3
                                    // stays silent, recorded, racy cell).
                                    // RECORDED DEVIATION (#384 r2 F1, GT
                                    // bug not mirrored): a FIN/hold at
                                    // EXACTLY 36 cookie bytes is ACCEPTED
                                    // by GT as the real stream — its Nread
                                    // returns the partial 36 and
                                    // strncmp(cookie, buf, 37) passes via
                                    // the zero-filled buffer byte matching
                                    // the trailing NUL (iperf_util.c:
                                    // 121-124); it then runs a zero-byte
                                    // test on the dead socket. riperf3's
                                    // exact-37 read denies the truncated
                                    // cookie instead. Cookie-knowledge-
                                    // gated cell (only the real client or
                                    // an on-path observer holds the
                                    // prefix); the #271 do-not-mirror
                                    // ethos.
                                    // RECORDED DEVIATION (#384 r1 F3, GT
                                    // bug not mirrored): with negotiated
                                    // TOS != 0, GT runs sockopts on the
                                    // just-denied CLOSED fd before its
                                    // is_closed guard
                                    // (iperf_server_api.c:780 vs :786) and
                                    // IESETTOS-kills the round; riperf3
                                    // continues (the #271 ethos).
                                    Ok(_) => {
                                        let _ = protocol::send_state(
                                            &mut candidate,
                                            TestState::AccessDenied,
                                        )
                                        .await;
                                        // Dropped here; the slot stays
                                        // unclaimed and the select re-arms.
                                    }
                                    Err(RiperfError::Io(ref io))
                                        if io.kind()
                                            == std::io::ErrorKind::UnexpectedEof =>
                                    {
                                        let _ = protocol::send_state(
                                            &mut candidate,
                                            TestState::AccessDenied,
                                        )
                                        .await;
                                    }
                                    // #384 r1 F2: a HARD read error (e.g.
                                    // ECONNRESET) is NOT a deny — GT's
                                    // Nread < 0 arm takes IERECVCOOKIE
                                    // through cleanup_server
                                    // (iperf_tcp.c:155-159): the fe+106
                                    // wire-back, the populated setup doc,
                                    // round dead, keep-serving.
                                    Err(_) => {
                                        abort_setup_streams(ctx).await;
                                        let _ = protocol::send_server_error(
                                            &mut ctx.ctrl,
                                            106,
                                        )
                                        .await;
                                        self.emit_setup_phase_error(
                                            ctx,
                                            &format!(
                                                "error - {}: ",
                                                RiperfError::RecvDataCookieFailed
                                            ),
                                        );
                                        return Err(
                                            RiperfError::RecvDataCookieFailed,
                                        );
                                    }
                                }
                            }
                            activity = ctrl_activity(&ctx.ctrl) => match activity {
                                // EOF: the peer closed without connecting its
                                // data streams — GT's read-site surface. The
                                // -J doc emits here (the serve loop's arm
                                // prints the text sentence). #342 relay like
                                // the mid-test/end-loop EOF siblings —
                                // observable by a half-closed peer,
                                // best-effort no-op on a full close (r2 F1).
                                Ok(CtrlActivity::Eof) => {
                                    abort_setup_streams(ctx).await;
                                    let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                                    self.emit_setup_phase_error(ctx, CTRL_CLOSED_MSG);
                                    return Err(RiperfError::ControlSocketClosed);
                                }
                                Ok(CtrlActivity::Data) => {
                                    if let SetupFlow::ClientDone =
                                        self.dispatch_setup_ctrl_byte(ctx).await?
                                    {
                                        return Ok(SetupFlow::ClientDone);
                                    }
                                }
                                Err(e) => return Err(e.into()),
                            },
                            _ = tokio::time::sleep(rcv_timeout), if idle_armed => {
                                // GT's no-progress bound: wire-back
                                // SERVER_ERROR + IENOMSG(144) + errno 0
                                // (live: fe 00000090 00000000), the doc with
                                // the prefixed key, exit-0 keep-serving.
                                abort_setup_streams(ctx).await;
                                let _ = protocol::send_server_error(&mut ctx.ctrl, 144).await;
                                self.emit_setup_phase_error(
                                    ctx,
                                    &format!("error - {}", RiperfError::DataIdleTimeout),
                                );
                                return Err(RiperfError::DataIdleTimeout);
                            }
                        }
                    };
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
                        // #414: honor the client's wire repeating_payload —
                        // GT fills the pattern in iperf_new_stream on BOTH
                        // roles (iperf_api.c:4891).
                        let buf = make_send_buffer(ctx.cfg.blksize, ctx.cfg.repeating_payload);
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

                    // #381: cancel-cover the task the moment it spawns —
                    // the accept select's awaits sit between spawns.
                    abort_guard.push(task.abort_handle());
                    ctx.streams.push(DataStream {
                        meta: StreamMeta {
                            id: stream_id,
                            is_sender,
                            counters,
                            raw_fd,
                            sock,
                            congestion_used,
                            udp_offload: None,
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

                // #358: both UDP designs run the same #356 select machinery
                // as the TCP arm, so they can dispatch ctrl bytes — a
                // ClientDone (IPERF_DONE) surfaces here like TCP's.
                let flow = if udp_use_demux {
                    self.setup_udp_demux_streams(ctx, recv_count, total, max_duration, abort_guard)
                        .await?
                } else {
                    self.setup_udp_recycling_streams(
                        ctx,
                        recv_count,
                        total,
                        max_duration,
                        abort_guard,
                    )
                    .await?
                };
                if let SetupFlow::ClientDone = flow {
                    return Ok(SetupFlow::ClientDone);
                }
            }
        }
        Ok(SetupFlow::Proceed)
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
        // #351: iperf_strerror(IENOMSG) — must match RiperfError::DataIdleTimeout's
        // Display (the #338 pre-test sibling renders through that variant).
        const SELF_TERM_IDLE_MSG: &str = "idle timeout for receiving data";
        let mut interrupt_rx = self.interrupt.clone().map(|w| w.0);

        // #351: GT's TEST_RUNNING data-idle watchdog (iperf_server_api.c:
        // 720-739): armed only when the server RECEIVES (mode != SENDER —
        // reverse rounds are exempt); ctrl traffic does NOT reset it. On
        // expiry: IENOMSG(144) — the relay, then the self-terminate surface
        // (the prefixed doc key over the accumulated intervals + bare end,
        // the stderr line, no summary), exit-0 keep-serving like GT's
        // restart. RECORDED DEVIATIONS (PR #369 r1+r2, live-probed):
        // (1) progress — riperf3 resets on received BYTES advancing; GT's
        // blocks_received advances ONLY on full-`len` block completions
        // (the running-phase reads are Nrecv_no_select, net.c:511-553 — a
        // timeout-free full-block accumulate; the 10s/30s statics are
        // control-path only). The divergence is RATE-scoped, at ANY bound:
        // GT false-kills every receiving flow slower than len*8/bound
        // (~8.7 kbit/s at stock 128K/120s — a GT `-b 8k` TCP client dies
        // against its own server; probed at bounds 3s/35s/120s). riperf3
        // never kills a byte-flowing round — the liveness-preserving
        // reading (the #356 precedent). Flip side, recorded honestly: a
        // 1-byte-per-(bound-epsilon) trickle holds a riperf3 slot
        // indefinitely where GT reaps at ~bound — but a full-block cadence
        // >= bound holds GT forever too (probed), so neither watchdog is a
        // security bound.
        // (2) kill envelope — riperf3 fires within [bound, bound+~250ms];
        // GT's baseline lags its main-loop wakes and expiry needs a full
        // select timeout, so its band is [bound, ~bound+2s] (a full-block
        // 3.6s cadence under a 3s bound survives GT, dies here). Neither
        // tool fires before a genuine bound-length gap.
        // (3) the killed round's doc keeps riperf3's #210/#325 partial
        // catch-up interval row; GT's fatal wake precedes its reporter
        // tick, so GT's doc holds one fewer whole row + no partial.
        let idle_armed = !ctx.cfg.reverse || ctx.cfg.bidir;
        let idle_bound =
            std::time::Duration::from_millis(self.rcv_timeout.unwrap_or(DEFAULT_RCV_TIMEOUT_MS));
        let mut idle_check = tokio::time::interval(std::time::Duration::from_millis(250));
        idle_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut idle_last_rx: u64 = 0;
        let mut idle_last_at = tokio::time::Instant::now();

        loop {
            tokio::select! {
                state = protocol::recv_state(&mut ctx.ctrl) => {
                    match state {
                        Ok(TestState::TestEnd) => break,
                        Ok(TestState::ClientTerminate) => {
                            // iperf_got_sigend's peer half (#210): dump the
                            // partial results in the finalize phases (the old
                            // early return leaked the reporter — the #147
                            // class — and skipped the dump iperf3 performs).
                            // #342 (r1 F1): the terminate arm sets the
                            // i_errno GLOBAL (iperf_server_api.c:290) and
                            // cleanup_server relays it at the loop's normal
                            // exit (:1001, :466) — the relay does not key on
                            // an error return. RECORDED DEVIATION
                            // (value-level): GT's live value is
                            // NONDETERMINISTIC — 119 vs a 206 IESTREAMREAD
                            // clobber (post-teardown stream reads overwrite
                            // the plain global; either value can dominate
                            // depending on timing); riperf3 pins the
                            // intended IECLIENTTERM(119).
                            let _ = protocol::send_server_error(&mut ctx.ctrl, 119).await;
                            ctx.client_terminated = true;
                            break;
                        }
                        // GT's TEST_START arm is a bare no-op mid-test too
                        // (iperf_server_api.c:266-267).
                        Ok(TestState::TestStart) => {}
                        // #330 r1 F1: IPERF_DONE has its own CLEAN arm in
                        // GT (:287-288) — the byte lands in test->state and
                        // the run loop exits with no error surface at all
                        // (live-probed: doc error null, bare end, exit 0).
                        Ok(TestState::IperfDone) => {
                            ctx.early_done = true;
                            ctx.bare_end = true;
                            break;
                        }
                        // #330: every OTHER known state mid-test hits GT's
                        // IEMESSAGE default (live-probed: byte 9 = the
                        // prefixed doc key, exit 0; the full stray set is
                        // 2/9/10/11/13/14/15/-1/-2). The #145 tolerance is
                        // gone on this loop like the end loop's (#329).
                        // #342: cleanup_server's best-effort relay —
                        // SERVER_ERROR + htonl(IEMESSAGE=110) + htonl(errno)
                        // to a still-live peer (iperf_server_api.c:460-473;
                        // live: fe 0000006e 00000000).
                        Ok(_) => {
                            let _ = protocol::send_server_error(&mut ctx.ctrl, 110).await;
                            ctx.unknown_message = true;
                            ctx.bare_end = true;
                            break;
                        }
                        // #325 r2 F1: an UNMAPPED byte during the data phase
                        // is the same IEMESSAGE default — GT's end processing
                        // never runs, so the doc keeps the accumulated
                        // intervals with a bare end{} and text skips the
                        // summary (live-verified against GT 3.21).
                        // RECORDED DEVIATION (r3 F3): riperf3's flush adds
                        // one partial catch-up interval (the #210 terminate
                        // convention); GT's reporter dies with only whole
                        // ticks in the doc.
                        Err(RiperfError::UnknownControlMessage) => {
                            // #342: the unmapped-byte arm relays like the
                            // known-stray arm above — GT's default: switches
                            // on the byte value, mapped or not.
                            let _ = protocol::send_server_error(&mut ctx.ctrl, 110).await;
                            ctx.unknown_message = true;
                            ctx.bare_end = true;
                            break;
                        }
                        // #330: control EOF mid-test — GT's IECTRLCLOSE
                        // read-site surface, a CLEAN round (IPERF_DONE):
                        // bare sentence in the doc, no summary, exit 0.
                        // #342 (r1 F2): the rval==0 arm sets IECTRLCLOSE
                        // (iperf_server_api.c:251-254) and cleanup_server
                        // relays it — observable by a half-closed peer whose
                        // read half is still open (deterministic live:
                        // fe 0000006d 00000000); best-effort no-op on a
                        // full close.
                        Err(RiperfError::PeerDisconnected) => {
                            let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                            ctx.ctrl_closed = true;
                            ctx.bare_end = true;
                            break;
                        }
                        Err(e) => return Err(e),
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
                                // #350 RECORDED DEVIATION: GT relays this
                                // frame TWICE — the explicit rate-path write
                                // (iperf_server_api.c:626-643), then
                                // cleanup_server re-reads the stale i_errno
                                // global at loop exit (:466) and relays
                                // again (live: back-to-back fe 0000001b
                                // frames). Same stale-global class as the
                                // #349 terminate clobber; conforming clients
                                // stop after the first frame, so riperf3
                                // sends exactly one (pinned).
                                // #224: SERVER_ERROR + IETOTALRATE(27), not
                                // SERVER_TERMINATE (iperf 3.21 GT).
                                protocol::send_server_error(&mut ctx.ctrl, 27).await?;
                                ctx.server_error = Some(SELF_TERM_RATE_MSG);
                                // #368: GT's rate-breach kill path
                                // (cleanup_server + return -1,
                                // iperf_server_api.c:624-646) never runs end
                                // processing — the -J doc keeps the
                                // accumulated intervals over a BARE end{}
                                // (live-probed: GT end keys []). Same shape
                                // as the idle-arm sibling below.
                                ctx.bare_end = true;
                                break;
                            }
                        }
                    }
                }
                _ = idle_check.tick(), if idle_armed => {
                    let rx: u64 = ctx
                        .streams
                        .iter()
                        .map(|s| s.meta.counters.bytes_received())
                        .sum();
                    if rx > idle_last_rx {
                        idle_last_rx = rx;
                        idle_last_at = tokio::time::Instant::now();
                    } else if idle_last_at.elapsed() >= idle_bound {
                        // Best-effort like the #338/#349 relay sites — the
                        // idle peer may be gone entirely.
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 144).await;
                        ctx.server_error = Some(SELF_TERM_IDLE_MSG);
                        // GT's kill path never runs end processing — the
                        // doc keeps the accumulated intervals over a bare
                        // end{} (live-probed; the rate-breach sibling's
                        // populated end is the pre-existing divergence
                        // filed from this probe).
                        ctx.bare_end = true;
                        break;
                    }
                }
                _ = &mut watchdog_deadline, if watchdog_secs > 0 => {
                    // #224: iperf3's server_timer_proc — SERVER_ERROR +
                    // IESERVERTESTDURATIONEXPIRED(160) on the wire.
                    protocol::send_server_error(&mut ctx.ctrl, 160).await?;
                    ctx.server_error = Some(SELF_TERM_DURATION_MSG);
                    // #368: the duration-watchdog kill path is the rate
                    // breach's sibling — server_timer_proc frees the streams
                    // and the run loop exits without end processing, so GT's
                    // -J doc is BARE end{} here too. Probe-confirmed: a
                    // wedged client past the (duration+omit+40 s) watchdog
                    // yields GT end keys [] (the error key then clobbers to a
                    // stale select-fail global — the recorded #349/#350
                    // nondeterminism class — but the end SHAPE is bare). Not
                    // CI-pinnable: the 40 s grace can't be shortened, same as
                    // the #230 watchdog-timing behavior.
                    ctx.bare_end = true;
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
        // (exchange_recv_failed is NOT here — this phase runs before the
        // exchange, so the flag can't be set yet; its abort lives at the
        // post-emit gate. r1 F5.)
        if ctx.server_error.is_some()
            || ctx.interrupted.is_some()
            || ctx.unknown_message
            || ctx.client_terminated
            || ctx.ctrl_closed
            || ctx.early_done
        {
            // #322 r1 F1: interrupts take the same abort — a wedged peer
            // holding sockets open must not park the joins (GT closes its
            // data sockets at TEST_END and sigend exits in milliseconds).
            // #325 r3 F1: the mid-test IEMESSAGE too — GT cleanup_servers
            // immediately on a failed handle_message (iperf_server_api.c:
            // 764-767); without the abort a hostile peer holding its
            // sockets open parks the joins (and, persistent, the accept
            // loop) for the whole hold.
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
                // #344: iperf_err's --timestamps stamp (server_timer_proc
                // prints through iperf_err directly).
                eprintln!(
                    "{}riperf3: error - {msg}",
                    crate::macros::output_timestamp_prefix()
                );
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
        // #325 r3 F2: the mid-test IEMESSAGE path renders NO summary — GT's
        // reporter is dead (live-verified: GT with get_server_output prints
        // only the stderr line). Dropping the capture un-sets was_captured,
        // and the exchange skip already discards the relay.
        let capture = ctx.capture.take().filter(|_| !ctx.bare_end);
        let (server_output_text, server_output_json) = if let Some(capture) = capture {
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

    /// The one report-build plumbing site for the pipeline (#289), split
    /// from the printer so `--get-server-output` (#33) can build ONCE
    /// pre-exchange and reuse at print time. The drained collections arrive
    /// BY VALUE and move into the report, so the "must be built exactly
    /// once" invariant is structural (#287): the pre-exchange build consumes
    /// them into `ReportSource::Built` (#33/#137), else they ride `Pending`
    /// to the single post-exchange build.
    fn build_ctx_report(
        &self,
        ctx: &TestRunCtx,
        end: &EndState,
        collected: crate::reporter::CollectedIntervals,
    ) -> crate::json_report::Report {
        let mut input = self.build_report_input(
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
        );
        // #325 r2 F1: the mid-test IEMESSAGE doc renders GT's bare end{}
        // (the final stats dump never ran); every other path keeps the
        // populated structure the input builder assembled.
        input.bare_end = ctx.bare_end;
        input.build()
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
        if !ctx.client_terminated
            && !ctx.unknown_message
            && !ctx.ctrl_closed
            && !ctx.early_done
            && ctx.interrupted.is_none()
            && ctx.server_error.is_none()
        {
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

            // #371: an exchange-phase send failing against a peer that RST
            // the ctrl after TEST_END is GT's IESENDMESSAGE — the reporter
            // already ran, so render the POPULATED doc + the typed key
            // (return Ok(()) into the finalize path), not the raw-io skeleton
            // the generic serve-loop arm would emit. `send_state` maps to
            // IESENDMESSAGE; `send_results` to IESENDRESULTS.
            if let Err(e) = protocol::send_state(&mut ctx.ctrl, TestState::ExchangeResults).await {
                ctx.exchange_send_error = Some(exchange_send_message_error(e));
                return Ok(());
            }
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
                r = protocol::recv_results(&mut ctx.ctrl) => match r {
                    Ok(v) => v,
                    // #330: a malformed results read routes to the
                    // exchange_recv_failed surface — the Nread_json warning
                    // printed at the read site; the finalize phases render
                    // the doc. (The rendered error KEY is IERECVRESULTS on
                    // every close-type here — a deliberate deviation from
                    // GT's RST→IESENDMESSAGE flip; see the field's docs.)
                    // #342: cleanup_server's relay — SERVER_ERROR +
                    // htonl(IERECVRESULTS=117) + htonl(errno) to a peer that
                    // still holds the socket (live: fe 00000075 00000000);
                    // best-effort, a vanished peer just fails the write.
                    Err(_) => {
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 117).await;
                        ctx.exchange_recv_failed = true;
                        return Ok(());
                    }
                },
                msg = crate::client::wait_interrupt(exchange_interrupt.as_mut()) => {
                    let _ = protocol::send_state(&mut ctx.ctrl, TestState::ServerTerminate).await;
                    ctx.interrupted = Some(msg);
                    return Ok(());
                }
            };
            if let Err(e) = protocol::send_results(&mut ctx.ctrl, &server_results).await {
                ctx.exchange_send_error = Some(exchange_send_results_error(e));
                return Ok(());
            }

            // ---- DisplayResults / IperfDone ----
            if let Err(e) = protocol::send_state(&mut ctx.ctrl, TestState::DisplayResults).await {
                ctx.exchange_send_error = Some(exchange_send_message_error(e));
                return Ok(());
            }

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
                    // GT's TEST_START arm is a bare no-op
                    // (iperf_server_api.c:266-267).
                    Ok(TestState::TestStart) => continue,
                    // #325: GT honors CLIENT_TERMINATE at ANY message point
                    // (iperf_server_api.c:289-308: dump under
                    // DISPLAY_RESULTS + IECLIENTTERM) — the finalize phases
                    // render the terminated shape like the mid-test arm.
                    // RECORDED DEVIATION (r2 F2, text mode only): GT prints
                    // the summary TWICE here — its TEST_END arm already ran
                    // reporter_callback (:276) and the terminate arm re-runs
                    // it under DISPLAY_RESULTS (:293-297, live-verified two
                    // full blocks). riperf3 prints one dump; -J is identical
                    // on both (single doc, error key).
                    Ok(TestState::ClientTerminate) => {
                        // #342 (r1 F1): IECLIENTTERM(119) relay like the
                        // mid-test terminate arm. RECORDED DEVIATION
                        // (value-level): GT's end-loop frame carries a
                        // LEFTOVER errno word (fe 00000077 00000009 live —
                        // EBADF from its own closed-socket reads); riperf3
                        // pins errno 0, the #336 honest-errno-0 convention.
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 119).await;
                        ctx.client_terminated = true;
                        return Ok(());
                    }
                    // GT's switch has arms for only TEST_START / TEST_END /
                    // IPERF_DONE / CLIENT_TERMINATE — every OTHER byte,
                    // known state or not, is `default: IEMESSAGE`
                    // (iperf_server_api.c:265-311; r1 F3 — live-verified:
                    // GT errors on bytes 2/9/14 here). The old #145
                    // tolerance is gone. RECORDED DEVIATION for TEST_END
                    // only: GT re-runs the whole end block — stats, stream
                    // close, a SECOND results exchange (:268-286) — but a
                    // conforming client can't resend 4 from this window
                    // (it only sends TEST_END from TEST_RUNNING), and
                    // re-entering the exchange against a peer that didn't
                    // would wedge both sides; it takes the IEMESSAGE class
                    // instead.
                    Ok(_) | Err(RiperfError::UnknownControlMessage) => {
                        // #342: the same cleanup_server relay as the
                        // mid-test arms — GT routes both loops through one
                        // handle_message_server switch.
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 110).await;
                        ctx.unknown_message = true;
                        return Ok(());
                    }
                    // #330: EOF instead of IperfDone — the fast-close
                    // cell. GT prints its read-site sentence and the doc
                    // keeps the error key over the POPULATED end (the
                    // exchange completed; live-probed).
                    // #342 (r1 F2): IECTRLCLOSE(109) relay like the
                    // mid-test EOF arm — observable on a half-close only.
                    Err(RiperfError::PeerDisconnected) => {
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                        ctx.ctrl_closed = true;
                        break;
                    }
                    // #406: a HARD read error (the peer RST'd after the
                    // completed exchange) — GT's read-site rval<0 arm is
                    // IERECVMESSAGE (iperf_server_api.c:256) over the
                    // POPULATED end, the error sibling of the EOF arm
                    // above. No relay: the ctrl is dead. See the
                    // ExchangeRecvMessageFailed deviation record for GT's
                    // clobbered-class loopback observable.
                    Err(RiperfError::Io(e)) => {
                        ctx.exchange_recv_message_error =
                            Some(RiperfError::ExchangeRecvMessageFailed(e));
                        break;
                    }
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
        } else if !was_captured && ctx.server_error.is_none() && !ctx.bare_end {
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
    async fn setup_udp_recycling_streams(
        &self,
        ctx: &mut TestRunCtx,
        recv_count: u32,
        total: u32,
        max_duration: Option<std::time::Duration>,
        abort_guard: &mut stream::AbortStreamsOnDrop,
    ) -> Result<SetupFlow> {
        // The scalar knobs, copied out so the select arms below can borrow
        // ctx whole (dispatch_setup_ctrl_byte / emit_setup_phase_error).
        let blksize = ctx.cfg.blksize;
        let tos = ctx.cfg.tos;
        let window = ctx.cfg.window;
        let bandwidth = ctx.cfg.bandwidth;
        let pacing_timer = ctx.cfg.pacing_timer;
        let burst = ctx.cfg.burst;
        let udp_counters_64bit = ctx.cfg.udp_counters_64bit;
        let fq_rate = ctx.cfg.fq_rate;
        let gso_dg_size = ctx.cfg.gso_dg_size;

        let mut udp_listener = net::udp_bind_reusable(
            self.bind_address.as_deref(),
            self.port,
            self.ip_version,
            self.bind_dev.as_deref(),
        )
        .await?;
        protocol::send_state(&mut ctx.ctrl, TestState::CreateStreams).await?;

        // #358: GT's UDP CREATE_STREAMS wait runs inside its select loop
        // like the TCP arm's — a ctrl-EOF is IECTRLCLOSE at once, a
        // no-progress round bounds at rcv_timeout (IENOMSG), and a waiting
        // ctrl byte dispatches. The old fixed per-stream
        // UDP_CONNECT_TOTAL_TIMEOUT budget was riperf3-only: GT has no
        // per-stream connect budget (the client keeps its own 30 s
        // handshake budget in udp_connect_client).
        let rcv_timeout =
            std::time::Duration::from_millis(self.rcv_timeout.unwrap_or(DEFAULT_RCV_TIMEOUT_MS));
        // #383 r1 F2: the sender-mode exemption — see the TCP arm's note.
        let idle_armed = !ctx.cfg.reverse || ctx.cfg.bidir;

        // #316: the client's GSO/GRO request, probed per accepted socket
        // below. Once a probe fails, GT zeroes the setting and later
        // sockets don't retry (iperf_udp.c:459-515).
        let (mut gso_on, mut gro_on) = (ctx.cfg.gso, ctx.cfg.gro);

        // #178: each stream's spawn_blocking data thread is spawned through
        // the gate; the barrier below holds TestStart until the data plane
        // actually exists.
        let mut thread_gate = stream::StreamThreadGate::new();
        for i in 0..total {
            // Accept: recv magic (via the select), then connect() to the
            // client and send the reply AFTER the arm wins — a ctrl byte
            // cannot cancel a half-done handshake into a lost reply (the
            // select-cancellation window the retired udp_connect_server
            // call would have had).
            let mut magic_buf = [0u8; 65536];
            loop {
                tokio::select! {
                    r = udp_listener.recv_from(&mut magic_buf) => {
                        let (n, addr) = r?;
                        // Validation identical to the retired
                        // udp_connect_server: short/foreign datagrams stay
                        // fatal on this connected-per-stream design.
                        if n < 4 {
                            return Err(RiperfError::Protocol(
                                "UDP connect message too short".into(),
                            ));
                        }
                        let msg = u32::from_ne_bytes(magic_buf[..4].try_into().unwrap());
                        if msg != protocol::UDP_CONNECT_MSG {
                            return Err(RiperfError::Protocol(format!(
                                "unexpected UDP connect message: {msg:#x}"
                            )));
                        }
                        udp_listener.connect(addr).await?;
                        udp_listener
                            .send(&protocol::UDP_CONNECT_REPLY.to_ne_bytes())
                            .await?;
                        break;
                    }
                    activity = ctrl_activity(&ctx.ctrl) => match activity {
                        // EOF: GT's read-site surface, noticed at once —
                        // the TCP arm's exact shape (#338/#342).
                        Ok(CtrlActivity::Eof) => {
                            abort_setup_streams(ctx).await;
                            let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                            self.emit_setup_phase_error(ctx, CTRL_CLOSED_MSG);
                            return Err(RiperfError::ControlSocketClosed);
                        }
                        Ok(CtrlActivity::Data) => {
                            if let SetupFlow::ClientDone =
                                self.dispatch_setup_ctrl_byte(ctx).await?
                            {
                                return Ok(SetupFlow::ClientDone);
                            }
                        }
                        Err(e) => return Err(e.into()),
                    },
                    _ = tokio::time::sleep(rcv_timeout), if idle_armed => {
                        // GT's no-progress bound: wire-back SERVER_ERROR +
                        // IENOMSG(144) + errno 0, the doc with the prefixed
                        // key, exit-0 keep-serving — the TCP arm's shape.
                        abort_setup_streams(ctx).await;
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 144).await;
                        self.emit_setup_phase_error(
                            ctx,
                            &format!("error - {}", RiperfError::DataIdleTimeout),
                        );
                        return Err(RiperfError::DataIdleTimeout);
                    }
                }
            }
            // The listener is now locked to this client — use it as the data socket
            let data_sock = udp_listener;
            // #316: per-accept like GT's iperf_udp_accept (iperf_udp.c:
            // 576-579) — the #320-class placement: the loop recycles a
            // FRESH listener per stream, so a pre-loop call covered
            // stream 0 only. Best-effort like GT (failure zeroes, never
            // fatal). The dg value saturates i64→i32 like cJSON's valueint
            // and passes RAW — the kernel EINVALs nonsense exactly like it
            // does for GT (r2 F3). (GT probes BEFORE its 4-byte connect
            // reply, riperf3 after — no observable delta, the reply is
            // never segmented; r2 F4.)
            if gso_on {
                gso_on = net::set_udp_gso(&data_sock, saturate_i32(gso_dg_size)).is_ok();
            }
            if gro_on {
                gro_on = net::set_udp_gro(&data_sock).is_ok();
            }
            // #302 r2: pace EVERY accepted stream — GT's block lives in
            // iperf_udp_accept (iperf_udp.c:581-595), once per stream; the
            // pre-loop listener call covered stream 0 only.
            net::apply_fq_rate(&data_sock, fq_rate);

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
            if tos != 0 {
                net::set_tos(&data_sock, tos as u32)?;
            }
            // Socket addresses + buffer sizes + the #97 window-clamp check for
            // the `-J` blob (#50), captured before the socket is converted to
            // std + moved. apply_window=true: honor -w/--window on the server's
            // UDP data socket too (#59) so reverse/bidir UDP matches iperf3,
            // before the read-back (#144).
            let sock = net::capture_stream_meta(socket2::SockRef::from(&data_sock), window, true)?;

            let std_sock = data_sock.into_std().map_err(RiperfError::Io)?;

            if is_sender {
                let c = counters.clone();
                let d = ctx.done.clone();
                let bs = blksize;
                // Already resolved in TestConfig (#17); 0 = unlimited.
                let rate = bandwidth;
                let pt = pacing_timer; // #185: pace the UDP batch too
                let u64bit = udp_counters_64bit;
                let bu = burst;
                let uw = window.is_some();
                let df = ctx.cfg.dont_fragment;
                let st = ctx.start.clone();
                let md = max_duration;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_sender_blocking(
                        std_sock, c, bs, d, rate, pt, bu, uw, df, u64bit, st, md,
                    )
                });
                // #381 (#427 r1 F3): a running spawn_blocking task ignores
                // abort() (it exits via `done` + its 500 ms poll); the push
                // still stops a queued-not-yet-started runner, like the
                // client UDP arm. Untested by design (#427 r2 F3): no
                // deterministic pin can catch the not-yet-started window.
                abort_guard.push(task.abort_handle());
                ctx.streams.push(DataStream {
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock,
                        congestion_used: None,
                        udp_offload: Some((gso_on, gro_on)),
                    },
                    task,
                    udp_recv_stats: None,
                });
            } else {
                let c = counters.clone();
                let d = ctx.done.clone();
                let bs = blksize;
                let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
                let stats_clone = stats.clone();
                let u64bit = udp_counters_64bit;
                let task = thread_gate.spawn(move || {
                    stream::run_udp_receiver_blocking(std_sock, c, stats_clone, bs, d, u64bit)
                });
                // #381 (#427 r1 F3): queued-runner coverage, as above.
                abort_guard.push(task.abort_handle());
                ctx.streams.push(DataStream {
                    meta: StreamMeta {
                        id: stream_id,
                        is_sender,
                        counters,
                        raw_fd: None,
                        sock,
                        congestion_used: None,
                        udp_offload: Some((gso_on, gro_on)),
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
        Ok(SetupFlow::Proceed)
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
    async fn setup_udp_demux_streams(
        &self,
        ctx: &mut TestRunCtx,
        recv_count: u32,
        total: u32,
        max_duration: Option<std::time::Duration>,
        abort_guard: &mut stream::AbortStreamsOnDrop,
    ) -> Result<SetupFlow> {
        use std::collections::{HashMap, HashSet};
        use std::net::SocketAddr;

        // The scalar knobs, copied out so the select arms below can borrow
        // ctx whole (dispatch_setup_ctrl_byte / emit_setup_phase_error).
        let blksize = ctx.cfg.blksize;
        let tos = ctx.cfg.tos;
        let window = ctx.cfg.window;
        let bandwidth = ctx.cfg.bandwidth;
        let pacing_timer = ctx.cfg.pacing_timer;
        let burst = ctx.cfg.burst;
        let udp_counters_64bit = ctx.cfg.udp_counters_64bit;
        let fq_rate = ctx.cfg.fq_rate;
        let gso_dg_size = ctx.cfg.gso_dg_size;

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
        net::apply_fq_rate(&udp_sock, fq_rate);
        // #316: the demux shared socket IS the data socket — same GSO/GRO
        // honor, best-effort, probed once for every stream's meta.
        let (mut gso_on, mut gro_on) = (ctx.cfg.gso, ctx.cfg.gro);
        if gso_on {
            gso_on = net::set_udp_gso(&udp_sock, saturate_i32(gso_dg_size)).is_ok();
        }
        if gro_on {
            gro_on = net::set_udp_gro(&udp_sock).is_ok();
        }

        protocol::send_state(&mut ctx.ctrl, TestState::CreateStreams).await?;

        // Accept the connect handshake from every client stream on the one
        // socket. Record the source address of each NEW client in arrival order
        // (slot i == stream i). Reply to every valid magic — including a
        // retransmit from an already-seen client whose reply was lost — but only
        // a new source claims a slot.
        // #358: the wait runs the #356 select machinery like the TCP arm —
        // ctrl-EOF is IECTRLCLOSE at once, a no-progress round bounds at
        // rcv_timeout (IENOMSG; the per-iteration sleep re-arm makes any
        // datagram or ctrl byte progress — mirroring GT's select window
        // restarting per fd wake; its last_receive_time anchor never
        // resets pre-streams, observably equivalent in every reachable
        // cell — #383 r2 F5), and a waiting ctrl byte dispatches. The old fixed
        // per-stream UDP_CONNECT_TOTAL_TIMEOUT budget was riperf3-only
        // (the client keeps its own 30 s handshake budget).
        let rcv_timeout =
            std::time::Duration::from_millis(self.rcv_timeout.unwrap_or(DEFAULT_RCV_TIMEOUT_MS));
        // #383 r1 F2: the sender-mode exemption — see the TCP arm's note.
        let idle_armed = !ctx.cfg.reverse || ctx.cfg.bidir;
        let mut client_addrs: Vec<SocketAddr> = Vec::with_capacity(total as usize);
        let mut seen: HashSet<SocketAddr> = HashSet::new();
        let mut magic_buf = [0u8; 65536];
        while client_addrs.len() < total as usize {
            tokio::select! {
                r = udp_sock.recv_from(&mut magic_buf) => {
                    let (n, src) = match r {
                        Ok(v) => v,
                        // Reset-class noise: our own UDP_CONNECT_REPLY to a
                        // client port that just closed (e.g. a retry on a
                        // fresh socket) queues WSAECONNRESET on Windows — it
                        // must not abort setup for EVERY stream; skip like
                        // the data-phase receivers (#180). The continue
                        // re-arms the no-progress clock (#383 r1 F5):
                        // sustained ICMP feedback keeps the wait alive past
                        // any bound, but GT can't reach the cell at all
                        // (recvfrom error -> IESTREAMACCEPT fatal) and the
                        // client's own 30 s handshake budget ends the round
                        // via ctrl-EOF in practice.
                        Err(e) if crate::stream::is_reset_class(&e) => continue,
                        Err(e) => return Err(e.into()),
                    };
                    // Drop anything that isn't the connect magic (too short,
                    // or a stray datagram) and keep waiting.
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
                    }
                }
                activity = ctrl_activity(&ctx.ctrl) => match activity {
                    // EOF: GT's read-site surface, noticed at once — the
                    // TCP arm's exact shape (#338/#342).
                    Ok(CtrlActivity::Eof) => {
                        abort_setup_streams(ctx).await;
                        let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                        self.emit_setup_phase_error(ctx, CTRL_CLOSED_MSG);
                        return Err(RiperfError::ControlSocketClosed);
                    }
                    Ok(CtrlActivity::Data) => {
                        if let SetupFlow::ClientDone =
                            self.dispatch_setup_ctrl_byte(ctx).await?
                        {
                            return Ok(SetupFlow::ClientDone);
                        }
                    }
                    Err(e) => return Err(e.into()),
                },
                _ = tokio::time::sleep(rcv_timeout), if idle_armed => {
                    // GT's no-progress bound: wire-back SERVER_ERROR +
                    // IENOMSG(144) + errno 0, the doc with the prefixed
                    // key, exit-0 keep-serving — the TCP arm's shape.
                    abort_setup_streams(ctx).await;
                    let _ = protocol::send_server_error(&mut ctx.ctrl, 144).await;
                    self.emit_setup_phase_error(
                        ctx,
                        &format!("error - {}", RiperfError::DataIdleTimeout),
                    );
                    return Err(RiperfError::DataIdleTimeout);
                }
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
            net::apply_socket_window(&sock, window);
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
        net::check_socket_window(window, sndbuf_actual, rcvbuf_actual)?;
        // iperf3 applies IP_TOS/IPV6_TCLASS per UDP stream socket on both
        // roles; every stream here shares this one socket and one cfg.tos,
        // so once-per-socket is semantically identical (#154). Fatal per #45.
        if tos != 0 {
            net::set_tos(&udp_std, tos as u32)?;
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
        let bs = blksize;
        let rate = bandwidth;
        let pt = pacing_timer; // #185: pace the UDP batch too
        let u64bit = udp_counters_64bit;
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
                let d = ctx.done.clone();
                let bu = burst;
                let uw = window.is_some();
                let df = ctx.cfg.dont_fragment;
                let st = ctx.start.clone();
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
                        df,
                        u64bit,
                        st,
                        md,
                    )
                });
                // #381 (#427 r1 F3): queued-runner coverage — a running
                // spawn_blocking task ignores abort() and rides `done` +
                // its 500 ms poll; the push only stops one that has not
                // started yet. (The receiving arm's placeholder tasks are
                // NOT pushed: they are already-resolved dummies — the real
                // handle is the demux receiver's, pushed below.)
                abort_guard.push(task.abort_handle());
                ctx.streams.push(DataStream {
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
                        udp_offload: Some((gso_on, gro_on)),
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
                ctx.streams.push(DataStream {
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
                        udp_offload: Some((gso_on, gro_on)),
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
            let d = ctx.done.clone();
            let bs = blksize;
            let demux = thread_gate
                .spawn(move || stream::run_udp_server_demux_receiver(s, routes, bs, d, u64bit));
            // #381 (#427 r1 F3): queued-runner coverage for the demux too,
            // mirroring the post-setup arm()'s chain.
            abort_guard.push(demux.abort_handle());
            ctx.udp_demux_handle = Some(demux);
        }
        // #178: hold TestStart (sent by the caller right after this returns)
        // until every data thread is running — the test clock must not outrun
        // OS-thread creation, which stalls for seconds on loaded hosts. On
        // timeout proceed anyway (degraded = pre-fix behavior).
        thread_gate.wait(stream::STREAM_THREAD_START_TIMEOUT).await;
        Ok(SetupFlow::Proceed)
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
            // The exchanged -w, rendered VERBATIM like GT — negatives included
            // (#392; the old .max(0) clamp rendered 0 where GT emits -1).
            sock_bufsize: Some(cfg.window.map(i64::from).unwrap_or(0)),
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
            // test_start carries the adopted values POST-probe (a failed
            // setsockopt zeroes settings->gso/gro, iperf_udp.c:459-515;
            // live-probed gso:1). Folded across streams like GT's
            // settings zeroing; no UDP streams → the request as-is.
            gso: i32::from(
                cfg.gso
                    && streams
                        .iter()
                        .filter_map(|s| s.meta.udp_offload)
                        .all(|(g, _)| g),
            ),
            gro: i32::from(
                cfg.gro
                    && streams
                        .iter()
                        .filter_map(|s| s.meta.udp_offload)
                        .all(|(_, g)| g),
            ),
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

    /// `-J`: pretty-print the server's single batched report blob (#50), or a
    /// prebuilt one (#33).
    fn print_results_json(&self, report: &crate::json_report::Report) {
        // #290: a quiet run returns the report without printing the document.
        if crate::macros::output_quiet() {
            return;
        }
        match serde_json::to_string_pretty(report) {
            Ok(s) => println!("{s}"),
            // #344: stamped for sink consistency (riperf3-internal class —
            // GT has no serialize-fail arm to match).
            Err(e) => eprintln!(
                "{}riperf3: error - failed to serialize JSON: {e}",
                crate::macros::output_timestamp_prefix()
            ),
        }
    }

    /// #386: GT's refused round does not END at the relay — cleanup_server
    /// closes the ctrl through iperf_sync_close_socket (net.c:877-886):
    /// shutdown(SHUT_WR), then a drain loop `while (Nread(...) > 0)`. The
    /// drain is BOUNDED, not read-until-EOF (#429 r1 F1 — GT's own "Read
    /// until EOF" comment misleads): each Nread front-selects with
    /// nread_read_timeout = 10 s (net.c:75, :415-436) and returns 0 on
    /// silence, which fails the `> 0` test exactly like EOF — GT
    /// live-probed self-freeing at ~10 s against a wedged holder, refusal
    /// doc rendered. So the park ends on client EOF/RST, 10 s of ctrl
    /// silence (both -> None: emit the refusal doc), or a signal (Some:
    /// the doc is ABANDONED — GT's sigend longjmps past the unprinted doc
    /// and emits the interrupt skeleton alone; cells A/B live-probed
    /// 2026-07-10). The idle clock is per-read, like GT's per-Nread
    /// select: a dripping peer extends the park on BOTH tools (probed,
    /// 1 byte/5 s). Recorded micro-deviation (#429 r2 F2): after the LAST
    /// dripped byte riperf3 frees at exactly the bound (10.0 s) while
    /// GT's >0 partial return restarts a full Nread, so its tail runs up
    /// to ~2x (probed 15.2 s) — adversarial-peer-only, boundedness
    /// matches.
    async fn park_refused_round(&self, ctrl: &mut tokio::net::TcpStream) -> Option<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = ctrl.shutdown().await; // GT's SHUT_WR half-close
        let mut interrupt = self.interrupt.clone().map(|w| w.0);
        let mut buf = [0u8; 128];
        loop {
            tokio::select! {
                r = ctrl.read(&mut buf) => match r {
                    Ok(0) | Err(_) => return None,
                    Ok(_) => continue, // drain, like GT's read loop
                },
                _ = tokio::time::sleep(protocol::NREAD_IDLE_TIMEOUT) => return None,
                msg = crate::client::wait_interrupt(interrupt.as_mut()) => {
                    return Some(msg);
                }
            }
        }
    }

    /// The refusal sinks, rendered at ROUND END (post-park, #386) — GT's
    /// refusal shapes per output mode (live-captured, iperf 3.21): text
    /// gets the one stderr line (#344: iperf_err's --timestamps stamp —
    /// the refusal reaches main.c:174's iperf_err via the -1 return); -J
    /// gets the skeleton error document (no accepted_connection/cookie —
    /// GT skips on_connect on this path); a --json-stream server gets the
    /// error + empty-end event pair with no start event (#198's pre-test
    /// tail, byte-identical to GT's refusal events). iperf_strerror is
    /// perr=0 for both classes, so no trailing ': '.
    fn emit_refusal(&self, msg: &str, target_bitrate: Option<u64>) {
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::error_stream_events(
                &format!("error - {msg}"),
            ));
        } else if self.json_output {
            if !crate::macros::output_quiet() {
                println!(
                    "{}",
                    crate::json_report::refusal_document(&format!("error - {msg}"), target_bitrate)
                );
            }
        } else if !crate::macros::output_quiet() {
            eprintln!(
                "{}riperf3: error - {msg}",
                crate::macros::output_timestamp_prefix()
            );
        }
    }

    /// #330: the JSON half of a pre-test error's iperf_err sink — the -J
    /// skeleton document (populated `start`/`intervals:[]`/`end:{}` + `error`)
    /// or the --json-stream error+empty-end event pair. No-op in text mode and
    /// under the #290 console-quiet scope. Shared by [`Self::emit_pretest_error`]
    /// and the serve loop's residual generic arm.
    fn emit_pretest_error_doc(&self, doc_error: &str, target_bitrate: Option<u64>) {
        if crate::macros::output_quiet() {
            return;
        }
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::error_stream_events(
                doc_error,
            ));
        } else if self.json_output {
            // #377 r2 F1: the skeleton carries the ingested -b when the
            // caller still had params in scope (the auth deny site) — GT's
            // get_parameters stamps json_start before every later gate.
            println!(
                "{}",
                crate::json_report::refusal_document(doc_error, target_bitrate)
            );
        }
    }

    /// #356 r1 F1: a state byte arrived during the CREATE_STREAMS wait —
    /// dispatch it through GT's handle_message_server arms
    /// (iperf_server_api.c:236-311; each cell live-probed on issue #338):
    /// TEST_START keeps waiting (see the arm's deviation record — GT's
    /// state-write quirk is not mirrored), CLIENT_TERMINATE relays 119 and takes the
    /// terminate surfaces, IPERF_DONE is the clean errorless arm, and
    /// everything else is the IEMESSAGE default with the 110 relay.
    /// RECORDED DEVIATION: "everything else" includes TEST_END, where GT
    /// instead runs its end processing against streams that never existed
    /// (live: the report skeleton, an EXCHANGE_RESULTS byte, then a
    /// stale-errno IERECVRESULTS "Bad file descriptor" tangle) —
    /// nonconforming-only, not mirrored.
    async fn dispatch_setup_ctrl_byte(&self, ctx: &mut TestRunCtx) -> Result<SetupFlow> {
        match protocol::recv_state(&mut ctx.ctrl).await {
            // GT's no-op arm — keep waiting (the recreated sleep IS the
            // clock reset: a received byte is progress). RECORDED DEVIATION
            // (r2 F2): GT's Nread writes the byte INTO test->state
            // (iperf_server_api.c:249), so after a 0x01 GT is no longer in
            // CREATE_STREAMS and ACCESS_DENIED's a subsequent correct-cookie
            // data connect (live-probed); riperf3 keeps accepting — the
            // liveness-preserving reading of "no-op", not a mirror of the
            // state-variable quirk. The byte-then-EOF sub-cell matches GT.
            Ok(TestState::TestStart) => Ok(SetupFlow::Proceed),
            Ok(TestState::ClientTerminate) => {
                abort_setup_streams(ctx).await;
                let _ = protocol::send_server_error(&mut ctx.ctrl, 119).await;
                self.emit_setup_phase_terminate(ctx);
                Err(RiperfError::ClientTerminated)
            }
            Ok(TestState::IperfDone) => {
                abort_setup_streams(ctx).await;
                self.emit_setup_phase_done(ctx);
                Ok(SetupFlow::ClientDone)
            }
            Ok(_) | Err(RiperfError::UnknownControlMessage) => {
                abort_setup_streams(ctx).await;
                let _ = protocol::send_server_error(&mut ctx.ctrl, 110).await;
                self.emit_setup_phase_error(
                    ctx,
                    &format!("error - {}", RiperfError::UnknownControlMessage),
                );
                Err(RiperfError::UnknownControlMessage)
            }
            // The byte vanished between the readiness watch and the read —
            // only a racing EOF does that; take the EOF surface (with the
            // #342 relay, like the watch's own EOF arm — r2 F1).
            Err(RiperfError::PeerDisconnected) => {
                abort_setup_streams(ctx).await;
                let _ = protocol::send_server_error(&mut ctx.ctrl, 109).await;
                self.emit_setup_phase_error(ctx, CTRL_CLOSED_MSG);
                Err(RiperfError::ControlSocketClosed)
            }
            Err(e) => Err(e),
        }
    }

    /// The setup-phase (CREATE_STREAMS) doc input: GT's bufsize trio comes
    /// off the LISTENER at listen time (iperf_tcp.c:337-377) — read the
    /// same socket (best-effort: a failed getsockopt omits the key rather
    /// than inventing one). The timestamp is the on-connect stamp carried
    /// on ctx, not the emit time (#356 r1 F4).
    ///
    /// #357: with -w exchanged, GT re-listens with the window applied and
    /// reads the actuals off THAT socket (probed 131072/131072 for window
    /// 65536); riperf3 has no re-listen step (deliberate), so a scratch
    /// socket with the same window applied yields the identical kernel
    /// read-back. Without -w the un-windowed listener matches GT's
    /// re-listened defaults.
    /// RECORDED DEVIATION (#391 r1 F3, pre-existing design consequence):
    /// at a window past 2x wmem_max GT fires IESETBUF2 at its re-listen
    /// (error doc, no trio, fe frame instead of CreateStreams); riperf3
    /// has no re-listen (deliberate) and its #97 check lives on data
    /// sockets, which never arrive in wedge cells — the doc carries the
    /// clamped actuals instead (probed w=16M: GT errors, riperf3 emits
    /// 8388608/8388608).
    /// #391 r1 F1: GT's buffer-APPLY guard is C truthiness (the re-listen
    /// decision is iperf_tcp.c:195, the apply gate :257 — r2 F2 cite) —
    /// an exchanged window:0 is NOT applied; it reads the listener like
    /// the no-window state (probed: GT 16384/131072 for window:0, and the
    /// nodelay-forced re-listen without buffer-apply reads the same
    /// defaults). Negative windows ARE applied (truthiness; both tools
    /// clamp identically — probed w=-1).
    /// #392: all of the above runs ONCE, at ctx construction — GT
    /// computes its trio at param ingest (protocol->listen right after
    /// get_parameters, iperf_api.c:2373-2382) and caches, so fd
    /// exhaustion at emit time can't blank the keys (the per-emit scratch
    /// failed EMFILE there), and the scratch follows the LISTENER's
    /// address family (the v6-only corner: an IPV4-hardcoded scratch read
    /// nothing on a v6-only stack).
    fn compute_setup_bufs(
        cfg: &TestConfig,
        listener: &tokio::net::TcpListener,
    ) -> (Option<u64>, Option<u64>) {
        if matches!(cfg.protocol, TransportProtocol::Udp) {
            return (None, None);
        }
        match cfg.window {
            Some(w) if w != 0 => {
                let domain = match listener.local_addr() {
                    Ok(std::net::SocketAddr::V6(_)) => socket2::Domain::IPV6,
                    _ => socket2::Domain::IPV4,
                };
                match socket2::Socket::new(domain, socket2::Type::STREAM, None) {
                    Ok(sc) => {
                        net::apply_socket_window(&socket2::SockRef::from(&sc), Some(w));
                        (
                            sc.send_buffer_size().ok().map(|v| v as u64),
                            sc.recv_buffer_size().ok().map(|v| v as u64),
                        )
                    }
                    // RECORDED DEVIATION (#416 r1, the no-re-listen
                    // umbrella): fd exhaustion at INGEST time with -w set —
                    // GT's re-listen socket() fails and IESTREAMLISTEN
                    // aborts the round; riperf3 has no re-listen
                    // (deliberate), caches absent keys, and the round
                    // proceeds.
                    Err(_) => (None, None),
                }
            }
            _ => {
                let sock = socket2::SockRef::from(listener);
                (
                    sock.send_buffer_size().ok().map(|v| v as u64),
                    sock.recv_buffer_size().ok().map(|v| v as u64),
                )
            }
        }
    }

    fn setup_phase_doc_input(&self, ctx: &TestRunCtx) -> crate::json_report::SetupPhaseDoc {
        // #383 r1 F1a: the four TCP-flavored keys are absent for UDP —
        // see the SetupPhaseDoc doc for the GT gates.
        let udp = matches!(ctx.cfg.protocol, TransportProtocol::Udp);
        // #392: computed ONCE at ctx construction (GT's param-ingest
        // timing) — see compute_setup_bufs for the machinery and records.
        let (sndbuf_actual, rcvbuf_actual) = ctx.setup_bufs;
        crate::json_report::SetupPhaseDoc {
            // Verbatim like GT, negatives included (#392 — `as u64` wrapped
            // an exchanged -1 to 18446744073709551615).
            sock_bufsize: (!udp).then(|| ctx.cfg.window.map(i64::from).unwrap_or(0)),
            sndbuf_actual,
            rcvbuf_actual,
            // The server never reads the ctrl MSS — 0, the #50 convention.
            tcp_mss_default: (!udp).then_some(0),
            udp,
            timemillisecs: ctx.accepted_millis,
            accepted_host: ctx.accepted_host.clone(),
            accepted_port: ctx.accepted_port,
            // The wire cookie is 37 bytes with a trailing NUL; the -J
            // string drops it (the :1092/:2925 convention).
            cookie: String::from_utf8_lossy(&ctx.cookie[..protocol::COOKIE_SIZE - 1]).to_string(),
            target_bitrate: ctx.cfg.bandwidth,
            fq_rate: ctx.cfg.fq_rate,
        }
    }

    /// #338: render a setup-phase (CREATE_STREAMS) error in GT's sink shape.
    /// Under -J: the POPULATED setup doc (on_connect + listener metadata,
    /// live-probed — see json_report::setup_error_document); --json-stream:
    /// the same error+end event pair GT emits here (live-probed, no start
    /// event); text: nothing — the serve loop's arms print the stderr line.
    fn emit_setup_phase_error(&self, ctx: &TestRunCtx, doc_error: &str) {
        if crate::macros::output_quiet() {
            return;
        }
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::error_stream_events(
                doc_error,
            ));
        } else if self.json_output {
            let doc = crate::json_report::setup_error_document(
                &self.setup_phase_doc_input(ctx),
                doc_error,
            );
            println!("{doc}");
        }
    }

    /// #356 r1 F1: the CLIENT_TERMINATE-at-setup surfaces. -J carries GT's
    /// FULL-zeros end (real host cpu, remote zeros — the reporter_callback
    /// ran); --json-stream the error + zeros-end pair; text prints the
    /// report skeleton on stdout here (the serve loop's arm adds the
    /// stderr sentence).
    fn emit_setup_phase_terminate(&self, ctx: &TestRunCtx) {
        if crate::macros::output_quiet() {
            return;
        }
        let error = RiperfError::ClientTerminated.to_string();
        let measured = crate::cpu::CpuSnapshot::now().utilization_since(&ctx.cpu_start);
        let cpu = crate::json_report::CpuUtilization {
            // Like build_report: only the server's own figures — GT never
            // surfaces the remote side here (no exchange happened at all).
            // RECORDED DEVIATION (r2 F4, value-level): GT's cpu_util
            // baseline arms just before TEST_START (iperf_server_api.c:925),
            // so in this pre-TEST_START cell GT measures against zeroed
            // statics (an epoch window, ~1e-05 live); riperf3 reports the
            // honest round window.
            host_total: measured.host_total,
            host_user: measured.host_user,
            host_system: measured.host_system,
            remote_total: 0.0,
            remote_user: 0.0,
            remote_system: 0.0,
        };
        let udp = matches!(ctx.cfg.protocol, TransportProtocol::Udp);
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::terminate_stream_events(
                udp, &error, &cpu,
            ));
        } else if self.json_output {
            let doc = crate::json_report::setup_terminate_document(
                &self.setup_phase_doc_input(ctx),
                &error,
                &cpu,
            );
            println!("{doc}");
        } else {
            crate::reporter::print_terminate_skeleton(udp);
        }
    }

    /// #356 r1 F1: the IPERF_DONE-at-setup surfaces — GT's clean arm: the
    /// errorless setup doc under -J, one bare end event under
    /// --json-stream, silence in text.
    fn emit_setup_phase_done(&self, ctx: &TestRunCtx) {
        if crate::macros::output_quiet() {
            return;
        }
        if self.json_stream {
            crate::reporter::emit_json_stream_line(&crate::json_report::done_stream_events());
        } else if self.json_output {
            let doc = crate::json_report::setup_done_document(&self.setup_phase_doc_input(ctx));
            println!("{doc}");
        }
    }

    /// #330: render a pre-test control error (IERECVCOOKIE / IERECVPARAMS) in
    /// GT's iperf_err sink shape — silent stderr under -J with the message in
    /// the skeleton doc, one text line otherwise. Both codes are perr=1, so the
    /// #248 dangling ": " rides along; riperf3 pins errno 0, leaving the suffix
    /// bare where GT would append a (often stale) strerror — the same honest
    /// deviation as #336.
    fn emit_pretest_error(&self, err: &RiperfError) {
        // #345: SendControlFailed's Display already carries the live
        // strerror (GT perr with a real errno); the errno-0 siblings keep
        // the #248 dangling ": ".
        let doc_error = match err {
            // #362 r1 F1: the strerror-carrying classes take no dangling
            // suffix — their Displays already end with the errno text.
            RiperfError::SendControlFailed(_)
            | RiperfError::AcceptFailed(_)
            | RiperfError::SetNoDelayFailed(_) => format!("error - {err}"),
            _ => format!("error - {err}: "),
        };
        if self.json_output || self.json_stream {
            self.emit_pretest_error_doc(&doc_error, None);
        } else if !crate::macros::output_quiet() {
            // #339 r2b F2: iperf_err stamps its stderr line with the
            // --timestamps prefix (iperf_error.c:51-57, :77) — same
            // output_timestamp_prefix() the stdout banner rides.
            eprintln!(
                "{}riperf3: {doc_error}",
                crate::macros::output_timestamp_prefix()
            );
        }
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

    /// Serve exactly one test on the held listener and return its
    /// [`RunOutcome`](crate::RunOutcome) — the same contract as
    /// [`Server::run_once`], minus the per-call rebind. No "Server
    /// listening" banner (a library entry point, not the daemon); quiet by
    /// default like every lib run (#294) — the test report prints in `-J` /
    /// text mode only if the server was built with `emit_output(true)`.
    pub async fn run_once(&self) -> Result<crate::outcome::RunOutcome> {
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
    rcv_timeout: Option<u64>,
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
            rcv_timeout: None,
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
            // #294: quiet by default (see ClientBuilder); the CLI opts in.
            emit_output: false,
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

    /// Console output from `run`/`run_once` (#290). `false` (the default
    /// since 0.9.0, #294) runs silently — reports flow only via the return
    /// value and the wire (`--get-server-output` still relays the text
    /// report to the requesting client); `true` prints iperf3's full
    /// text/JSON output, exactly like the CLI (which sets it). See
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

    /// `--rcv-timeout` (ms): the server's no-progress bound (#338). Unset:
    /// GT's DEFAULT_NO_MSG_RCVD_TIMEOUT (120000 ms).
    pub fn rcv_timeout(mut self, ms: u64) -> Self {
        self.rcv_timeout = Some(ms);
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
            rcv_timeout: self.rcv_timeout,
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

    /// #356 r1 F9: the unset-`--rcv-timeout` bound is GT's
    /// DEFAULT_NO_MSG_RCVD_TIMEOUT (iperf_api.h:70). The 120 s value can't
    /// be exercised by the suite, so the constant is the pin.
    #[test]
    fn default_rcv_timeout_is_gt_default() {
        assert_eq!(super::DEFAULT_RCV_TIMEOUT_MS, 120_000);
    }

    /// #316: the server adopts the client's exchanged GSO/GRO request
    /// (GT iperf_api.c:2599-2619), including the zero-dg_size recompute
    /// from the negotiated blksize (:2607-2613).
    /// #414: the wire trio's ingest semantics, per GT's get_parameters.
    #[test]
    fn from_params_ingests_the_414_trio_like_gt() {
        // repeating_payload is PRESENCE-triggered (GT sets 1 whatever the
        // value, iperf_api.c:2645-2646); dont_fragment takes the value
        // truthily (:2650-2651).
        let set = crate::protocol::TestParams {
            repeating_payload: Some(0), // presence wins — GT ignores the value
            dont_fragment: Some(1),
            ..Default::default()
        };
        let cfg = TestConfig::from_params(&set).unwrap();
        assert!(cfg.repeating_payload, "presence-triggered like GT");
        assert!(cfg.dont_fragment);

        let unset = crate::protocol::TestParams::default();
        let cfg = TestConfig::from_params(&unset).unwrap();
        assert!(!cfg.repeating_payload);
        assert!(!cfg.dont_fragment);

        let df_zero = crate::protocol::TestParams {
            dont_fragment: Some(0), // falsy value → off, GT truthiness
            ..Default::default()
        };
        assert!(!TestConfig::from_params(&df_zero).unwrap().dont_fragment);
    }

    /// #415 (r1 F1): a wire `"window": 0` ingests as UNSET — GT's
    /// socket_bufsize 0 is the unset sentinel in EVERY consumer, including
    /// the UDP auto-increase arm (iperf_udp.c:563/:691, the GT analogue of
    /// the #163 sndbuf bump). Pre-fix the two UDP-sender arms'
    /// `uw = window.is_some()` treated a pre-0.9.0 client's `"window": 0`
    /// as user-set and suppressed the bump. Nonzero values — negatives
    /// included (#392) — ingest verbatim.
    #[test]
    fn from_params_window_zero_ingests_as_unset() {
        let zero = crate::protocol::TestParams {
            window: Some(0),
            ..Default::default()
        };
        assert_eq!(
            TestConfig::from_params(&zero).unwrap().window,
            None,
            "wire window 0 must ride every unset arm, like GT's 0 sentinel"
        );
        let explicit = crate::protocol::TestParams {
            window: Some(65536),
            ..Default::default()
        };
        assert_eq!(
            TestConfig::from_params(&explicit).unwrap().window,
            Some(65536)
        );
        let negative = crate::protocol::TestParams {
            window: Some(-1),
            ..Default::default()
        };
        assert_eq!(
            TestConfig::from_params(&negative).unwrap().window,
            Some(-1),
            "negatives stay verbatim (#392) — GT applies them and lets the kernel decide"
        );
    }

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

    /// A minimal TestRunCtx for driving the setup fns directly (#358: they
    /// take the ctx whole so the select arms can dispatch ctrl bytes).
    #[cfg(target_os = "linux")]
    fn test_run_ctx(
        ctrl: tokio::net::TcpStream,
        params: TestParams,
        cfg: TestConfig,
    ) -> TestRunCtx {
        TestRunCtx {
            ctrl,
            accepted_host: "127.0.0.1".into(),
            accepted_port: 0,
            accepted_millis: 0,
            setup_bufs: (None, None),
            cookie: [b'x'; 37],
            params,
            cfg,
            want_server_output: false,
            capture: None,
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
            unknown_message: false,
            ctrl_closed: false,
            early_done: false,
            exchange_recv_failed: false,
            exchange_recv_message_error: None,
            exchange_send_error: None,
            bare_end: false,
            interrupted: None,
        }
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
        let (ctrl_srv, _) = l.accept().await.unwrap();

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
        let mut ctx = test_run_ctx(ctrl_srv, params, cfg);

        let client = udp_tos_probe_client(format!("127.0.0.1:{port}").parse().unwrap());

        let mut abort_guard = stream::AbortStreamsOnDrop::new();
        srv.setup_udp_recycling_streams(&mut ctx, 0, 1, None, &mut abort_guard)
            .await
            .unwrap();
        abort_guard.disarm();
        ctx.start.store(true, Ordering::Relaxed);

        let tos = client.join().unwrap();
        ctx.done.store(true, Ordering::Relaxed);
        for s in ctx.streams.drain(..) {
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
        let (ctrl_srv, _) = l.accept().await.unwrap();

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
        let mut ctx = test_run_ctx(ctrl_srv, params, cfg);

        let client = udp_tos_probe_client(format!("127.0.0.1:{port}").parse().unwrap());

        let mut abort_guard = stream::AbortStreamsOnDrop::new();
        srv.setup_udp_demux_streams(&mut ctx, 0, 1, None, &mut abort_guard)
            .await
            .unwrap();
        abort_guard.disarm();
        ctx.start.store(true, Ordering::Relaxed);

        let tos = client.join().unwrap();
        ctx.done.store(true, Ordering::Relaxed);
        for s in ctx.streams.drain(..) {
            let _ = s.task.await;
        }
        if let Some(h) = ctx.udp_demux_handle.take() {
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
