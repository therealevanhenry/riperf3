//! Typed, iperf3-compatible JSON report model (issue #36).
//!
//! riperf3's `-J` output must be a faithful drop-in for iperf3's, so machine
//! consumers (Telegraf, Grafana plugins, CI harnesses) that parse iperf3 JSON
//! work unchanged. This replaces the previous hand-rolled `serde_json::json!`
//! blob, which diverged from iperf3's schema (flat `end.streams`, empty
//! `intervals`, fabricated addresses).
//!
//! Built incrementally (see #36):
//! - **This PR**: the `end` block — per-stream objects nested as
//!   `{sender, receiver}` (TCP) or `{udp}` (UDP), the `sum_sent`/`sum_received`
//!   aggregates plus the UDP `sum` and bidir `sum_*_bidir_reverse`, CPU
//!   utilization, and `sender`/`receiver_tcp_congestion`; plus real connection
//!   addresses in `start.connected`.
//! - **PR2**: populate `intervals` (and the per-stream TCP_INFO extremes
//!   `max_snd_cwnd` / `min`/`max`/`mean_rtt`, which derive from per-interval
//!   `TCP_INFO` accumulation).
//! - **PR3**: full `start` metadata (`cookie`, `timestamp`, `system_info`,
//!   `tcp_mss_default`, socket buffer sizes, …).
//!
//! Fields iperf3 emits but riperf3 cannot yet produce are omitted
//! (`skip_serializing_if`) rather than emitted with placeholder values, so the
//! shape only ever contains real data. The one exception is `system_info`, which
//! iperf3 always emits: it ships as an empty string until PR3 fills in the uname.

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
}

// ---------------------------------------------------------------------------
// start{}
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Start {
    pub connected: Vec<Connection>,
    pub version: String,
    pub system_info: String,
    pub connecting_to: ConnectingTo,
    pub test_start: TestStart,
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
    // Sender-side TCP_INFO extremes, accumulated across the test's intervals
    // (PR2). Omitted on the receiver side and where TCP_INFO is unavailable.
    // `max_snd_wnd` is intentionally absent: libc's `tcp_info` doesn't expose
    // `tcpi_snd_wnd` on Linux, so riperf3 can't produce it (omit, don't fake).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_snd_cwnd: Option<u64>,
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
    pub version: String,
    pub system_info: String,
    pub cpu: CpuUtilization,
    pub congestion_used: Option<String>,
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

impl ReportInput {
    /// Assemble the iperf3-schema report: `start`, the collected `intervals`, and
    /// the `end` block. `start` metadata stays minimal (PR3).
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

        let (sum_sent, sum_received) = if self.bidir {
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

        Report {
            start: Start {
                connected,
                version: self.version.clone(),
                system_info: self.system_info.clone(),
                connecting_to: ConnectingTo {
                    host: self.connecting_host.clone(),
                    port: self.connecting_port,
                },
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
                },
            },
            intervals: self.intervals.clone(),
            end,
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
            return EndStream {
                sender: None,
                receiver: None,
                udp: Some(UdpStreamEnd {
                    socket: s.id,
                    start: 0.0,
                    end: dur,
                    seconds: dur,
                    bytes: s.local_bytes,
                    bits_per_second: bps(s.local_bytes, dur),
                    jitter_ms: u.jitter_secs * 1000.0,
                    lost_packets: u.lost_packets,
                    packets: u.packets,
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
        let (cwnd, maxr, minr, meanr, reord) = match s.tcp_end {
            Some(e) => (
                Some(e.max_snd_cwnd),
                Some(e.max_rtt),
                Some(e.min_rtt),
                Some(e.mean_rtt),
                Some(e.reorder),
            ),
            None => (None, None, None, None, None),
        };
        let local = TcpStreamSide {
            socket: s.id,
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes: s.local_bytes,
            bits_per_second: bps(s.local_bytes, dur),
            retransmits: if s.is_sender { s.retransmits } else { None },
            max_snd_cwnd: cwnd,
            max_rtt: maxr,
            min_rtt: minr,
            mean_rtt: meanr,
            reorder: reord,
            sender: dir,
        };
        let remote_bytes = s.remote_bytes.unwrap_or(s.local_bytes);
        let remote = TcpStreamSide {
            socket: s.id,
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes: remote_bytes,
            bits_per_second: bps(remote_bytes, dur),
            retransmits: if s.is_sender { None } else { s.retransmits },
            max_snd_cwnd: None,
            max_rtt: None,
            min_rtt: None,
            mean_rtt: None,
            reorder: None,
            sender: dir,
        };
        if s.is_sender {
            EndStream {
                sender: Some(local),
                receiver: Some(remote),
                udp: None,
            }
        } else {
            EndStream {
                sender: Some(remote),
                receiver: Some(local),
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
            "connecting_to",
            "test_start",
        ] {
            assert!(v["start"].get(k).is_some(), "start.{k} missing");
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

        // snd_wnd / max_snd_wnd are intentionally absent (Linux libc gap).
        assert!(i0.get("snd_wnd").is_none(), "{i0}");
        assert!(snd.get("max_snd_wnd").is_none(), "{snd}");
    }
}
