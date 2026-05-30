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

use serde::Serialize;

use crate::protocol::TransportProtocol;

// ---------------------------------------------------------------------------
// Top-level report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub start: Start,
    pub intervals: Vec<Interval>,
    pub end: End,
    /// `--extra-data` string, emitted at the top level (after `end`) only when
    /// given — matching iperf3's placement (#35). Present on both client and
    /// server (the server receives it via the parameter exchange).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_data: Option<String>,
}

// ---------------------------------------------------------------------------
// start{}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
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
    pub sock_bufsize: u64,
    pub sndbuf_actual: u64,
    pub rcvbuf_actual: u64,
    pub test_start: TestStart,
}

#[derive(Debug, Clone, Serialize)]
pub struct Timestamp {
    /// RFC 1123 / HTTP-date GMT string, e.g. "Sat, 30 May 2026 02:20:49 GMT".
    pub time: String,
    pub timesecs: u64,
    pub timemillisecs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    pub socket: i32,
    pub local_host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectingTo {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
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
    pub interval: f64,
    pub gso: i32,
    pub gro: i32,
}

// ---------------------------------------------------------------------------
// intervals[] (populated in PR2; the shape is defined now so the model is whole)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Interval {
    pub streams: Vec<IntervalStream>,
    pub sum: IntervalSum,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntervalStream {
    pub socket: i32,
    pub start: f64,
    pub end: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    // TCP per-interval detail (sender side); omitted where TCP_INFO is
    // unavailable. `snd_wnd` is absent — see TcpStreamSide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snd_cwnd: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snd_wnd: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rttvar: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmtu: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reorder: Option<u32>,
    // UDP per-interval detail (receiver side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_percent: Option<f64>,
    pub omitted: bool,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntervalSum {
    pub start: f64,
    pub end: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_percent: Option<f64>,
    pub omitted: bool,
    pub sender: bool,
}

// ---------------------------------------------------------------------------
// end{}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct End {
    pub streams: Vec<EndStream>,
    pub sum_sent: SumSide,
    pub sum_received: SumSide,
    /// UDP only: the single-direction datagram aggregate iperf3 emits as `sum`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<SumSide>,
    /// Bidir only: the reverse-direction aggregates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_sent_bidir_reverse: Option<SumSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum_received_bidir_reverse: Option<SumSide>,
    pub cpu_utilization_percent: CpuUtilization,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_tcp_congestion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver_tcp_congestion: Option<String>,
}

/// One `end.streams[]` entry. iperf3 nests the per-direction stats: TCP carries
/// `{sender, receiver}`, UDP carries `{udp}`. Exactly one shape is populated.
#[derive(Debug, Clone, Serialize)]
pub struct EndStream {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<TcpStreamSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<TcpStreamSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp: Option<UdpStreamEnd>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TcpStreamSide {
    pub socket: i32,
    pub start: f64,
    pub end: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    /// Sender side only; iperf3 reports -1 when the OS doesn't expose retransmits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    // Sender-side TCP_INFO fields. iperf3 ALWAYS emits these on the sender
    // sub-object (0 when it couldn't measure them), so riperf3 does too for
    // drop-in schema parity; they're omitted on the receiver sub-object.
    // `max_snd_wnd` and `reorder` are always 0 on Linux — libc's `tcp_info`
    // exposes neither `tcpi_snd_wnd` nor `tcpi_reord_seen` — matching what iperf3
    // emits when those are unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_snd_cwnd: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_snd_wnd: Option<u64>,
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
pub struct UdpStreamEnd {
    pub socket: i32,
    pub start: f64,
    pub end: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    pub jitter_ms: f64,
    pub lost_packets: i64,
    pub packets: i64,
    pub lost_percent: f64,
    pub out_of_order: i64,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SumSide {
    pub start: f64,
    pub end: f64,
    pub seconds: f64,
    pub bytes: u64,
    pub bits_per_second: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packets: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_percent: Option<f64>,
    pub sender: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuUtilization {
    pub host_total: f64,
    pub host_user: f64,
    pub host_system: f64,
    pub remote_total: f64,
    pub remote_user: f64,
    pub remote_system: f64,
}

// ---------------------------------------------------------------------------
// Builder inputs — plain data so the assembly is pure and unit-testable without
// a live Client/socket.
// ---------------------------------------------------------------------------

/// Per-stream end data, already resolved to the local (this host) and remote
/// (peer, from the exchanged results) byte counts and roles.
#[derive(Debug, Clone)]
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
    pub retransmits: Option<i64>,
    /// Sender-side TCP_INFO extremes for the `end.streams[].sender` object (PR2).
    /// Only set for streams this host sent (local TCP_INFO); `None` otherwise.
    pub tcp_end: Option<TcpEndExtras>,
    /// UDP receiver stats (jitter seconds, lost, total packets, out-of-order),
    /// from whichever side measured them. `None` for TCP.
    pub udp: Option<UdpStreamStats>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TcpEndExtras {
    pub max_snd_cwnd: u64,
    pub max_rtt: u32,
    pub min_rtt: u32,
    pub mean_rtt: u32,
    pub reorder: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct UdpStreamStats {
    pub jitter_secs: f64,
    pub lost_packets: i64,
    pub packets: i64,
    pub out_of_order: i64,
}

pub struct ReportInput {
    pub protocol: TransportProtocol,
    pub reverse: bool,
    pub bidir: bool,
    pub duration: f64,
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
    pub sock_bufsize: u64,
    pub sndbuf_actual: u64,
    pub rcvbuf_actual: u64,
    pub interval: f64,
    pub gso: i32,
    pub gro: i32,
    /// Wall-clock at test start, ms since the Unix epoch — for `start.timestamp`.
    pub start_time_millis: u64,
    /// `--extra-data` string, emitted at the top level when present (#35).
    pub extra_data: Option<String>,
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
fn http_date(epoch_secs: u64) -> String {
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
                self.streams
                    .iter()
                    .filter_map(|s| s.udp)
                    .map(|u| u.jitter_secs)
                    .fold(0.0_f64, f64::max),
            )
        } else {
            (0_i64, 0_i64, 0.0_f64)
        };
        let blk = self.blksize.max(1) as u64;
        // iperf3's `stream_must_be_sender` for the aggregate `sender` flag.
        let fwd_sender = !self.reverse;
        let retransmits = self.sender_retransmits();

        let mut sum = None;
        let mut sum_sent_bidir_reverse = None;
        let mut sum_received_bidir_reverse = None;

        let (sum_sent, sum_received) = if self.is_server {
            // Server: report only this host's OWN measured bytes — iperf3 sums
            // local per-stream counters filtered by `sp->sender` and never grafts
            // the peer's reported bytes, so the side the server didn't measure is
            // genuinely 0 (forward → sent 0, reverse → received 0). The aggregate
            // `sender` flag is the server's role: it is the sender only in reverse.
            let server_is_sender = self.reverse;
            if self.bidir {
                // Two flows: forward (client→server, server receives → sender=false)
                // in sum_sent/sum_received; reverse (server→client, server sends →
                // sender=true) in the *_bidir_reverse pair. Retransmits, measured on
                // the server's send path, attach to the reverse (sent) side.
                sum_sent_bidir_reverse = Some(self.tcp_sum(local_sent, true, retransmits));
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
                ));
                (
                    self.udp_sum(local_sent, true, sent_packets, 0, 0.0),
                    self.udp_sum(local_recv, false, udp_packets, udp_lost, udp_jitter),
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
        } else if self.bidir {
            // Forward (this host → peer) goes in sum_sent/sum_received; reverse
            // (peer → this host) in the *_bidir_reverse pair, matching iperf3 —
            // rather than folding the reverse flow into sum_received (which would
            // make the two aggregates describe different directions).
            let fwd_recv = if peer_recv > 0 { peer_recv } else { local_sent };
            let rev_sent = if peer_sent > 0 { peer_sent } else { local_recv };
            sum_sent_bidir_reverse = Some(self.tcp_sum(rev_sent, false, None));
            sum_received_bidir_reverse = Some(self.tcp_sum(local_recv, false, None));
            (
                self.tcp_sum(local_sent, true, retransmits),
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
            let sent_packets = (sent_bytes / blk) as i64;
            sum = Some(self.udp_sum(sent_bytes, fwd_sender, sent_packets, udp_lost, udp_jitter));
            (
                self.udp_sum(sent_bytes, true, sent_packets, 0, 0.0),
                self.udp_sum(recv_bytes, false, udp_packets, udp_lost, udp_jitter),
            )
        } else {
            // TCP single direction (forward or reverse); both aggregates carry the
            // test's sender flag (!reverse), like iperf3.
            let sent_bytes = match (local_sent, peer_sent) {
                (0, p) if p > 0 => p,
                (0, _) => local_recv,
                (s, _) => s,
            };
            let recv_bytes = match (local_recv, peer_recv) {
                (0, p) if p > 0 => p,
                (0, _) => local_sent,
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
            // server, so only one side is emitted — the server's local algorithm,
            // on the side matching its role: receiver in forward, sender in reverse
            // and bidir (iperf_api.c:4544 swaps by stream_must_be_sender). Until
            // #37 reads the applied algorithm back, `congestion_used` is None on
            // both client and server, so both currently omit the field.
            let local = self.congestion_used.clone();
            if self.reverse || self.bidir {
                (local, None)
            } else {
                (None, local)
            }
        } else {
            (self.congestion_used.clone(), self.congestion_used.clone())
        };

        let end = End {
            streams: end_streams,
            sum_sent,
            sum_received,
            sum,
            sum_sent_bidir_reverse,
            sum_received_bidir_reverse,
            cpu_utilization_percent: self.cpu.clone(),
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
                sock_bufsize: self.sock_bufsize,
                sndbuf_actual: self.sndbuf_actual,
                rcvbuf_actual: self.rcvbuf_actual,
                test_start: TestStart {
                    protocol: if is_udp { "UDP" } else { "TCP" }.to_string(),
                    num_streams: self.num_streams,
                    blksize: self.blksize,
                    omit: self.omit,
                    duration: dur as i32,
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
                },
            },
            intervals: self.intervals.clone(),
            end,
            extra_data: self.extra_data.clone(),
        }
    }

    fn end_stream(&self, s: &StreamReport) -> EndStream {
        let dur = self.duration;
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
            let (bytes, packets) = if self.is_server {
                if s.is_sender {
                    let blk = self.blksize.max(1) as u64;
                    (s.local_bytes, (s.local_bytes / blk) as i64)
                } else {
                    (0, u.packets)
                }
            } else {
                (s.local_bytes, u.packets)
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
                    lost_percent: pct_lost(u.lost_packets, u.packets),
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
        let remote_bytes = if self.is_server {
            0
        } else {
            s.remote_bytes.unwrap_or(s.local_bytes)
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
            max_snd_wnd: emit_extras.then_some(0), // tcpi_snd_wnd unavailable via libc; 0 like iperf3
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

    /// Sender-side retransmit total. Collapses the -1 "unavailable" sentinel
    /// rather than summing it (summing N sentinels would emit a nonsensical -N
    /// that iperf3 never produces). Real per-stream values arrive with PR2.
    fn sender_retransmits(&self) -> Option<i64> {
        let vals: Vec<i64> = self.streams.iter().filter_map(|s| s.retransmits).collect();
        if vals.is_empty() {
            None
        } else if vals.iter().all(|&r| r < 0) {
            Some(-1) // all unavailable → iperf3's single sentinel
        } else {
            Some(vals.iter().map(|&r| r.max(0)).sum())
        }
    }

    fn tcp_sum(&self, bytes: u64, sender: bool, retransmits: Option<i64>) -> SumSide {
        let dur = self.duration;
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

    fn udp_sum(
        &self,
        bytes: u64,
        sender: bool,
        packets: i64,
        lost: i64,
        jitter_secs: f64,
    ) -> SumSide {
        let dur = self.duration;
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
            lost_percent: Some(pct_lost(lost, packets)),
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

    fn base_input() -> ReportInput {
        ReportInput {
            protocol: TransportProtocol::Tcp,
            reverse: false,
            bidir: false,
            duration: 10.0,
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
            sock_bufsize: 0,
            sndbuf_actual: 16384,
            rcvbuf_actual: 87380,
            interval: 1.0,
            gso: 0,
            gro: 0,
            start_time_millis: 1_780_107_649_449,
            extra_data: None,
            intervals: vec![],
            streams: vec![],
        }
    }

    fn tcp_stream(id: i32, is_sender: bool, local: u64, remote: u64) -> StreamReport {
        StreamReport {
            id,
            local_host: "127.0.0.1".into(),
            local_port: 40000 + id as u16,
            remote_host: "127.0.0.1".into(),
            remote_port: 5201,
            is_sender,
            local_bytes: local,
            remote_bytes: Some(remote),
            retransmits: Some(3),
            tcp_end: None,
            udp: None,
        }
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
        // TCP_INFO extras (real cwnd/rtt, snd_wnd 0 like iperf3).
        let mut input = base_input();
        input.is_server = true;
        input.reverse = true;
        input.streams = vec![StreamReport {
            tcp_end: Some(TcpEndExtras {
                max_snd_cwnd: 65535,
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
        assert_eq!(sender["max_snd_wnd"], 0); // libc has no tcpi_snd_wnd
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
        }];
        let mut s = tcp_stream(1, true, 1000, 1000);
        s.retransmits = Some(2);
        s.tcp_end = Some(TcpEndExtras {
            max_snd_cwnd: 64000,
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

        // snd_wnd / max_snd_wnd are emitted as 0 — libc can't read tcpi_snd_wnd,
        // and iperf3 likewise emits 0 when the field is unavailable.
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
        s.retransmits = Some(0); // iperf3 emits 0 here on a retransmit-capable OS
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
