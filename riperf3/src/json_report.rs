//! Typed, iperf3-compatible JSON report model (issue #36).
//!
//! riperf3's `-J` output must be a faithful drop-in for iperf3's, so machine
//! consumers (Telegraf, Grafana plugins, CI harnesses) that parse iperf3 JSON
//! work unchanged. This replaces the previous hand-rolled `serde_json::json!`
//! blob, which diverged from iperf3's schema (flat `end.streams`, empty
//! `intervals`, fabricated addresses).
//!
//! The model covers all three top-level blocks (built incrementally across #36
//! PR1–PR3):
//! - `start`: connection metadata and addresses (`connected`, `connecting_to`),
//!   `timestamp`, `cookie`, `system_info` (uname), `tcp_mss_default`, the socket
//!   buffer sizes (`sock_bufsize`, `sndbuf_actual`, `rcvbuf_actual`), and the
//!   `test_start` parameters.
//! - `intervals`: per-interval stream samples and sums, with the per-stream
//!   `TCP_INFO` extremes (`max_snd_cwnd` / `min`/`max`/`mean_rtt`) accumulated
//!   from per-interval `TCP_INFO` reads and surfaced on the `end` streams.
//! - `end`: per-stream objects nested as `{sender, receiver}` (TCP) or `{udp}`
//!   (UDP), the `sum_sent`/`sum_received` aggregates plus the UDP `sum` and bidir
//!   `sum_*_bidir_reverse`, CPU utilization, and `sender`/`receiver_tcp_congestion`.
//!
//! Fields iperf3 emits but riperf3 cannot yet produce are omitted
//! (`skip_serializing_if`) rather than emitted with placeholder values, so the
//! shape only ever contains real data.

use serde::ser::Serializer;
use serde::Serialize;

use crate::protocol::TransportProtocol;

// ---------------------------------------------------------------------------
// cJSON-compatible float rendering (#57)
// ---------------------------------------------------------------------------
//
// serde_json prints every f64 with a fractional part (`0.0`, `1.0`,
// `10485760.0`). iperf3 uses cJSON, which prints an *integral* double as an
// integer token (`0`, `1`, `10485760`) and a fractional one via C's
// `%.15g`/`%.17g`. These helpers reproduce cJSON's `print_number` so the `-J`
// blob is byte-compatible with iperf3 for consumers that diff raw text, not just
// parsed values. Applied to the report's f64 fields via `serialize_with`.

/// Render an `f64` exactly the way cJSON's `print_number` does.
fn cjson_number(d: f64) -> String {
    if !d.is_finite() {
        return "null".to_string(); // cJSON emits the bareword null for NaN/Inf
    }
    // Integral and representable as i64 → integer token (drops the `.0`).
    if d.abs() < 9_223_372_036_854_775_000.0 && d == (d as i64) as f64 {
        return (d as i64).to_string();
    }
    // Fractional: 15 significant digits, falling back to 17 if 15 doesn't
    // round-trip — exactly cJSON's strategy.
    let s = format_g(d, 15);
    let round_trips = s
        .parse::<f64>()
        .map(|t| compare_double(t, d))
        .unwrap_or(false);
    if round_trips {
        s
    } else {
        format_g(d, 17)
    }
}

/// cJSON's `compare_double`: equal within a one-ULP-ish epsilon.
fn compare_double(a: f64, b: f64) -> bool {
    let maxval = a.abs().max(b.abs());
    (a - b).abs() <= maxval * f64::EPSILON
}

/// C `printf("%.*g", precision, d)` for a finite `d`. `precision` is the number
/// of significant digits (15 or 17 here).
fn format_g(d: f64, precision: usize) -> String {
    let p = precision.max(1);
    if d == 0.0 {
        return "0".to_string();
    }
    // `{:.*e}` rounds correctly and carries the exponent (9.99e9 → 1.00e10), so
    // read the decimal exponent from it rather than from log10 (which mis-rounds
    // at powers of ten).
    let sci = format!("{:.*e}", p - 1, d);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();

    if exp < -4 || exp >= p as i32 {
        // Scientific: trim trailing zeros in the mantissa; C-style signed,
        // ≥2-digit exponent.
        let mant = trim_trailing_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.unsigned_abs())
    } else {
        // Fixed: `p-1-exp` fraction digits, trailing zeros trimmed.
        let frac = (p as i32 - 1 - exp).max(0) as usize;
        trim_trailing_zeros(&format!("{:.*}", frac, d))
    }
}

/// Trim trailing fractional zeros (and a now-bare decimal point).
fn trim_trailing_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// `serialize_with` for an `f64` field: emit the cJSON-formatted token as a raw
/// JSON number (serde_json), so integral values lose the `.0`.
fn ser_f64<S: Serializer>(v: &f64, ser: S) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;
    let raw = serde_json::value::RawValue::from_string(cjson_number(*v)).map_err(Error::custom)?;
    raw.serialize(ser)
}

/// `serialize_with` for an `Option<f64>` field. Paired with
/// `skip_serializing_if = "Option::is_none"`, so `None` is normally skipped.
fn ser_opt_f64<S: Serializer>(v: &Option<f64>, ser: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => ser_f64(x, ser),
        None => ser.serialize_none(),
    }
}

// ---------------------------------------------------------------------------
// Top-level report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Report {
    pub start: Start,
    pub intervals: Vec<Interval>,
    pub end: End,
    /// `--extra-data` string, emitted at the top level (after `end`) only when
    /// given — matching iperf3's placement (#35). Present on both client and
    /// server (the server receives it via the parameter exchange).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<String>,
    /// `--get-server-output` (#33): the server's diverted text report or its
    /// full `-J` report, appended at the end of the top level like iperf3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_output_text: Option<String>,
    /// Top-level `"error"` key, like iperf_json_finish: a -J run that ends in
    /// IESERVERTERM still emits the partial blob, error attached (#170).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_output_json: Option<serde_json::Value>,
}

/// Serialize one `--json-stream` NDJSON line: `{"event":<event>,"data":<data>}`,
/// compact (one object per line, no pretty-printing), matching iperf3's
/// `--json-stream` (#62). The `data` payload keeps its own serde formatting —
/// notably the cJSON-style float rendering (`ser_f64`, #57) — so a streamed
/// `start` / `interval` / `end` object is byte-for-byte the same as the
/// corresponding section of the batched `-J` report.
/// The minimal pre-test error document for `-J` runs (#198): iperf3's
/// iperf_errexit emits `{start:{connected:[],version,system_info},
/// intervals:[], end:{}, error}` on stdout and nothing to stderr
/// (live-captured against 3.20+). Pretty-printed like the normal `-J` body.
pub fn error_document(error: &str) -> String {
    // Field-ordered structs, not serde_json::json! — its maps serialize
    // alphabetically, breaking iperf3's start/intervals/end/error order
    // (the #168 envelope lesson).
    #[derive(Serialize)]
    struct ErrStart {
        connected: [(); 0],
        version: String,
        system_info: String,
    }
    #[derive(Serialize)]
    struct ErrDoc {
        start: ErrStart,
        intervals: [(); 0],
        end: serde_json::Map<String, serde_json::Value>,
        error: String,
    }
    serde_json::to_string_pretty(&ErrDoc {
        start: ErrStart {
            connected: [],
            version: format!("riperf3 {}", env!("CARGO_PKG_VERSION")),
            system_info: crate::utils::system_info(),
        },
        intervals: [],
        end: serde_json::Map::new(),
        error: error.to_string(),
    })
    .unwrap()
}

/// The `--json-stream` pre-test error tail (#198): an `error` event followed
/// by an empty `end` event, iperf3's JSONStream_Output order on errexit.
pub fn error_stream_events(error: &str) -> String {
    format!(
        "{}\n{}",
        json_stream_event("error", &error),
        json_stream_event("end", &serde_json::json!({}))
    )
}

pub(crate) fn json_stream_event<T: Serialize>(event: &'static str, data: &T) -> String {
    #[derive(Serialize)]
    struct Event<'a, T: Serialize> {
        event: &'static str,
        data: &'a T,
    }
    // Infallible for the report structs (plain owned data), like the other
    // `to_string` sites in this crate.
    serde_json::to_string(&Event { event, data }).unwrap()
}

// ---------------------------------------------------------------------------
// start{}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Start {
    pub connected: Vec<Connection>,
    pub version: String,
    pub system_info: String,
    pub timestamp: Timestamp,
    // The client emits `connecting_to` (the server it dialed); the server emits
    // `accepted_connection` (the client's control-socket address). Exactly one is
    // present, and they share the `{host, port}` shape. They sit in the same slot
    // (right after `timestamp`), so a single struct serializes both roles in
    // iperf3's order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connecting_to: Option<ConnectingTo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_connection: Option<ConnectingTo>,
    pub cookie: String,
    // iperf3 emits exactly one of these, and only for TCP (iperf_api.c:1021):
    // `tcp_mss` when `-M`/`--set-mss` was given, else `tcp_mss_default` (the
    // control-socket MSS). UDP emits neither.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_mss: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_mss_default: Option<u32>,
    pub target_bitrate: u64,
    pub fq_rate: u64,
    // #261: these four are populated only once the test reaches stream-setup /
    // TestStart. On an upfront refusal (server-side rejection BEFORE TestStart,
    // e.g. --server-max-duration / code 37) the client never sets up streams, so
    // GT (iperf 3.21) OMITS them entirely — the document carries the early start
    // metadata (timestamp, cookie, connecting_to) and an empty `end`. `Option` +
    // skip-if-none reproduces that: `Some(..)` on a run that reached TestStart
    // (every success and every mid-test interrupt), `None` on the refusal path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sock_bufsize: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sndbuf_actual: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rcvbuf_actual: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_start: Option<TestStart>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Timestamp {
    /// RFC 1123 / HTTP-date GMT string, e.g. "Sat, 30 May 2026 02:20:49 GMT".
    pub time: String,
    pub timesecs: u64,
    pub timemillisecs: u64,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Connection {
    pub socket: i32,
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ConnectingTo {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TestStart {
    pub protocol: String,
    pub num_streams: i32,
    pub blksize: i64,
    pub omit: i32,
    pub duration: i32,
    pub bytes: u64,
    pub blocks: u64,
    pub reverse: i32,
    pub tos: i32,
    pub target_bitrate: u64,
    pub bidir: i32,
    pub fqrate: u64,
    #[serde(serialize_with = "ser_f64")]
    pub interval: f64,
    pub gso: i32,
    pub gro: i32,
}

// ---------------------------------------------------------------------------
// intervals[] (populated in PR2; the shape is defined now so the model is whole)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Interval {
    pub streams: Vec<IntervalStream>,
    pub sum: IntervalSum,
    /// Bidir only (#54): the reverse direction's aggregate, split out of `sum`
    /// per interval exactly like the end block's `sum_*_bidir_reverse` pair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_bidir_reverse: Option<IntervalSum>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct IntervalStream {
    pub socket: i32,
    #[serde(serialize_with = "ser_f64")]
    pub start: f64,
    #[serde(serialize_with = "ser_f64")]
    pub end: f64,
    #[serde(serialize_with = "ser_f64")]
    pub seconds: f64,
    pub bytes: u64,
    #[serde(serialize_with = "ser_f64")]
    pub bits_per_second: f64,
    // TCP per-interval detail (sender side); omitted where TCP_INFO is
    // unavailable. `snd_wnd` carries the live tcpi_snd_wnd where the
    // platform reader has it (#161).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snd_cwnd: Option<u64>,
    // Signed so macOS can emit the faithful -1 (like iperf3's get_snd_wnd);
    // non-negative values serialize identically to the old u64 (#161).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snd_wnd: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rttvar: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmtu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reorder: Option<u32>,
    // UDP per-interval detail (receiver side).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub lost_percent: Option<f64>,
    pub omitted: bool,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct IntervalSum {
    #[serde(serialize_with = "ser_f64")]
    pub start: f64,
    #[serde(serialize_with = "ser_f64")]
    pub end: f64,
    #[serde(serialize_with = "ser_f64")]
    pub seconds: f64,
    pub bytes: u64,
    #[serde(serialize_with = "ser_f64")]
    pub bits_per_second: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub lost_percent: Option<f64>,
    pub omitted: bool,
    pub sender: bool,
}

// ---------------------------------------------------------------------------
// end{}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct End {
    // #261: a run that reached TestStart always has ≥1 stream, so on success/
    // partial this serializes unchanged; on an upfront refusal it is empty and
    // omitted, contributing to GT's bare `end: {}` (which carries no `streams`
    // key either).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub streams: Vec<EndStream>,
    /// UDP only: the datagram aggregate iperf3 emits as `sum` — BEFORE the
    /// sent/received pair in its key order (GT 3.21, fwd and bidir alike;
    /// the old field position serialized it after, a raw-diff divergence).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<SumSide>,
    // #261: the summary aggregates and CPU figure exist only once the test
    // reached TestStart. On an upfront refusal GT emits `end: {}` — every key
    // omitted. With these three as `Option` + skip-if-none, an `End` whose
    // optional fields are all `None` serializes as `{}`, matching GT byte-for-
    // byte. A run that reached TestStart (success, mid-test interrupt) sets them
    // `Some(..)`, so the success/partial shape is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_sent: Option<SumSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_received: Option<SumSide>,
    /// UDP bidir only (#214): the reverse-direction datagram aggregate,
    /// between the forward pair and the reverse pair, like iperf3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_bidir_reverse: Option<SumSide>,
    /// Bidir only: the reverse-direction aggregates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_sent_bidir_reverse: Option<SumSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_received_bidir_reverse: Option<SumSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_utilization_percent: Option<CpuUtilization>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_tcp_congestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_tcp_congestion: Option<String>,
}

/// One `end.streams[]` entry. iperf3 nests the per-direction stats: TCP carries
/// `{sender, receiver}`, UDP carries `{udp}`. Exactly one shape is populated.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct EndStream {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<TcpStreamSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<TcpStreamSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp: Option<UdpStreamEnd>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TcpStreamSide {
    pub socket: i32,
    #[serde(serialize_with = "ser_f64")]
    pub start: f64,
    #[serde(serialize_with = "ser_f64")]
    pub end: f64,
    #[serde(serialize_with = "ser_f64")]
    pub seconds: f64,
    pub bytes: u64,
    #[serde(serialize_with = "ser_f64")]
    pub bits_per_second: f64,
    /// Sender side only; iperf3 reports -1 when the OS doesn't expose retransmits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    // Sender-side TCP_INFO fields. iperf3 ALWAYS emits these on the sender
    // sub-object (0 when it couldn't measure them), so riperf3 does too for
    // drop-in schema parity; they're omitted on the receiver sub-object.
    // `max_snd_wnd` and `reorder` carry the live tcpi_snd_wnd/tcpi_reord_seen
    // on Linux via the UAPI tcp_info mirror (#161), matching what iperf3
    // emits when those are unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_snd_cwnd: Option<u64>,
    // Signed i64 (the signed-max keeps macOS's -1 at 0); non-negative values
    // serialize byte-identically to the old u64 (#161).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_snd_wnd: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rtt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_rtt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_rtt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reorder: Option<u32>,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct UdpStreamEnd {
    pub socket: i32,
    #[serde(serialize_with = "ser_f64")]
    pub start: f64,
    #[serde(serialize_with = "ser_f64")]
    pub end: f64,
    #[serde(serialize_with = "ser_f64")]
    pub seconds: f64,
    pub bytes: u64,
    #[serde(serialize_with = "ser_f64")]
    pub bits_per_second: f64,
    #[serde(serialize_with = "ser_f64")]
    pub jitter_ms: f64,
    pub lost_packets: i64,
    pub packets: i64,
    #[serde(serialize_with = "ser_f64")]
    pub lost_percent: f64,
    pub out_of_order: i64,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct SumSide {
    #[serde(serialize_with = "ser_f64")]
    pub start: f64,
    #[serde(serialize_with = "ser_f64")]
    pub end: f64,
    #[serde(serialize_with = "ser_f64")]
    pub seconds: f64,
    pub bytes: u64,
    #[serde(serialize_with = "ser_f64")]
    pub bits_per_second: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub lost_percent: Option<f64>,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct CpuUtilization {
    #[serde(serialize_with = "ser_f64")]
    pub host_total: f64,
    #[serde(serialize_with = "ser_f64")]
    pub host_user: f64,
    #[serde(serialize_with = "ser_f64")]
    pub host_system: f64,
    #[serde(serialize_with = "ser_f64")]
    pub remote_total: f64,
    #[serde(serialize_with = "ser_f64")]
    pub remote_user: f64,
    #[serde(serialize_with = "ser_f64")]
    pub remote_system: f64,
}

// ---------------------------------------------------------------------------
// Builder inputs — plain data so the assembly is pure and unit-testable without
// a live Client/socket.
// ---------------------------------------------------------------------------

/// Per-stream end data, already resolved to the local (this host) and remote
/// (peer, from the exchanged results) byte counts and roles.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StreamReport {
    pub id: i32,
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    /// True if the local endpoint is the sender for this stream.
    pub is_sender: bool,
    /// Bytes moved on this stream (local perspective).
    pub local_bytes: u64,
    /// Bytes the peer reports for the opposite side of this stream, if known.
    pub remote_bytes: Option<u64>,
    /// Sender-side retransmit total: the LOCAL TCP_INFO cumulative for
    /// streams this host sent, the PEER's exchanged per-stream figure for
    /// streams it received (gated on the peer's sender_has_retransmits
    /// flag; None when ungated) — #236.
    pub retransmits: Option<i64>,
    /// Sender-side TCP_INFO extremes for the `end.streams[].sender` object (PR2).
    /// Only set for streams this host sent (local TCP_INFO); `None` otherwise.
    pub tcp_end: Option<TcpEndExtras>,
    /// UDP receiver stats (jitter seconds, lost, total packets, out-of-order),
    /// from whichever side measured them. `None` for TCP.
    pub udp: Option<UdpStreamStats>,
    /// The peer's exchanged per-stream SENT datagram count, NETTED of the
    /// peer's omitted baseline at attach time (#235) — GT keeps the
    /// gross/omitted split (`peer_packet_count` is gross, iperf_api.c:2942/
    /// 2948) and nets at consumption (:4245); a future per-stream omit
    /// rework (#31/#214) will need a `remote_omitted_packets` sibling or a
    /// shape change here. Set ONLY on streams this host received (a sent
    /// stream's peer figure is the peer's RECEIVE counter — r2 item 1).
    /// Exact when the peer keeps true counters (iperf3); riperf3 peers
    /// exchange bytes-derived figures until #235's counter half. `None`
    /// when the peer never reported (terminated runs) or for sent streams;
    /// a 3.12-class peer omitting only `omitted_*` yields Some(gross) —
    /// the #24 default-0 posture, NOT GT's all-omitted zeroing
    /// (iperf_api.c:2945-2949), a documented faithfulness gap. Consumers
    /// fall back to the bytes-derived figure on None/non-positive.
    pub remote_packets: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct TcpEndExtras {
    pub max_snd_cwnd: u64,
    /// Peak peer-advertised send window, where the platform reader captures
    /// it (Linux UAPI mirror / FreeBSD) — iperf3's stream_max_snd_wnd. Signed:
    /// the signed-max accumulation keeps macOS's faithful -1 at 0 (#161).
    pub max_snd_wnd: i64,
    pub max_rtt: u32,
    pub min_rtt: u32,
    pub mean_rtt: u32,
    pub reorder: u32,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct UdpStreamStats {
    pub jitter_secs: f64,
    pub lost_packets: i64,
    pub packets: i64,
    pub out_of_order: i64,
}

#[non_exhaustive]
pub struct ReportInput {
    pub protocol: TransportProtocol,
    /// iperf3's `"error"` blob key (e.g. "the server has terminated") (#170).
    pub error: Option<String>,
    pub reverse: bool,
    pub bidir: bool,
    /// The requested `-t` duration parameter, reported under `test_start`. Stays
    /// the nominal value even for a byte/block-limited (`-n`/`-k`) run.
    pub duration: f64,
    /// The measured elapsed test time, used for the summary window and the
    /// derived per-stream/aggregate bitrate. Equals `duration` for a duration
    /// run; for `-n`/`-k` it is the actual time the transfer took (#103).
    pub elapsed: f64,
    pub num_streams: i32,
    pub blksize: i64,
    pub omit: i32,
    pub tos: i32,
    pub target_bitrate: u64,
    pub bytes: u64,
    pub blocks: u64,
    pub connecting_host: String,
    pub connecting_port: u16,
    /// True when this report is the server's (`-s -J`). It flips the role-specific
    /// behavior: emit `accepted_connection` instead of `connecting_to`, report
    /// only this host's measured bytes (no peer graft — the un-measured side is 0),
    /// and gate the single `*_tcp_congestion` side on the server's direction.
    pub is_server: bool,
    /// The client's control-socket address, for the server's `accepted_connection`.
    /// Unused on the client.
    pub accepted_host: String,
    pub accepted_port: u16,
    pub version: String,
    pub system_info: String,
    pub cpu: CpuUtilization,
    pub congestion_used: Option<String>,
    // start{} metadata (PR3).
    pub cookie: String,
    /// The control-socket MSS, emitted as `start.tcp_mss_default` for a TCP test
    /// that did not pass `-M`.
    pub tcp_mss_default: u32,
    /// The requested `-M`/`--set-mss` value, if any. When set on a TCP test it is
    /// emitted as `start.tcp_mss` and suppresses `tcp_mss_default` (iperf3 parity).
    pub mss: Option<u32>,
    pub fq_rate: u64,
    /// Socket buffer sizes (`start.sock_bufsize` / `sndbuf_actual` /
    /// `rcvbuf_actual`). `None` on a path that never set up data sockets (the
    /// upfront-refusal path), so `build()` omits them like GT (#261).
    pub sock_bufsize: Option<u64>,
    pub sndbuf_actual: Option<u64>,
    pub rcvbuf_actual: Option<u64>,
    /// True once the test reached stream-setup / TestStart (#261). Drives the
    /// stage-gated `start.test_start` and the `end` summary aggregates: GT
    /// populates them only by how far the test progressed, and OMITS them on an
    /// upfront refusal (server rejection before TestStart, e.g. code 37). A
    /// mid-test interrupt or SERVER_TERMINATE DID reach TestStart, so it stays
    /// `true` there — the partial document keeps the late fields it has.
    pub reached_test_start: bool,
    pub interval: f64,
    pub gso: i32,
    pub gro: i32,
    /// Wall-clock at test start, ms since the Unix epoch — for `start.timestamp`.
    pub start_time_millis: u64,
    /// `--extra-data` string, emitted at the top level when present (#35).
    pub extra_data: Option<String>,
    /// `--get-server-output` (#33), client side: the server's returned output
    /// for the -J report tail.
    pub server_output_text: Option<String>,
    pub server_output_json: Option<serde_json::Value>,
    /// Per-interval samples collected during the run (PR2). Empty if interval
    /// reporting was disabled (`-i 0`).
    pub intervals: Vec<Interval>,
    pub streams: Vec<StreamReport>,
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

fn bps(bytes: u64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        bytes as f64 * 8.0 / seconds
    } else {
        0.0
    }
}

fn pct_lost(lost: i64, total: i64) -> f64 {
    crate::reporter::lost_percent(lost, total)
}

/// Format a Unix timestamp (seconds) as an RFC 1123 GMT string, e.g.
/// "Sat, 30 May 2026 02:20:49 GMT" — matching iperf3's `start.timestamp.time`.
/// Pure safe Rust (no chrono): epoch → civil date via Howard Hinnant's algorithm.
pub(crate) fn http_date(epoch_secs: u64) -> String {
    const DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (epoch_secs / 86_400) as i64;
    let tod = epoch_secs % 86_400;
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // 1970-01-01 was a Thursday (index 4 with Sunday = 0).
    let dow = (((days % 7) + 4) % 7) as usize;
    // civil_from_days (Hinnant): days since the epoch -> (year, month, day).
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DOW[dow],
        day,
        MON[(month - 1) as usize],
        year,
        hh,
        mm,
        ss
    )
}

impl ReportInput {
    /// Assemble the iperf3-schema report: the `start` block (timestamp, cookie,
    /// `system_info`, `tcp_mss_default`, socket buffers, and `test_start`
    /// parameters), the collected `intervals`, and the `end` block.
    pub fn build(&self) -> Report {
        let dur = self.duration;
        let is_udp = matches!(self.protocol, TransportProtocol::Udp);

        let connected: Vec<Connection> = self
            .streams
            .iter()
            .map(|s| Connection {
                socket: s.id,
                local_host: s.local_host.clone(),
                local_port: s.local_port,
                remote_host: s.remote_host.clone(),
                remote_port: s.remote_port,
            })
            .collect();

        let end_streams: Vec<EndStream> = self.streams.iter().map(|s| self.end_stream(s)).collect();

        // Aggregates, direction-partitioned so forward / reverse / bidir each
        // report the right flow under the right key, matching iperf3.
        let local_sent: u64 = self
            .streams
            .iter()
            .filter(|s| s.is_sender)
            .map(|s| s.local_bytes)
            .sum();
        let local_recv: u64 = self
            .streams
            .iter()
            .filter(|s| !s.is_sender)
            .map(|s| s.local_bytes)
            .sum();
        // The peer's reported bytes for each direction (forward → peer received,
        // reverse → peer sent), used to fill the side this host didn't measure.
        let peer_recv: u64 = self
            .streams
            .iter()
            .filter(|s| s.is_sender)
            .filter_map(|s| s.remote_bytes)
            .sum();
        let peer_sent: u64 = self
            .streams
            .iter()
            .filter(|s| !s.is_sender)
            .filter_map(|s| s.remote_bytes)
            .sum();

        // Receiver-measured UDP loss/jitter, aggregated across measured streams.
        let (udp_lost, udp_packets, udp_jitter) = if is_udp {
            (
                self.streams
                    .iter()
                    .filter_map(|s| s.udp)
                    .map(|u| u.lost_packets)
                    .sum::<i64>(),
                self.streams
                    .iter()
                    .filter_map(|s| s.udp)
                    .map(|u| u.packets)
                    .sum::<i64>(),
                {
                    // #214 (2): iperf3 AVERAGES jitter — and divides by
                    // num_streams (the -P value), not the measured-stream
                    // count (r1 review): on a #170-terminated partial doc
                    // iperf3 still divides by the full count. Equal in
                    // every complete run. fold(max) overstated it.
                    let sum_jitter: f64 = self
                        .streams
                        .iter()
                        .filter_map(|s| s.udp)
                        .map(|u| u.jitter_secs)
                        .sum();
                    sum_jitter / (self.num_streams.max(1) as f64)
                },
            )
        } else {
            (0_i64, 0_i64, 0.0_f64)
        };
        let blk = self.blksize.max(1) as u64;
        // iperf3's `stream_must_be_sender` for the aggregate `sender` flag.
        let fwd_sender = !self.reverse;
        let retransmits = self.sender_retransmits();

        let mut sum = None;
        let mut sum_bidir_reverse = None;
        let mut sum_sent_bidir_reverse = None;
        let mut sum_received_bidir_reverse = None;

        let (sum_sent, sum_received) = if self.is_server {
            // Server: report only this host's OWN measured bytes — iperf3 sums
            // local per-stream counters filtered by `sp->sender` and never grafts
            // the peer's reported bytes, so the side the server didn't measure is
            // genuinely 0 (forward → sent 0, reverse → received 0). The aggregate
            // `sender` flag is the server's role: it is the sender only in reverse.
            let server_is_sender = self.reverse;
            if self.bidir && is_udp {
                // #214 (1), server role — GT iperf 3.21 (-s -J vs -u --bidir):
                // six aggregates, every one UDP-shaped, strict no-graft:
                // the fwd direction the server RECEIVES has measured
                // packets/loss/jitter but `bytes` stays the SENDER-side
                // figure the server lacks (0 — the iperf3 quirk, mirrored);
                // the reverse direction it SENDS has real bytes + derived
                // packets and zero measurement; the perspectives it never
                // held (sum_sent fwd, sum_received_bidir_reverse) are
                // all-zero.
                let fwd_packets = self
                    .streams
                    .iter()
                    .filter(|s| !s.is_sender)
                    .filter_map(|s| s.udp)
                    .map(|u| u.packets)
                    .sum::<i64>();
                let fwd_lost = self
                    .streams
                    .iter()
                    .filter(|s| !s.is_sender)
                    .filter_map(|s| s.udp)
                    .map(|u| u.lost_packets)
                    .sum::<i64>();
                // num_streams divisor, like iperf3 (r1 review).
                let fwd_jitter = self
                    .streams
                    .iter()
                    .filter(|s| !s.is_sender)
                    .filter_map(|s| s.udp)
                    .map(|u| u.jitter_secs)
                    .sum::<f64>()
                    / (self.num_streams.max(1) as f64);
                let rev_sent_packets = (local_sent / blk) as i64;
                sum = Some(self.udp_sum(0, false, fwd_packets, fwd_lost, fwd_jitter, fwd_packets));
                sum_bidir_reverse = Some(self.udp_sum(
                    local_sent,
                    true,
                    rev_sent_packets,
                    0,
                    0.0,
                    rev_sent_packets,
                ));
                sum_sent_bidir_reverse = Some(self.udp_sum(
                    local_sent,
                    true,
                    rev_sent_packets,
                    0,
                    0.0,
                    rev_sent_packets,
                ));
                sum_received_bidir_reverse = Some(self.udp_sum(0, false, 0, 0, 0.0, 0));
                (
                    self.udp_sum(0, true, 0, 0, 0.0, 0),
                    self.udp_sum(
                        local_recv,
                        false,
                        fwd_packets,
                        fwd_lost,
                        fwd_jitter,
                        fwd_packets,
                    ),
                )
            } else if self.bidir {
                // Two flows: forward (client→server, server receives → sender=false)
                // in sum_sent/sum_received; reverse (server→client, server sends →
                // sender=true) in the *_bidir_reverse pair. Retransmits, measured on
                // the server's send path, attach to the reverse (sent) side —
                // direction-filtered like GT's per-pass accumulator (#236).
                sum_sent_bidir_reverse =
                    Some(self.tcp_sum(local_sent, true, self.retransmits_for(Some(true))));
                sum_received_bidir_reverse = Some(self.tcp_sum(0, true, None));
                (
                    self.tcp_sum(0, false, None),
                    self.tcp_sum(local_recv, false, None),
                )
            } else if is_udp {
                // sum_sent is always the sender perspective (bytes the server sent,
                // no loss); sum_received the receiver perspective (bytes received,
                // with measured loss/jitter). `sum` carries the server's sent bytes
                // tagged with its role, and the packet/loss/jitter of whichever side
                // the server actually measured (received in forward, sent in reverse).
                let sent_packets = (local_sent / blk) as i64;
                let (sum_packets, sum_lost, sum_jitter) = if server_is_sender {
                    (sent_packets, 0, 0.0)
                } else {
                    (udp_packets, udp_lost, udp_jitter)
                };
                sum = Some(self.udp_sum(
                    local_sent,
                    server_is_sender,
                    sum_packets,
                    sum_lost,
                    sum_jitter,
                    sum_packets,
                ));
                (
                    self.udp_sum(local_sent, true, sent_packets, 0, 0.0, sent_packets),
                    self.udp_sum(
                        local_recv,
                        false,
                        udp_packets,
                        udp_lost,
                        udp_jitter,
                        sum_packets,
                    ),
                )
            } else {
                // Single-direction TCP. sum_sent = bytes the server sent (0 forward),
                // sum_received = bytes received (0 reverse); both carry the server's
                // role flag. Retransmits live on the sent side.
                (
                    self.tcp_sum(local_sent, server_is_sender, retransmits),
                    self.tcp_sum(local_recv, server_is_sender, None),
                )
            }
        } else if self.bidir && is_udp {
            // #214 (1), client role — GT iperf 3.21 (-u --bidir -J): six
            // aggregates, all UDP-shaped. The fwd direction's measured stats
            // ride this host's SENDER streams (the peer-measured exchange,
            // #182); the reverse direction's are measured locally on the
            // receiving streams. Per GT: `sum` is the fwd direction
            // (sender=true, peer-measured jitter attached), the *_sent
            // perspectives carry zero measurement, and byte values follow
            // the same graft-with-#170-error-guard rules as the TCP arm.
            // Measured (receiver-side) figures per direction, folded
            // without the intermediate Vec (r2 nit).
            let dir_stats = |want_sender: bool| {
                let (packets, lost, jitter_sum) = self
                    .streams
                    .iter()
                    .filter(|s| s.is_sender == want_sender)
                    .filter_map(|s| s.udp)
                    .fold((0i64, 0i64, 0f64), |(p, l, j), u| {
                        (p + u.packets, l + u.lost_packets, j + u.jitter_secs)
                    });
                // num_streams divisor, like iperf3 (one direction's pass
                // divides by the -P value, r1 review) — not measured-count.
                (packets, lost, jitter_sum / (self.num_streams.max(1) as f64))
            };
            let (fwd_packets, fwd_lost, fwd_jitter) = dir_stats(true);
            let (rev_packets, rev_lost, rev_jitter) = dir_stats(false);
            let fwd_recv = match (peer_recv, self.error.is_some()) {
                (p, _) if p > 0 => p,
                (_, true) => 0,
                _ => local_sent,
            };
            let rev_sent = match (peer_sent, self.error.is_some()) {
                (p, _) if p > 0 => p,
                (_, true) => 0,
                _ => local_recv,
            };
            // SENT-side packet figures derive from the sender's byte counts
            // (r2 review, the packets analog of the r1 bytes blocker —
            // live-proven incl. a deterministic terminated-run divergence):
            // local sent/blk for the direction we send, the peer-grafted
            // rev_sent/blk for the direction we receive (0 when terminated,
            // exactly like iperf3's never-arrived figure). `sum`-class
            // aggregates fall back to the receiver count when the sent
            // figure is absent (iperf_api.c ~4242, the same fallback the
            // single-direction arm already uses).
            let fwd_sent_packets = (local_sent / blk) as i64;
            // #235: the reverse (peer-sent) figure prefers the exchanged
            // true counts; bytes-derived only as the fallback. (GT's
            // sum-class nets LOCAL omitted, :4243 — peer-netted here;
            // identical at omit=0, ppm-scale under -O, #31/#214 scope.)
            let rev_sent_packets = self
                .exchanged_sent_packets(false)
                .unwrap_or((rev_sent / blk) as i64);
            let sum_fwd_packets = if fwd_sent_packets > 0 {
                fwd_sent_packets
            } else {
                fwd_packets
            };
            let sum_rev_packets = if rev_sent_packets > 0 {
                rev_sent_packets
            } else {
                rev_packets
            };
            sum = Some(self.udp_sum(
                local_sent,
                true,
                sum_fwd_packets,
                fwd_lost,
                fwd_jitter,
                sum_fwd_packets,
            ));
            // sum_bidir_reverse.bytes is the SENDER-side figure — iperf3
            // feeds it from total_sent, the same variable as
            // sum_sent_bidir_reverse (iperf_api.c:4504/4514; r1 review
            // proved it live on a lossy run: both stay equal while
            // *_received_bidir_reverse drops). local_recv here diverged 18%
            // under loss.
            sum_bidir_reverse = Some(self.udp_sum(
                rev_sent,
                false,
                sum_rev_packets,
                rev_lost,
                rev_jitter,
                sum_rev_packets,
            ));
            sum_sent_bidir_reverse =
                Some(self.udp_sum(rev_sent, true, rev_sent_packets, 0, 0.0, rev_sent_packets));
            sum_received_bidir_reverse = Some(self.udp_sum(
                local_recv,
                false,
                rev_packets,
                rev_lost,
                rev_jitter,
                sum_rev_packets,
            ));
            (
                self.udp_sum(local_sent, true, fwd_sent_packets, 0, 0.0, fwd_sent_packets),
                self.udp_sum(
                    fwd_recv,
                    false,
                    fwd_packets,
                    fwd_lost,
                    fwd_jitter,
                    sum_fwd_packets,
                ),
            )
        } else if self.bidir {
            // Forward (this host → peer) goes in sum_sent/sum_received; reverse
            // (peer → this host) in the *_bidir_reverse pair, matching iperf3 —
            // rather than folding the reverse flow into sum_received (which would
            // make the two aggregates describe different directions).
            // No cross-graft on a terminated run (#170 review r2 f1): the
            // peer halves never arrived — iperf3's bidir sums carry 0 there.
            let fwd_recv = match (peer_recv, self.error.is_some()) {
                (p, _) if p > 0 => p,
                (_, true) => 0,
                _ => local_sent,
            };
            let rev_sent = match (peer_sent, self.error.is_some()) {
                (p, _) if p > 0 => p,
                (_, true) => 0,
                _ => local_recv,
            };
            // #236: per-direction retransmit totals, like GT's per-pass
            // accumulator — the reverse-sent aggregate carries the PEER's
            // exchanged counts (riding this host's receiving streams;
            // live-observed 2), the forward one ONLY the local senders'.
            sum_sent_bidir_reverse =
                Some(self.tcp_sum(rev_sent, false, self.retransmits_for(Some(false))));
            sum_received_bidir_reverse = Some(self.tcp_sum(local_recv, false, None));
            (
                self.tcp_sum(local_sent, true, self.retransmits_for(Some(true))),
                self.tcp_sum(fwd_recv, true, None),
            )
        } else if is_udp {
            // UDP single direction. iperf3: sum_sent.sender=1, sum_received.sender=0,
            // sum.sender=stream_must_be_sender. `sum.bytes` is the *sent* count with
            // receiver-measured loss attached; the sender side measures no loss.
            let sent_bytes = if local_sent > 0 {
                local_sent
            } else {
                peer_sent
            };
            let recv_bytes = if local_recv > 0 {
                local_recv
            } else {
                peer_recv
            };
            // #235: when the sent side is the PEER's (-R), prefer its
            // exchanged true counts over the bytes-derived figure (same
            // local-vs-peer omit-netting hedge as the bidir arm).
            let sent_packets = (!fwd_sender)
                .then(|| self.exchanged_sent_packets(false))
                .flatten()
                .unwrap_or((sent_bytes / blk) as i64);
            // iperf3's `sum` packet count falls back to the RECEIVER count
            // when the sender count is absent (iperf_api.c:4242, the
            // `packet_count = sender ? sender : receiver` running total) —
            // reachable when a terminated -R run never exchanged (#170 r2 f2).
            let sum_packets = if sent_packets > 0 {
                sent_packets
            } else {
                udp_packets
            };
            sum = Some(self.udp_sum(
                sent_bytes,
                fwd_sender,
                sum_packets,
                udp_lost,
                udp_jitter,
                sum_packets,
            ));
            (
                self.udp_sum(sent_bytes, true, sent_packets, 0, 0.0, sent_packets),
                self.udp_sum(
                    recv_bytes,
                    false,
                    udp_packets,
                    udp_lost,
                    udp_jitter,
                    sum_packets,
                ),
            )
        } else {
            // TCP single direction (forward or reverse); both aggregates carry the
            // test's sender flag (!reverse), like iperf3.
            let sent_bytes = match (local_sent, peer_sent) {
                (0, p) if p > 0 => p,
                // No cross-graft on a terminated run (#170): iperf3's
                // sum_sent/sum_received carry 0 for the half it never got.
                (0, _) if self.error.is_none() => local_recv,
                (s, _) => s,
            };
            let recv_bytes = match (local_recv, peer_recv) {
                (0, p) if p > 0 => p,
                (0, _) if self.error.is_none() => local_sent,
                (r, _) => r,
            };
            (
                self.tcp_sum(sent_bytes, fwd_sender, retransmits),
                self.tcp_sum(recv_bytes, fwd_sender, None),
            )
        };

        let (cong_sender, cong_receiver) = if is_udp {
            (None, None)
        } else if self.is_server {
            // The peer's (client's) congestion algorithm is never exchanged to the
            // server, so only one side is emitted — the server's local algorithm
            // (read back via getsockopt(TCP_CONGESTION), #37), on the side matching
            // its role: receiver in forward, sender in reverse and bidir
            // (iperf_api.c:4544 swaps by stream_must_be_sender). None for UDP /
            // platforms without TCP_CONGESTION, in which case the field is omitted.
            let local = self.congestion_used.clone();
            if self.reverse || self.bidir {
                (local, None)
            } else {
                (None, local)
            }
        } else {
            (self.congestion_used.clone(), self.congestion_used.clone())
        };

        // #261: stage-gate the `end` summary aggregates on whether the test
        // reached TestStart. On an upfront refusal (never reached it) every
        // optional field is None and `streams` is empty, so `End` serializes as
        // GT's bare `end: {}`. A run that reached TestStart (success, mid-test
        // interrupt, SERVER_TERMINATE) keeps them — the partial document carries
        // the late fields it has, unchanged from before.
        let reached = self.reached_test_start;
        let end = End {
            streams: end_streams,
            sum_sent: reached.then_some(sum_sent),
            sum_received: reached.then_some(sum_received),
            sum,
            sum_bidir_reverse,
            sum_sent_bidir_reverse,
            sum_received_bidir_reverse,
            cpu_utilization_percent: reached.then(|| self.cpu.clone()),
            sender_tcp_congestion: cong_sender,
            receiver_tcp_congestion: cong_receiver,
        };

        let secs = self.start_time_millis / 1000;
        // Role-specific connection target: the client dialed a server
        // (`connecting_to`), the server accepted a client (`accepted_connection`).
        let (connecting_to, accepted_connection) = if self.is_server {
            (
                None,
                Some(ConnectingTo {
                    host: self.accepted_host.clone(),
                    port: self.accepted_port,
                }),
            )
        } else {
            (
                Some(ConnectingTo {
                    host: self.connecting_host.clone(),
                    port: self.connecting_port,
                }),
                None,
            )
        };
        // iperf3 (iperf_api.c:1021) emits the MSS key only for TCP, and picks
        // exactly one: `tcp_mss` when `-M` was given, else `tcp_mss_default`.
        // UDP emits neither. `self.mss.filter(|&m| m > 0)` mirrors iperf3's
        // `if (settings->mss)` truthiness check.
        let (tcp_mss, tcp_mss_default) = if is_udp {
            (None, None)
        } else if let Some(m) = self.mss.filter(|&m| m > 0) {
            (Some(m), None)
        } else {
            (None, Some(self.tcp_mss_default))
        };
        Report {
            start: Start {
                connected,
                version: self.version.clone(),
                system_info: self.system_info.clone(),
                timestamp: Timestamp {
                    time: http_date(secs),
                    timesecs: secs,
                    timemillisecs: self.start_time_millis,
                },
                connecting_to,
                accepted_connection,
                cookie: self.cookie.clone(),
                tcp_mss,
                tcp_mss_default,
                target_bitrate: self.target_bitrate,
                fq_rate: self.fq_rate,
                // #261: the socket buffers and `test_start` block are populated
                // only once the test reached stream-setup / TestStart. On the
                // upfront-refusal path `reached` is false → all omitted (the
                // buffer fields are also `None` at the source there); GT does the
                // same, so the refusal `start` carries only the early metadata.
                sock_bufsize: reached.then_some(self.sock_bufsize).flatten(),
                sndbuf_actual: reached.then_some(self.sndbuf_actual).flatten(),
                rcvbuf_actual: reached.then_some(self.rcvbuf_actual).flatten(),
                test_start: reached.then(|| TestStart {
                    protocol: if is_udp { "UDP" } else { "TCP" }.to_string(),
                    num_streams: self.num_streams,
                    blksize: self.blksize,
                    omit: self.omit,
                    // #114: iperf3 zeroes test_start.duration for byte/block-limited
                    // (-n/-k) runs — the -t window doesn't apply. bytes/blocks below
                    // carry the actual limit, mirroring iperf3.
                    duration: if self.bytes > 0 || self.blocks > 0 {
                        0
                    } else {
                        dur as i32
                    },
                    bytes: self.bytes,
                    blocks: self.blocks,
                    reverse: self.reverse as i32,
                    tos: self.tos,
                    target_bitrate: self.target_bitrate,
                    bidir: self.bidir as i32,
                    fqrate: self.fq_rate,
                    interval: self.interval,
                    gso: self.gso,
                    gro: self.gro,
                }),
            },
            intervals: self.intervals.clone(),
            end,
            extra_data: self.extra_data.clone(),
            server_output_text: self.server_output_text.clone(),
            // #261 DELIBERATE DEVIATION: riperf3 emits a SINGLE `"error"` key
            // holding the bare strerror message. GT (iperf 3.21) emits the
            // `"error"` key TWICE on a client-side refusal — once as
            // `SERVER ERROR - <msg>` and once as the bare `<msg>` — because two
            // code paths both `cJSON_AddStringToObject(json, "error", ...)` the
            // same key (an upstream defect we filed as esnet/iperf#2051). A
            // duplicate object key is undefined in JSON; a conformant last-wins
            // parser of GT's document resolves to the bare message, which is what
            // riperf3 emits — so we are faithful to the *parsed* result while
            // declining to reproduce the malformed wire bytes.
            error: self.error.clone(),
            server_output_json: self.server_output_json.clone(),
        }
    }

    fn end_stream(&self, s: &StreamReport) -> EndStream {
        let dur = self.elapsed;
        // Shape is driven by the test protocol, not by whether stats happen to be
        // present: a UDP stream with missing stats still emits a valid `udp`
        // object (zeroed datagram fields), never a TCP `{sender,receiver}` body.
        if matches!(self.protocol, TransportProtocol::Udp) {
            let u = s.udp.unwrap_or(UdpStreamStats {
                jitter_secs: 0.0,
                lost_packets: 0,
                packets: 0,
                out_of_order: 0,
            });
            // `bytes` is a sender-side count. The client reports its own local
            // bytes (it grafts the peer's count elsewhere); the server only knows
            // the bytes it *sent*, so a stream it received reports 0 bytes (iperf3
            // parity) while still carrying the packet/loss/jitter it measured. A
            // stream the server sent has no receiver stats, so its sent packet
            // count is derived from the bytes it pushed.
            // The third element says whether `packets` carries SENDER-side
            // provenance: iperf3's pct denominator is strictly the sender
            // count, and the two figures have ASYMMETRIC fallbacks when it's
            // absent — `packets` falls back to the measured receiver count
            // (iperf_api.c:4311), lost_percent goes to 0.0 (:4288-4293
            // else-branch), never the measured pct (#238; live-proven on a
            // terminated bidir probe — iperf3 0 vs riperf3's old 14.75).
            let (bytes, packets, has_sender_count) = if self.is_server {
                if s.is_sender {
                    let blk = self.blksize.max(1) as u64;
                    (s.local_bytes, (s.local_bytes / blk) as i64, true)
                } else {
                    // Sender count absent on EVERY server run, not just
                    // terminated ones (#238): GT's server does receive the
                    // client's per-stream figures (get_results,
                    // iperf_api.c:2942) but PRINTS first — reporter_callback
                    // at iperf_server_api.c:277 runs before the exchange at
                    // :280 — so they never reach its report. Plain
                    // forward-UDP server docs included, not just bidir.
                    (0, u.packets, false)
                }
            } else if s.is_sender && s.udp.is_none() && self.error.is_some() {
                // Terminated before the exchange (#170): no peer-measured
                // stats — iperf3 reports the sender's LOCAL packet count.
                let blk = self.blksize.max(1) as u64;
                (s.local_bytes, (s.local_bytes / blk) as i64, true)
            } else if !s.is_sender && self.error.is_some() {
                // Terminated receiver stream (-R): `bytes` is a SENDER-side
                // count the dead peer never reported — iperf3 emits 0 while
                // keeping the locally measured packets (#170 r2 f2).
                (0, u.packets, false)
            } else if !s.is_sender {
                // #214 (3): `bytes` is a SENDER-side count — a stream this
                // client RECEIVED reports the peer's exchanged sent figure
                // (diverges from local received under loss); fall back to
                // local only when the peer never reported one. `packets`
                // follows the same provenance (r2 review): derive from the
                // sender-side bytes like the server arm, with the measured
                // count as the zero-bytes fallback — for `packets` ONLY;
                // the pct has no such fallback in iperf3 (#238).
                let bytes = s.remote_bytes.unwrap_or(s.local_bytes);
                let blk = self.blksize.max(1) as u64;
                if let Some(rp) = s.remote_packets.filter(|&p| p > 0) {
                    // #235: the peer's exchanged sent count — exact when the
                    // peer keeps true counters (iperf3); our own senders
                    // exchange bytes-derived figures until #235's counter
                    // half. The >0 filter is load-bearing: a pre-#184
                    // riperf3 server exchanges packets:0 with real bytes,
                    // which must fall back, not zero the entry. (Per-stream
                    // emission is iperf_api.c:4312; the netting nuance vs
                    // GT's local-omit subtraction is #31/#214 scope.)
                    (bytes, rp, true)
                } else if bytes > 0 {
                    (bytes, (bytes / blk) as i64, true)
                } else {
                    (bytes, u.packets, false)
                }
            } else {
                // #239: a client SENDER stream reports its LOCAL sent count
                // (sender_packet_count = sp->packet_count, iperf_api.c:
                // 4220-4221), not the peer-measured #182 figure — they
                // diverge under forward trailing loss. The peer-measured
                // jitter/lost VALUES stay on the entry below; exact tail-
                // partial-datagram counts are #235's residue.
                let blk = self.blksize.max(1) as u64;
                (s.local_bytes, (s.local_bytes / blk) as i64, true)
            };
            return EndStream {
                sender: None,
                receiver: None,
                udp: Some(UdpStreamEnd {
                    socket: s.id,
                    start: 0.0,
                    end: dur,
                    seconds: dur,
                    bytes,
                    bits_per_second: bps(bytes, dur),
                    jitter_ms: u.jitter_secs * 1000.0,
                    lost_packets: u.lost_packets,
                    packets,
                    // #238: the pct exists only over a sender-side
                    // denominator; absent sender count -> 0.0 (GT's
                    // `if (sender_packet_count - sender_omitted_packet_count
                    // > 0)` else-branch at :4288; the bare-truthiness form is
                    // the PACKETS fallback at :4311 — equivalent here at
                    // omit=0, the per-stream omit subtraction being #31/#214
                    // scope).
                    lost_percent: if has_sender_count {
                        pct_lost(u.lost_packets, packets)
                    } else {
                        0.0
                    },
                    out_of_order: u.out_of_order,
                    sender: s.is_sender,
                }),
            };
        }

        // TCP: nested sender + receiver. Both sub-objects carry this stream's
        // direction flag (`s.is_sender`) — correct in forward, reverse, and
        // per-stream in bidir (where both directions coexist). The local count
        // covers our side; the peer's reported bytes the other (falling back to
        // the local count when the peer reported no per-stream figure).
        let dir = s.is_sender;
        // Sender-side TCP_INFO extremes go on whichever sub-object is the local
        // sender (forward). The peer's extremes aren't exchanged, so the sender
        // sub-object of a reverse stream omits them.
        // The server never learns the peer's per-stream byte count, so its
        // un-measured side is 0 (iperf3 reports only local counters) — never
        // grafted from `local_bytes` the way the client fills the peer side.
        let remote_bytes = match (self.is_server, self.error.is_some()) {
            (true, _) => 0,
            // Terminated mid-test (#170): the peer half never arrived —
            // iperf3 zeroes it (live-verified), never grafts local as peer.
            (false, true) => s.remote_bytes.unwrap_or(0),
            (false, false) => s.remote_bytes.unwrap_or(s.local_bytes),
        };
        // The client always emits the sender sub-object's TCP_INFO keys (real on
        // the forward side, 0 on the reverse side it didn't measure). iperf3's
        // *server*, by contrast, omits them entirely on a stream it didn't send
        // (a forward receiver) and emits them only on a stream it sent
        // (reverse/bidir). Match that asymmetry: emit the extras unless this is a
        // server stream the server received.
        let emit_extras = !self.is_server || s.is_sender;
        let e = s.tcp_end.unwrap_or_default();
        let sender_side = |bytes: u64, retransmits: Option<i64>| TcpStreamSide {
            socket: s.id,
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes,
            bits_per_second: bps(bytes, dur),
            retransmits: if emit_extras { retransmits } else { None },
            max_snd_cwnd: emit_extras.then_some(e.max_snd_cwnd),
            max_snd_wnd: emit_extras.then_some(e.max_snd_wnd),
            max_rtt: emit_extras.then_some(e.max_rtt),
            min_rtt: emit_extras.then_some(e.min_rtt),
            mean_rtt: emit_extras.then_some(e.mean_rtt),
            reorder: emit_extras.then_some(e.reorder),
            sender: dir,
        };
        let receiver_side = |bytes: u64| TcpStreamSide {
            socket: s.id,
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes,
            bits_per_second: bps(bytes, dur),
            retransmits: None,
            max_snd_cwnd: None,
            max_snd_wnd: None,
            max_rtt: None,
            min_rtt: None,
            mean_rtt: None,
            reorder: None,
            sender: dir,
        };
        if s.is_sender {
            EndStream {
                sender: Some(sender_side(s.local_bytes, s.retransmits)),
                receiver: Some(receiver_side(remote_bytes)),
                udp: None,
            }
        } else {
            EndStream {
                sender: Some(sender_side(remote_bytes, s.retransmits)),
                receiver: Some(receiver_side(s.local_bytes)),
                udp: None,
            }
        }
    }

    /// Sender-side retransmit total over every stream. Correct for
    /// single-direction runs (one direction exists, local or exchanged);
    /// bidir aggregates must use [`Self::retransmits_for`] — GT's results
    /// loop runs once per direction with a per-pass total_retransmits
    /// (iperf_api.c:4138/4235), so the two sent-aggregates never mix (#236).
    fn sender_retransmits(&self) -> Option<i64> {
        self.retransmits_for(None)
    }

    /// Retransmit total over the streams of one direction (`Some(true)` =
    /// this host's senders, `Some(false)` = its receiving streams, whose
    /// `retransmits` carry the PEER's exchanged per-stream counts; `None` =
    /// every stream). Collapses the -1 "unavailable" sentinel rather than
    /// summing it (summing N sentinels would emit a nonsensical -N that
    /// iperf3 never produces).
    fn retransmits_for(&self, want_sender: Option<bool>) -> Option<i64> {
        let vals: Vec<i64> = self
            .streams
            .iter()
            .filter(|s| want_sender.is_none_or(|w| s.is_sender == w))
            .filter_map(|s| s.retransmits)
            .collect();
        if vals.is_empty() {
            None
        } else if vals.iter().all(|&r| r < 0) {
            Some(-1) // all unavailable → iperf3's single sentinel
        } else {
            Some(vals.iter().map(|&r| r.max(0)).sum())
        }
    }

    fn tcp_sum(&self, bytes: u64, sender: bool, retransmits: Option<i64>) -> SumSide {
        let dur = self.elapsed;
        SumSide {
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes,
            bits_per_second: bps(bytes, dur),
            retransmits,
            jitter_ms: None,
            lost_packets: None,
            packets: None,
            lost_percent: None,
            sender,
        }
    }

    /// #235: the sum of the peers' exchanged per-stream SENT counts for one
    /// direction, when EVERY stream of that direction reported one — GT's
    /// sender_total_packets running total over peer_packet_count
    /// (iperf_api.c:4227/4245). All-or-nothing: a partial set (unreachable
    /// with real peers — results carry every stream or none) falls back to
    /// the caller's bytes-derived figure, preserving the #170 terminated
    /// and odd-peer graft rules unchanged.
    fn exchanged_sent_packets(&self, want_sender: bool) -> Option<i64> {
        let counts: Vec<Option<i64>> = self
            .streams
            .iter()
            .filter(|s| s.is_sender == want_sender)
            .map(|s| s.remote_packets.filter(|&p| p > 0))
            .collect();
        (!counts.is_empty() && counts.iter().all(Option::is_some))
            .then(|| counts.into_iter().flatten().sum())
    }

    /// `pct_packets` is the lost_percent DENOMINATOR — iperf3 computes one
    /// per-direction pct from (lost, total_packets) where total is the
    /// sent-with-fallback figure, and REUSES it on the received-side
    /// aggregates (iperf_api.c:4492-4497 → 4514; r3 review F2). Pass the
    /// direction's total; for sent-side aggregates (lost=0) it is inert.
    #[allow(clippy::too_many_arguments)]
    fn udp_sum(
        &self,
        bytes: u64,
        sender: bool,
        packets: i64,
        lost: i64,
        jitter_secs: f64,
        pct_packets: i64,
    ) -> SumSide {
        let dur = self.elapsed;
        SumSide {
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes,
            bits_per_second: bps(bytes, dur),
            retransmits: None,
            jitter_ms: Some(jitter_secs * 1000.0),
            lost_packets: Some(lost),
            packets: Some(packets),
            lost_percent: Some(pct_lost(lost, pct_packets)),
            sender,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // #57: cjson_number must match C cJSON's print_number byte-for-byte. Expected
    // values were cross-checked against Python's %.15g/%.17g (same libc path as
    // cJSON), including the two high-precision bits_per_second cases where 15g
    // fails the round-trip and 17g kicks in.
    #[test]
    fn cjson_number_matches_cjson() {
        let cases = [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-5.0, "-5"),
            (10485760.0, "10485760"),
            (4194304.0, "4194304"),
            (0.5, "0.5"),
            (1.002098, "1.002098"),
            (99.99, "99.99"),
            (0.045, "0.045"),
            (1.0000349, "1.0000349"),
            // 15g round-trips → kept:
            (943161195.674271, "943161195.674271"),
            // 15g fails round-trip → 17g fallback (more digits than ryu's shortest):
            (943718412.3076923, "943718412.30769229"),
            (349525333.3333333, "349525333.33333331"),
        ];
        for (v, want) in cases {
            assert_eq!(cjson_number(v), want, "cjson_number({v})");
        }
        // Non-finite degrades to JSON null, like cJSON.
        assert_eq!(cjson_number(f64::NAN), "null");
        assert_eq!(cjson_number(f64::INFINITY), "null");
    }

    // #62: the --json-stream envelope is `{"event":<event>,"data":<data>}`,
    // compact (one line), `event` first, and the `data` payload is byte-identical
    // to the standalone encoding of the typed value (so it keeps the cJSON float
    // formatting and matches the corresponding `-J` section).
    #[test]
    fn json_stream_event_wraps_compactly() {
        let report = base_input().build();
        let line = json_stream_event("start", &report.start);
        assert!(!line.contains('\n'), "must be one compact line: {line}");
        assert!(
            line.starts_with(r#"{"event":"start","data":"#),
            "envelope/field order wrong: {line}"
        );
        let data = serde_json::to_string(&report.start).unwrap();
        assert_eq!(line, format!(r#"{{"event":"start","data":{data}}}"#));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "start");
        assert!(v["data"].is_object());
    }

    // The whole point of #57: a serialized report carries no integral `N.0`
    // token. Build a report whose floats are all integral and assert the raw
    // text has no `<digit>.0` anywhere.
    #[test]
    fn report_json_has_no_integral_dot_zero() {
        let json = serde_json::to_string_pretty(&base_input().build()).unwrap();
        let bytes = json.as_bytes();
        for (i, w) in bytes.windows(3).enumerate() {
            if w[0].is_ascii_digit() && w[1] == b'.' && w[2] == b'0' {
                // allow `.0` only if followed by another digit (e.g. 1.02)
                let next = bytes.get(i + 3).copied().unwrap_or(b' ');
                assert!(
                    next.is_ascii_digit(),
                    "integral N.0 token at byte {i}: ...{}...",
                    &json[i.saturating_sub(8)..(i + 6).min(json.len())]
                );
            }
        }
    }

    fn base_input() -> ReportInput {
        ReportInput {
            error: None,
            server_output_text: None,
            server_output_json: None,
            protocol: TransportProtocol::Tcp,
            reverse: false,
            bidir: false,
            duration: 10.0,
            elapsed: 10.0,
            num_streams: 1,
            blksize: 131072,
            omit: 0,
            tos: 0,
            target_bitrate: 0,
            bytes: 0,
            blocks: 0,
            connecting_host: "host".into(),
            connecting_port: 5201,
            is_server: false,
            accepted_host: String::new(),
            accepted_port: 0,
            version: "riperf3 0.5.4".into(),
            system_info: String::new(),
            cpu: CpuUtilization {
                host_total: 1.0,
                host_user: 0.5,
                host_system: 0.5,
                remote_total: 2.0,
                remote_user: 1.0,
                remote_system: 1.0,
            },
            congestion_used: Some("cubic".into()),
            cookie: "testcookie000000000000000000000000000".into(),
            tcp_mss_default: 1448,
            mss: None,
            fq_rate: 0,
            sock_bufsize: Some(0),
            sndbuf_actual: Some(16384),
            rcvbuf_actual: Some(87380),
            reached_test_start: true,
            interval: 1.0,
            gso: 0,
            gro: 0,
            start_time_millis: 1_780_107_649_449,
            extra_data: None,
            intervals: vec![],
            streams: vec![],
        }
    }

    /// #238: iperf3's per-stream lost_percent denominator is STRICTLY the
    /// sender-side count; when that count is absent it emits 0.0
    /// (iperf_api.c:4288-4293 else-branch) — NEVER the receiver-measured
    /// count, even though `packets` DOES fall back to it (:4311). Live-proven
    /// on a terminated bidir probe (#233 r4: iperf3 `lost_percent: 0` where
    /// riperf3 said 14.75). The three sender-count-absent arms:
    #[test]
    fn udp_lost_percent_zero_when_sender_count_absent() {
        let blk = 131_072u64;

        // (1) SERVER receiving stream — sender count absent pre-exchange
        // (plain forward-UDP server docs included, not just bidir).
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.is_server = true;
        input.streams = vec![udp_stream(1, false, blk * 96, None, 0.001, 4, 96)];
        let v = serde_json::to_value(input.build()).unwrap();
        let u = &v["end"]["streams"][0]["udp"];
        assert_eq!(
            u["packets"],
            serde_json::json!(96i64),
            "packets keeps the measured fallback (:4311): {u}"
        );
        assert_eq!(
            u["lost_percent"].as_f64(),
            Some(0.0),
            "sender count absent -> 0.0, never the measured pct: {u}"
        );

        // (2) client receiver on a TERMINATED run (#170) — the peer's sent
        // figure never arrived.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.error = Some("error - the server has terminated".into());
        input.streams = vec![udp_stream(
            1,
            false,
            blk * 96,
            Some(blk * 100),
            0.001,
            4,
            96,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        let u = &v["end"]["streams"][0]["udp"];
        assert_eq!(u["packets"], serde_json::json!(96i64));
        assert_eq!(
            u["lost_percent"].as_f64(),
            Some(0.0),
            "the #233 r4 live shape — iperf3 emits 0 on terminated streams: {u}"
        );

        // (3) client receiver whose peer reported ZERO sender bytes — the
        // measured-packets fallback engages for `packets`, but the pct
        // denominator is gone (GT's pct gate at :4288 — the
        // `sender_packet_count - sender_omitted_packet_count > 0` test —
        // fails on zero).
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.streams = vec![udp_stream(1, false, blk * 96, Some(0), 0.001, 4, 96)];
        let v = serde_json::to_value(input.build()).unwrap();
        let u = &v["end"]["streams"][0]["udp"];
        assert_eq!(u["packets"], serde_json::json!(96i64));
        assert_eq!(u["lost_percent"].as_f64(), Some(0.0), "{u}");
    }

    /// #239: a client SENDER stream's packets/lost_percent use the LOCAL
    /// sent count (sender_packet_count = sp->packet_count,
    /// iperf_api.c:4220-4221), not the peer-measured #182 figure — they
    /// diverge under forward trailing loss (the receiver's highest-seq view
    /// trails what was sent). The peer-measured jitter/lost VALUES stay on
    /// the entry; only the packets/pct provenance changes. Exact-count
    /// (tail partial datagrams) is #235's residue.
    #[test]
    fn udp_client_sender_packets_use_local_count() {
        let blk = 131_072u64;
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        // Sent 100 blocks; the receiver measured highest-seq 99 with 2 lost.
        input.streams = vec![udp_stream(
            1,
            true,
            blk * 100,
            Some(blk * 99),
            0.0015,
            2,
            99,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        let u = &v["end"]["streams"][0]["udp"];
        assert_eq!(
            u["packets"],
            serde_json::json!(100i64),
            "the LOCAL sent count, not the peer-measured 99: {u}"
        );
        assert!(
            (u["lost_percent"].as_f64().unwrap() - 2.0).abs() < 1e-9,
            "2/100 on the sender denominator (not 2/99): {u}"
        );
        assert!(
            (u["jitter_ms"].as_f64().unwrap() - 1.5).abs() < 1e-9,
            "peer-measured jitter VALUE stays (#182): {u}"
        );
        assert_eq!(
            u["lost_packets"],
            serde_json::json!(2i64),
            "peer-measured loss VALUE stays: {u}"
        );
        assert_eq!(u["bytes"], serde_json::json!(blk * 100));
    }

    fn udp_stream(
        id: i32,
        is_sender: bool,
        local: u64,
        remote: Option<u64>,
        jitter_secs: f64,
        lost: i64,
        packets: i64,
    ) -> StreamReport {
        StreamReport {
            id,
            local_host: "127.0.0.1".into(),
            local_port: 40000 + id as u16,
            remote_host: "127.0.0.1".into(),
            remote_port: 5201,
            is_sender,
            local_bytes: local,
            remote_bytes: remote,
            retransmits: None,
            tcp_end: None,
            remote_packets: None,
            udp: Some(UdpStreamStats {
                jitter_secs,
                lost_packets: lost,
                packets,
                out_of_order: 0,
            }),
        }
    }

    /// #214 (1): a UDP bidir end block carries SIX aggregates, every one
    /// UDP-shaped (packets/lost_packets/lost_percent/jitter_ms) — GT iperf3
    /// 3.21 live-capture 2026-06-11. TCP bidir stays four TCP-shaped
    /// aggregates (no sum/sum_bidir_reverse) — also GT. Pre-fix the bidir
    /// branch preceded the is_udp branch and emitted tcp_sum for all four,
    /// with no sum/sum_bidir_reverse at all.
    #[test]
    fn udp_bidir_end_aggregates_are_udp_shaped() {
        // P=2 per direction with exact-blksize byte counts, so every
        // aggregate VALUE pins (r2 review: the earlier single-stream case
        // let both the packets provenance and the jitter divisor revert
        // unnoticed). blk = 131072 (base_input).
        let blk = 131_072u64;
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.bidir = true;
        input.num_streams = 2;
        input.streams = vec![
            // fwd senders: peer-measured stats ride the sender entries (#182)
            udp_stream(1, true, blk * 100, Some(blk * 99), 0.001, 2, 99),
            udp_stream(2, true, blk * 100, Some(blk * 99), 0.003, 2, 99),
            // reverse receivers: locally measured; the peer SENT blk*100 each
            udp_stream(3, false, blk * 95, Some(blk * 100), 0.002, 4, 96),
            udp_stream(4, false, blk * 95, Some(blk * 100), 0.006, 4, 96),
        ];
        let v = serde_json::to_value(input.build()).unwrap();

        // SENT-side packet provenance (r2 blocker): derived from the
        // sender-side byte figures, NOT the measured receiver counts.
        let end = &v["end"];
        assert_eq!(end["sum"]["packets"], serde_json::json!(200i64));
        assert_eq!(end["sum_sent"]["packets"], serde_json::json!(200i64));
        assert_eq!(
            end["sum_received"]["packets"],
            serde_json::json!(198i64),
            "received side keeps the measured count: {end}"
        );
        assert_eq!(
            end["sum_bidir_reverse"]["packets"],
            serde_json::json!(200i64),
            "reverse sum carries the derived peer-sent count, not the 192 measured: {end}"
        );
        assert_eq!(
            end["sum_sent_bidir_reverse"]["packets"],
            serde_json::json!(200i64)
        );
        assert_eq!(
            end["sum_received_bidir_reverse"]["packets"],
            serde_json::json!(192i64)
        );
        // Jitter: per-direction sums divided by num_streams (=2):
        // fwd (0.001+0.003)/2 = 2ms — fold(max) would say 3ms;
        // rev (0.002+0.006)/2 = 4ms.
        assert!((end["sum_received"]["jitter_ms"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert!(
            (end["sum_received_bidir_reverse"]["jitter_ms"]
                .as_f64()
                .unwrap()
                - 4.0)
                .abs()
                < 1e-9
        );
        // lost_percent denominators are the direction's SENT-side totals
        // (r3 F1/F2): fwd 4/200 = 2.0 (not 4/198), rev 8/200 = 4.0 (not
        // 8/192), per-stream 4/100 = 4.0 (not 4/96).
        assert!((end["sum_received"]["lost_percent"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert!(
            (end["sum_received_bidir_reverse"]["lost_percent"]
                .as_f64()
                .unwrap()
                - 4.0)
                .abs()
                < 1e-9
        );

        // Per-stream receiving entries: sender-side bytes AND derived
        // packets (not the 96 measured).
        let streams = end["streams"].as_array().unwrap();
        let recv = streams
            .iter()
            .find(|s| s["udp"]["sender"] == serde_json::json!(false))
            .unwrap();
        assert_eq!(recv["udp"]["bytes"], serde_json::json!(blk * 100));
        assert_eq!(recv["udp"]["packets"], serde_json::json!(100i64));
        assert!((recv["udp"]["lost_percent"].as_f64().unwrap() - 4.0).abs() < 1e-9);
        for key in [
            "sum",
            "sum_sent",
            "sum_received",
            "sum_bidir_reverse",
            "sum_sent_bidir_reverse",
            "sum_received_bidir_reverse",
        ] {
            let agg = &v["end"][key];
            assert!(agg.is_object(), "missing end.{key}: {v}");
            for f in ["packets", "lost_packets", "lost_percent", "jitter_ms"] {
                assert!(
                    agg.get(f).is_some(),
                    "end.{key} lacks {f} — the tcp_sum leak (#214): {agg}"
                );
            }
        }
        // Sender flags per GT: sum carries the fwd direction (sender=true),
        // sum_bidir_reverse the reverse (sender=false); the *_sent/_received
        // pairs carry their fixed perspectives.
        assert_eq!(v["end"]["sum"]["sender"], serde_json::json!(true));
        assert_eq!(
            v["end"]["sum_bidir_reverse"]["sender"],
            serde_json::json!(false)
        );
        assert_eq!(v["end"]["sum_sent"]["sender"], serde_json::json!(true));
        assert_eq!(
            v["end"]["sum_received_bidir_reverse"]["sender"],
            serde_json::json!(false)
        );

        // The r1-review blocker, value-pinned: sum_bidir_reverse.bytes is
        // the SENDER-side figure (== sum_sent_bidir_reverse.bytes, fed from
        // the peer's exchanged sent count) — NOT local received. Diverges
        // 18% on a lossy run (live-proven against iperf3 3.21).
        let rev_sum = &v["end"]["sum_bidir_reverse"];
        let rev_sent_sum = &v["end"]["sum_sent_bidir_reverse"];
        assert_eq!(
            rev_sum["bytes"], rev_sent_sum["bytes"],
            "iperf3 invariant: sum_bidir_reverse.bytes == sum_sent_bidir_reverse.bytes: {v}"
        );
        assert_eq!(
            rev_sum["bytes"],
            serde_json::json!(131_072u64 * 200),
            "the peer-sent figure (blk*100 per reverse stream), not the \
             blk*95-per-stream local received: {v}"
        );

        // And the TCP-bidir negative: no sum/sum_bidir_reverse there (GT).
        let mut tcp = base_input();
        tcp.bidir = true;
        tcp.streams = vec![
            tcp_stream(1, true, 1_000_000, 990_000),
            tcp_stream(2, false, 980_000, 1_000_000),
        ];
        let tv = serde_json::to_value(tcp.build()).unwrap();
        assert!(
            tv["end"].get("sum").is_none() && tv["end"].get("sum_bidir_reverse").is_none(),
            "TCP bidir emits four aggregates, not six (GT): {tv}"
        );
    }

    /// #214 (2): the -J builder's end-sum UDP jitter is the AVERAGE across
    /// measured streams (iperf3's avg_jitter /= num_streams) — the reporter-
    /// path twin was fixed in #193/#169; this pins the builder's own
    /// aggregation. Pre-fix: fold(max) → 4.0 here.
    #[test]
    fn udp_multistream_end_sum_jitter_is_the_average() {
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.reverse = true; // receiving side measures jitter locally
        input.num_streams = 2;
        input.streams = vec![
            udp_stream(1, false, 1_000_000, Some(1_000_000), 0.002, 0, 100),
            udp_stream(2, false, 1_000_000, Some(1_000_000), 0.004, 0, 100),
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let jitter = v["end"]["sum"]["jitter_ms"].as_f64().expect("jitter_ms");
        assert!(
            (jitter - 3.0).abs() < 1e-9,
            "sum.jitter_ms must average (2ms+4ms)/2=3ms, not max: {jitter}"
        );

        // Divisor discrimination (r2 S1): with only ONE of the two streams
        // carrying measured stats (a #170-style partial), iperf3 still
        // divides by num_streams — 4ms/2 = 2ms; a measured-count divisor
        // would say 4ms.
        let mut partial = base_input();
        partial.protocol = TransportProtocol::Udp;
        partial.reverse = true;
        partial.num_streams = 2;
        partial.streams = vec![
            udp_stream(1, false, 1_000_000, Some(1_000_000), 0.004, 0, 100),
            tcp_stream(2, false, 1_000_000, 1_000_000), // udp: None — unmeasured
        ];
        let pv = serde_json::to_value(partial.build()).unwrap();
        let pj = pv["end"]["sum"]["jitter_ms"].as_f64().expect("jitter_ms");
        assert!(
            (pj - 2.0).abs() < 1e-9,
            "num_streams divisor on a partial direction: 4ms/2=2ms, got {pj}"
        );
    }

    /// #214 (3): a reverse-UDP per-stream entry reports the EXCHANGED
    /// peer-sent byte count, not the local received count — they diverge
    /// under loss (iperf3's stream accounting uses the sender's figure).
    /// Pre-fix: local 900k reported.
    #[test]
    fn reverse_udp_stream_bytes_use_the_exchanged_peer_sent_count() {
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.reverse = true;
        input.streams = vec![udp_stream(
            1,
            false,
            900_000,
            Some(1_000_000),
            0.001,
            10,
            100,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(
            v["end"]["streams"][0]["udp"]["bytes"],
            serde_json::json!(1_000_000u64),
            "stream bytes = peer-sent (exchanged), not local received: {v}"
        );
    }

    #[allow(clippy::too_many_arguments)] // fixture mirror of udp_stream + one
    fn udp_stream_rp(
        id: i32,
        is_sender: bool,
        local: u64,
        remote: Option<u64>,
        remote_packets: Option<i64>,
        jitter_secs: f64,
        lost: i64,
        packets: i64,
    ) -> StreamReport {
        let mut s = udp_stream(id, is_sender, local, remote, jitter_secs, lost, packets);
        s.remote_packets = remote_packets;
        s
    }

    /// #235: where the peer's exchanged SENT count is present, it wins over
    /// the bytes-derived figure — which is off by the tail partial datagram
    /// (GT: sender_packet_count = peer_packet_count for receiving streams,
    /// iperf_api.c:4227, a true counter, never bytes/blksize).
    #[test]
    fn udp_receiving_entries_prefer_the_exchanged_sender_count() {
        let blk = 131_072u64;
        // The peer sent 100 datagrams: 99 full blocks + a 500-byte tail.
        let sent_bytes = blk * 99 + 500;
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.streams = vec![udp_stream_rp(
            1,
            false,
            blk * 96,
            Some(sent_bytes),
            Some(100),
            0.001,
            4,
            96,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        let u = &v["end"]["streams"][0]["udp"];
        assert_eq!(
            u["packets"],
            serde_json::json!(100i64),
            "the exchanged count, not bytes/blk's 99: {u}"
        );
        assert!(
            (u["lost_percent"].as_f64().unwrap() - 4.0).abs() < 1e-9,
            "4/100 on the exchanged denominator: {u}"
        );

        // Absent (a terminated run / an odd peer): the derived figure stays.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.streams = vec![udp_stream_rp(
            1,
            false,
            blk * 96,
            Some(sent_bytes),
            None,
            0.001,
            4,
            96,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(
            v["end"]["streams"][0]["udp"]["packets"],
            serde_json::json!(99i64),
            "bytes-derived fallback when the peer never reported: {v}"
        );
    }

    /// r1 item 5c: the >0 filter is load-bearing — a pre-#184 riperf3
    /// server exchanges packets:0 alongside real bytes, and a hostile peer
    /// can send negatives; both must take the bytes-derived fallback, not
    /// zero (or poison) the entry and the aggregates.
    #[test]
    fn udp_zero_or_negative_exchanged_counts_force_the_fallback() {
        let blk = 131_072u64;
        for bad in [Some(0i64), Some(-3i64)] {
            // Per-stream entry.
            let mut input = base_input();
            input.protocol = TransportProtocol::Udp;
            input.streams = vec![udp_stream_rp(
                1,
                false,
                blk * 96,
                Some(blk * 99),
                bad,
                0.001,
                4,
                96,
            )];
            let v = serde_json::to_value(input.build()).unwrap();
            assert_eq!(
                v["end"]["streams"][0]["udp"]["packets"],
                serde_json::json!(99i64),
                "exchanged {bad:?} must fall back to bytes/blk: {v}"
            );

            // -R aggregates.
            let mut input = base_input();
            input.protocol = TransportProtocol::Udp;
            input.reverse = true;
            input.streams = vec![udp_stream_rp(
                1,
                false,
                blk * 96,
                Some(blk * 99),
                bad,
                0.001,
                4,
                96,
            )];
            let v = serde_json::to_value(input.build()).unwrap();
            assert_eq!(
                v["end"]["sum_sent"]["packets"],
                serde_json::json!(99i64),
                "aggregates must not zero on exchanged {bad:?}: {v}"
            );
        }
    }

    /// r1 item 5b: the all-or-nothing aggregate rule — a MIXED set (one
    /// stream reported, one didn't; unreachable from conforming peers but
    /// the documented contract) falls back wholesale rather than summing a
    /// partial.
    #[test]
    fn udp_mixed_exchanged_sets_fall_back_wholesale() {
        let blk = 131_072u64;
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.reverse = true;
        input.num_streams = 2;
        input.streams = vec![
            udp_stream_rp(1, false, blk * 96, Some(blk * 99), Some(100), 0.001, 4, 96),
            udp_stream_rp(2, false, blk * 96, Some(blk * 99), None, 0.001, 4, 96),
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(
            v["end"]["sum_sent"]["packets"],
            serde_json::json!(198i64),
            "mixed set -> wholesale bytes-derived fallback (99+99), never a \
             partial 100+99 or 100+0 sum: {v}"
        );
    }

    /// #235, the aggregate analog: the reverse-direction sent figures sum
    /// the per-stream exchanged counts (GT's sender_total_packets running
    /// total over peer_packet_count) with the bytes-derived per-stream
    /// fallback.
    #[test]
    fn udp_reverse_aggregates_sum_the_exchanged_counts() {
        let blk = 131_072u64;
        let tail = blk * 99 + 500; // 100 true datagrams, 99 derived

        // Bidir: two receiving streams with exchanged counts.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.bidir = true;
        input.num_streams = 2;
        input.streams = vec![
            udp_stream(1, true, blk * 100, Some(blk * 99), 0.001, 2, 99),
            udp_stream(2, true, blk * 100, Some(blk * 99), 0.003, 2, 99),
            udp_stream_rp(3, false, blk * 95, Some(tail), Some(100), 0.002, 4, 96),
            udp_stream_rp(4, false, blk * 95, Some(tail), Some(100), 0.006, 4, 96),
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let end = &v["end"];
        for key in ["sum_bidir_reverse", "sum_sent_bidir_reverse"] {
            assert_eq!(
                end[key]["packets"],
                serde_json::json!(200i64),
                "{key} sums the exchanged 100s, not the derived 99s: {end}"
            );
        }

        // Single-direction -R: same rule on sum/sum_sent.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.reverse = true;
        input.streams = vec![udp_stream_rp(
            1,
            false,
            blk * 96,
            Some(tail),
            Some(100),
            0.001,
            4,
            96,
        )];
        let v = serde_json::to_value(input.build()).unwrap();
        for key in ["sum", "sum_sent"] {
            assert_eq!(
                v["end"][key]["packets"],
                serde_json::json!(100i64),
                "-R {key} carries the exchanged count: {v}"
            );
        }
    }

    fn tcp_stream(id: i32, is_sender: bool, local: u64, remote: u64) -> StreamReport {
        tcp_stream_retr(id, is_sender, local, remote, Some(3))
    }

    fn tcp_stream_retr(
        id: i32,
        is_sender: bool,
        local: u64,
        remote: u64,
        retransmits: Option<i64>,
    ) -> StreamReport {
        StreamReport {
            id,
            local_host: "127.0.0.1".into(),
            local_port: 40000 + id as u16,
            remote_host: "127.0.0.1".into(),
            remote_port: 5201,
            is_sender,
            local_bytes: local,
            remote_bytes: Some(remote),
            retransmits,
            tcp_end: None,
            remote_packets: None,
            udp: None,
        }
    }

    /// #236: in TCP bidir, GT's results loop runs once per direction with a
    /// PER-PASS total_retransmits (iperf_api.c:4138 reset, :4235 accumulate)
    /// — `sum_sent` carries the local senders' total; `sum_sent_bidir_
    /// reverse` carries the peer's exchanged per-stream counts riding the
    /// receiving streams (live-observed: 2, the #233 r1 capture). riperf3
    /// passed None on the reverse-sent aggregate and, with receiving streams
    /// now carrying the exchanged figure, an unfiltered sum would mix both
    /// directions into the forward aggregate.
    #[test]
    fn tcp_bidir_reverse_sent_carries_peer_retransmits() {
        let mut input = base_input();
        input.bidir = true;
        input.num_streams = 1;
        input.streams = vec![
            // local sender: 3 local retransmits (forward direction)
            tcp_stream_retr(1, true, 131_072, 131_072, Some(3)),
            // receiving stream: the peer exchanged 2 for its send side
            tcp_stream_retr(2, false, 131_072, 131_072, Some(2)),
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let end = &v["end"];
        assert_eq!(
            end["sum_sent"]["retransmits"],
            serde_json::json!(3i64),
            "forward = LOCAL senders only, never mixed with the peer's: {end}"
        );
        assert_eq!(
            end["sum_sent_bidir_reverse"]["retransmits"],
            serde_json::json!(2i64),
            "the peer's exchanged reverse count (#236): {end}"
        );
        for key in ["sum_received", "sum_received_bidir_reverse"] {
            assert!(
                end[key].get("retransmits").is_none_or(|r| r.is_null()),
                "received aggregates carry no retransmits (GT): {end}"
            );
        }
    }

    /// The server-role twin (a green pin guarding the direction-filter
    /// refactor): the server's reverse channel is what IT sends — local
    /// retransmits — and the forward pair carries none. NOT because the
    /// client withholds them (the exchange is symmetric, iperf_api.c:2764/
    /// 2944): GT's server PRINTS before it exchanges (reporter_callback at
    /// iperf_server_api.c:277 vs iperf_exchange_results at :280), so the
    /// peer's figures never reach its report (r1 item 4).
    #[test]
    fn tcp_bidir_server_reverse_sent_uses_local_retransmits() {
        let mut input = base_input();
        input.bidir = true;
        input.is_server = true;
        input.num_streams = 1;
        input.streams = vec![
            tcp_stream_retr(1, true, 131_072, 0, Some(4)),
            tcp_stream_retr(2, false, 131_072, 0, None),
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let end = &v["end"];
        assert_eq!(
            end["sum_sent_bidir_reverse"]["retransmits"],
            serde_json::json!(4i64),
            "the server's reverse = its own send path: {end}"
        );
        assert!(
            end["sum_sent"]
                .get("retransmits")
                .is_none_or(|r| r.is_null()),
            "the fwd (client->server) sent aggregate has no figure server-side: {end}"
        );
    }

    /// #170 r3: the terminated-run shapes only existed in live matrices —
    /// these pins keep a json_report refactor from silently regrowing the
    /// fabrication family (it escaped two cold rounds for exactly this lack).
    #[test]
    fn terminated_bidir_sums_zero_the_absent_peer_halves() {
        let mut input = base_input();
        input.bidir = true;
        input.error = Some("the server has terminated".into());
        let mut fwd = tcp_stream(1, true, 1_000_000, 0);
        fwd.remote_bytes = None;
        fwd.retransmits = None;
        let mut rev = tcp_stream(3, false, 500_000, 0);
        rev.remote_bytes = None;
        rev.retransmits = None;
        input.streams = vec![fwd, rev];
        let v = serde_json::to_value(input.build()).unwrap();
        // iperf3 (live-captured at #170 r2): locals kept, peer halves 0.
        assert_eq!(v["end"]["sum_sent"]["bytes"], 1_000_000);
        assert_eq!(v["end"]["sum_received"]["bytes"], 0);
        assert_eq!(v["end"]["sum_sent_bidir_reverse"]["bytes"], 0);
        assert_eq!(v["end"]["sum_received_bidir_reverse"]["bytes"], 500_000);
    }

    #[test]
    fn terminated_reverse_udp_keeps_measured_packets_with_zero_bytes() {
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.reverse = true;
        input.blksize = 1460;
        input.error = Some("the server has terminated".into());
        let mut st = tcp_stream(1, false, 360_151, 0);
        st.remote_bytes = None;
        st.retransmits = None;
        st.udp = Some(UdpStreamStats {
            jitter_secs: 0.0001,
            lost_packets: 1,
            packets: 13,
            out_of_order: 0,
        });
        input.streams = vec![st];
        let v = serde_json::to_value(input.build()).unwrap();
        // iperf3 (live-captured at #170 r2/r3): the sender-side bytes the
        // dead peer never reported are 0; the locally measured packets stay;
        // sum.packets falls back to the receiver count (iperf_api.c:4242).
        assert_eq!(v["end"]["streams"][0]["udp"]["bytes"], 0);
        assert_eq!(v["end"]["streams"][0]["udp"]["packets"], 13);
        assert_eq!(v["end"]["sum"]["bytes"], 0);
        assert_eq!(v["end"]["sum"]["packets"], 13);
    }

    /// The error=None peer-absent graft (legacy-peer tolerance) is unchanged:
    /// locks the normal-path equivalence r3 verified by inspection.
    #[test]
    fn peer_absent_without_error_still_grafts() {
        let mut input = base_input();
        let mut st = tcp_stream(1, true, 1_000_000, 0);
        st.remote_bytes = None;
        input.streams = vec![st];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(v["end"]["streams"][0]["receiver"]["bytes"], 1_000_000);
        assert_eq!(v["end"]["sum_received"]["bytes"], 1_000_000);
    }

    #[test]
    fn tcp_forward_end_streams_are_nested_sender_receiver() {
        let mut input = base_input();
        input.streams = vec![tcp_stream(1, true, 1_000_000, 999_000)];
        let v = serde_json::to_value(input.build()).unwrap();

        // Nested, not flat: end.streams[0] has `sender` and `receiver`, no `udp`.
        let s0 = &v["end"]["streams"][0];
        assert!(s0["sender"].is_object(), "expected nested sender: {s0}");
        assert!(s0["receiver"].is_object(), "expected nested receiver: {s0}");
        assert!(s0.get("udp").is_none());
        assert_eq!(s0["sender"]["bytes"], 1_000_000);
        assert_eq!(s0["receiver"]["bytes"], 999_000);
        assert_eq!(s0["sender"]["retransmits"], 3);
        // Receiver side must not carry retransmits.
        assert!(s0["receiver"].get("retransmits").is_none());
        // tcp_congestion present for TCP.
        assert_eq!(v["end"]["sender_tcp_congestion"], "cubic");
        assert_eq!(v["end"]["receiver_tcp_congestion"], "cubic");
        // No UDP `sum` for TCP.
        assert!(v["end"].get("sum").is_none());
    }

    #[test]
    fn real_addresses_in_connected() {
        let mut input = base_input();
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        let c0 = &v["start"]["connected"][0];
        assert_eq!(c0["local_host"], "127.0.0.1");
        assert_eq!(c0["local_port"], 40001);
        assert_eq!(c0["remote_port"], 5201);
        // Not the fabricated connecting host/port duplicated into both ends.
        assert_ne!(c0["local_port"], c0["remote_port"]);
    }

    #[test]
    fn udp_forward_emits_udp_object_and_sum() {
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.blksize = 1460;
        // Forward UDP: local stream is the sender; loss measured by the peer
        // (receiver) is attached to this stream's udp stats (#25).
        input.streams = vec![StreamReport {
            udp: Some(UdpStreamStats {
                jitter_secs: 0.00003,
                lost_packets: 5,
                packets: 1000,
                out_of_order: 0,
            }),
            ..tcp_stream(1, true, 1_460_000, 1_452_700)
        }];
        let v = serde_json::to_value(input.build()).unwrap();

        let s0 = &v["end"]["streams"][0];
        assert!(s0["udp"].is_object(), "expected udp object: {s0}");
        assert!(s0.get("sender").is_none());
        assert_eq!(s0["udp"]["lost_packets"], 5);
        assert_eq!(s0["udp"]["packets"], 1000);
        assert!((s0["udp"]["jitter_ms"].as_f64().unwrap() - 0.03).abs() < 1e-9);

        // UDP emits a single-direction `sum`, and the receiver aggregate carries loss.
        assert!(v["end"]["sum"].is_object(), "UDP must emit end.sum");
        assert_eq!(v["end"]["sum"]["lost_packets"], 5);
        assert_eq!(v["end"]["sum_received"]["lost_packets"], 5);
        // No tcp_congestion for UDP.
        assert!(v["end"].get("sender_tcp_congestion").is_none());
    }

    #[test]
    fn reverse_marks_sides_as_receiver() {
        let mut input = base_input();
        input.reverse = true;
        // Reverse: the local stream is the receiver; peer is the sender.
        input.streams = vec![tcp_stream(1, false, 2_000_000, 2_000_000)];
        let v = serde_json::to_value(input.build()).unwrap();
        let s0 = &v["end"]["streams"][0];
        // Still nested sender+receiver, with sender=false (this host received).
        assert!(s0["sender"].is_object());
        assert!(s0["receiver"].is_object());
        assert_eq!(s0["sender"]["sender"], false);
    }

    #[test]
    fn top_level_shape_matches_iperf3() {
        let mut input = base_input();
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        // iperf3's three top-level keys, in a stable set.
        assert!(v["start"].is_object());
        assert!(v["intervals"].is_array());
        assert!(v["end"].is_object());
        for k in [
            "connected",
            "version",
            "system_info",
            "timestamp",
            "connecting_to",
            "cookie",
            "tcp_mss_default",
            "target_bitrate",
            "fq_rate",
            "sock_bufsize",
            "sndbuf_actual",
            "rcvbuf_actual",
            "test_start",
        ] {
            assert!(v["start"].get(k).is_some(), "start.{k} missing");
        }
        for k in [
            "protocol",
            "num_streams",
            "blksize",
            "omit",
            "duration",
            "bytes",
            "blocks",
            "reverse",
            "tos",
            "target_bitrate",
            "bidir",
            "fqrate",
            "interval",
            "gso",
            "gro",
        ] {
            assert!(
                v["start"]["test_start"].get(k).is_some(),
                "start.test_start.{k} missing"
            );
        }
        for k in [
            "streams",
            "sum_sent",
            "sum_received",
            "cpu_utilization_percent",
        ] {
            assert!(v["end"].get(k).is_some(), "end.{k} missing");
        }
    }

    /// #261: a run that reached TestStart (`reached_test_start = true`) carries
    /// the full late shape — the four `start` late fields AND the `end` summary
    /// aggregates are all present (`Some`). This is the SUCCESS-path invariant:
    /// the Option-ification must NEVER drop a field from a real run.
    #[test]
    fn success_run_keeps_all_late_fields() {
        let mut input = base_input();
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        assert!(input.reached_test_start, "base_input models a real run");
        let report = input.build();
        // start late fields: typed Option, all Some.
        assert!(report.start.sock_bufsize.is_some(), "sock_bufsize");
        assert!(report.start.sndbuf_actual.is_some(), "sndbuf_actual");
        assert!(report.start.rcvbuf_actual.is_some(), "rcvbuf_actual");
        assert!(report.start.test_start.is_some(), "test_start");
        // end summaries: typed Option, all Some.
        assert!(report.end.sum_sent.is_some(), "sum_sent");
        assert!(report.end.sum_received.is_some(), "sum_received");
        assert!(
            report.end.cpu_utilization_percent.is_some(),
            "cpu_utilization_percent"
        );
        assert!(!report.end.streams.is_empty(), "streams");
        // And the serialized shape still emits every late key.
        let v = serde_json::to_value(&report).unwrap();
        for k in [
            "sock_bufsize",
            "sndbuf_actual",
            "rcvbuf_actual",
            "test_start",
        ] {
            assert!(v["start"].get(k).is_some(), "serialized start.{k} dropped");
        }
        for k in [
            "streams",
            "sum_sent",
            "sum_received",
            "cpu_utilization_percent",
        ] {
            assert!(v["end"].get(k).is_some(), "serialized end.{k} dropped");
        }
    }

    /// #261: the GT-faithful upfront-REFUSAL shape. When the test never reached
    /// TestStart (`reached_test_start = false`, as on a code-37 server rejection
    /// that arrives before stream setup), GT OMITS the late `start` fields and
    /// emits a bare `end: {}` — but still carries the early start metadata
    /// (timestamp/cookie/connecting_to) and the bare error message. Pinned
    /// against the live GT capture (iperf 3.21 @ d39cf41).
    #[test]
    fn refusal_omits_late_fields_and_empty_end() {
        let mut input = base_input();
        // The refusal path: error set BEFORE TestStart, no streams, the late
        // buffer inputs are None, and a REAL connect-time wall-clock (not 0).
        input.error =
            Some("client's requested duration exceeds the server's maximum permitted limit".into());
        input.reached_test_start = false;
        input.streams = vec![];
        input.sock_bufsize = None;
        input.sndbuf_actual = None;
        input.rcvbuf_actual = None;
        // No data streams were ever created, so the client never read back a
        // congestion algorithm — the *_tcp_congestion keys are naturally absent
        // on the real refusal path (driven by stream presence, not the gate).
        input.congestion_used = None;
        // base_input's start_time_millis stands in for the connect-time stamp the
        // client passes on this path (non-zero — never epoch-0).
        let v = serde_json::to_value(input.build()).unwrap();

        // start: the EARLY metadata survives …
        let start = v["start"].as_object().expect("start object");
        for present in [
            "connected",
            "version",
            "system_info",
            "timestamp",
            "connecting_to",
        ] {
            assert!(
                start.contains_key(present),
                "refusal start keeps {present}: {v}"
            );
        }
        // … but the four late fields are OMITTED (GT's refusal shape).
        for absent in [
            "sock_bufsize",
            "sndbuf_actual",
            "rcvbuf_actual",
            "test_start",
        ] {
            assert!(
                !start.contains_key(absent),
                "refusal start must omit {absent}: {v}"
            );
        }
        // timestamp carries a REAL wall-clock — never epoch-0.
        assert_ne!(v["start"]["timestamp"]["timesecs"], serde_json::json!(0));
        assert_ne!(
            v["start"]["timestamp"]["timemillisecs"],
            serde_json::json!(0)
        );
        assert_ne!(
            v["start"]["timestamp"]["time"],
            serde_json::json!("Thu, 01 Jan 1970 00:00:00 GMT"),
            "refusal timestamp must not be the epoch: {v}"
        );

        // end is a bare, EMPTY object — no streams/sum_sent/sum_received/cpu keys.
        assert_eq!(
            v["end"].as_object().map(serde_json::Map::len),
            Some(0),
            "refusal end must serialize as the GT bare `end: {{}}`: {v}"
        );

        // intervals stays an empty array (the test never ran).
        assert_eq!(v["intervals"].as_array().map(Vec::len), Some(0));

        // exactly ONE `error` key holding the bare message (NOT GT's duplicate;
        // esnet/iperf#2051) — verified at the raw-bytes level since serde_json's
        // Value would silently de-duplicate object keys.
        let raw = serde_json::to_string(&input.build()).unwrap();
        assert_eq!(
            raw.matches("\"error\"").count(),
            1,
            "exactly one error key (single clean key, not GT's #2051 duplicate): {raw}"
        );
        assert_eq!(
            v["error"].as_str(),
            Some("client's requested duration exceeds the server's maximum permitted limit"),
            "the bare strerror, what a last-wins parser of GT's doc resolves to: {v}"
        );
    }

    #[test]
    fn start_metadata_values_match_input() {
        let mut input = base_input();
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        let start = &v["start"];
        // timestamp: timesecs = millis / 1000, timemillisecs verbatim, and the
        // RFC 1123 GMT string derived from timesecs.
        let ts = &start["timestamp"];
        assert_eq!(ts["timemillisecs"], 1_780_107_649_449u64);
        assert_eq!(ts["timesecs"], 1_780_107_649u64);
        assert_eq!(ts["time"], "Sat, 30 May 2026 02:20:49 GMT");
        // pass-through metadata. TCP without -M: tcp_mss_default present, tcp_mss
        // absent (iperf3 emits exactly one).
        assert_eq!(start["cookie"], "testcookie000000000000000000000000000");
        assert_eq!(start["tcp_mss_default"], 1448);
        assert!(
            start.get("tcp_mss").is_none(),
            "tcp_mss must be absent: {start}"
        );
        assert_eq!(start["sndbuf_actual"], 16384);
        assert_eq!(start["rcvbuf_actual"], 87380);
        // test_start additions.
        let test_start = &start["test_start"];
        assert_eq!(test_start["fqrate"], 0);
        assert_eq!(test_start["interval"], 1.0);
        assert_eq!(test_start["gso"], 0);
        assert_eq!(test_start["gro"], 0);
    }

    #[test]
    fn byte_block_limited_zeroes_test_start_duration() {
        // #114: iperf3 reports test_start.duration=0 for byte/block-limited
        // (-n/-k) runs — the -t window doesn't apply; the limit lives in
        // bytes/blocks. A plain -t run keeps the nominal duration.
        let mk = || {
            let mut i = base_input();
            i.streams = vec![tcp_stream(1, true, 10, 10)];
            i
        };

        // -n (byte-limited): duration zeroed, bytes carry the limit.
        let mut n = mk();
        n.bytes = 50 * 1024 * 1024;
        let v = serde_json::to_value(n.build()).unwrap();
        assert_eq!(v["start"]["test_start"]["duration"], 0);
        assert_eq!(v["start"]["test_start"]["bytes"], 50 * 1024 * 1024);

        // -k (block-limited): duration zeroed.
        let mut k = mk();
        k.blocks = 1000;
        let v = serde_json::to_value(k.build()).unwrap();
        assert_eq!(v["start"]["test_start"]["duration"], 0);

        // Control: a plain -t run keeps the nominal -t (base_input duration = 10).
        let v = serde_json::to_value(mk().build()).unwrap();
        assert_eq!(v["start"]["test_start"]["duration"], 10);
    }

    #[test]
    fn udp_start_omits_tcp_mss_keys() {
        // iperf3 (iperf_api.c:1021) gates the MSS key on SOCK_STREAM; a UDP test
        // emits neither tcp_mss nor tcp_mss_default.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.mss = Some(1400); // even with -M, UDP must not emit either key
        input.streams = vec![tcp_stream(1, false, 1000, 1000)];
        let v = serde_json::to_value(input.build()).unwrap();
        let start = &v["start"];
        assert!(start.get("tcp_mss").is_none(), "{start}");
        assert!(start.get("tcp_mss_default").is_none(), "{start}");
    }

    #[test]
    fn set_mss_emits_tcp_mss_and_suppresses_default() {
        // TCP with -M/--set-mss: iperf3 emits tcp_mss = <value> and omits
        // tcp_mss_default (the two are mutually exclusive).
        let mut input = base_input();
        input.mss = Some(1400);
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        let start = &v["start"];
        assert_eq!(start["tcp_mss"], 1400);
        assert!(
            start.get("tcp_mss_default").is_none(),
            "tcp_mss_default must be suppressed under -M: {start}"
        );
    }

    // ---- server-perspective JSON (#50) --------------------------------------

    #[test]
    fn server_emits_accepted_connection_not_connecting_to() {
        let mut input = base_input();
        input.is_server = true;
        input.accepted_host = "10.0.0.5".into();
        input.accepted_port = 41810;
        input.streams = vec![tcp_stream(1, false, 1000, 0)];
        let v = serde_json::to_value(input.build()).unwrap();
        let start = &v["start"];
        assert!(
            start.get("connecting_to").is_none(),
            "server must not emit connecting_to: {start}"
        );
        assert_eq!(start["accepted_connection"]["host"], "10.0.0.5");
        assert_eq!(start["accepted_connection"]["port"], 41810);
    }

    #[test]
    fn client_emits_connecting_to_not_accepted_connection() {
        let mut input = base_input(); // is_server = false
        input.streams = vec![tcp_stream(1, true, 1000, 1000)];
        let v = serde_json::to_value(input.build()).unwrap();
        let start = &v["start"];
        assert!(start["connecting_to"].is_object());
        assert!(
            start.get("accepted_connection").is_none(),
            "client must not emit accepted_connection: {start}"
        );
    }

    #[test]
    fn server_forward_tcp_zeroes_sent_side() {
        // Forward: the server is the receiver. It measured the received bytes; it
        // sent nothing, and never grafts the client's count. Both aggregates carry
        // sender=false (the server is not the sender in forward).
        let mut input = base_input();
        input.is_server = true;
        input.streams = vec![tcp_stream(1, false, 1_000_000, 7_777)]; // remote ignored
        let v = serde_json::to_value(input.build()).unwrap();
        let e = &v["end"];
        assert_eq!(e["sum_sent"]["bytes"], 0);
        assert_eq!(e["sum_received"]["bytes"], 1_000_000);
        assert_eq!(e["sum_sent"]["sender"], false);
        assert_eq!(e["sum_received"]["sender"], false);
        assert_eq!(e["streams"][0]["sender"]["bytes"], 0);
        assert_eq!(e["streams"][0]["receiver"]["bytes"], 1_000_000);
    }

    #[test]
    fn server_reverse_tcp_zeroes_received_side() {
        // Reverse: the server is the sender. sum_received is 0; both aggregates
        // carry sender=true; retransmits live on the sent side.
        let mut input = base_input();
        input.is_server = true;
        input.reverse = true;
        let mut s = tcp_stream(1, true, 2_000_000, 9_999);
        s.retransmits = Some(5);
        input.streams = vec![s];
        let v = serde_json::to_value(input.build()).unwrap();
        let e = &v["end"];
        assert_eq!(e["sum_sent"]["bytes"], 2_000_000);
        assert_eq!(e["sum_received"]["bytes"], 0);
        assert_eq!(e["sum_sent"]["sender"], true);
        assert_eq!(e["sum_received"]["sender"], true);
        assert_eq!(e["sum_sent"]["retransmits"], 5);
        assert_eq!(e["streams"][0]["sender"]["bytes"], 2_000_000);
        assert_eq!(e["streams"][0]["receiver"]["bytes"], 0);
    }

    #[test]
    fn server_forward_congestion_receiver_only() {
        // base_input has congestion_used = Some("cubic"). Forward server → the
        // local algorithm appears on the receiver side only; sender side absent.
        let mut input = base_input();
        input.is_server = true;
        input.streams = vec![tcp_stream(1, false, 1000, 0)];
        let v = serde_json::to_value(input.build()).unwrap();
        let e = &v["end"];
        assert_eq!(e["receiver_tcp_congestion"], "cubic");
        assert!(
            e.get("sender_tcp_congestion").is_none(),
            "forward server: sender congestion must be absent: {e}"
        );
    }

    #[test]
    fn server_reverse_congestion_sender_only() {
        let mut input = base_input();
        input.is_server = true;
        input.reverse = true;
        input.streams = vec![tcp_stream(1, true, 1000, 0)];
        let v = serde_json::to_value(input.build()).unwrap();
        let e = &v["end"];
        assert_eq!(e["sender_tcp_congestion"], "cubic");
        assert!(e.get("receiver_tcp_congestion").is_none(), "{e}");
    }

    #[test]
    fn server_bidir_congestion_sender_only_and_directions_split() {
        // Bidir server: congestion on the sender side only (verified vs iperf3
        // 3.21). Forward flow (received) → sum_sent/sum_received with sender=false;
        // reverse flow (sent) → *_bidir_reverse with sender=true.
        let mut input = base_input();
        input.is_server = true;
        input.bidir = true;
        input.streams = vec![
            tcp_stream(1, false, 1_000_000, 0), // forward: server receives
            tcp_stream(3, true, 2_000_000, 0),  // reverse: server sends
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let e = &v["end"];
        assert_eq!(e["sender_tcp_congestion"], "cubic");
        assert!(e.get("receiver_tcp_congestion").is_none(), "{e}");
        assert_eq!(e["sum_sent"]["bytes"], 0);
        assert_eq!(e["sum_received"]["bytes"], 1_000_000);
        assert_eq!(e["sum_sent"]["sender"], false);
        assert_eq!(e["sum_sent_bidir_reverse"]["bytes"], 2_000_000);
        assert_eq!(e["sum_received_bidir_reverse"]["bytes"], 0);
        assert_eq!(e["sum_sent_bidir_reverse"]["sender"], true);
    }

    #[test]
    fn server_forward_sender_omits_tcp_info_keys() {
        // iperf3's server emits the sender sub-object's TCP_INFO keys only for a
        // stream it sent. On a forward test (server receives) the sender block is
        // bytes-only — no retransmits / max_snd_cwnd / *_rtt / reorder.
        let mut input = base_input();
        input.is_server = true;
        input.streams = vec![tcp_stream(1, false, 1_000_000, 0)];
        let v = serde_json::to_value(input.build()).unwrap();
        let sender = &v["end"]["streams"][0]["sender"];
        for k in [
            "retransmits",
            "max_snd_cwnd",
            "max_snd_wnd",
            "max_rtt",
            "min_rtt",
            "mean_rtt",
            "reorder",
        ] {
            assert!(
                sender.get(k).is_none(),
                "forward server sender must omit {k}: {sender}"
            );
        }
        // The bytes-only fields remain.
        assert!(sender["bytes"].is_number());
        assert!(sender["bits_per_second"].is_number());
    }

    #[test]
    fn server_reverse_sender_emits_tcp_info_keys() {
        // On a reverse test the server sends, so its sender block carries the
        // TCP_INFO extras (real cwnd/rtt/snd_wnd, like iperf3 — #161).
        let mut input = base_input();
        input.is_server = true;
        input.reverse = true;
        input.streams = vec![StreamReport {
            tcp_end: Some(TcpEndExtras {
                max_snd_cwnd: 65535,
                max_snd_wnd: 1_500_000,
                max_rtt: 200,
                min_rtt: 90,
                mean_rtt: 120,
                reorder: 0,
            }),
            ..tcp_stream(1, true, 2_000_000, 0)
        }];
        let v = serde_json::to_value(input.build()).unwrap();
        let sender = &v["end"]["streams"][0]["sender"];
        assert_eq!(sender["max_snd_cwnd"], 65535);
        assert_eq!(sender["max_rtt"], 200);
        assert_eq!(sender["max_snd_wnd"], 1_500_000); // real value forwarded (#161)
        assert!(sender["retransmits"].is_number());
    }

    #[test]
    fn server_udp_forward_stream_reports_zero_bytes_measured_packets() {
        // Forward UDP: the server received the datagrams. iperf3's per-stream udp
        // `bytes` is a sender-side count the server doesn't know → 0, while the
        // measured packet/loss/jitter it observed are reported.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.blksize = 1460;
        input.is_server = true;
        input.streams = vec![StreamReport {
            udp: Some(UdpStreamStats {
                jitter_secs: 0.00002,
                lost_packets: 3,
                packets: 700,
                out_of_order: 0,
            }),
            ..tcp_stream(1, false, 1_022_000, 0)
        }];
        let v = serde_json::to_value(input.build()).unwrap();
        let udp = &v["end"]["streams"][0]["udp"];
        assert_eq!(
            udp["bytes"], 0,
            "server received → sender-side bytes 0: {udp}"
        );
        assert_eq!(udp["packets"], 700, "measured received packets reported");
        assert_eq!(udp["lost_packets"], 3);
        assert_eq!(udp["sender"], false);
    }

    #[test]
    fn server_udp_reverse_stream_derives_sent_packets() {
        // Reverse UDP: the server sent the datagrams; it has no receiver stats, so
        // the sent packet count is derived from the bytes pushed (bytes / blksize).
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.blksize = 1000;
        input.reverse = true;
        input.is_server = true;
        // 20_000 bytes / 1000 blksize = 20 packets, no udp_recv_stats (sender).
        input.streams = vec![tcp_stream(1, true, 20_000, 0)];
        let v = serde_json::to_value(input.build()).unwrap();
        let udp = &v["end"]["streams"][0]["udp"];
        assert_eq!(udp["bytes"], 20_000);
        assert_eq!(udp["packets"], 20, "sent packets = bytes / blksize: {udp}");
        assert_eq!(udp["sender"], true);
    }

    #[test]
    fn extra_data_emitted_at_top_level_when_set() {
        // iperf3 emits --extra-data as a top-level key (after `end`), only when
        // given — on both client and server (#35).
        let mut input = base_input();
        input.extra_data = Some("payload-tag-42".into());
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(v["extra_data"], "payload-tag-42");
        // Not nested in start.
        assert!(v["start"].get("extra_data").is_none());
    }

    #[test]
    fn extra_data_absent_when_unset() {
        let mut input = base_input(); // extra_data: None
        input.streams = vec![tcp_stream(1, true, 10, 10)];
        let v = serde_json::to_value(input.build()).unwrap();
        assert!(
            v.get("extra_data").is_none(),
            "extra_data must be absent when unset: {v}"
        );
    }

    #[test]
    fn http_date_matches_rfc1123_gmt() {
        // Reference values cross-checked against `date -u -d @<epoch>`.
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_780_107_649), "Sat, 30 May 2026 02:20:49 GMT");
        // Leap-year boundary: 2000-02-29 (a leap day) must format as Feb 29.
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    // ---- cold-review round 1 regressions ------------------------------------

    #[test]
    fn multi_stream_retransmits_collapses_sentinel() {
        // -1 is the per-stream "unavailable" sentinel; the SUM must stay -1, not
        // sum to -N (iperf3 never emits below -1).
        let mut input = base_input();
        let mut a = tcp_stream(1, true, 1000, 1000);
        a.retransmits = Some(-1);
        let mut b = tcp_stream(3, true, 1000, 1000);
        b.retransmits = Some(-1);
        input.streams = vec![a, b];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(v["end"]["sum_sent"]["retransmits"], -1, "{v}");
    }

    #[test]
    fn tcp_reverse_aggregate_sender_flags_false() {
        // iperf3 TCP reverse: both aggregates carry sender=false.
        let mut input = base_input();
        input.reverse = true;
        input.streams = vec![tcp_stream(1, false, 2_000_000, 2_000_000)];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(v["end"]["sum_sent"]["sender"], false);
        assert_eq!(v["end"]["sum_received"]["sender"], false);
    }

    #[test]
    fn udp_aggregate_sender_flags_match_iperf3() {
        // iperf3 UDP forward: sum_sent.sender=1, sum_received.sender=0, sum.sender=1.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.blksize = 1460;
        input.streams = vec![StreamReport {
            udp: Some(UdpStreamStats {
                jitter_secs: 0.0,
                lost_packets: 0,
                packets: 100,
                out_of_order: 0,
            }),
            ..tcp_stream(1, true, 146_000, 146_000)
        }];
        let v = serde_json::to_value(input.build()).unwrap();
        assert_eq!(v["end"]["sum_sent"]["sender"], true);
        assert_eq!(v["end"]["sum_received"]["sender"], false);
        assert_eq!(v["end"]["sum"]["sender"], true);
        // sum.bytes is the sent count; sum_sent carries packets + zero loss.
        assert_eq!(v["end"]["sum"]["bytes"], 146_000);
        assert_eq!(v["end"]["sum_sent"]["packets"], 100); // 146000 / 1460
        assert_eq!(v["end"]["sum_sent"]["lost_packets"], 0);
    }

    #[test]
    fn udp_stream_without_stats_still_emits_udp_object() {
        // Shape follows the protocol, not stats presence: a UDP stream missing its
        // datagram stats must NOT fall back to a TCP {sender,receiver} body.
        let mut input = base_input();
        input.protocol = TransportProtocol::Udp;
        input.streams = vec![StreamReport {
            udp: None,
            ..tcp_stream(1, true, 146_000, 146_000)
        }];
        let v = serde_json::to_value(input.build()).unwrap();
        let s0 = &v["end"]["streams"][0];
        assert!(s0["udp"].is_object(), "must be a udp object: {s0}");
        assert!(s0.get("sender").is_none(), "must not be a TCP body: {s0}");
        assert_eq!(s0["udp"]["lost_packets"], 0);
    }

    #[test]
    fn bidir_emits_four_aggregates_with_correct_directions() {
        // Bidir: forward in sum_sent/sum_received, reverse in *_bidir_reverse;
        // per-stream sender flags follow each stream's direction, not !reverse.
        let mut input = base_input();
        input.bidir = true;
        input.num_streams = 1;
        input.streams = vec![
            tcp_stream(1, true, 1_000_000, 990_000),    // forward
            tcp_stream(3, false, 2_000_000, 2_000_000), // reverse
        ];
        let v = serde_json::to_value(input.build()).unwrap();
        let end = &v["end"];
        // All four aggregates present.
        for k in [
            "sum_sent",
            "sum_received",
            "sum_sent_bidir_reverse",
            "sum_received_bidir_reverse",
        ] {
            assert!(end.get(k).is_some(), "bidir must emit {k}: {end}");
        }
        // Forward goes in sum_sent/sum_received (this host sent 1_000_000; peer
        // received 990_000), NOT the reverse stream's 2_000_000.
        assert_eq!(end["sum_sent"]["bytes"], 1_000_000);
        assert_eq!(end["sum_received"]["bytes"], 990_000);
        assert_eq!(end["sum_sent"]["sender"], true);
        // Reverse direction in the bidir-reverse pair, sender=false.
        assert_eq!(end["sum_received_bidir_reverse"]["bytes"], 2_000_000);
        assert_eq!(end["sum_sent_bidir_reverse"]["sender"], false);
        // Per-stream sender flags: forward stream true, reverse stream false.
        let flags: Vec<bool> = v["end"]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["sender"]["sender"].as_bool().unwrap())
            .collect();
        assert_eq!(flags, vec![true, false], "{:?}", v["end"]["streams"]);
    }

    // ---- PR2: intervals + sender extremes -----------------------------------

    #[test]
    fn interval_sum_bidir_reverse_serialized_only_when_present() {
        // #54: bidir intervals carry `sum` + `sum_bidir_reverse` (forward /
        // reverse split); non-bidir intervals must not emit the key at all.
        let sum = IntervalSum {
            start: 0.0,
            end: 1.0,
            seconds: 1.0,
            bytes: 1000,
            bits_per_second: 8000.0,
            retransmits: None,
            jitter_ms: None,
            lost_packets: None,
            packets: None,
            lost_percent: None,
            omitted: false,
            sender: true,
        };
        let bidir = Interval {
            streams: vec![],
            sum: sum.clone(),
            sum_bidir_reverse: Some(IntervalSum {
                bytes: 2000,
                sender: false,
                ..sum.clone()
            }),
        };
        let v = serde_json::to_value(&bidir).unwrap();
        assert_eq!(v["sum"]["sender"], true);
        assert_eq!(v["sum_bidir_reverse"]["bytes"], 2000);
        assert_eq!(v["sum_bidir_reverse"]["sender"], false);
        // iperf3 key order: streams, sum, sum_bidir_reverse.
        let s = serde_json::to_string(&bidir).unwrap();
        let (p_streams, p_sum, p_rev) = (
            s.find("\"streams\"").unwrap(),
            s.find("\"sum\"").unwrap(),
            s.find("\"sum_bidir_reverse\"").unwrap(),
        );
        assert!(p_streams < p_sum && p_sum < p_rev, "key order: {s}");

        let forward = Interval {
            streams: vec![],
            sum,
            sum_bidir_reverse: None,
        };
        let v = serde_json::to_value(&forward).unwrap();
        assert!(
            v.get("sum_bidir_reverse").is_none(),
            "non-bidir interval must omit the key: {v}"
        );
    }

    #[test]
    fn intervals_and_sender_extremes_are_emitted() {
        let mut input = base_input();
        input.intervals = vec![Interval {
            streams: vec![IntervalStream {
                socket: 1,
                start: 0.0,
                end: 1.0,
                seconds: 1.0,
                bytes: 1000,
                bits_per_second: 8000.0,
                retransmits: Some(2),
                snd_cwnd: Some(64000),
                snd_wnd: Some(0),
                rtt: Some(15),
                rttvar: Some(3),
                pmtu: Some(1500),
                reorder: Some(0),
                jitter_ms: None,
                lost_packets: None,
                packets: None,
                lost_percent: None,
                omitted: false,
                sender: true,
            }],
            sum: IntervalSum {
                start: 0.0,
                end: 1.0,
                seconds: 1.0,
                bytes: 1000,
                bits_per_second: 8000.0,
                retransmits: Some(2),
                jitter_ms: None,
                lost_packets: None,
                packets: None,
                lost_percent: None,
                omitted: false,
                sender: true,
            },
            sum_bidir_reverse: None,
        }];
        let mut s = tcp_stream(1, true, 1000, 1000);
        s.retransmits = Some(2);
        s.tcp_end = Some(TcpEndExtras {
            max_snd_cwnd: 64000,
            max_snd_wnd: 0,
            max_rtt: 17,
            min_rtt: 14,
            mean_rtt: 15,
            reorder: 0,
        });
        input.streams = vec![s];
        let v = serde_json::to_value(input.build()).unwrap();

        // intervals populated with TCP per-interval detail.
        assert_eq!(v["intervals"].as_array().unwrap().len(), 1);
        let i0 = &v["intervals"][0]["streams"][0];
        assert_eq!(i0["snd_cwnd"], 64000);
        assert_eq!(i0["rtt"], 15);
        assert_eq!(i0["retransmits"], 2);
        assert_eq!(v["intervals"][0]["sum"]["retransmits"], 2);

        // end.sender carries the accumulated extremes.
        let snd = &v["end"]["streams"][0]["sender"];
        assert_eq!(snd["max_snd_cwnd"], 64000);
        assert_eq!(snd["min_rtt"], 14);
        assert_eq!(snd["max_rtt"], 17);
        assert_eq!(snd["mean_rtt"], 15);
        assert_eq!(snd["reorder"], 0);

        // This fixture feeds snd_wnd / max_snd_wnd as 0 — live runs carry the
        // platform reader's real value since #161.
        assert_eq!(i0["snd_wnd"], 0);
        assert_eq!(snd["max_snd_wnd"], 0);
    }

    #[test]
    fn reverse_sender_block_emits_zeroed_extremes_not_omitted() {
        // iperf3 always emits the sender sub-object's TCP_INFO keys; for a reverse
        // stream (peer is the sender, its TCP_INFO isn't exchanged) they're 0, not
        // absent. A consumer reading e.g. sender.max_snd_cwnd must not hit a gap.
        let mut input = base_input();
        input.reverse = true;
        let mut s = tcp_stream(1, false, 2_000_000, 2_000_000);
        s.tcp_end = None; // reverse: no local sender TCP_INFO
        s.retransmits = Some(0); // the peer's exchanged count of 0 (flag on)
        input.streams = vec![s];
        let snd =
            serde_json::to_value(input.build()).unwrap()["end"]["streams"][0]["sender"].clone();
        for key in [
            "max_snd_cwnd",
            "max_snd_wnd",
            "max_rtt",
            "min_rtt",
            "mean_rtt",
            "reorder",
            "retransmits",
        ] {
            assert_eq!(
                snd[key], 0,
                "reverse sender.{key} must be 0, not absent: {snd}"
            );
        }
    }
}
