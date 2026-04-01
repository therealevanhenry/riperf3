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

/// Print the header line for interval reports.
pub fn print_header(protocol: TransportProtocol, has_retransmits: bool) {
    match protocol {
        TransportProtocol::Tcp => {
            if has_retransmits {
                println!("[ ID] Interval           Transfer     Bitrate         Retr  Cwnd");
            } else {
                println!("[ ID] Interval           Transfer     Bitrate");
            }
        }
        TransportProtocol::Udp => {
            println!(
                "[ ID] Interval           Transfer     Bitrate         Jitter    Lost/Total Datagrams"
            );
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
        let pct = if total > 0 {
            lost as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!(
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
        );
    } else if let (Some(retr), Some(cwnd)) = (interval.retransmits, interval.snd_cwnd) {
        let cwnd_str = units::format_bytes(cwnd as f64, 'A');
        println!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}   {:>10}  {}",
            interval.start, interval.end, transfer, rate, retr, cwnd_str, omit_tag,
        );
    } else {
        println!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {}",
            interval.start, interval.end, transfer, rate, omit_tag,
        );
    }
}

/// Print the separator line.
pub fn print_separator() {
    println!("- - - - - - - - - - - - - - - - - - - - - - - - -");
}

/// Print a final summary line.
pub fn print_summary(summary: &StreamSummary, format_char: char) {
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
        let pct = if total > 0 {
            lost as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!(
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
        );
    } else if let Some(retr) = summary.retransmits {
        println!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}             {}",
            summary.start, summary.end, transfer, rate, retr, role,
        );
    } else {
        println!(
            "[{id}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}                    {}",
            summary.start, summary.end, transfer, rate, role,
        );
    }
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
}

/// Spawn an async task that prints interval reports periodically.
///
/// Returns `None` if interval reporting is disabled (interval_secs <= 0).
/// The handle should be awaited after the test's `done` flag is set.
pub fn spawn_interval_reporter(
    config: IntervalReporterConfig,
    streams: Vec<IntervalStreamRef>,
    done: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    if config.interval_secs <= 0.0 {
        return None;
    }

    let interval_dur = Duration::from_secs_f64(config.interval_secs);
    let has_retransmits = tcp_info::has_retransmit_info()
        && config.protocol == TransportProtocol::Tcp
        && streams.iter().any(|s| s.is_sender);

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

        loop {
            ticker.tick().await;

            if done.load(Ordering::Relaxed) {
                break;
            }

            interval_num += 1;
            let omitted = interval_num <= omit_intervals;
            let start = (interval_num - 1) as f64 * config.interval_secs;
            let end = interval_num as f64 * config.interval_secs;

            // Timestamp prefix for this tick
            if config.timestamp_format.is_some() {
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

            if !header_printed {
                print_header(config.protocol, has_retransmits);
                header_printed = true;
            }

            let mut sum_bytes: u64 = 0;
            let mut sum_retransmits: i64 = 0;
            // UDP sums
            let mut sum_lost: i64 = 0;
            let mut sum_packets: i64 = 0;
            let mut last_jitter: f64 = 0.0;

            for (i, stream) in streams.iter().enumerate() {
                let bytes = if stream.is_sender {
                    stream.counters.take_sent_interval()
                } else {
                    stream.counters.take_received_interval()
                };

                // TCP_INFO for retransmits and cwnd
                let (retransmits, snd_cwnd, rtt) = if has_retransmits && stream.is_sender {
                    if let Some(fd) = stream.raw_fd {
                        if let Some(info) = tcp_info::get_tcp_info(fd) {
                            let delta = info.total_retransmits.saturating_sub(prev_retransmits[i]);
                            prev_retransmits[i] = info.total_retransmits;
                            (Some(delta as i64), Some(info.snd_cwnd), Some(info.rtt))
                        } else {
                            (None, None, None)
                        }
                    } else {
                        (None, None, None)
                    }
                } else {
                    (None, None, None)
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
                    let seconds = end - start;
                    let bps = if seconds > 0.0 {
                        bytes as f64 * 8.0 / seconds
                    } else {
                        0.0
                    };
                    let mut j = serde_json::json!({
                        "socket": stream.id,
                        "start": start,
                        "end": end,
                        "seconds": seconds,
                        "bytes": bytes,
                        "bits_per_second": bps,
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
            if config.num_streams > 1 {
                let is_udp = config.protocol == TransportProtocol::Udp;
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

            // Flush after each interval if requested
            if config.forceflush {
                use std::io::Write;
                let _ = std::io::stdout().flush();
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
}
