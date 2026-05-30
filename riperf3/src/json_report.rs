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
    /// UDP receiver stats (jitter seconds, lost, total packets, out-of-order),
    /// from whichever side measured them. `None` for TCP.
    pub udp: Option<UdpStreamStats>,
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
    /// Assemble the iperf3-schema `end` block and connection list. `intervals`
    /// is left empty (PR2) and `start` metadata minimal (PR3).
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

        // Aggregates. sum_sent / sum_received always present; `sum` (UDP) and the
        // bidir-reverse pair are conditional, matching iperf3.
        let sent_bytes: u64 = self
            .streams
            .iter()
            .filter(|s| s.is_sender)
            .map(|s| s.local_bytes)
            .sum();
        let recv_bytes: u64 = self
            .streams
            .iter()
            .filter(|s| !s.is_sender)
            .map(|s| s.local_bytes)
            .sum();

        // Forward (all-sender) and reverse (all-receiver) tests only populate one
        // local side; fill the other aggregate from the peer's reported bytes so
        // both carry the real figure, like iperf3 (falling back to the local count
        // only if the peer reported nothing).
        let peer_sent: u64 = self
            .streams
            .iter()
            .filter(|s| !s.is_sender)
            .filter_map(|s| s.remote_bytes)
            .sum();
        let peer_recv: u64 = self
            .streams
            .iter()
            .filter(|s| s.is_sender)
            .filter_map(|s| s.remote_bytes)
            .sum();
        let sum_sent_bytes = match (sent_bytes, peer_sent) {
            (0, p) if p > 0 => p,
            (0, _) => recv_bytes,
            (s, _) => s,
        };
        let sum_recv_bytes = match (recv_bytes, peer_recv) {
            (0, p) if p > 0 => p,
            (0, _) => sent_bytes,
            (r, _) => r,
        };

        let retransmits_total: Option<i64> = if self.streams.iter().any(|s| s.retransmits.is_some())
        {
            Some(self.streams.iter().filter_map(|s| s.retransmits).sum())
        } else {
            None
        };

        // UDP datagram aggregates for `sum` / `sum_received`.
        let udp_agg = if is_udp {
            let lost: i64 = self
                .streams
                .iter()
                .filter_map(|s| s.udp)
                .map(|u| u.lost_packets)
                .sum();
            let packets: i64 = self
                .streams
                .iter()
                .filter_map(|s| s.udp)
                .map(|u| u.packets)
                .sum();
            let jitter = self
                .streams
                .iter()
                .filter_map(|s| s.udp)
                .map(|u| u.jitter_secs)
                .fold(0.0_f64, f64::max);
            Some((lost, packets, jitter))
        } else {
            None
        };

        let sum_sent = SumSide {
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes: sum_sent_bytes,
            bits_per_second: bps(sum_sent_bytes, dur),
            retransmits: retransmits_total,
            jitter_ms: None,
            lost_packets: None,
            packets: None,
            lost_percent: None,
            sender: true,
        };
        let mut sum_received = SumSide {
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes: sum_recv_bytes,
            bits_per_second: bps(sum_recv_bytes, dur),
            retransmits: None,
            jitter_ms: None,
            lost_packets: None,
            packets: None,
            lost_percent: None,
            sender: true,
        };

        let mut sum = None;
        if let Some((lost, packets, jitter)) = udp_agg {
            // The receiver aggregate carries the datagram loss/jitter (#25), and
            // UDP additionally emits a single-direction `sum`.
            sum_received.jitter_ms = Some(jitter * 1000.0);
            sum_received.lost_packets = Some(lost);
            sum_received.packets = Some(packets);
            sum_received.lost_percent = Some(pct_lost(lost, packets));
            // The sender aggregate stays byte-only, like iperf3.
            sum = Some(SumSide {
                jitter_ms: Some(jitter * 1000.0),
                lost_packets: Some(lost),
                packets: Some(packets),
                lost_percent: Some(pct_lost(lost, packets)),
                ..sum_received.clone()
            });
        }

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
            // Bidir per-direction aggregates: riperf3's bidir uses 2N half-duplex
            // sockets (vs iperf3's N full-duplex), so per-stream nesting can't
            // mirror iperf3 here; the reverse aggregate is left None until the
            // bidir transport model is reconciled (tracked separately under #36).
            sum_sent_bidir_reverse: None,
            sum_received_bidir_reverse: None,
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
            intervals: Vec::new(),
            end,
        }
    }

    fn end_stream(&self, s: &StreamReport) -> EndStream {
        let dur = self.duration;
        if let Some(u) = s.udp {
            // UDP: a single `udp` object. The measuring side (the receiver) holds
            // the datagram stats; bytes are the local count for this stream.
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

        // TCP: both a sender and a receiver side. The local count covers our side;
        // the peer's reported bytes cover the other side (falling back to the
        // local count when the peer didn't report a per-stream figure).
        let local = TcpStreamSide {
            socket: s.id,
            start: 0.0,
            end: dur,
            seconds: dur,
            bytes: s.local_bytes,
            bits_per_second: bps(s.local_bytes, dur),
            retransmits: if s.is_sender { s.retransmits } else { None },
            sender: !self.reverse,
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
            sender: !self.reverse,
        };
        // Place the local side under sender/receiver by its role; the peer fills
        // the opposite slot.
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
}
