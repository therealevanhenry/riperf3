use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use crate::error::{Result, RiperfError, UnknownState};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Size of the authentication cookie on the wire (36 random chars + null).
pub const COOKIE_SIZE: usize = 37;

/// Character set used by iperf3's make_cookie() — base32-like alphabet.
const COOKIE_CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Maximum size of the parameter JSON the server will accept (8 KiB).
pub const MAX_PARAMS_JSON_LEN: usize = 8 * 1024;

/// UDP "connect" handshake magic values.
pub const UDP_CONNECT_MSG: u32 = 0x3637_3839; // "6789" in ASCII
pub const UDP_CONNECT_REPLY: u32 = 0x3938_3736; // "9876" in ASCII

// ---------------------------------------------------------------------------
// Test state — the single-byte state machine on the control connection
// ---------------------------------------------------------------------------

/// Protocol states exchanged as a single signed byte on the TCP control socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestState {
    TestStart,
    TestRunning,
    TestEnd,
    ParamExchange,
    CreateStreams,
    ServerTerminate,
    ClientTerminate,
    ExchangeResults,
    DisplayResults,
    IperfStart,
    IperfDone,
    AccessDenied,
    ServerError,
}

impl TestState {
    /// Encode this state for the wire as a signed byte.
    pub fn to_wire(self) -> i8 {
        match self {
            Self::TestStart => 1,
            Self::TestRunning => 2,
            Self::TestEnd => 4,
            Self::ParamExchange => 9,
            Self::CreateStreams => 10,
            Self::ServerTerminate => 11,
            Self::ClientTerminate => 12,
            Self::ExchangeResults => 13,
            Self::DisplayResults => 14,
            Self::IperfStart => 15,
            Self::IperfDone => 16,
            Self::AccessDenied => -1,
            Self::ServerError => -2,
        }
    }

    /// Decode a wire byte into a TestState.
    pub fn from_wire(b: i8) -> std::result::Result<Self, UnknownState> {
        match b {
            1 => Ok(Self::TestStart),
            2 => Ok(Self::TestRunning),
            4 => Ok(Self::TestEnd),
            9 => Ok(Self::ParamExchange),
            10 => Ok(Self::CreateStreams),
            11 => Ok(Self::ServerTerminate),
            12 => Ok(Self::ClientTerminate),
            13 => Ok(Self::ExchangeResults),
            14 => Ok(Self::DisplayResults),
            15 => Ok(Self::IperfStart),
            16 => Ok(Self::IperfDone),
            -1 => Ok(Self::AccessDenied),
            -2 => Ok(Self::ServerError),
            other => Err(UnknownState(other)),
        }
    }
}

// ---------------------------------------------------------------------------
// Control-state transition table (#145) — AUDITABILITY ONLY
// ---------------------------------------------------------------------------

/// Which side of the control connection a peer is, for the transition table.
///
/// CLIENT-ONLY since #330: the server's message handling is arm-explicit
/// like GT's single switch (#325 removed its end-loop consult, #330 the
/// data-phase one), so only the client's diagnostics consult the table.
/// The enum stays so a future server row would be an additive change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Client,
}

/// The states it is LEGAL to RECEIVE next, given the last state this side
/// processed and which `role` it plays.
///
/// AUDITABILITY TABLE, NOT AN IDEALIZED PROTOCOL. This deliberately matches
/// iperf3's *actual* tolerances — derived from the server's send sequence
/// plus its one documented looseness (a re-sent `TEST_RUNNING` is a no-op on
/// the client) — rather than a stricter ideal. (#325 established that GT's
/// server end-of-test loop does NOT tolerate intervening bytes — its switch
/// IEMESSAGEs everything but TEST_START/TEST_END/IPERF_DONE/CLIENT_TERMINATE
/// — so the server end loop no longer consults this table at all.) It is
/// consulted ONLY to emit diagnostics (`log::debug!`, off by default), and
/// ONLY by the client (#330 removed the server's last consult — its message
/// loops are arm-explicit like GT's switch). It is NEVER used to reject or
/// error on a sequence: iperf3 itself is loose in the surviving rows, and a
/// stricter riperf3 would break interop. Default-tolerant by design (#145).
///
/// `ServerTerminate`/`ServerError` can arrive in any client state (the client
/// adopts them via their own dispatch arms, and `watch_control` returns on
/// them), so they appear in every applicable row to avoid false out-of-sequence
/// logs. `AccessDenied` is sent only in the early param/cookie phase, so it
/// lives in the `IperfStart` row alone and never reaches a data-phase consult.
pub(crate) fn legal_next(current: TestState, role: Role) -> &'static [TestState] {
    use TestState::*;
    match role {
        Role::Client => match current {
            IperfStart => &[ParamExchange, AccessDenied, ServerError, ServerTerminate],
            ParamExchange => &[CreateStreams, ServerError, ServerTerminate],
            CreateStreams => &[TestStart, ServerError, ServerTerminate],
            TestStart => &[TestRunning, ServerError, ServerTerminate],
            // re-sent TEST_RUNNING = iperf3 no-op tolerance.
            TestRunning => &[TestRunning, ExchangeResults, ServerError, ServerTerminate],
            ExchangeResults => &[DisplayResults, ServerError, ServerTerminate],
            DisplayResults => &[IperfDone, ServerError, ServerTerminate],
            // Terminal — the client adopts these and stops.
            ServerTerminate | ServerError | AccessDenied => &[],
            _ => &[],
        },
    }
}

/// Membership test over [`legal_next`]: is `got` a legal state to receive next,
/// given `current` and `role`? Diagnostics-only, like [`legal_next`] — never
/// consulted to reject a sequence (#145).
pub(crate) fn is_legal_next(current: TestState, got: TestState, role: Role) -> bool {
    legal_next(current, role).contains(&got)
}

// ---------------------------------------------------------------------------
// Transport protocol
// ---------------------------------------------------------------------------

/// Which data-plane transport to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive] // a future transport (e.g. SCTP) must be additive, not breaking
pub enum TransportProtocol {
    #[default]
    Tcp,
    Udp,
}

// ---------------------------------------------------------------------------
// Cookie generation and I/O
// ---------------------------------------------------------------------------

/// Generate a 37-byte cookie matching iperf3's `make_cookie()`.
/// 36 random characters from the base32-like charset, followed by a null byte.
pub fn make_cookie() -> [u8; COOKIE_SIZE] {
    let mut rng = rand::rng();
    let mut cookie = [0u8; COOKIE_SIZE];
    for byte in cookie[..COOKIE_SIZE - 1].iter_mut() {
        *byte = COOKIE_CHARSET[rng.random_range(0..COOKIE_CHARSET.len())];
    }
    // cookie[36] is already 0 (null terminator)
    cookie
}

/// Send the 37-byte cookie on a TCP stream.
pub async fn send_cookie(stream: &mut TcpStream, cookie: &[u8; COOKIE_SIZE]) -> Result<()> {
    stream.write_all(cookie).await?;
    Ok(())
}

/// Read a 37-byte cookie from a TCP stream. Bounded like every GT Nread
/// (net.c:75-76) — iperf_server_api.c:194-200's IERECVCOOKIE comment names
/// the timed-out read explicitly, and live GT self-recovers from a
/// connect-and-hold peer in ~20 s. Unbounded, one hostile peer parked the
/// serial serve loop forever (#339 r2b F1). Both call sites are server-side
/// (the control cookie and the data-stream cookie).
pub async fn recv_cookie(stream: &mut TcpStream) -> Result<[u8; COOKIE_SIZE]> {
    let deadline = tokio::time::Instant::now() + NREAD_OVERALL_TIMEOUT;
    let mut cookie = [0u8; COOKIE_SIZE];
    nread_exact(stream, &mut cookie, deadline).await?;
    Ok(cookie)
}

// ---------------------------------------------------------------------------
// State I/O
// ---------------------------------------------------------------------------

/// Send a state transition as a single signed byte.
pub async fn send_state(stream: &mut TcpStream, state: TestState) -> Result<()> {
    let buf = [state.to_wire() as u8];
    stream.write_all(&buf).await?;
    Ok(())
}

/// Send SERVER_ERROR with iperf3's (i_errno, errno) u32-pair payload (#224):
/// the state byte, then both words big-endian — iperf_server_api.c's Nwrite
/// pair (the bitrate, duration-timer, and cleanup_server relay sites). The os
/// errno word is 0 from this form — most relayed causes carry none, and
/// where one exists (#345's send failure) the peer's RST makes the relay
/// unobservable anyway; GT itself often leaks a stale word here (#336).
/// A cause with a REAL live errno uses [`send_server_error_errno`].
pub async fn send_server_error(stream: &mut TcpStream, i_errno: u32) -> Result<()> {
    send_server_error_errno(stream, i_errno, 0).await
}

/// [`send_server_error`] with a live os errno word (#387 r1 F2, wording
/// corrected r2 F2): GT's cleanup_server wires htonl(errno) captured
/// live (iperf_server_api.c:470-471). A GT client prints its immediate
/// `SERVER ERROR - …` line in BOTH branches — the wire word gates only
/// the `, errno: <strerror>` suffix and the strerror content
/// (iperf_client_api.c:403-407; live: fe+203+0 still prints the dangling
/// line) — so the accept-failure relays carry the real errno for the
/// CONTENT parity, not the line's existence.
pub async fn send_server_error_errno(
    stream: &mut TcpStream,
    i_errno: u32,
    os_errno: u32,
) -> Result<()> {
    send_state(stream, TestState::ServerError).await?;
    stream.write_all(&i_errno.to_be_bytes()).await?;
    stream.write_all(&os_errno.to_be_bytes()).await?;
    Ok(())
}

/// Read SERVER_ERROR's (i_errno, errno) payload. `None` when it never
/// arrives (a peer that died mid-relay, held the socket, or sent a bare
/// -2): the caller degrades to its generic message — a payloadless
/// SERVER_ERROR must error cleanly, never hang or panic (tested
/// in-crate). Bounded like every GT Nread (#382 r1 F1 — GT bounds these
/// two reads, iperf_client_api.c:393-401, exiting a hold at ~30 s with a
/// garbage-errno line from the UNINITIALIZED short-read buffer; the
/// deterministic None-fallback is the recorded deviation — GT's rc<0
/// arm maps to IECTRLREAD (iperf_client_api.c:394-400) and folds into
/// the same fallback here, pre-existing — and the house 10 s idle bound
/// fires first on a fully-silent hold). This read races NO interrupt
/// arm, so an unbounded read was signal-immune; post-fix a signal in
/// this window is honored when the bound fires (≤10 s silent, ≤30 s
/// dripping — GT's select EINTRs immediately; adversarial-only, the
/// recv_cookie/json_read_bounded house pattern) (#382 r2 F2/F3).
pub async fn read_server_error_payload(stream: &mut TcpStream) -> Option<(u32, u32)> {
    let deadline = tokio::time::Instant::now() + NREAD_OVERALL_TIMEOUT;
    let mut buf = [0u8; 8];
    match nread_exact(stream, &mut buf, deadline).await {
        Ok(()) => Some((
            u32::from_be_bytes(buf[0..4].try_into().unwrap()),
            u32::from_be_bytes(buf[4..8].try_into().unwrap()),
        )),
        Err(_) => None,
    }
}

/// Read a state-transition byte from the control connection (r1 n4: this doc
/// was absorbed by the send_server_error insertion; restored).
pub async fn recv_state(stream: &mut TcpStream) -> Result<TestState> {
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Err(RiperfError::PeerDisconnected);
    }
    // #325: GT's IEMESSAGE — an unmapped control byte fails with its exact
    // sentence (iperf_error.c:302; the server's state switch has no
    // tolerant default, iperf_server_api.c:309-311). Dedicated variant so
    // the sentence prints bare, not "protocol violation: "-wrapped (r1 F2).
    TestState::from_wire(buf[0] as i8).map_err(|u| {
        log::debug!("control byte {} is not a known state", u.0);
        RiperfError::UnknownControlMessage
    })
}

// ---------------------------------------------------------------------------
// Length-prefixed JSON I/O
// ---------------------------------------------------------------------------

/// Write a JSON value with a 4-byte big-endian length prefix (iperf3 wire format).
pub async fn json_write(stream: &mut TcpStream, value: &serde_json::Value) -> Result<()> {
    let payload = serde_json::to_string(value)?;
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(payload.as_bytes()).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Test parameters — exchanged as JSON during ParamExchange
// ---------------------------------------------------------------------------

/// UTF-8 BOM (#367 cell 1): cJSON's parse entry runs `skip_utf8_bom` before
/// `parse_value` (cjson.c:1074-1088, :1131), so a leading BOM ahead of valid
/// JSON parses on every blob. serde has no such skip; strip it here to match.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// #343: GT parses every length-prefixed blob with
/// cJSON_Parse(require_null_terminated=0) — the first JSON value wins and
/// trailing bytes inside the declared length are ignored. Deliberate wire
/// leniency GT depends on (a peer that over-declares its length
/// interoperates), mirrored per the #328 fidelity precedent.
///
/// SCOPE (#343 r1 F2 / #367): the mirror covers self-delimiting first values
/// with whitespace-separated tails, plus a leading UTF-8 BOM (#367 cell 1,
/// now stripped here — cJSON's skip_utf8_bom). The bare-scalar params root
/// (#367 cell 2) is mirrored in `params_from_value`. Four residual cJSON
/// cells stay divergent as RECORDED DEVIATIONS — serde is a conforming
/// parser and mirroring them means a bespoke lenient parser for shapes no
/// real iperf3 encoder emits: scalar-adjacent garbage (`42GARBAGE`), nesting
/// past serde's 128-deep limit (cJSON's is 1000), raw control chars in
/// strings, and cJSON's lenient strtod grammar (`01`, `1.`). Locked by
/// `json_first_value_deviations_stay_strict`.
fn json_first_value(buf: &[u8]) -> serde_json::Result<serde_json::Value> {
    let buf = buf.strip_prefix(UTF8_BOM).unwrap_or(buf);
    let mut stream = serde_json::Deserializer::from_slice(buf).into_iter();
    match stream.next() {
        Some(v) => v,
        // Empty/whitespace-only: surface the standard EOF error shape.
        None => serde_json::from_slice(buf),
    }
}

fn is_false_opt(v: &Option<bool>) -> bool {
    matches!(v, None | Some(false))
}

/// Test parameters sent by the client and received by the server.
/// Fields use `Option` so that unset values are omitted from the wire JSON,
/// matching iperf3's conditional `cJSON_AddItemToObject` behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TestParams {
    #[serde(skip_serializing_if = "is_false_opt", default)]
    pub tcp: Option<bool>,

    #[serde(skip_serializing_if = "is_false_opt", default)]
    pub udp: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub omit: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub time: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub num: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub blockcount: Option<u64>,

    #[serde(rename = "MSS", skip_serializing_if = "Option::is_none", default)]
    pub mss: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub nodelay: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parallel: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reverse: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bidirectional: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub window: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub len: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bandwidth: Option<u64>,

    /// `-b rate/burst` block count; iperf3 sends it only when nonzero (#160).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub burst: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fqrate: Option<u64>,

    // #316: GT sends the UDP GSO/GRO block unconditionally for UDP
    // ("Always send these fields to allow server to use GSO/GRO even if
    // client doesn't support it", iperf_api.c:2465-2472) — flags plus the
    // datagram/buffer sizes the peer's send/recv paths adopt (:2599-2619).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gso: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gso_dg_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gso_bf_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gro: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gro_bf_size: Option<i64>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pacing_timer: Option<i32>,

    #[serde(rename = "TOS", skip_serializing_if = "Option::is_none", default)]
    pub tos: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub flowlabel: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub congestion: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub congestion_used: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub extra_data: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub get_server_output: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub udp_counters_64bit: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub repeating_payload: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dont_fragment: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub client_version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub authtoken: Option<String>,
}

impl TestParams {
    /// Normalize a `0` byte/block limit to `None` (= no limit). iperf3
    /// *unconditionally* serializes `num`/`blockcount`, sending `0` for a plain
    /// `-t` run — and since #303 riperf3 sends them the same way — while serde
    /// `default` only fills a *missing* field, so the pair arrives `Some(0)`. Without
    /// this, the server's `is_none()`/`is_some()` limit checks misread a duration
    /// run as byte-limited: it disables the UDP-sender `-t` deadline (#5 hang
    /// risk) and skews the summary window (#103). Call once at param ingest. #119
    pub(crate) fn normalize_unlimited(&mut self) {
        if self.num == Some(0) {
            self.num = None;
        }
        if self.blockcount == Some(0) {
            self.blockcount = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Test results — exchanged as JSON during ExchangeResults
// ---------------------------------------------------------------------------

/// Deserialize an integer that iperf3 may report as a "-1 = unavailable"
/// sentinel (e.g. retransmit info when the OS doesn't expose it). iperf3 ≤ 3.12
/// serializes that -1 as `u64::MAX` in the results JSON, which overflows the
/// signed Rust type and would otherwise fail the whole test at result decode
/// (issue #24). Normalize any value past `i64::MAX` (incl. `u64::MAX`) to -1.
/// No `visit_u128` is needed: these counts derive from a `u32`
/// (`tcpi_total_retrans`), so a JSON integer above `u64::MAX` never occurs.
fn de_retransmit_sentinel<'de, D>(deserializer: D) -> std::result::Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct SentinelVisitor;
    impl serde::de::Visitor<'_> for SentinelVisitor {
        type Value = i64;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an integer (iperf3 may send u64::MAX or -1 for \"unavailable\")")
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<i64, E> {
            Ok(v)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<i64, E> {
            Ok(if v > i64::MAX as u64 { -1 } else { v as i64 })
        }
    }
    deserializer.deserialize_any(SentinelVisitor)
}

/// Per-stream result data included in the results JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive] // result model: consumers read it; future fields must be additive
pub struct StreamResultJson {
    pub id: i32,
    pub bytes: u64,
    #[serde(deserialize_with = "de_retransmit_sentinel")]
    pub retransmits: i64,
    pub jitter: f64,
    pub errors: i64,
    // iperf 3.12 omits the omitted_* fields from the stream object (#24).
    // `None` = the keys were ABSENT (an old peer) — distinct from an
    // exchanged 0, because GT's old-peer posture substitutes all-omitted
    // baselines (#271, iperf_api.c:2914-2950); resolve via
    // [`resolve_peer_omitted`] before netting. riperf3 itself always sends
    // both keys (the serializer skips None, which riperf3 never produces).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_errors: Option<i64>,
    pub packets: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_packets: Option<i64>,
    pub start_time: f64,
    pub end_time: f64,
}

/// Resolve the exchanged omit baselines when the peer sent none (#271:
/// iperf3 <= 3.12 baselines its counters at the omit boundary exactly like
/// 3.21 — 3.12's iperf_api.c:3151 `omitted_packet_count = packet_count` —
/// it never exchanges the baselines, so its `errors`/`packets` arrive GROSS).
///
/// RECORDED DEVIATION (upstream: esnet/iperf#2055): GT's own
/// substitution (iperf_api.c:2914-2950) renders self-inconsistent figures
/// against old peers — on no-omit runs its JSON stream/sum figures swallow
/// real loss entirely (omitted_cnt_error := cnt_error; the TEXT receiver
/// line escapes via an omit==0 carve-out at :4383-4391 and shows the true
/// count), with `-O` it leaks the "-1 unknown" sentinel into the
/// per-stream JSON subtraction (stream lost = cnt_error + 1 while the sum
/// skips the sentinel at :4246-4248 — the same document disagrees with
/// itself by one; the TEXT line renders `Unknown/N` via
/// report_bw_udp_format_no_omitted_error, GT's one deliberate posture
/// here), and marks the peer's WHOLE packet count omitted on the receiver
/// arm (:4288: lost_percent renders 0 beside a nonzero lost_packets). All
/// live-verified 3.21<->3.12 (2026-07-02). riperf3 renders a NUMERIC
/// estimate where GT's text says `Unknown` — an observable text
/// difference, accepted as part of this deviation. riperf3 nets with the
/// best estimate available on this side:
///
/// - `omitted_packets` := this host's own omitted count for the stream —
///   the sender's omitted SENT datagrams (the peer received at most that
///   many in the window; GT's sender arm uses the same estimate) or the
///   receiver's omitted RECEIVED count (GT collapses instead). 0 without
///   `-O`, where gross == net and the figures are exact.
/// - `omitted_errors` := 0 always — the error split is unknowable from a
///   gross total, so the honest figure is the un-netted count (under `-O`
///   this can slightly overstate loss by errors from the omit window;
///   consistent across the stream, sum, and percent surfaces).
pub fn resolve_peer_omitted(x: &StreamResultJson, local_omitted: i64) -> (i64, i64) {
    match (x.omitted_errors, x.omitted_packets) {
        (Some(e), Some(p)) => (e, p),
        _ => (0, local_omitted),
    }
}

/// Top-level results JSON exchanged between client and server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive] // result model: consumers read it; future fields must be additive
pub struct TestResultsJson {
    pub cpu_util_total: f64,
    pub cpu_util_user: f64,
    pub cpu_util_system: f64,
    #[serde(deserialize_with = "de_retransmit_sentinel")]
    pub sender_has_retransmits: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub congestion_used: Option<String>,
    // --get-server-output (#33): the server's console output (text mode) or
    // its full -J report (JSON mode), attached to the exchange exactly like
    // iperf3's server_output_text / server_output_json. `default` keeps blobs
    // from peers without the keys (incl. iperf3 without the flag) decoding.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_output_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_output_json: Option<serde_json::Value>,
    pub streams: Vec<StreamResultJson>,
}

// ---------------------------------------------------------------------------
// Convenience: send/recv typed structs as JSON
// ---------------------------------------------------------------------------

/// Send test parameters as length-prefixed JSON.
pub async fn send_params(stream: &mut TcpStream, params: &TestParams) -> Result<()> {
    let value = serde_json::to_value(params)?;
    json_write(stream, &value).await
}

/// Receive test parameters from length-prefixed JSON. Bounded like every GT
/// Nread (#339 r2b F1): a peer that holds mid-prefix or mid-body times out
/// into the IERECVPARAMS surface instead of parking the serve loop.
pub async fn recv_params(stream: &mut TcpStream) -> Result<TestParams> {
    let value = json_read_bounded(stream, MAX_PARAMS_JSON_LEN).await?;
    Ok(params_from_value(value)?)
}

/// Deserialize a params blob's root JSON value into `TestParams`.
///
/// #367 cell 2: a NON-OBJECT root yields all-defaults. GT's `get_parameters`
/// (iperf_api.c:2533+) is a sequence of `iperf_cJSON_GetObjectItemType`
/// lookups (iperf_util.c:444); on a non-object root every lookup misses, so
/// no field is set and GT proceeds to CREATE_STREAMS with defaults. serde's
/// `from_value` instead hard-errors on a non-map, so map the non-object case
/// to defaults to match. (A malformed object — e.g. a field of the wrong
/// JSON type, where GT's wrapper warns and defaults the field but serde
/// hard-errors — is a separate, broader cJSON-vs-serde gap, tracked in a
/// follow-up, not this cell.)
fn params_from_value(value: serde_json::Value) -> serde_json::Result<TestParams> {
    if value.is_object() {
        serde_json::from_value(value)
    } else {
        Ok(TestParams::default())
    }
}

/// Send test results as length-prefixed JSON.
pub async fn send_results(stream: &mut TcpStream, results: &TestResultsJson) -> Result<()> {
    let value = serde_json::to_value(results)?;
    json_write(stream, &value).await
}

/// The #271 shape validation shared by both results readers.
fn validate_results(value: serde_json::Value) -> Result<TestResultsJson> {
    let results: TestResultsJson = serde_json::from_value(value)?;
    // #271: GT accepts BOTH omitted_* keys (>=3.14 peer) or NEITHER (old
    // peer), and fails the exchange with IERECVRESULTS when exactly one is
    // present (iperf_api.c:2888-2892 — "For backward compatibility allow to
    // not receive 'omitted' statistics").
    if results
        .streams
        .iter()
        .any(|x| x.omitted_errors.is_some() != x.omitted_packets.is_some())
    {
        return Err(crate::error::RiperfError::RecvResultsFailed);
    }
    Ok(results)
}

/// GT's Nread_json warning lines bypass every sink (warning() is a raw
/// fprintf(stderr), iperf_api.c:126-129) — but the #290 quiet-guard
/// contract wins for embedded library runs (r1 F6 decision): a quiet
/// caller gets silence, everyone else gets GT's exact line.
fn gt_warning(msg: std::fmt::Arguments<'_>) {
    if !crate::macros::output_quiet() {
        eprintln!("warning: {msg}");
    }
}

/// GT's Nrecv read bounds (net.c:75-76): 10 s of idle silence or 30 s
/// overall ends the read with the partial count — the warnings then carry
/// that count like any short read (r1 F3: without these a peer that
/// half-sends and HOLDS parked the exchange forever; GT self-recovers).
const NREAD_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const NREAD_OVERALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// GT's `NET_HARDERROR` (net.h:50): the value `Nrecv` returns on a hard read
/// error, echoed verbatim into the size-read warning's `read returned %d`
/// (iperf_api.c:3074). It is -2, NOT -1 (`NET_SOFTERROR`, which `Nrecv`
/// never emits) — r2 finding 1, live-confirmed GT prints `-2`.
const NET_HARDERROR: i32 = -2;

/// One GT-bounded read step: Ok(Some(n)) = n bytes, Ok(None) = EOF or a
/// timeout (GT's Nrecv returns the partial), Err = hard read error.
///
/// RECORDED DEVIATION (r2 finding 5): the 10 s idle bound is fresh PER read
/// step (idle-since-last-progress). GT-on-Linux instead threads ONE
/// `select(2)` timeout that the kernel decrements in place across a single
/// `Nread` (net.c:422), i.e. a cumulative-per-Nread budget; GT-on-BSD/macOS
/// (where `select` leaves the timeout untouched, per POSIX) matches our
/// per-step reset. Divergence is adversarial-only (a byte-dripping peer:
/// GT-Linux gives up at ~10 s cumulative, we at up to the 30 s overall) and
/// the real results blob is one atomic write; both paths stay bounded.
///
/// The OTHER direction (#340 audit N6): callers thread one OVERALL deadline
/// across BOTH the size and blob reads, where GT arms a fresh `ftimeout`
/// per `Nread` (net.c:485-489) — GT's worst case is ~2×30 s, riperf3's is
/// strictly tighter at 30 s total. Adversarial-only, same surface.
async fn nread_step(
    stream: &mut TcpStream,
    buf: &mut [u8],
    deadline: tokio::time::Instant,
) -> std::result::Result<Option<usize>, std::io::Error> {
    tokio::select! {
        r = stream.read(buf) => match r {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(n)),
            Err(e) => Err(e),
        },
        _ = tokio::time::sleep(NREAD_IDLE_TIMEOUT) => Ok(None),
        _ = tokio::time::sleep_until(deadline) => Ok(None),
    }
}

/// GT-bounded read_exact on top of [`nread_step`]: fills `buf` fully or
/// fails — EOF, the idle bound, and the overall deadline all collapse to
/// `UnexpectedEof` (GT's Nread returns the partial count and every
/// exact-size caller treats a short read as the failure; the pre-test
/// callers have no warning surface — that parity is a #330 residual).
async fn nread_exact(
    stream: &mut TcpStream,
    buf: &mut [u8],
    deadline: tokio::time::Instant,
) -> std::result::Result<(), std::io::Error> {
    let mut got = 0usize;
    while got < buf.len() {
        match nread_step(stream, &mut buf[got..], deadline).await? {
            None => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Some(n) => got += n,
        }
    }
    Ok(())
}

/// The bounded length-prefixed JSON read (#339 r2b F1): Nread's
/// idle/overall bounds so a holding peer can't park the reader. The
/// params slot's reader; the results slot has its own warning-parity
/// reader below (#330/#374 — which retired the plain unbounded reader
/// this fn was once the bounded variant of).
async fn json_read_bounded(stream: &mut TcpStream, max_len: usize) -> Result<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + NREAD_OVERALL_TIMEOUT;
    let mut len_buf = [0u8; 4];
    nread_exact(stream, &mut len_buf, deadline).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if max_len > 0 && len > max_len {
        return Err(RiperfError::Protocol(format!(
            "JSON payload too large: {len} bytes (max {max_len})"
        )));
    }

    let mut buf = vec![0u8; len];
    nread_exact(stream, &mut buf, deadline).await?;
    let value: serde_json::Value = json_first_value(&buf)?;
    Ok(value)
}

/// The results read, BOTH roles (#330 server, #374 client — GT's
/// get_results, iperf_api.c:2801, is called by both roles at
/// iperf_api.c:2400/2404): a malformed read gets GT's Nread_json warning
/// surface (JSON_read, iperf_api.c:3036-3080 — all five arms,
/// deterministic text and counts, live-probed under -J and text)
/// and maps to the IERECVRESULTS class each role renders at its own site.
/// Reads are bounded like GT's Nrecv (#374 live probes: the client's
/// state-byte WAITS are unbounded in GT — silent post-accept,
/// post-TestEnd, and pre-DisplayResults wedges all exceeded 45 s — so
/// recv_state stays unbounded by design; only in-message reads bound).
pub async fn recv_results(stream: &mut TcpStream) -> Result<TestResultsJson> {
    let deadline = tokio::time::Instant::now() + NREAD_OVERALL_TIMEOUT;
    let mut len_buf = [0u8; 4];
    let mut got = 0usize;
    while got < 4 {
        match nread_step(stream, &mut len_buf[got..], deadline).await {
            // GT's Nread returns the partial count on EOF/timeout; the
            // warning echoes it verbatim (live: "read returned 0; errno=0").
            Ok(None) => {
                gt_warning(format_args!(
                    "Failed to read JSON data size - read returned {got}; errno=0"
                ));
                return Err(crate::error::RiperfError::RecvResultsFailed);
            }
            Ok(Some(n)) => got += n,
            // A hard read error: GT's Nrecv returns NET_HARDERROR and the
            // size warning echoes that raw rc (iperf_api.c:3074) — so the
            // literal is -2, not -1 (r2 finding 1).
            Err(e) => {
                gt_warning(format_args!(
                    "Failed to read JSON data size - read returned {NET_HARDERROR}; errno={}",
                    e.raw_os_error().unwrap_or(0)
                ));
                return Err(crate::error::RiperfError::RecvResultsFailed);
            }
        }
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        // GT's hsize>0 gate (iperf_api.c:3038/:3069): a zero size warns
        // the overflow line — live-probed wording (r1 F4).
        gt_warning(format_args!(
            "JSON data length overflow - 0 bytes JSON size is not allowed"
        ));
        return Err(crate::error::RiperfError::RecvResultsFailed);
    }
    // GT calloc's the buffer upfront and NULL-guards it (iperf_api.c:3042-
    // 3043), degrading to IERECVRESULTS with NO warning on alloc failure —
    // and calloc's zero pages are LAZY. The previous try_reserve+resize here
    // memset every page in, COMMITTING a hostile 4 GiB prefix as real RSS
    // (#340, unauthenticated). riperf3 instead grows the buffer as bytes
    // arrive (fallible reserve per chunk = GT's NULL-guard degrade class),
    // so the hostile prefix costs nothing anywhere — a deliberate
    // improvement over GT's upfront virtual reservation, same surface.
    const READ_CHUNK: usize = 64 * 1024;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK.min(len)];
    let mut got = 0usize;
    while got < len {
        let take = chunk.len().min(len - got);
        match nread_step(stream, &mut chunk[..take], deadline).await {
            // EOF/timeout: GT's Nread returned the partial (rc >= 0) — the
            // expected/received arm (live: "expected 500 bytes but
            // received 5; errno=0"). GT prints BOTH counts through %d
            // (iperf_api.c:3056: hsize, rc), so each two's-complement wraps
            // past INT_MAX (#341: 0xFFFFFFF0 → "expected -16"; the received
            // arm needs a >2^31-byte transfer to diverge — r1's note; the
            // cast is well-defined since got <= len <= u32::MAX).
            Ok(None) => {
                gt_warning(format_args!(
                    "JSON size of data read does not correspond to offered length - \
                     expected {} bytes but received {}; errno=0",
                    len as u32 as i32, got as u32 as i32
                ));
                return Err(crate::error::RiperfError::RecvResultsFailed);
            }
            Ok(Some(n)) => {
                if buf.try_reserve(n).is_err() {
                    return Err(crate::error::RiperfError::RecvResultsFailed);
                }
                buf.extend_from_slice(&chunk[..n]);
                got += n;
            }
            // A hard read error is GT's rc<0 arm (iperf_api.c:3061; r1 F1 —
            // live RST probe: "JSON data read failed; errno=104").
            Err(e) => {
                gt_warning(format_args!(
                    "JSON data read failed; errno={}",
                    e.raw_os_error().unwrap_or(0)
                ));
                return Err(crate::error::RiperfError::RecvResultsFailed);
            }
        }
    }
    let value: serde_json::Value =
        json_first_value(&buf).map_err(|_| crate::error::RiperfError::RecvResultsFailed)?;
    validate_results(value)
}

// ---------------------------------------------------------------------------
// UDP "connect" handshake
// ---------------------------------------------------------------------------

/// Total time the client keeps (re)sending its connect magic while waiting for
/// the server's reply. Matches iperf3's ~30s connect read timeout so a slow or
/// lossy (high-RTT) path isn't given up on earlier than iperf3 would. Also used
/// by the server as its per-stream connect wait, so neither side aborts while
/// the other is still trying.
pub const UDP_CONNECT_TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the client resends the magic while waiting (recovers a lost magic).
const UDP_CONNECT_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Is this send/recv error the kernel relaying ICMP feedback for an earlier
/// datagram on a connected UDP socket (reset/refused), rather than a real
/// socket failure? Transient by definition: the peer state it reports is
/// already in the past, and the handshake deadline bounds how long we keep
/// trying past it.
fn transient_udp_handshake_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
    )
}

/// Client-side UDP connect handshake.
/// Sends the magic word and waits for the server's reply.
/// Note: iperf3 uses native byte order (not network byte order) for the magic values.
///
/// The handshake is over UDP, so a lost magic word would otherwise stall setup
/// forever (issue #11). The client resends its magic at most once per
/// `UDP_CONNECT_RETRY_INTERVAL` until it gets the reply or the overall
/// `UDP_CONNECT_TOTAL_TIMEOUT` (~iperf3's read-timeout budget) elapses, then
/// errors instead of hanging. A non-reply datagram (a stray/early packet) is
/// drained and ignored rather than treated fatally — like iperf3, which drains
/// packets while waiting for the reply. The resend is rate-limited by the
/// interval floor so a flood of stray datagrams can't be amplified into a flood
/// of resends.
///
/// This recovers a *lost magic* (the server is still waiting on the same
/// listener and replies to the resend). It does not recover a *lost reply*:
/// by then the server has connected its data socket and moved on, so the
/// resent magic lands harmlessly on that data socket and no second reply comes
/// — the client then fails cleanly at the deadline instead of hanging forever.
/// The retransmit is a riperf3 addition (iperf3's own client sends the magic
/// once and relies on a long read timeout); it stays interoperable with an
/// iperf3 server, which replies on first receipt.
pub async fn udp_connect_client(socket: &UdpSocket) -> Result<()> {
    let mut buf = [0u8; 4];
    let deadline = tokio::time::Instant::now() + UDP_CONNECT_TOTAL_TIMEOUT;
    let mut saw_traffic = false;
    let mut saw_icmp_feedback = false;
    // Resend no more than once per interval, even while draining strays, so a
    // stray-datagram flood can't turn into a magic-send flood (amplification).
    let mut next_send = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        if tokio::time::Instant::now() >= next_send {
            // A queued ICMP error can surface on send too — same transient
            // class as the recv side below; the interval still rate-limits.
            match socket.send(&UDP_CONNECT_MSG.to_ne_bytes()).await {
                Ok(_) => {}
                Err(e) if transient_udp_handshake_error(&e) => {
                    saw_icmp_feedback = true;
                }
                Err(e) => return Err(RiperfError::Io(e)),
            }
            next_send = tokio::time::Instant::now() + UDP_CONNECT_RETRY_INTERVAL;
        }
        // Wait until the next resend is due (or the deadline), whichever first.
        let wait = next_send
            .min(deadline)
            .saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(wait, socket.recv(&mut buf)).await {
            Ok(Ok(n))
                if n >= 4 && {
                    let reply = u32::from_ne_bytes(buf);
                    reply == UDP_CONNECT_REPLY || reply == 0xb168_de3a
                } =>
            {
                return Ok(())
            }
            // A stray/short datagram that isn't the reply: drain and ignore
            // (the real reply may be behind it). Don't resend here — the
            // interval floor gates the next send.
            Ok(Ok(_)) => {
                saw_traffic = true;
                continue;
            }
            // ECONNREFUSED/ECONNRESET here is the kernel relaying an ICMP
            // port-unreachable for a PRIOR datagram — on a connected UDP
            // socket it is transient feedback ("that one bounced"), not a
            // terminal socket state. Platform renderings differ (r1 review,
            // verified empirically): Unix delivers ECONNREFUSED (111) per
            // udp(7)/icmp_err_convert; Windows delivers WSAECONNRESET
            // (mio leaves SIO_UDP_CONNRESET on). Both kinds are needed.
            // The live producer is the server's per-stream listener REBIND
            // gap: udp_connect_server does recv → connect() → reply, and
            // only then does the caller bind the fresh listener — the next
            // stream's magic races that window, torn wide open on loaded
            // 2-core runners. (The #195 dossier's "os error 104" -J docs
            // were the OTHER leg: TCP control resets from a dying one-off's
            // backlog, which the harness-level retry covers.) The deadline
            // still bounds the whole handshake; the next interval's resend
            // recovers once the fresh listener is up. iperf3's
            // single-send/long-read client dies here — but the retry loop
            // is already a documented riperf3 robustness addition, and
            // riding through transient ICMP feedback is its natural scope.
            Ok(Err(e)) if transient_udp_handshake_error(&e) => {
                saw_icmp_feedback = true;
                continue;
            }
            Ok(Err(e)) => return Err(RiperfError::Io(e)),
            Err(_) => continue, // interval elapsed with no reply — loop resends
        }
    }
    // Distinct exhaustion diagnoses (r1 n5): persistent ICMP feedback means
    // the server's UDP port never came up — saying "only unexpected
    // datagrams" there (or a bare timeout) would misdirect the next session.
    Err(RiperfError::Protocol(
        match (saw_icmp_feedback, saw_traffic) {
            (true, false) => {
                "UDP connect handshake failed: ICMP port unreachable received \
                              and no valid reply for the whole budget"
            }
            (true, true) => {
                "UDP connect handshake failed: no valid reply (unexpected \
                             datagrams and ICMP feedback received)"
            }
            (false, true) => {
                "UDP connect handshake failed: no valid reply (only unexpected datagrams \
                 received)"
            }
            (false, false) => "UDP connect handshake timed out (no server reply)",
        }
        .into(),
    ))
}

/// Server-side UDP connect handshake — retained as the REFERENCE
/// implementation for the handshake round-trip tests below. #358 retired
/// its production call: the recycling setup now receives the magic inside
/// the #356 select machinery (server.rs) so ctrl-EOF/rcv-timeout are
/// honored, and runs the connect+reply after the arm wins (this fn's
/// select-cancellation would risk a lost reply). Validation semantics are
/// duplicated verbatim at that site.
/// Note: iperf3 uses native byte order (not network byte order) for the magic values.
#[cfg(test)]
pub async fn udp_connect_server(
    socket: &UdpSocket,
    timeout: std::time::Duration,
) -> Result<std::net::SocketAddr> {
    let mut buf = [0u8; 65536];
    let (n, addr) = match tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(RiperfError::Aborted(
                "timed out waiting for UDP stream connect".into(),
            ))
        }
    };
    if n < 4 {
        return Err(RiperfError::Protocol(
            "UDP connect message too short".into(),
        ));
    }
    let msg = u32::from_ne_bytes(buf[..4].try_into().unwrap());
    if msg != UDP_CONNECT_MSG {
        return Err(RiperfError::Protocol(format!(
            "unexpected UDP connect message: {msg:#x}"
        )));
    }

    socket.connect(addr).await?;
    socket.send(&UDP_CONNECT_REPLY.to_ne_bytes()).await?;

    Ok(addr)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unlimited_maps_zero_to_none() {
        // iperf3's plain `-t` run arrives as Some(0) → must become None.
        let mut p = TestParams {
            num: Some(0),
            blockcount: Some(0),
            ..Default::default()
        };
        p.normalize_unlimited();
        assert_eq!(p.num, None);
        assert_eq!(p.blockcount, None);

        // Real limits and an already-absent limit are untouched.
        let mut p = TestParams {
            num: Some(5_000_000),
            blockcount: None,
            ..Default::default()
        };
        p.normalize_unlimited();
        assert_eq!(p.num, Some(5_000_000));
        assert_eq!(p.blockcount, None);
    }

    #[test]
    fn cookie_is_correct_size_and_null_terminated() {
        let cookie = make_cookie();
        assert_eq!(cookie.len(), COOKIE_SIZE);
        assert_eq!(cookie[COOKIE_SIZE - 1], 0);
    }

    #[test]
    fn cookie_uses_valid_charset() {
        let cookie = make_cookie();
        for &b in &cookie[..COOKIE_SIZE - 1] {
            assert!(
                COOKIE_CHARSET.contains(&b),
                "byte {b:#x} not in cookie charset"
            );
        }
    }

    #[test]
    fn cookie_is_random() {
        let a = make_cookie();
        let b = make_cookie();
        // Astronomically unlikely to collide
        assert_ne!(a, b);
    }

    #[test]
    fn state_round_trip() {
        let states = [
            TestState::TestStart,
            TestState::TestRunning,
            TestState::TestEnd,
            TestState::ParamExchange,
            TestState::CreateStreams,
            TestState::ServerTerminate,
            TestState::ClientTerminate,
            TestState::ExchangeResults,
            TestState::DisplayResults,
            TestState::IperfStart,
            TestState::IperfDone,
            TestState::AccessDenied,
            TestState::ServerError,
        ];
        for state in states {
            let wire = state.to_wire();
            let decoded = TestState::from_wire(wire).unwrap();
            assert_eq!(state, decoded);
        }
    }

    #[test]
    fn unknown_state_is_error() {
        assert!(TestState::from_wire(0).is_err());
        assert!(TestState::from_wire(3).is_err());
        assert!(TestState::from_wire(127).is_err());
        assert!(TestState::from_wire(-128).is_err());
    }

    #[test]
    fn test_params_omits_defaults() {
        let params = TestParams::default();
        let json = serde_json::to_string(&params).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn test_params_includes_set_fields() {
        let params = TestParams {
            tcp: Some(true),
            time: Some(10),
            parallel: Some(4),
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["tcp"], true);
        assert_eq!(v["time"], 10);
        assert_eq!(v["parallel"], 4);
        // Unset fields should not appear
        assert!(v.get("udp").is_none());
        assert!(v.get("reverse").is_none());
    }

    #[test]
    fn test_params_mss_serializes_as_uppercase() {
        let params = TestParams {
            mss: Some(1400),
            ..Default::default()
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"MSS\""));
    }

    #[test]
    fn test_results_round_trip() {
        let results = TestResultsJson {
            cpu_util_total: 1.5,
            cpu_util_user: 1.0,
            cpu_util_system: 0.5,
            sender_has_retransmits: -1,
            congestion_used: Some("cubic".to_string()),
            server_output_text: None,
            server_output_json: None,
            streams: vec![StreamResultJson {
                id: 5,
                bytes: 1_000_000,
                retransmits: -1,
                jitter: 0.0,
                errors: 0,
                omitted_errors: Some(0),
                packets: 0,
                omitted_packets: Some(0),
                start_time: 0.0,
                end_time: 10.0,
            }],
        };
        let json = serde_json::to_string(&results).unwrap();
        let decoded: TestResultsJson = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.streams.len(), 1);
        assert_eq!(decoded.streams[0].bytes, 1_000_000);
    }

    #[test]
    fn results_json_from_iperf_3_12_decodes() {
        // iperf 3.12 (e.g. the networkstatic/iperf3 Docker image) serializes its
        // -1 "retransmit info unavailable" sentinel as u64::MAX, and omits
        // omitted_errors/omitted_packets from the stream object. riperf3 must
        // tolerate both rather than failing the whole test at result decode
        // (issue #24). Verbatim results JSON captured from the 3.12 server.
        let json = r#"{"congestion_used":"bbr","cpu_util_system":78.41548529516153,"cpu_util_total":85.9062232248101,"cpu_util_user":7.490704603857994,"sender_has_retransmits":18446744073709551615,"streams":[{"bytes":25371082752,"end_time":3.000672,"errors":0,"id":1,"jitter":0,"packets":0,"retransmits":18446744073709551615,"start_time":0}]}"#;
        let r: TestResultsJson =
            serde_json::from_str(json).expect("must decode iperf 3.12 results JSON");
        assert_eq!(r.sender_has_retransmits, -1, "u64::MAX sentinel maps to -1");
        assert_eq!(r.streams[0].retransmits, -1, "u64::MAX sentinel maps to -1");
        assert_eq!(r.streams[0].bytes, 25_371_082_752);
        // #271 refined the #24 posture: absence is preserved as None so
        // resolve_peer_omitted can distinguish an old peer from an
        // exchanged 0.
        assert_eq!(
            r.streams[0].omitted_errors, None,
            "absent field decodes as None (old peer)"
        );
        assert_eq!(
            r.streams[0].omitted_packets, None,
            "absent field decodes as None (old peer)"
        );
    }

    /// #271: the clean old-peer resolution — present keys pass through;
    /// absent keys net by this host's own omitted count for the stream and
    /// never net the error total (the split is unknowable from a gross
    /// count). Deliberately NOT GT's substitution, whose rendered figures
    /// are self-inconsistent (see resolve_peer_omitted's deviation record;
    /// upstream bug filed).
    /// #271 r1 F3(c): GT fails the exchange with IERECVRESULTS when exactly
    /// ONE omitted_* key is present (iperf_api.c:2888-2892 — both or
    /// neither). The guard lives in recv_results and surfaces GT's exact
    /// sentence via RiperfError::RecvResultsFailed.
    #[tokio::test]
    async fn one_sided_omitted_keys_fail_the_exchange() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            // Hand-built doc: omitted_errors WITHOUT omitted_packets.
            let doc = serde_json::json!({
                "cpu_util_total": 1.0, "cpu_util_user": 0.5, "cpu_util_system": 0.5,
                "sender_has_retransmits": 1,
                "streams": [{
                    "id": 1, "bytes": 1000, "retransmits": 0, "jitter": 0.0,
                    "errors": 3, "omitted_errors": 1, "packets": 10,
                    "start_time": 0.0, "end_time": 2.0
                }]
            });
            super::json_write(&mut stream, &doc).await.unwrap();
        });
        let (mut stream, _) = listener.accept().await.unwrap();
        let err = super::recv_results(&mut stream).await.unwrap_err();
        writer.await.unwrap();
        assert!(
            matches!(err, crate::error::RiperfError::RecvResultsFailed),
            "one-sided omitted_* is GT's IERECVRESULTS class: {err}"
        );
        assert_eq!(
            err.to_string(),
            "unable to receive results",
            "GT's exact sentence, no wrapper"
        );
    }

    #[test]
    fn resolve_peer_omitted_uses_clean_local_estimates() {
        let mk = |errors: i64, packets: i64, oe: Option<i64>, op: Option<i64>| StreamResultJson {
            id: 1,
            bytes: 0,
            retransmits: -1,
            jitter: 0.0,
            errors,
            omitted_errors: oe,
            packets,
            omitted_packets: op,
            start_time: 0.0,
            end_time: 2.0,
        };
        // Present keys pass through untouched.
        let x = mk(9, 100, Some(2), Some(10));
        assert_eq!(resolve_peer_omitted(&x, 5), (2, 10));
        // Absent, no local omit: gross == net — exact figures (GT swallows
        // the loss here; live 3.21<->3.12 fwd rendered 0/417713).
        let x = mk(643, 417_713, None, None);
        assert_eq!(resolve_peer_omitted(&x, 0), (0, 0));
        // Absent, with a local omit window: net packets by OUR omitted
        // count; error total stays un-netted (no -1 sentinel arithmetic —
        // GT's stream and sum disagree by one here, live 2852 vs 2851).
        let x = mk(2851, 416_693, None, None);
        assert_eq!(resolve_peer_omitted(&x, 1_020), (0, 1_020));
    }
    #[test]
    fn results_json_serializes_sentinel_as_signed_minus_one() {
        // When riperf3 is the server sending results to an (older) iperf3 client,
        // the -1 "unavailable" sentinel must go on the wire as signed -1 (what
        // iperf3's reader expects), never u64::MAX — the send side of #24.
        let results = TestResultsJson {
            cpu_util_total: 0.0,
            cpu_util_user: 0.0,
            cpu_util_system: 0.0,
            sender_has_retransmits: -1,
            congestion_used: None,
            server_output_text: None,
            server_output_json: None,
            streams: vec![StreamResultJson {
                id: 1,
                bytes: 0,
                retransmits: -1,
                jitter: 0.0,
                errors: 0,
                omitted_errors: Some(0),
                packets: 0,
                omitted_packets: Some(0),
                start_time: 0.0,
                end_time: 1.0,
            }],
        };
        let json = serde_json::to_string(&results).unwrap();
        assert!(json.contains("\"sender_has_retransmits\":-1"), "{json}");
        assert!(json.contains("\"retransmits\":-1"), "{json}");
        assert!(
            !json.contains("18446744073709551615"),
            "must serialize -1 signed, not as u64::MAX: {json}"
        );
    }

    #[tokio::test]
    async fn json_framing_round_trip() {
        let (client, server) = tokio::io::duplex(1024);

        // We need TcpStream-like objects; duplex gives us DuplexStream.
        // For this test, we'll use a real TCP loopback pair.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let writer = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            let value = serde_json::json!({"tcp": true, "time": 10});
            json_write(&mut stream, &value).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let value = json_read_bounded(&mut stream, MAX_PARAMS_JSON_LEN)
            .await
            .unwrap();
        writer.await.unwrap();

        assert_eq!(value["tcp"], true);
        assert_eq!(value["time"], 10);

        // Clean up unused duplex
        drop(client);
        drop(server);
    }

    #[tokio::test]
    async fn state_send_recv_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let writer = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            send_state(&mut stream, TestState::ParamExchange)
                .await
                .unwrap();
            send_state(&mut stream, TestState::CreateStreams)
                .await
                .unwrap();
            send_state(&mut stream, TestState::TestRunning)
                .await
                .unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(
            recv_state(&mut stream).await.unwrap(),
            TestState::ParamExchange
        );
        assert_eq!(
            recv_state(&mut stream).await.unwrap(),
            TestState::CreateStreams
        );
        assert_eq!(
            recv_state(&mut stream).await.unwrap(),
            TestState::TestRunning
        );

        writer.await.unwrap();
    }

    #[tokio::test]
    async fn cookie_send_recv_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let original = make_cookie();

        let cookie_copy = original;
        let writer = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            send_cookie(&mut stream, &cookie_copy).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let received = recv_cookie(&mut stream).await.unwrap();
        writer.await.unwrap();

        assert_eq!(original, received);
    }

    #[tokio::test]
    async fn udp_connect_handshake() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client_sock.connect(server_addr).await.unwrap();

        let server_task = tokio::spawn(async move {
            udp_connect_server(&server_sock, std::time::Duration::from_secs(5))
                .await
                .unwrap()
        });

        udp_connect_client(&client_sock).await.unwrap();
        let client_addr = server_task.await.unwrap();
        assert_eq!(client_addr, client_sock.local_addr().unwrap());
    }

    /// Issue #11: the server must give up (not hang forever) if no client
    /// magic ever arrives.
    #[tokio::test]
    async fn udp_connect_server_times_out_when_no_client() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let start = std::time::Instant::now();
        let res = udp_connect_server(&server_sock, std::time::Duration::from_millis(150)).await;
        assert!(
            matches!(res, Err(RiperfError::Aborted(_))),
            "expected a timeout abort, got {res:?}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "should not hang"
        );
    }

    /// Issue #11: the client gives up (not hangs) if the server never replies,
    /// after exhausting its retransmit budget. Uses a paused clock so the full
    /// ~30s budget elapses in virtual time (instant in real time), and asserts
    /// the *lower* bound too — it must actually wait the budget, not bail early.
    #[tokio::test(start_paused = true)]
    async fn udp_connect_client_times_out_when_no_reply() {
        // A bound socket that never replies (nothing recv_from's it).
        let dead_server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = dead_server.local_addr().unwrap();
        let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client_sock.connect(dead_addr).await.unwrap();

        let start = tokio::time::Instant::now();
        let res = udp_connect_client(&client_sock).await;
        let elapsed = start.elapsed();
        assert!(res.is_err(), "expected handshake to fail, got {res:?}");
        // Must have waited essentially the whole budget (retried), not bailed.
        assert!(
            elapsed >= UDP_CONNECT_TOTAL_TIMEOUT - UDP_CONNECT_RETRY_INTERVAL,
            "client gave up too early ({elapsed:?}); should retry for the full budget"
        );
        assert!(
            elapsed <= UDP_CONNECT_TOTAL_TIMEOUT + UDP_CONNECT_RETRY_INTERVAL,
            "client overran its budget ({elapsed:?})"
        );
    }

    /// Issue #11: a lost first reply is recovered by the client's retransmit —
    /// the server replies on the second magic and the handshake completes.
    #[tokio::test]
    async fn udp_connect_recovers_from_lost_first_magic() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client_sock.connect(server_addr).await.unwrap();

        // Server drains and drops the first datagram, then handshakes normally
        // on the client's retransmit.
        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let _ = server_sock.recv_from(&mut buf).await.unwrap(); // drop #1
            udp_connect_server(&server_sock, std::time::Duration::from_secs(5))
                .await
                .unwrap()
        });

        // Client must succeed despite the dropped first magic (it resends).
        let start = std::time::Instant::now();
        udp_connect_client(&client_sock).await.unwrap();
        // Success can only have come via the retransmit, which fires one
        // retry-interval after the dropped magic — assert that path was taken.
        assert!(
            start.elapsed() >= UDP_CONNECT_RETRY_INTERVAL,
            "should have recovered via the retransmit (>= one interval), took {:?}",
            start.elapsed()
        );
        let client_addr = server_task.await.unwrap();
        assert_eq!(client_addr, client_sock.local_addr().unwrap());
    }

    /// Issue #11: a stray non-reply datagram during the handshake is ignored,
    /// not treated as a fatal error (matches iperf3's drain behavior).
    #[tokio::test]
    async fn udp_connect_client_tolerates_stray_datagram() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client_sock.connect(server_addr).await.unwrap();

        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            // First magic: reply with junk (not the connect reply value).
            let (_, caddr) = server_sock.recv_from(&mut buf).await.unwrap();
            server_sock
                .send_to(&0xdead_beef_u32.to_ne_bytes(), caddr)
                .await
                .unwrap();
            // Client ignores the junk and resends; reply properly this time.
            let (_, caddr) = server_sock.recv_from(&mut buf).await.unwrap();
            server_sock
                .send_to(&UDP_CONNECT_REPLY.to_ne_bytes(), caddr)
                .await
                .unwrap();
        });

        udp_connect_client(&client_sock).await.unwrap();
        server_task.await.unwrap();
    }

    /// Issue #11 round-3: a flood of stray datagrams must NOT be amplified into
    /// a flood of magic resends — the per-interval floor caps the resend rate.
    /// (A prior revision resent on every drained stray, sending millions.)
    #[tokio::test]
    async fn udp_connect_client_rate_limits_resends_under_stray_flood() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let client_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client_sock.connect(server_addr).await.unwrap();

        let magics = Arc::new(AtomicUsize::new(0));
        let m2 = magics.clone();
        // For ~1.1s, count each magic and flood junk back (never the reply).
        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let end = tokio::time::Instant::now() + std::time::Duration::from_millis(1100);
            loop {
                let remaining = end.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, server_sock.recv_from(&mut buf)).await {
                    Ok(Ok((n, addr))) => {
                        if n >= 4
                            && u32::from_ne_bytes(buf[..4].try_into().unwrap()) == UDP_CONNECT_MSG
                        {
                            m2.fetch_add(1, Ordering::Relaxed);
                        }
                        let _ = server_sock
                            .send_to(&0xdead_beef_u32.to_ne_bytes(), addr)
                            .await;
                    }
                    _ => break,
                }
            }
        });

        // Client never gets a valid reply; let it run ~1.1s then stop.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1100),
            udp_connect_client(&client_sock),
        )
        .await;
        server_task.await.unwrap();

        let n = magics.load(Ordering::Relaxed);
        // ~1.1s / 500ms ≈ 2–3 resends; a regression would send thousands.
        assert!(
            (1..=6).contains(&n),
            "client sent {n} magics in ~1.1s under a stray flood; expected ~2–3 (rate-limited)"
        );
    }

    // -----------------------------------------------------------------------
    // #367: residual cJSON-leniency cells beyond the #343 mirror. Two are
    // now mirrored (BOM, bare-scalar params root); the other four are
    // recorded deviations (serde is a conforming parser; mirroring them
    // would mean a bespoke lenient parser for shapes no real iperf3 encoder
    // emits). GT facts source-verified: cjson.c skip_utf8_bom (:1074-1088,
    // run at every parse entry :1131); get_parameters typed key-lookup
    // (iperf_api.c:2533+) so a non-object root sets no fields → defaults.
    // -----------------------------------------------------------------------

    /// #367 cell 1 (MIRRORED): a UTF-8 BOM ahead of valid JSON parses, like
    /// cJSON's skip_utf8_bom. Red pre-fix (serde rejects the BOM bytes).
    #[test]
    fn json_first_value_skips_utf8_bom() {
        let buf = b"\xEF\xBB\xBF{\"time\":10}";
        let v = json_first_value(buf).expect("BOM-prefixed JSON parses like cJSON");
        assert_eq!(v["time"], 10);
    }

    /// #367 cell 2 (MIRRORED): a bare-scalar params root yields all-defaults
    /// (GT's get_parameters key-lookup misses every field on a non-object and
    /// proceeds to CREATE_STREAMS). Red pre-fix (from_value::<TestParams> on a
    /// scalar errors). An object root still deserializes normally.
    #[test]
    fn params_from_value_defaults_on_non_object_root() {
        // Bare scalar, array, string, bool, null — all non-objects → defaults.
        for v in [
            serde_json::json!(42),
            serde_json::json!([1, 2, 3]),
            serde_json::json!("hi"),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let p = params_from_value(v.clone()).unwrap_or_else(|e| panic!("{v} → default: {e}"));
            assert_eq!(p.time, None, "{v} yields defaults");
            assert_eq!(p.tcp, None, "{v} yields defaults");
        }
        // An object root still parses its fields.
        let p = params_from_value(serde_json::json!({"time": 10, "tcp": true})).unwrap();
        assert_eq!(p.time, Some(10));
        assert_eq!(p.tcp, Some(true));
    }

    /// #367 cells 3-6 (RECORDED DEVIATIONS): riperf3's serde parser is
    /// stricter than cJSON for these non-conforming shapes no real iperf3
    /// encoder emits. This locks the deviation — a future serde change that
    /// silently started accepting any of them should trip here.
    #[test]
    fn json_first_value_deviations_stay_strict() {
        // cell 3: scalar with adjacent (non-whitespace) garbage — cJSON stops
        // at the first non-value byte; serde's non-self-delimiting first
        // value must be whitespace/EOF-terminated.
        assert!(json_first_value(b"42GARBAGE").is_err(), "adjacent garbage");
        // cell 5: raw control character inside a string — cJSON copies it;
        // serde (strict JSON) rejects unescaped control bytes.
        assert!(
            json_first_value(b"\"\x01\"").is_err(),
            "raw control char in string"
        );
        // cell 6: cJSON's strtod number grammar — leading zero and bare
        // trailing dot both parse in cJSON; serde rejects.
        assert!(json_first_value(b"{\"omit\":01}").is_err(), "leading zero");
        assert!(json_first_value(b"{\"time\":1.}").is_err(), "bare dot");
        // cell 4: nesting past serde's 128-deep recursion limit — cJSON's
        // CJSON_NESTING_LIMIT is 1000, so 128<n<=1000 diverges.
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(
            json_first_value(deep.as_bytes()).is_err(),
            "nesting past serde's 128 limit"
        );
    }
}

// ---------------------------------------------------------------------------
// Protocol param/results round-trip (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod protocol_tests {
    use crate::protocol::{self, StreamResultJson, TestParams, TestResultsJson};

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
            protocol::send_params(&mut stream, &params_clone)
                .await
                .unwrap();
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
            server_output_text: None,
            server_output_json: None,
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
                    omitted_errors: Some(0),
                    packets: 0,
                    omitted_packets: Some(0),
                    start_time: 0.0,
                    end_time: 10.0,
                },
                StreamResultJson {
                    id: 3,
                    bytes: 9_500_000,
                    retransmits: 0,
                    jitter: 0.0,
                    errors: 0,
                    omitted_errors: Some(0),
                    packets: 0,
                    omitted_packets: Some(0),
                    start_time: 0.0,
                    end_time: 10.0,
                },
            ],
        };

        let results_clone = results.clone();
        let writer = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            protocol::send_results(&mut stream, &results_clone)
                .await
                .unwrap();
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
        let result = protocol::json_read_bounded(&mut stream, protocol::MAX_PARAMS_JSON_LEN).await;
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

    // -- migrated from json_output_tests: TestParams serialization --

    #[test]
    fn test_params_serializes_all_fields() {
        use crate::protocol::TestParams;
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

    // Migrated in-crate when TestResultsJson/StreamResultJson became
    // #[non_exhaustive] (an external test crate can no longer construct them).
    #[test]
    fn test_results_json_structure() {
        let r = TestResultsJson {
            server_output_text: None,
            server_output_json: None,
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
                omitted_errors: Some(0),
                packets: 10000,
                omitted_packets: Some(0),
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
// Control-state transition table tests (#145) — AUDITABILITY ONLY
// ---------------------------------------------------------------------------

#[cfg(test)]
mod transition_table_tests {
    use crate::protocol::{is_legal_next, legal_next, Role, TestState::*};

    // -- Role::Client: exact legal sets per the #145 canonical table --

    #[test]
    fn client_legal_sets_are_exact() {
        assert_eq!(
            legal_next(IperfStart, Role::Client),
            &[ParamExchange, AccessDenied, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(ParamExchange, Role::Client),
            &[CreateStreams, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(CreateStreams, Role::Client),
            &[TestStart, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(TestStart, Role::Client),
            &[TestRunning, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(TestRunning, Role::Client),
            &[TestRunning, ExchangeResults, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(ExchangeResults, Role::Client),
            &[DisplayResults, ServerError, ServerTerminate]
        );
        assert_eq!(
            legal_next(DisplayResults, Role::Client),
            &[IperfDone, ServerError, ServerTerminate]
        );
    }

    #[test]
    fn client_terminal_states_have_no_successors() {
        // The client adopts these and stops — no legal next.
        assert_eq!(legal_next(ServerTerminate, Role::Client), &[]);
        assert_eq!(legal_next(ServerError, Role::Client), &[]);
        assert_eq!(legal_next(AccessDenied, Role::Client), &[]);
    }

    #[test]
    fn client_other_currents_have_no_successors() {
        // States the client never PROCESSES as a "current" (server-only or
        // peer-only states) map to the empty set.
        assert_eq!(legal_next(TestEnd, Role::Client), &[]);
        assert_eq!(legal_next(ClientTerminate, Role::Client), &[]);
        assert_eq!(legal_next(IperfDone, Role::Client), &[]);
    }

    // (#325/#330: the server-side rows and their tests are gone — the
    // server's message loops are arm-explicit like GT's single switch, and
    // Role is client-only now.)

    // -- is_legal_next membership: documented loosenesses + sample rejects --

    #[test]
    fn resent_test_running_is_legal_for_client() {
        // iperf3 no-op tolerance: a re-sent TEST_RUNNING is legal to receive.
        assert!(is_legal_next(TestRunning, TestRunning, Role::Client));
    }

    #[test]
    fn sampled_illegal_transitions_are_rejected() {
        // A scattering of out-of-sequence transitions must report false.
        // Client side:
        assert!(!is_legal_next(IperfStart, TestRunning, Role::Client));
        assert!(!is_legal_next(ParamExchange, DisplayResults, Role::Client));
        assert!(!is_legal_next(CreateStreams, IperfDone, Role::Client));
        assert!(!is_legal_next(TestStart, ExchangeResults, Role::Client));
        assert!(!is_legal_next(TestRunning, DisplayResults, Role::Client));
        assert!(!is_legal_next(ExchangeResults, IperfDone, Role::Client));
        assert!(!is_legal_next(DisplayResults, TestRunning, Role::Client));
        // The client never receives a client/server-internal byte as "next":
        assert!(!is_legal_next(TestRunning, TestEnd, Role::Client));
        assert!(!is_legal_next(TestRunning, ClientTerminate, Role::Client));
    }

    #[test]
    fn is_legal_next_agrees_with_legal_next() {
        // Membership is exactly slice containment over legal_next, for every
        // (current, role, got) triple — guards the two from drifting apart.
        let all = [
            TestStart,
            TestRunning,
            TestEnd,
            ParamExchange,
            CreateStreams,
            ServerTerminate,
            ClientTerminate,
            ExchangeResults,
            DisplayResults,
            IperfStart,
            IperfDone,
            AccessDenied,
            ServerError,
        ];
        for role in [Role::Client] {
            for current in all {
                let legal = legal_next(current, role);
                for got in all {
                    assert_eq!(
                        is_legal_next(current, got, role),
                        legal.contains(&got),
                        "is_legal_next disagrees with legal_next at \
                         ({current:?}, {got:?}, {role:?})"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol error state tests (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod protocol_error_tests {
    use crate::protocol::{self, TestState};

    /// #195 root cause: a transient ICMP-feedback error (ECONNRESET /
    /// ECONNREFUSED on the connected socket — the server's per-stream
    /// listener REBIND gap under load) must not kill the handshake while
    /// budget remains. The loop resends the magic and succeeds once the
    /// fresh listener is up. Sequence here: the client's first magic lands
    /// on an unbound port (ICMP bounce queued), the real replier binds
    /// ~600 ms later and answers the resend.
    #[tokio::test]
    async fn udp_connect_client_rides_through_transient_reset() {
        // Sub-ephemeral allocation (r1 n4): a bind(:0)-then-reuse port can be
        // re-taken by any concurrent ephemeral bind during the 600 ms it must
        // stay unbound — the exact race free_port()'s own doc rejects.
        let port = riperf3_test_support::free_port();

        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", port)).await.unwrap();

        let replier = tokio::spawn(async move {
            // Let the first magic bounce and the error queue on the client
            // socket; then stand up the real listener and answer the resend.
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let sock = tokio::net::UdpSocket::bind(("127.0.0.1", port))
                .await
                .unwrap();
            let mut buf = [0u8; 4];
            let (_, from) = sock.recv_from(&mut buf).await.unwrap();
            sock.send_to(&protocol::UDP_CONNECT_REPLY.to_ne_bytes(), from)
                .await
                .unwrap();
        });

        protocol::udp_connect_client(&client)
            .await
            .expect("the handshake must survive the transient reset and complete");
        replier.await.unwrap();
    }

    #[tokio::test]
    async fn client_handles_access_denied() {
        // Server sends AccessDenied state — client should return AccessDenied error
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read cookie (37 bytes)
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie)
                .await
                .unwrap();
            // Send AccessDenied
            protocol::send_state(&mut stream, TestState::AccessDenied)
                .await
                .unwrap();
        });

        let client = crate::ClientBuilder::new("127.0.0.1")
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
        // #224: SERVER_ERROR carries an (i_errno, errno) u32-pair payload
        // (iperf_server_api.c cleanup_server / the bitrate + duration-timer
        // sites); the client adopts iperf_strerror(i_errno) as ITS error
        // (iperf_client_api.c:392). Payload here: IETOTALRATE=27, errno=0.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie)
                .await
                .unwrap();
            protocol::send_state(&mut stream, TestState::ServerError)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                &[27u32.to_be_bytes(), 0u32.to_be_bytes()].concat(),
            )
            .await
            .unwrap();
        });

        let client = crate::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        let err = result.expect_err("client should error on ServerError");
        assert_eq!(
            err.to_string(),
            "total required bandwidth is larger than server limit",
            "the client adopts the relayed iperf_strerror(27): {err:?}"
        );
        let _ = server_task.await;
    }

    /// #248: a SERVER_ERROR(160) — the --server-max-duration timer relay — is
    /// adopted as iperf_strerror(160), which is perr-class, so GT (and now
    /// riperf3) dangles a trailing ": ". The fast end-to-end pin for the suffix
    /// (the real watchdog fires at duration+omit+40s, too slow to drive live).
    #[tokio::test]
    async fn client_handles_server_error_duration_expired() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie)
                .await
                .unwrap();
            protocol::send_state(&mut stream, TestState::ServerError)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                &[160u32.to_be_bytes(), 0u32.to_be_bytes()].concat(),
            )
            .await
            .unwrap();
        });

        let client = crate::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        let err = result.expect_err("client should error on ServerError");
        assert_eq!(
            err.to_string(),
            "server test duration expired: ",
            "the client adopts the relayed perr-class strerror(160) with GT's dangling ': ' (#248): {err:?}"
        );
        let _ = server_task.await;
    }

    /// A SERVER_ERROR whose payload never arrives (peer died mid-relay, or a
    /// pre-payload sender): still an error, never a hang or a panic.
    #[tokio::test]
    async fn client_handles_server_error_without_payload() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut cookie = [0u8; 37];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut cookie)
                .await
                .unwrap();
            protocol::send_state(&mut stream, TestState::ServerError)
                .await
                .unwrap();
            // close without the payload
        });

        let client = crate::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "bare SERVER_ERROR is still an error");
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

        let client = crate::ClientBuilder::new("127.0.0.1")
            .port(Some(addr.port()))
            .duration(1)
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "client should error on peer disconnect");
        let _ = server_task.await;
    }
}
