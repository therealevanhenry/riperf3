use std::net::SocketAddr;

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

/// Read a 37-byte cookie from a TCP stream.
pub async fn recv_cookie(stream: &mut TcpStream) -> Result<[u8; COOKIE_SIZE]> {
    let mut cookie = [0u8; COOKIE_SIZE];
    stream.read_exact(&mut cookie).await?;
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

/// Read a state transition (single signed byte) from the control connection.
/// Send SERVER_ERROR with iperf3's (i_errno, errno) u32-pair payload (#224):
/// the state byte, then both words big-endian — iperf_server_api.c's Nwrite
/// pair (the bitrate, duration-timer, and cleanup_server relay sites). The os
/// errno is always 0 from riperf3: our self-terminate causes carry none.
pub async fn send_server_error(stream: &mut TcpStream, i_errno: u32) -> Result<()> {
    send_state(stream, TestState::ServerError).await?;
    stream.write_all(&i_errno.to_be_bytes()).await?;
    stream.write_all(&0u32.to_be_bytes()).await?;
    Ok(())
}

/// Read SERVER_ERROR's (i_errno, errno) payload. `None` when it never
/// arrives (a peer that died mid-relay, or a bare -2 sender): the caller
/// degrades to its generic message — a payloadless SERVER_ERROR must error
/// cleanly, never hang or panic (tested in-crate).
pub async fn read_server_error_payload(stream: &mut TcpStream) -> Option<(u32, u32)> {
    let mut buf = [0u8; 8];
    match stream.read_exact(&mut buf).await {
        Ok(_) => Some((
            u32::from_be_bytes(buf[0..4].try_into().unwrap()),
            u32::from_be_bytes(buf[4..8].try_into().unwrap()),
        )),
        Err(_) => None,
    }
}

pub async fn recv_state(stream: &mut TcpStream) -> Result<TestState> {
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Err(RiperfError::PeerDisconnected);
    }
    TestState::from_wire(buf[0] as i8).map_err(|u| RiperfError::Protocol(u.to_string()))
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

/// Read a length-prefixed JSON value. If `max_len` is 0, no size limit is enforced.
pub async fn json_read(stream: &mut TcpStream, max_len: usize) -> Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if max_len > 0 && len > max_len {
        return Err(RiperfError::Protocol(format!(
            "JSON payload too large: {len} bytes (max {max_len})"
        )));
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let value: serde_json::Value = serde_json::from_slice(&buf)?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// Test parameters — exchanged as JSON during ParamExchange
// ---------------------------------------------------------------------------

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
    /// `-t` run, whereas riperf3 omits them — and serde `default` only fills a
    /// *missing* field, so a real iperf3 client arrives as `Some(0)`. Without
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
    #[serde(default)]
    pub omitted_errors: i64,
    pub packets: i64,
    #[serde(default)]
    pub omitted_packets: i64,
    pub start_time: f64,
    pub end_time: f64,
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

/// Receive test parameters from length-prefixed JSON.
pub async fn recv_params(stream: &mut TcpStream) -> Result<TestParams> {
    let value = json_read(stream, MAX_PARAMS_JSON_LEN).await?;
    let params: TestParams = serde_json::from_value(value)?;
    Ok(params)
}

/// Send test results as length-prefixed JSON.
pub async fn send_results(stream: &mut TcpStream, results: &TestResultsJson) -> Result<()> {
    let value = serde_json::to_value(results)?;
    json_write(stream, &value).await
}

/// Receive test results from length-prefixed JSON (no size limit).
pub async fn recv_results(stream: &mut TcpStream) -> Result<TestResultsJson> {
    let value = json_read(stream, 0).await?;
    let results: TestResultsJson = serde_json::from_value(value)?;
    Ok(results)
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
    // Resend no more than once per interval, even while draining strays, so a
    // stray-datagram flood can't turn into a magic-send flood (amplification).
    let mut next_send = tokio::time::Instant::now();
    while tokio::time::Instant::now() < deadline {
        if tokio::time::Instant::now() >= next_send {
            socket.send(&UDP_CONNECT_MSG.to_ne_bytes()).await?;
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
            Ok(Err(e)) => return Err(RiperfError::Io(e)),
            Err(_) => continue, // interval elapsed with no reply — loop resends
        }
    }
    Err(RiperfError::Protocol(if saw_traffic {
        "UDP connect handshake failed: no valid reply (only unexpected datagrams received)".into()
    } else {
        "UDP connect handshake timed out (no server reply)".into()
    }))
}

/// Server-side UDP connect handshake.
/// Waits (up to `timeout`) for the client's magic word, "connects" the socket
/// to the client, sends the reply, and returns the client address. The bounded
/// wait means a client that never connects — or whose magic is lost on a real
/// network — fails the test instead of hanging setup forever (issue #11).
/// Note: iperf3 uses native byte order (not network byte order) for the magic values.
pub async fn udp_connect_server(
    socket: &UdpSocket,
    timeout: std::time::Duration,
) -> Result<SocketAddr> {
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
                omitted_errors: 0,
                packets: 0,
                omitted_packets: 0,
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
        assert_eq!(r.streams[0].omitted_errors, 0, "absent field defaults to 0");
        assert_eq!(
            r.streams[0].omitted_packets, 0,
            "absent field defaults to 0"
        );
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
                omitted_errors: 0,
                packets: 0,
                omitted_packets: 0,
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
        let value = json_read(&mut stream, MAX_PARAMS_JSON_LEN).await.unwrap();
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
// Protocol error state tests (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod protocol_error_tests {
    use crate::protocol::{self, TestState};

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
