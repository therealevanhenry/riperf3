use crate::protocol::TransportProtocol;
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

/// Print the header line for interval reports.
pub fn print_header(protocol: TransportProtocol, has_retransmits: bool) {
    match protocol {
        TransportProtocol::Tcp => {
            if has_retransmits {
                println!(
                    "[ ID] Interval           Transfer     Bitrate         Retr  Cwnd"
                );
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
        // UDP format
        let pct = if total > 0 {
            lost as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:7.3} ms  {}/{} ({:.2}%)  {}",
            interval.stream_id,
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
        // TCP with retransmits
        let cwnd_str = units::format_bytes(cwnd as f64, 'A');
        println!(
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}   {:>10}  {}",
            interval.stream_id,
            interval.start,
            interval.end,
            transfer,
            rate,
            retr,
            cwnd_str,
            omit_tag,
        );
    } else {
        // TCP without retransmits or basic
        println!(
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {}",
            interval.stream_id, interval.start, interval.end, transfer, rate, omit_tag,
        );
    }
}

/// Print the separator line.
pub fn print_separator() {
    println!("- - - - - - - - - - - - - - - - - - - - - - - - -");
}

/// Print a final summary line.
pub fn print_summary(summary: &StreamSummary, format_char: char) {
    let transfer = units::format_bytes(summary.bytes as f64, format_char.to_ascii_uppercase());
    let seconds = summary.end - summary.start;
    let bits_per_sec = if seconds > 0.0 {
        summary.bytes as f64 * 8.0 / seconds
    } else {
        0.0
    };
    let rate = units::format_rate(bits_per_sec, format_char);
    let role = if summary.is_sender { "sender" } else { "receiver" };

    if let (Some(jitter), Some(lost), Some(total)) =
        (summary.jitter, summary.lost, summary.total_packets)
    {
        let pct = if total > 0 {
            lost as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:7.3} ms  {}/{} ({:.2}%)  {}",
            summary.stream_id,
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
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}  {:4}             {}",
            summary.stream_id, summary.start, summary.end, transfer, rate, retr, role,
        );
    } else {
        println!(
            "[{:3}] {:5.2}-{:<5.2} sec  {:>10}  {:>12}                    {}",
            summary.stream_id, summary.start, summary.end, transfer, rate, role,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_interval_tcp_basic() {
        let interval = StreamInterval {
            stream_id: 5,
            start: 0.0,
            end: 1.0,
            bytes: 1024 * 1024 * 1024, // 1 GiB
            is_sender: true,
            retransmits: None,
            snd_cwnd: None,
            rtt: None,
            jitter: None,
            lost: None,
            total_packets: None,
            omitted: false,
        };
        // Just verify it doesn't panic
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
}
