use std::net::SocketAddr;

use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

use crate::error::{RiperfError, Result, UnknownState};

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
pub async fn recv_state(stream: &mut TcpStream) -> Result<TestState> {
    let mut buf = [0u8; 1];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Err(RiperfError::PeerDisconnected);
    }
    TestState::from_wire(buf[0] as i8)
        .map_err(|u| RiperfError::Protocol(u.to_string()))
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
pub async fn json_read(
    stream: &mut TcpStream,
    max_len: usize,
) -> Result<serde_json::Value> {
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

// ---------------------------------------------------------------------------
// Test results — exchanged as JSON during ExchangeResults
// ---------------------------------------------------------------------------

/// Per-stream result data included in the results JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResultJson {
    pub id: i32,
    pub bytes: u64,
    pub retransmits: i64,
    pub jitter: f64,
    pub errors: i64,
    pub omitted_errors: i64,
    pub packets: i64,
    pub omitted_packets: i64,
    pub start_time: f64,
    pub end_time: f64,
}

/// Top-level results JSON exchanged between client and server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResultsJson {
    pub cpu_util_total: f64,
    pub cpu_util_user: f64,
    pub cpu_util_system: f64,
    pub sender_has_retransmits: i32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub congestion_used: Option<String>,
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

/// Client-side UDP connect handshake.
/// Sends the magic word and waits for the server's reply.
/// Note: iperf3 uses native byte order (not network byte order) for the magic values.
pub async fn udp_connect_client(socket: &UdpSocket) -> Result<()> {
    socket.send(&UDP_CONNECT_MSG.to_ne_bytes()).await?;

    let mut buf = [0u8; 4];
    let n = socket.recv(&mut buf).await?;
    if n < 4 {
        return Err(RiperfError::Protocol("UDP connect reply too short".into()));
    }
    let reply = u32::from_ne_bytes(buf);
    // Accept both the current and legacy reply values
    if reply != UDP_CONNECT_REPLY && reply != 0xb168_de3a {
        return Err(RiperfError::Protocol(format!(
            "unexpected UDP connect reply: {reply:#x}"
        )));
    }
    Ok(())
}

/// Server-side UDP connect handshake.
/// Waits for the client's magic word, "connects" the socket to the client,
/// sends the reply, and returns the client address.
/// Note: iperf3 uses native byte order (not network byte order) for the magic values.
pub async fn udp_connect_server(socket: &UdpSocket) -> Result<SocketAddr> {
    let mut buf = [0u8; 65536];
    let (n, addr) = socket.recv_from(&mut buf).await?;
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
            send_state(&mut stream, TestState::ParamExchange).await.unwrap();
            send_state(&mut stream, TestState::CreateStreams).await.unwrap();
            send_state(&mut stream, TestState::TestRunning).await.unwrap();
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        assert_eq!(recv_state(&mut stream).await.unwrap(), TestState::ParamExchange);
        assert_eq!(recv_state(&mut stream).await.unwrap(), TestState::CreateStreams);
        assert_eq!(recv_state(&mut stream).await.unwrap(), TestState::TestRunning);

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
            let addr = udp_connect_server(&server_sock).await.unwrap();
            addr
        });

        udp_connect_client(&client_sock).await.unwrap();
        let client_addr = server_task.await.unwrap();
        assert_eq!(client_addr, client_sock.local_addr().unwrap());
    }
}
