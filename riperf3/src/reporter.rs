use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::protocol::TransportProtocol;
use crate::stream::{StreamCounters, UdpRecvStats};
use crate::tcp_info;
use crate::units;

/// A single interval report for one stream.
#[derive(Debug, Clone)]
pub struct StreamInterval {
    pub stream_id: i32,
    pub start: f64,
    pub end: f64,
    pub bytes: u64,
    pub is_sender: bool,
    pub retransmits: Option<i64>,
    pub snd_cwnd: Option<u64>,
    pub rtt: Option<u32>,
    // UDP specific
    pub jitter: Option<f64>,
    pub lost: Option<i64>,
    pub total_packets: Option<i64>,
    pub omitted: bool,
}

/// Accumulated summary for the final report.
#[derive(Debug, Clone)]
pub struct StreamSummary {
    pub stream_id: i32,
    pub start: f64,
    pub end: f64,
    pub bytes: u64,
    pub is_sender: bool,
    pub retransmits: Option<i64>,
    // UDP
    pub jitter: Option<f64>,
    pub lost: Option<i64>,
    pub total_packets: Option<i64>,
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Format a stream ID for display. Negative IDs render as "SUM".
fn fmt_id(id: i32) -> String {
    if id < 0 {
        "SUM".to_string()
    } else {
        format!("{id:3}")
    }
}

/// Emit one human-readable report line, prefixed with the `-T/--title` string
/// when a title is active (#34). Every report line routes through this so the
/// prefix matches iperf3 without changing the public printer signatures.
fn titled(line: std::fmt::Arguments) {
    println!("{}{}", crate::macros::output_title_prefix(), line);
}

/// Print the header line for interval reports.
pub fn print_header(protocol: TransportProtocol, has_retransmits: bool) {
    match protocol {
        TransportProtocol::Tcp => {
            if has_retransmits {
                titled(format_args!(
                    "[ ID] Interval           Transfer     Bitrate         Retr  Cwnd"
                ));
            } else {
                titled(format_args!(
                    "[ ID] Interval           Transfer     Bitrate"
                ));
            }
        }
        TransportProtocol::Udp => {
            titled(format_args!(
                "[ ID] Interval           Transfer     Bitrate         Jitter    Lost/Total Datagrams"
            ));
        }
    }
}

/// Print one interval line.
pub fn print_interval(interval: &StreamInterval, format_char: char) {
    let id = fmt_id(interval.stream_id);
    let transfer = units::format_bytes(interval.bytes as f64, format_char.to_ascii_uppercase());
    let seconds = interval.end - interval.start;
    let bits_per_sec = if seconds > 0.0 {
        interval.bytes as f64 * 8.0 / seconds
    } else {
        0.0
    };
    let rate = units::format_rate(bits_per_sec, format_char);

    let omit_tag = if interval.omitted { "(omitted) " } else { "" };

    if let (Some(jitter), Some(lost), Some(total)) =
        (interval.jitter, interval.lost, interval.total_packets)
    {
        let pct = lost_percent(lost, total);
        titled(format_args!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:7.3} ms  {}/{} ({:.2}%)  {}",
            interval.start,
            interval.end,
            transfer,
            rate,
            jitter * 1000.0,
            lost,
            total,
            pct,
            omit_tag,
        ));
    } else if let (Some(retr), Some(cwnd)) = (interval.retransmits, interval.snd_cwnd) {
        let cwnd_str = units::format_bytes(cwnd as f64, 'A');
        titled(format_args!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}   {:>10}  {}",
            interval.start, interval.end, transfer, rate, retr, cwnd_str, omit_tag,
        ));
    } else {
        titled(format_args!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {}",
            interval.start, interval.end, transfer, rate, omit_tag,
        ));
    }
}

/// Print the separator line.
pub fn print_separator() {
    titled(format_args!(
        "- - - - - - - - - - - - - - - - - - - - - - - - -"
    ));
}

/// UDP loss as a percentage of total datagrams, guarding the zero-total case
/// (no packets ⇒ 0%, not NaN). Single source of truth for the `(x.xx%)` figure
/// across interval lines, final summaries, and JSON output.
pub fn lost_percent(lost: i64, total: i64) -> f64 {
    if total > 0 {
        lost as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

/// Format a single final-summary line (no trailing newline). Pure, so the
/// rendered output can be unit-tested without capturing stdout.
pub fn format_summary_line(summary: &StreamSummary, format_char: char) -> String {
    let id = fmt_id(summary.stream_id);
    let transfer = units::format_bytes(summary.bytes as f64, format_char.to_ascii_uppercase());
    let seconds = summary.end - summary.start;
    let bits_per_sec = if seconds > 0.0 {
        summary.bytes as f64 * 8.0 / seconds
    } else {
        0.0
    };
    let rate = units::format_rate(bits_per_sec, format_char);
    let role = if summary.is_sender {
        "sender"
    } else {
        "receiver"
    };

    if let (Some(jitter), Some(lost), Some(total)) =
        (summary.jitter, summary.lost, summary.total_packets)
    {
        let pct = lost_percent(lost, total);
        format!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:7.3} ms  {}/{} ({:.2}%)  {}",
            summary.start,
            summary.end,
            transfer,
            rate,
            jitter * 1000.0,
            lost,
            total,
            pct,
            role,
        )
    } else if let Some(retr) = summary.retransmits {
        format!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}             {}",
            summary.start, summary.end, transfer, rate, retr, role,
        )
    } else {
        format!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}                    {}",
            summary.start, summary.end, transfer, rate, role,
        )
    }
}

/// Print a single final summary line.
pub fn print_summary(summary: &StreamSummary, format_char: char) {
    titled(format_args!(
        "{}",
        format_summary_line(summary, format_char)
    ));
}

/// Build the full set of final-report lines for a set of per-stream summaries:
/// the per-stream lines followed by aggregate `[SUM]` rows. Pure and testable.
/// Both the client and the server route their final report through this so the
/// two sides stay consistent: issue #4 was the final `[SUM]` row being omitted
/// for `-P > 1` (the client fix landed first, then the server), and a single
/// shared path keeps either side from regressing independently.
pub fn final_report_lines(per_stream: &[StreamSummary], format_char: char) -> Vec<String> {
    let mut lines: Vec<String> = per_stream
        .iter()
        .map(|s| format_summary_line(s, format_char))
        .collect();
    for sum in sum_summaries(per_stream) {
        lines.push(format_summary_line(&sum, format_char));
    }
    lines
}

/// Print the final report (per-stream summaries + aggregate `[SUM]` rows).
pub fn print_final_summaries(per_stream: &[StreamSummary], format_char: char) {
    for line in final_report_lines(per_stream, format_char) {
        titled(format_args!("{line}"));
    }
}

/// Derive the aggregate `[SUM]` rows for the final report from the per-stream
/// summaries. Returns one SUM per direction (sender / receiver) that has more
/// than one stream — matching iperf3, which prints a `[SUM]` for parallel
/// streams and omits it for a single stream. Bidir runs yield up to two SUM
/// rows (one per direction). UDP SUM rows aggregate lost/total datagrams and
/// carry the worst-case (max) jitter across the grouped streams.
pub fn sum_summaries(streams: &[StreamSummary]) -> Vec<StreamSummary> {
    let mut out = Vec::new();
    for is_sender in [true, false] {
        let group: Vec<&StreamSummary> = streams
            .iter()
            .filter(|s| s.is_sender == is_sender)
            .collect();
        if group.len() <= 1 {
            continue;
        }
        let bytes = group.iter().map(|s| s.bytes).sum();
        let is_udp = group.iter().any(|s| s.total_packets.is_some());
        let (jitter, lost, total_packets) = if is_udp {
            let lost = group.iter().filter_map(|s| s.lost).sum();
            let total = group.iter().filter_map(|s| s.total_packets).sum();
            // Jitter doesn't sum; report the worst stream's jitter on the SUM.
            let jitter = group
                .iter()
                .filter_map(|s| s.jitter)
                .fold(None, |acc, j| Some(acc.map_or(j, |a: f64| a.max(j))));
            (jitter, Some(lost), Some(total))
        } else {
            (None, None, None)
        };
        // Aggregate per-stream retransmits when present. The final per-stream
        // summaries don't yet carry retransmits (the producers pass `None`, so
        // this is dormant today), but the math is kept correct and tested so
        // the SUM stays right if/when end-of-test TCP_INFO is plumbed in.
        let retransmits = if group.iter().any(|s| s.retransmits.is_some()) {
            Some(group.iter().filter_map(|s| s.retransmits).sum())
        } else {
            None
        };
        out.push(StreamSummary {
            stream_id: -1, // renders as "SUM"
            start: group[0].start,
            end: group[0].end,
            bytes,
            is_sender,
            retransmits,
            jitter,
            lost,
            total_packets,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Interval reporter — spawned async task for periodic stats
// ---------------------------------------------------------------------------

/// Lightweight reference to a stream's shared state for the interval reporter.
/// Cloned from DataStream since we can't send references across spawn boundaries.
pub struct IntervalStreamRef {
    pub id: i32,
    pub is_sender: bool,
    pub counters: Arc<StreamCounters>,
    pub udp_recv_stats: Option<Arc<Mutex<UdpRecvStats>>>,
    pub raw_fd: Option<i32>,
}

/// Configuration for the interval reporter.
pub struct IntervalReporterConfig {
    pub interval_secs: f64,
    pub protocol: TransportProtocol,
    pub format_char: char,
    pub omit_secs: u32,
    pub num_streams: usize,
    pub forceflush: bool,
    pub timestamp_format: Option<String>,
    pub json_stream: bool,
    /// Print interval lines live (text or json-stream). When false the reporter
    /// runs purely to collect intervals for the final `-J` blob (issue #36 PR2).
    pub print: bool,
    /// Datagram size, used to derive the UDP *sender's* per-interval packet count
    /// (the sender doesn't measure loss/jitter, so iperf3 reports only `packets`).
    pub blksize: usize,
}

/// Per-stream sender-side TCP_INFO extremes accumulated across the run (#36 PR2),
/// for the `end.streams[].sender` object. Only meaningful for TCP sender streams.
#[derive(Debug, Default, Clone, Copy)]
pub struct StreamExtremes {
    pub stream_id: i32,
    pub max_snd_cwnd: u64,
    pub max_rtt: u32,
    pub min_rtt: u32,
    pub reorder: u32,
    rtt_sum: u64,
    rtt_samples: u64,
    /// Final cumulative retransmit total; `None` until a TCP_INFO read succeeds.
    pub total_retransmits: Option<u32>,
}

impl StreamExtremes {
    pub fn mean_rtt(&self) -> u32 {
        self.rtt_sum.checked_div(self.rtt_samples).unwrap_or(0) as u32
    }

    /// True once at least one TCP_INFO sample was recorded.
    pub fn has_samples(&self) -> bool {
        self.rtt_samples > 0
    }
}

/// Interval samples plus per-stream extremes collected during a run, for the
/// final `-J` report (#36 PR2). Written once when the reporter task finishes;
/// the client reads it after joining that task.
#[derive(Debug, Default)]
pub struct CollectedIntervals {
    pub intervals: Vec<crate::json_report::Interval>,
    pub extremes: Vec<StreamExtremes>,
}

/// Spawn an async task that prints interval reports periodically.
///
/// Returns `None` if interval reporting is disabled (interval_secs <= 0).
/// The handle should be awaited after the test's `done` flag is set.
pub fn spawn_interval_reporter(
    config: IntervalReporterConfig,
    streams: Vec<IntervalStreamRef>,
    done: Arc<AtomicBool>,
    collector: Option<Arc<Mutex<CollectedIntervals>>>,
) -> Option<JoinHandle<()>> {
    if config.interval_secs <= 0.0 {
        return None;
    }

    let interval_dur = Duration::from_secs_f64(config.interval_secs);
    let has_retransmits = tcp_info::has_retransmit_info()
        && config.protocol == TransportProtocol::Tcp
        && streams.iter().any(|s| s.is_sender);
    let collecting = collector.is_some();
    let is_udp = config.protocol == TransportProtocol::Udp;

    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval_dur);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // skip the immediate first tick

        let mut interval_num: u64 = 0;
        let mut header_printed = false;
        let omit_intervals = if config.interval_secs > 0.0 {
            (config.omit_secs as f64 / config.interval_secs).ceil() as u64
        } else {
            0
        };

        // Per-stream previous values for computing deltas
        let mut prev_retransmits: Vec<u32> = vec![0; streams.len()];
        let mut prev_cnt_error: Vec<i64> = vec![0; streams.len()];
        let mut prev_packet_count: Vec<i64> = vec![0; streams.len()];

        // Datagram size for the UDP sender's per-interval packet count.
        let blk = config.blksize.max(1) as u64;

        // Accumulated state for the final `-J` report (#36 PR2). Written to the
        // collector once the loop ends; the client reads it after joining us.
        let mut collected: Vec<crate::json_report::Interval> = Vec::new();
        let mut acc_extremes: Vec<StreamExtremes> = streams
            .iter()
            .map(|s| StreamExtremes {
                stream_id: s.id,
                min_rtt: u32::MAX,
                ..Default::default()
            })
            .collect();

        loop {
            ticker.tick().await;

            if done.load(Ordering::Relaxed) {
                break;
            }

            interval_num += 1;
            let omitted = interval_num <= omit_intervals;
            let start = (interval_num - 1) as f64 * config.interval_secs;
            let end = interval_num as f64 * config.interval_secs;
            let seconds = end - start;

            // Timestamp prefix for this tick
            if config.print && config.timestamp_format.is_some() {
                // Use libc strftime for iperf3-compatible timestamp formatting
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let secs = now.as_secs();
                // Simple ISO-ish format without pulling in chrono
                let hours = (secs % 86400) / 3600;
                let mins = (secs % 3600) / 60;
                let s = secs % 60;
                print!("{hours:02}:{mins:02}:{s:02} ");
            }

            if config.print && !header_printed {
                print_header(config.protocol, has_retransmits);
                header_printed = true;
            }

            let mut sum_bytes: u64 = 0;
            let mut sum_retransmits: i64 = 0;
            // UDP sums
            let mut sum_lost: i64 = 0;
            let mut sum_packets: i64 = 0;
            let mut last_jitter: f64 = 0.0;
            let mut collected_streams: Vec<crate::json_report::IntervalStream> = Vec::new();

            for (i, stream) in streams.iter().enumerate() {
                let bytes = if stream.is_sender {
                    stream.counters.take_sent_interval()
                } else {
                    stream.counters.take_received_interval()
                };

                // TCP_INFO for the interval detail and the end extremes.
                let (retransmits, snd_cwnd, rtt, rttvar, pmtu, reorder_iv) = if has_retransmits
                    && stream.is_sender
                {
                    if let Some(fd) = stream.raw_fd {
                        if let Some(info) = tcp_info::get_tcp_info(fd) {
                            let delta = info.total_retransmits.saturating_sub(prev_retransmits[i]);
                            prev_retransmits[i] = info.total_retransmits;
                            // Accumulate sender-side extremes for the end report.
                            let e = &mut acc_extremes[i];
                            e.max_snd_cwnd = e.max_snd_cwnd.max(info.snd_cwnd);
                            e.reorder = e.reorder.max(info.reorder);
                            if info.rtt > 0 {
                                e.max_rtt = e.max_rtt.max(info.rtt);
                                e.min_rtt = e.min_rtt.min(info.rtt);
                                e.rtt_sum += info.rtt as u64;
                                e.rtt_samples += 1;
                            }
                            e.total_retransmits = Some(info.total_retransmits);
                            (
                                Some(delta as i64),
                                Some(info.snd_cwnd),
                                Some(info.rtt),
                                Some(info.rttvar),
                                Some(info.pmtu),
                                Some(info.reorder),
                            )
                        } else {
                            (None, None, None, None, None, None)
                        }
                    } else {
                        (None, None, None, None, None, None)
                    }
                } else {
                    (None, None, None, None, None, None)
                };

                // UDP stats (compute deltas for loss/packets)
                let (jitter, lost, total) = if let Some(ref udp_stats) = stream.udp_recv_stats {
                    if let Ok(st) = udp_stats.lock() {
                        let delta_error = st.cnt_error - prev_cnt_error[i];
                        let delta_packets = st.packet_count - prev_packet_count[i];
                        prev_cnt_error[i] = st.cnt_error;
                        prev_packet_count[i] = st.packet_count;
                        last_jitter = st.jitter;
                        (Some(st.jitter), Some(delta_error), Some(delta_packets))
                    } else {
                        (None, None, None)
                    }
                } else {
                    (None, None, None)
                };

                let bps_val = if seconds > 0.0 {
                    bytes as f64 * 8.0 / seconds
                } else {
                    0.0
                };

                if config.print {
                    let interval = StreamInterval {
                        stream_id: stream.id,
                        start,
                        end,
                        bytes,
                        is_sender: stream.is_sender,
                        retransmits,
                        snd_cwnd,
                        rtt,
                        jitter,
                        lost,
                        total_packets: total,
                        omitted,
                    };

                    if config.json_stream {
                        let mut j = serde_json::json!({
                            "socket": stream.id,
                            "start": start,
                            "end": end,
                            "seconds": seconds,
                            "bytes": bytes,
                            "bits_per_second": bps_val,
                            "omitted": omitted,
                            "sender": stream.is_sender,
                        });
                        if let Some(r) = retransmits {
                            j["retransmits"] = serde_json::json!(r);
                        }
                        if let Some(c) = snd_cwnd {
                            j["snd_cwnd"] = serde_json::json!(c);
                        }
                        if let Some(ji) = jitter {
                            j["jitter_ms"] = serde_json::json!(ji * 1000.0);
                        }
                        if let Some(l) = lost {
                            j["lost_packets"] = serde_json::json!(l);
                        }
                        if let Some(p) = total {
                            j["packets"] = serde_json::json!(p);
                        }
                        println!("{}", serde_json::to_string(&j).unwrap());
                    } else {
                        print_interval(&interval, config.format_char);
                    }
                }

                if collecting {
                    // UDP datagram detail: a receiver stream reports measured
                    // loss/jitter; a sender stream reports only the sent packet
                    // count (bytes / datagram size), like iperf3.
                    let (j_ms, lost_p, pkts, lost_pct) = if stream.udp_recv_stats.is_some() {
                        (
                            jitter.map(|j| j * 1000.0),
                            lost,
                            total,
                            match (lost, total) {
                                (Some(l), Some(t)) => Some(lost_percent(l, t)),
                                _ => None,
                            },
                        )
                    } else if is_udp {
                        (None, None, Some((bytes / blk) as i64), None)
                    } else {
                        (None, None, None, None)
                    };
                    collected_streams.push(crate::json_report::IntervalStream {
                        socket: stream.id,
                        start,
                        end,
                        seconds,
                        bytes,
                        bits_per_second: bps_val,
                        retransmits,
                        snd_cwnd,
                        // snd_wnd is unavailable via libc (see TcpStreamSide); emit
                        // 0 alongside the other TCP detail, like iperf3.
                        snd_wnd: snd_cwnd.map(|_| 0u64),
                        rtt,
                        rttvar,
                        pmtu,
                        reorder: reorder_iv,
                        jitter_ms: j_ms,
                        lost_packets: lost_p,
                        packets: pkts,
                        lost_percent: lost_pct,
                        omitted,
                        sender: stream.is_sender,
                    });
                }

                sum_bytes += bytes;
                if let Some(r) = retransmits {
                    sum_retransmits += r;
                }
                if let Some(l) = lost {
                    sum_lost += l;
                }
                if let Some(p) = total {
                    sum_packets += p;
                }
            }

            // Print [SUM] line for parallel streams
            if config.print && config.num_streams > 1 {
                let sum_interval = StreamInterval {
                    stream_id: -1, // renders as "SUM"
                    start,
                    end,
                    bytes: sum_bytes,
                    is_sender: streams.first().is_none_or(|s| s.is_sender),
                    retransmits: if has_retransmits {
                        Some(sum_retransmits)
                    } else {
                        None
                    },
                    snd_cwnd: None,
                    rtt: None,
                    jitter: if is_udp { Some(last_jitter) } else { None },
                    lost: if is_udp { Some(sum_lost) } else { None },
                    total_packets: if is_udp { Some(sum_packets) } else { None },
                    omitted,
                };
                print_interval(&sum_interval, config.format_char);
            }

            if collecting {
                let sum_bps = if seconds > 0.0 {
                    sum_bytes as f64 * 8.0 / seconds
                } else {
                    0.0
                };
                // UDP sum: a receiving side reports measured loss/jitter; a pure
                // sending side reports only the sent packet count, like iperf3.
                let any_udp_recv = streams.iter().any(|s| s.udp_recv_stats.is_some());
                let (sum_j, sum_lostp, sum_pkts, sum_lostpct) = if is_udp && any_udp_recv {
                    (
                        Some(last_jitter * 1000.0),
                        Some(sum_lost),
                        Some(sum_packets),
                        Some(lost_percent(sum_lost, sum_packets)),
                    )
                } else if is_udp {
                    (None, None, Some((sum_bytes / blk) as i64), None)
                } else {
                    (None, None, None, None)
                };
                // iperf3 emits the sum's retransmits only on a sender-direction
                // sum (sender_has_retransmits && stream_must_be_sender). On the
                // server's bidir `sum` (which describes the received flow, so
                // sender=false) it must be omitted, not just gated on "any stream
                // sends" — otherwise the received-flow sum carries a spurious count.
                let sum_is_sender = streams.first().is_none_or(|s| s.is_sender);
                collected.push(crate::json_report::Interval {
                    streams: collected_streams,
                    sum: crate::json_report::IntervalSum {
                        start,
                        end,
                        seconds,
                        bytes: sum_bytes,
                        bits_per_second: sum_bps,
                        retransmits: if has_retransmits && sum_is_sender {
                            Some(sum_retransmits)
                        } else {
                            None
                        },
                        jitter_ms: sum_j,
                        lost_packets: sum_lostp,
                        packets: sum_pkts,
                        lost_percent: sum_lostpct,
                        omitted,
                        sender: sum_is_sender,
                    },
                });
            }

            // Flush after each interval if requested
            if config.print && config.forceflush {
                use std::io::Write;
                let _ = std::io::stdout().flush();
            }
        }

        // Hand the collected samples + extremes to the client (#36 PR2).
        if let Some(c) = collector {
            if let Ok(mut g) = c.lock() {
                for e in acc_extremes.iter_mut() {
                    if e.min_rtt == u32::MAX {
                        e.min_rtt = 0;
                    }
                }
                g.intervals = collected;
                g.extremes = acc_extremes;
            }
        }
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_interval_tcp_basic() {
        let interval = StreamInterval {
            stream_id: 5,
            start: 0.0,
            end: 1.0,
            bytes: 1024 * 1024 * 1024,
            is_sender: true,
            retransmits: None,
            snd_cwnd: None,
            rtt: None,
            jitter: None,
            lost: None,
            total_packets: None,
            omitted: false,
        };
        print_interval(&interval, 'm');
    }

    #[test]
    fn stream_summary_udp() {
        let summary = StreamSummary {
            stream_id: 5,
            start: 0.0,
            end: 10.0,
            bytes: 128_000,
            is_sender: false,
            retransmits: None,
            jitter: Some(0.012),
            lost: Some(5),
            total_packets: Some(100),
        };
        print_summary(&summary, 'm');
    }

    #[test]
    fn sum_line_formatting() {
        let interval = StreamInterval {
            stream_id: -1, // SUM
            start: 0.0,
            end: 1.0,
            bytes: 1_000_000,
            is_sender: true,
            retransmits: Some(3),
            snd_cwnd: None,
            rtt: None,
            jitter: None,
            lost: None,
            total_packets: None,
            omitted: false,
        };
        // Should print [SUM] instead of a number
        print_interval(&interval, 'm');
    }

    // ---- sum_summaries (issue #4: final [SUM] row for -P > 1) ----------------

    fn tcp_summary(id: i32, is_sender: bool, bytes: u64) -> StreamSummary {
        StreamSummary {
            stream_id: id,
            start: 0.0,
            end: 10.0,
            bytes,
            is_sender,
            retransmits: None,
            jitter: None,
            lost: None,
            total_packets: None,
        }
    }

    #[test]
    fn sum_summaries_single_stream_no_sum() {
        // One stream → no [SUM] row, matching iperf3.
        let streams = vec![tcp_summary(1, true, 1_000_000)];
        assert!(sum_summaries(&streams).is_empty());
    }

    #[test]
    fn sum_summaries_multi_sender_aggregates_bytes() {
        let streams = vec![
            tcp_summary(1, true, 1_000),
            tcp_summary(3, true, 2_000),
            tcp_summary(4, true, 3_000),
        ];
        let sums = sum_summaries(&streams);
        assert_eq!(sums.len(), 1, "one SUM row for the sender group");
        assert_eq!(sums[0].stream_id, -1, "SUM renders from id -1");
        assert!(sums[0].is_sender);
        assert_eq!(sums[0].bytes, 6_000, "bytes summed across streams");
        assert_eq!(sums[0].start, 0.0);
        assert_eq!(sums[0].end, 10.0);
    }

    #[test]
    fn sum_summaries_bidir_yields_two_rows() {
        // Bidir: senders and receivers each >1 → one SUM per direction.
        let streams = vec![
            tcp_summary(1, true, 1_000),
            tcp_summary(3, true, 1_000),
            tcp_summary(5, false, 2_000),
            tcp_summary(7, false, 2_000),
        ];
        let sums = sum_summaries(&streams);
        assert_eq!(sums.len(), 2);
        let sender = sums.iter().find(|s| s.is_sender).unwrap();
        let receiver = sums.iter().find(|s| !s.is_sender).unwrap();
        assert_eq!(sender.bytes, 2_000);
        assert_eq!(receiver.bytes, 4_000);
    }

    #[test]
    fn sum_summaries_bidir_single_per_direction_no_sum() {
        // Bidir -P 1: one sender + one receiver → neither direction gets a SUM.
        let streams = vec![tcp_summary(1, true, 1_000), tcp_summary(3, false, 2_000)];
        assert!(sum_summaries(&streams).is_empty());
    }

    #[test]
    fn lost_percent_guards_zero_total() {
        assert_eq!(lost_percent(0, 0), 0.0, "no datagrams ⇒ 0%, not NaN");
        assert_eq!(lost_percent(5, 0), 0.0, "zero total never divides");
        assert_eq!(lost_percent(0, 1000), 0.0, "loss-free");
        assert!((lost_percent(4258, 267_190) - 1.5936).abs() < 1e-3);
        assert_eq!(lost_percent(1000, 1000), 100.0, "total loss");
    }

    #[test]
    fn sum_summaries_udp_aggregates_loss_and_max_jitter() {
        let streams = vec![
            StreamSummary {
                stream_id: 1,
                start: 0.0,
                end: 10.0,
                bytes: 100_000,
                is_sender: false,
                retransmits: None,
                jitter: Some(0.010),
                lost: Some(2),
                total_packets: Some(1000),
            },
            StreamSummary {
                stream_id: 3,
                start: 0.0,
                end: 10.0,
                bytes: 200_000,
                is_sender: false,
                retransmits: None,
                jitter: Some(0.025),
                lost: Some(5),
                total_packets: Some(2000),
            },
        ];
        let sums = sum_summaries(&streams);
        assert_eq!(sums.len(), 1);
        let s = &sums[0];
        assert_eq!(s.bytes, 300_000);
        assert_eq!(s.lost, Some(7), "lost datagrams summed");
        assert_eq!(s.total_packets, Some(3000), "total datagrams summed");
        assert_eq!(s.jitter, Some(0.025), "SUM carries worst-case jitter");
    }

    #[test]
    fn sum_summaries_aggregates_retransmits() {
        // Forward-compat: when per-stream summaries carry retransmits, the SUM
        // must sum them (the producers don't set this yet — see sum_summaries).
        let mut a = tcp_summary(1, true, 1_000);
        a.retransmits = Some(3);
        let mut b = tcp_summary(3, true, 1_000);
        b.retransmits = Some(4);
        let sums = sum_summaries(&[a, b]);
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].retransmits, Some(7), "retransmits summed on SUM");
    }

    // ---- final_report_lines: the rendered output both client & server emit ---

    /// The blocker behind issue #4: parallel streams must produce a rendered
    /// `[SUM]` line. Both the client and server route through final_report_lines,
    /// so this pins the rendered behavior for both sides without stdout capture.
    #[test]
    fn final_report_lines_includes_sum_for_multistream() {
        let streams = vec![
            tcp_summary(1, true, 1_000),
            tcp_summary(3, true, 2_000),
            tcp_summary(4, true, 3_000),
        ];
        let lines = final_report_lines(&streams, 'm');
        assert_eq!(lines.len(), 4, "3 per-stream lines + 1 SUM");
        assert_eq!(
            lines.iter().filter(|l| l.contains("[SUM]")).count(),
            1,
            "exactly one [SUM] line; got:\n{}",
            lines.join("\n")
        );
        let sum_line = lines.last().unwrap();
        assert!(sum_line.contains("[SUM]"));
        // Pin the rendered aggregate value, not just the presence of a SUM
        // row: the SUM line must render the summed bytes (6000), so a
        // regression that printed a per-stream or zero value would be caught.
        let expected_transfer = units::format_bytes(6_000.0, 'M');
        assert!(
            sum_line.contains(&expected_transfer),
            "SUM must render summed transfer {expected_transfer:?}; got {sum_line:?}"
        );
    }

    #[test]
    fn final_report_lines_no_sum_for_single_stream() {
        let lines = final_report_lines(&[tcp_summary(1, true, 1_000)], 'm');
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("[SUM]"));
    }

    #[test]
    fn final_report_lines_bidir_has_two_sums() {
        let streams = vec![
            tcp_summary(1, true, 1_000),
            tcp_summary(3, true, 1_000),
            tcp_summary(5, false, 2_000),
            tcp_summary(7, false, 2_000),
        ];
        let lines = final_report_lines(&streams, 'm');
        let sum_lines: Vec<&String> = lines.iter().filter(|l| l.contains("[SUM]")).collect();
        assert_eq!(sum_lines.len(), 2, "one SUM per direction");
        assert!(sum_lines.iter().any(|l| l.ends_with("sender")));
        assert!(sum_lines.iter().any(|l| l.ends_with("receiver")));
    }

    #[test]
    fn format_summary_line_renders_retransmits_column() {
        let mut s = tcp_summary(1, true, 1_000_000);
        s.retransmits = Some(12);
        let line = format_summary_line(&s, 'm');
        assert!(line.contains(" 12 "), "Retr value should appear: {line}");
        assert!(line.ends_with("sender"));
    }
}
