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
    pub retransmits: Option<i64>,
    pub snd_cwnd: Option<u64>,
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
    /// Bidir role tag (`TX-C`/`RX-C`/`TX-S`/`RX-S`), rendered as iperf3's
    /// `[ ID][Role]` column (#184). `None` outside bidir. Tags the STREAM's
    /// direction, so both halves of a pair carry the same tag, and `[SUM]`
    /// grouping never mixes directions.
    pub role_tag: Option<&'static str>,
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

/// The line prefix: `[ ID]`, or `[ ID][Role]` when the summary carries a bidir
/// role tag (#184) — matching iperf3's bidir column.
fn fmt_id_role(id: i32, role_tag: Option<&'static str>) -> String {
    match role_tag {
        Some(tag) => format!("{}][{tag}", fmt_id(id)),
        None => fmt_id(id),
    }
}

/// Emit one human-readable report line, prefixed with the `-T/--title` string
/// when a title is active (#34). Every report line routes through this so the
/// prefix matches iperf3 without changing the public printer signatures.
fn titled(line: std::fmt::Arguments) {
    let rendered = format!(
        "{}{}{}",
        crate::macros::output_timestamp_prefix(),
        crate::macros::output_title_prefix(),
        line
    );
    // --get-server-output (#33): a capturing server TEES its report lines
    // into the exchange buffer while still printing — iperf3's iperf_printf
    // dual-writes (console + server_output_list).
    crate::macros::capture_line(&rendered);
    println!("{rendered}");
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

/// Print one `--json-stream` event line and flush immediately, so a piped
/// consumer sees the `start`/`end` event as soon as it is produced (#62). The
/// reporter flushes its own `interval` events via the per-tick flush.
pub(crate) fn emit_json_stream_line(line: &str) {
    use std::io::Write;
    println!("{line}");
    let _ = std::io::stdout().flush();
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
    let id = fmt_id_role(summary.stream_id, summary.role_tag);
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

/// Print a single final summary line. Test-only: production reporting routes
/// through `final_report_lines`; this helper exists for unit-testing the format.
#[cfg(test)]
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
/// summaries. Returns one SUM per (role, line-direction) group that has more
/// than one stream — matching iperf3, which prints a `[SUM]` for parallel
/// streams and omits it for a single stream. Grouping by the bidir role tag
/// keeps a `[SUM]` from ever mixing the two directions of a bidir run (#184):
/// a `P=1` bidir end block (one stream per direction, two lines each) gets no
/// SUM at all, exactly like iperf3. UDP SUM rows aggregate lost/total
/// datagrams and carry the MEAN jitter across the grouped streams, matching
/// iperf3's END block (`avg_jitter += sp->jitter` per stream of the
/// direction, then `avg_jitter /= test->num_streams` — #169).
pub fn sum_summaries(streams: &[StreamSummary]) -> Vec<StreamSummary> {
    // Distinct (role_tag, is_sender) keys in first-seen order, so SUM rows
    // come out in the same order iperf3 lists the groups.
    let mut keys: Vec<(Option<&'static str>, bool)> = Vec::new();
    for s in streams {
        let key = (s.role_tag, s.is_sender);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    let mut out = Vec::new();
    for (role_tag, is_sender) in keys {
        let group: Vec<&StreamSummary> = streams
            .iter()
            .filter(|s| s.is_sender == is_sender && s.role_tag == role_tag)
            .collect();
        if group.len() <= 1 {
            continue;
        }
        let bytes = group.iter().map(|s| s.bytes).sum();
        let is_udp = group.iter().any(|s| s.total_packets.is_some());
        let (jitter, lost, total_packets) = if is_udp {
            let lost = group.iter().filter_map(|s| s.lost).sum();
            let total = group.iter().filter_map(|s| s.total_packets).sum();
            // iperf3 averages jitter across the direction's streams in the
            // END block (it divides by num_streams, the group size — #169).
            let jitter_sum: f64 = group.iter().filter_map(|s| s.jitter).sum();
            let jitter = group
                .iter()
                .any(|s| s.jitter.is_some())
                .then(|| jitter_sum / group.len() as f64);
            (jitter, Some(lost), Some(total))
        } else {
            (None, None, None)
        };
        // Aggregate per-stream retransmits when present — live since #184 wired
        // the TCP sender lines' omit-adjusted totals into the producers; a SUM
        // over a TCP sender group carries the summed Retr iperf3 prints.
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
            role_tag,
        });
    }
    out
}

/// Print the column header iperf3 reprints above the final (end-block)
/// summaries (#184) — like the interval header, but TCP drops the `Cwnd`
/// column (per-stream cwnd is an interval-only figure) and bidir adds the
/// `[Role]` column. `with_retr` gates the TCP `Retr` column on retransmit info
/// actually being available (iperf3 omits it on Windows / for a peer without
/// it), mirroring the interval header's `has_retransmits`.
pub fn print_final_header(protocol: TransportProtocol, with_role: bool, with_retr: bool) {
    let role = if with_role { "[Role]" } else { "" };
    match protocol {
        TransportProtocol::Tcp => {
            let retr = if with_retr { "         Retr" } else { "" };
            titled(format_args!(
                "[ ID]{role} Interval           Transfer     Bitrate{retr}"
            ));
        }
        TransportProtocol::Udp => {
            titled(format_args!(
                "[ ID]{role} Interval           Transfer     Bitrate         Jitter    Lost/Total Datagrams"
            ));
        }
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
    pub json_stream: bool,
    /// Print interval lines live (text or json-stream). When false the reporter
    /// runs purely to collect intervals for the final `-J` blob (issue #36 PR2).
    pub print: bool,
    /// Datagram size, used to derive the UDP *sender's* per-interval packet count
    /// (the sender doesn't measure loss/jitter, so iperf3 reports only `packets`).
    pub blksize: usize,
    /// json-stream normally streams intervals without collecting; a SERVER
    /// whose client requested --get-server-output keeps them too, so the
    /// attached server_output_json carries populated intervals like iperf3's
    /// json_top under discard_json (#168).
    pub keep_intervals: bool,
}

/// A single TCP_INFO sample reused for the final interval (#55) when the socket
/// has already closed by the time the reporter flushes it.
#[derive(Clone, Copy)]
struct TcpSample {
    snd_cwnd: u64,
    snd_wnd: u64,
    rtt: u32,
    rttvar: u32,
    pmtu: u32,
    reorder: u32,
}

/// Per-stream sender-side TCP_INFO extremes accumulated across the run (#36 PR2),
/// for the `end.streams[].sender` object. Only meaningful for TCP sender streams.
#[derive(Debug, Default, Clone, Copy)]
pub struct StreamExtremes {
    pub stream_id: i32,
    pub max_snd_cwnd: u64,
    pub max_snd_wnd: u64,
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

/// End-of-test signal from the test driver to the interval reporter (#55).
///
/// The reporter's periodic ticks fall on idealized boundaries, but a run can end
/// part-way through an interval. The driver calls [`ReporterEnd::finish`] with the
/// authoritative elapsed test time at the exact moment the run ends; the reporter
/// then flushes one final interval `[last_boundary, end_secs]` and stops. Using the
/// driver's measured end time (rather than the reporter's own late, polled
/// detection) keeps the final interval's boundary and bitrate exact, and because
/// the driver signals *before* it tears the streams down, the final flush still
/// reads live TCP_INFO.
#[derive(Debug)]
pub struct ReporterEnd {
    notify: tokio::sync::Notify,
    end_secs_bits: std::sync::atomic::AtomicU64,
}

impl ReporterEnd {
    pub fn new() -> Self {
        Self {
            notify: tokio::sync::Notify::new(),
            end_secs_bits: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Signal that the run ended at `end_secs` elapsed (test-relative seconds).
    /// Wakes the reporter to emit its final partial interval up to `end_secs`.
    pub fn finish(&self, end_secs: f64) {
        self.end_secs_bits
            .store(end_secs.to_bits(), Ordering::Relaxed);
        self.notify.notify_one();
    }

    fn end_secs(&self) -> f64 {
        f64::from_bits(self.end_secs_bits.load(Ordering::Relaxed))
    }
}

impl Default for ReporterEnd {
    fn default() -> Self {
        Self::new()
    }
}

/// Omit-boundary-crossed signal for the `-n`/`-k` driver (#31, review r3):
/// the reporter sets it at the END of its boundary block — after the byte
/// baselines are snapshotted and the budget refilled — so the driver's first
/// post-warm-up end check runs against consistent net accounting. Gating that
/// check on a parallel wall clock instead provably opened before the
/// boundary's re-baselining (race C) and read gross-as-net.
pub struct OmitBoundary {
    passed: AtomicBool,
    notify: tokio::sync::Notify,
}

impl OmitBoundary {
    pub fn new() -> Self {
        Self {
            passed: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
        }
    }

    /// Reporter side: mark the boundary crossed and wake the waiting driver.
    /// `notify_one` stores a permit, so a driver that starts waiting after
    /// the boundary fired still wakes immediately. Release pairs with the
    /// fast path's Acquire so the boundary block's baseline/refill stores are
    /// visible to a driver that skips the Notify (which synchronizes on its
    /// own) — review r4.
    fn cross(&self) {
        self.passed.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Driver side: wait until the boundary has been crossed. `fallback`
    /// bounds the wait for liveness if the reporter died before its boundary
    /// (error paths); the caller then degrades to wall-clock gating.
    pub async fn crossed(&self, fallback: Duration) {
        if self.passed.load(Ordering::Acquire) {
            return;
        }
        tokio::select! {
            _ = self.notify.notified() => {}
            _ = tokio::time::sleep(fallback) => {}
        }
    }
}

impl Default for OmitBoundary {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-direction interval aggregates (#54). In bidir both directions run
/// concurrently; iperf3 sums each separately (`sum` + `sum_bidir_reverse`).
#[derive(Default)]
struct DirAcc {
    count: usize,
    bytes: u64,
    retransmits: i64,
    // UDP
    lost: i64,
    packets: i64,
    /// Sum of receiving streams' jitter — emitted as the MEAN, like iperf3's
    /// avg_jitter (#142). `udp_recv_count > 0` doubles as "this direction has
    /// UDP receiving streams".
    jitter_sum: f64,
    udp_recv_count: usize,
}

/// Build the typed `-J` interval sum for one direction's aggregates.
#[allow(clippy::too_many_arguments)] // interval geometry + direction flags, 1:1 with the emit site
fn direction_interval_sum(
    start: f64,
    end: f64,
    seconds: f64,
    acc: &DirAcc,
    dir_is_sender: bool,
    has_retransmits: bool,
    is_udp: bool,
    blk: u64,
    omitted: bool,
) -> crate::json_report::IntervalSum {
    let bps = if seconds > 0.0 {
        acc.bytes as f64 * 8.0 / seconds
    } else {
        0.0
    };
    // UDP: a receiving direction reports measured loss/jitter; a pure sending
    // direction reports only the sent packet count, like iperf3.
    let (jitter_ms, lost_packets, packets, lost_pct) = if is_udp && acc.udp_recv_count > 0 {
        (
            // iperf3 averages jitter across the direction's receiving
            // streams (avg_jitter /= num_streams, #142) — not last-wins.
            Some(acc.jitter_sum / acc.udp_recv_count.max(1) as f64 * 1000.0),
            Some(acc.lost),
            Some(acc.packets),
            Some(lost_percent(acc.lost, acc.packets)),
        )
    } else if is_udp {
        (None, None, Some((acc.bytes / blk) as i64), None)
    } else {
        (None, None, None, None)
    };
    crate::json_report::IntervalSum {
        start,
        end,
        seconds,
        bytes: acc.bytes,
        bits_per_second: bps,
        // iperf3 emits the sum's retransmits only on a sender-direction sum
        // (sender_has_retransmits && stream_must_be_sender). On a received-flow
        // sum (sender=false) it must be omitted, not just gated on "any stream
        // sends" — otherwise the received-flow sum carries a spurious count.
        retransmits: if has_retransmits && dir_is_sender {
            Some(acc.retransmits)
        } else {
            None
        },
        jitter_ms,
        lost_packets,
        packets,
        lost_percent: lost_pct,
        omitted,
        sender: dir_is_sender,
    }
}

/// Spawn an async task that prints interval reports periodically.
///
/// Returns `None` if interval reporting is disabled (interval_secs <= 0). On a
/// normal run the driver calls [`ReporterEnd::finish`] to flush the final partial
/// interval and stop the task; `done` is the fallback stop signal for error/early
/// teardown paths. The handle should be awaited after `finish`/`done`.
#[allow(clippy::too_many_arguments)] // reporter wiring, 1:1 with the drivers
pub fn spawn_interval_reporter(
    config: IntervalReporterConfig,
    streams: Vec<IntervalStreamRef>,
    done: Arc<AtomicBool>,
    reporter_end: Arc<ReporterEnd>,
    collector: Option<Arc<Mutex<CollectedIntervals>>>,
    byte_budget: Option<(Arc<std::sync::atomic::AtomicI64>, i64)>,
    boundary_signal: Option<Arc<OmitBoundary>>,
) -> Option<JoinHandle<()>> {
    if config.interval_secs < 0.0 {
        return None;
    }

    // `-i 0` (#107) means "one interval = the whole test" in iperf3, not "no
    // intervals". Use a period that won't fire within any realistic test so the
    // periodic tick never runs; the end-of-test flush (#55, below) then emits a
    // single [0, duration] interval (interval_num stays 0). A zero tokio
    // interval period panics, so it can't be `from_secs_f64(0.0)`.
    let interval_dur = if config.interval_secs > 0.0 {
        Duration::from_secs_f64(config.interval_secs)
    } else {
        Duration::from_secs(365 * 24 * 60 * 60)
    };
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
        // `-O/--omit` (#31): a real warm-up phase, like iperf3 — ticks during
        // it emit `omitted` intervals on a 0..omit timeline, then the boundary
        // snapshots every counter, the interval timeline restarts at 0, and
        // the summary covers only the post-omit window.
        let mut in_warmup = config.omit_secs > 0;
        let omit_deadline =
            tokio::time::Instant::now() + Duration::from_secs(config.omit_secs as u64);
        // Post-omit retransmit baselines: cumulative kernel counts at the
        // boundary, subtracted from the end-block extremes (iperf3 resets
        // stream retransmit stats at the omit boundary).
        let mut omit_retransmits: Vec<u32> = vec![0; streams.len()];

        // Per-stream previous values for computing deltas
        let mut prev_retransmits: Vec<u32> = vec![0; streams.len()];
        let mut prev_cnt_error: Vec<i64> = vec![0; streams.len()];
        let mut prev_packet_count: Vec<i64> = vec![0; streams.len()];
        // Last successfully sampled TCP_INFO per stream. Reused for the final
        // interval (#55) when the socket has already closed by the time it
        // flushes, so the final line still carries Cwnd/RTT like the periodic
        // ones rather than going blank.
        let mut last_tcp: Vec<Option<TcpSample>> = vec![None; streams.len()];

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

        // Emit one interval [start, end] (`omitted` marks warm-up under -O).
        // Shared by the periodic ticks and the final partial flush (#55) so both
        // render and collect identically. Each call drains the per-stream
        // interval counters, reporting exactly the bytes/stats accrued since the
        // previous call.
        let mut emit_interval = |start: f64,
                                 end: f64,
                                 omitted: bool,
                                 omit_boundary: bool,
                                 do_emit: bool| {
            if do_emit {
                let seconds = end - start;

                // The --timestamps prefix rides every titled() line now —
                // per line AND into the capture, like iperf3's prefixed
                // linebuffer (#168).

                // The text header banner is suppressed under --json-stream (pure NDJSON).
                if config.print && !config.json_stream && !header_printed {
                    print_header(config.protocol, has_retransmits);
                    header_printed = true;
                }

                // Per-direction aggregates (#54): `fwd` covers streams flowing the
                // same way as the first stream — the forward (client→server) flow
                // on both roles, since the client lists its senders first and the
                // server its receivers — `rev` the opposite. Non-bidir runs leave
                // `rev` empty.
                let fwd_is_sender = streams.first().is_none_or(|s| s.is_sender);
                let mut fwd = DirAcc::default();
                let mut rev = DirAcc::default();
                let mut collected_streams: Vec<crate::json_report::IntervalStream> = Vec::new();

                for (i, stream) in streams.iter().enumerate() {
                    let bytes = if stream.is_sender {
                        stream.counters.take_sent_interval()
                    } else {
                        stream.counters.take_received_interval()
                    };

                    // TCP_INFO for the interval detail and the end extremes.
                    let (retransmits, snd_cwnd, snd_wnd, rtt, rttvar, pmtu, reorder_iv) =
                        if has_retransmits && stream.is_sender {
                            if let Some(fd) = stream.raw_fd {
                                if let Some(info) = tcp_info::get_tcp_info(fd) {
                                    let delta =
                                        info.total_retransmits.saturating_sub(prev_retransmits[i]);
                                    prev_retransmits[i] = info.total_retransmits;
                                    // Accumulate sender-side extremes for the end report.
                                    let e = &mut acc_extremes[i];
                                    e.max_snd_cwnd = e.max_snd_cwnd.max(info.snd_cwnd);
                                    e.max_snd_wnd = e.max_snd_wnd.max(info.snd_wnd);
                                    e.reorder = e.reorder.max(info.reorder);
                                    if info.rtt > 0 {
                                        e.max_rtt = e.max_rtt.max(info.rtt);
                                        e.min_rtt = e.min_rtt.min(info.rtt);
                                        e.rtt_sum += info.rtt as u64;
                                        e.rtt_samples += 1;
                                    }
                                    e.total_retransmits = Some(info.total_retransmits);
                                    last_tcp[i] = Some(TcpSample {
                                        snd_cwnd: info.snd_cwnd,
                                        snd_wnd: info.snd_wnd,
                                        rtt: info.rtt,
                                        rttvar: info.rttvar,
                                        pmtu: info.pmtu,
                                        reorder: info.reorder,
                                    });
                                    (
                                        Some(delta as i64),
                                        Some(info.snd_cwnd),
                                        Some(info.snd_wnd),
                                        Some(info.rtt),
                                        Some(info.rttvar),
                                        Some(info.pmtu),
                                        Some(info.reorder),
                                    )
                                } else if let Some(s) = last_tcp[i] {
                                    // Final-interval fallback (#55): the socket closed as
                                    // the run ended, so a fresh read failed. Reuse the
                                    // last sample's cwnd/rtt; no new retransmit count is
                                    // measurable for the sub-interval, so report 0.
                                    (
                                        Some(0),
                                        Some(s.snd_cwnd),
                                        Some(s.snd_wnd),
                                        Some(s.rtt),
                                        Some(s.rttvar),
                                        Some(s.pmtu),
                                        Some(s.reorder),
                                    )
                                } else {
                                    (None, None, None, None, None, None, None)
                                }
                            } else {
                                (None, None, None, None, None, None, None)
                            }
                        } else {
                            (None, None, None, None, None, None, None)
                        };

                    // UDP stats (compute deltas for loss/packets)
                    let (jitter, lost, total) = if let Some(ref udp_stats) = stream.udp_recv_stats {
                        if let Ok(st) = udp_stats.lock() {
                            let delta_error = st.cnt_error - prev_cnt_error[i];
                            let delta_packets = st.packet_count - prev_packet_count[i];
                            prev_cnt_error[i] = st.cnt_error;
                            prev_packet_count[i] = st.packet_count;
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

                    // Text mode prints a per-stream line here. `--json-stream` emits
                    // one typed `interval` event per tick (assembled after the loop
                    // from the same typed streams the `-J` collector builds), so it
                    // has nothing to print per stream.
                    if config.print && !config.json_stream {
                        let interval = StreamInterval {
                            stream_id: stream.id,
                            start,
                            end,
                            bytes,
                            retransmits,
                            snd_cwnd,
                            jitter,
                            lost,
                            total_packets: total,
                            omitted,
                        };
                        print_interval(&interval, config.format_char);
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
                            // The live tcpi_snd_wnd where the platform reader
                            // captures it (Linux UAPI mirror / FreeBSD), like
                            // iperf3's get_snd_wnd (#161).
                            snd_wnd,
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

                    let acc = if stream.is_sender == fwd_is_sender {
                        &mut fwd
                    } else {
                        &mut rev
                    };
                    acc.count += 1;
                    acc.bytes += bytes;
                    if let Some(r) = retransmits {
                        acc.retransmits += r;
                    }
                    if stream.udp_recv_stats.is_some() {
                        acc.udp_recv_count += 1;
                        if let Some(j) = jitter {
                            acc.jitter_sum += j;
                        }
                    }
                    if let Some(l) = lost {
                        acc.lost += l;
                    }
                    if let Some(p) = total {
                        acc.packets += p;
                    }
                }

                // Print [SUM] line for parallel streams (text only; the json-stream
                // `interval` event below carries the typed `sum` instead).
                if config.print && !config.json_stream && config.num_streams > 1 {
                    // The text [SUM] stays one combined line (both directions in
                    // bidir); only the typed -J/json-stream sums split per direction.
                    // Jitter is the mean across receiving streams (#142).
                    let sum_jitter = if rev.udp_recv_count > 0 {
                        rev.jitter_sum / rev.udp_recv_count.max(1) as f64
                    } else {
                        fwd.jitter_sum / fwd.udp_recv_count.max(1) as f64
                    };
                    let sum_interval = StreamInterval {
                        stream_id: -1, // renders as "SUM"
                        start,
                        end,
                        bytes: fwd.bytes + rev.bytes,
                        retransmits: if has_retransmits {
                            Some(fwd.retransmits + rev.retransmits)
                        } else {
                            None
                        },
                        snd_cwnd: None,
                        jitter: if is_udp { Some(sum_jitter) } else { None },
                        lost: if is_udp {
                            Some(fwd.lost + rev.lost)
                        } else {
                            None
                        },
                        total_packets: if is_udp {
                            Some(fwd.packets + rev.packets)
                        } else {
                            None
                        },
                        omitted,
                    };
                    print_interval(&sum_interval, config.format_char);
                }

                if collecting {
                    // #54: iperf3 emits per-direction interval sums in bidir — `sum`
                    // for the forward flow, `sum_bidir_reverse` for the reverse —
                    // mirroring the end block's sum_*_bidir_reverse split.
                    let sum = direction_interval_sum(
                        start,
                        end,
                        seconds,
                        &fwd,
                        fwd_is_sender,
                        has_retransmits,
                        is_udp,
                        blk,
                        omitted,
                    );
                    let sum_bidir_reverse = (rev.count > 0).then(|| {
                        direction_interval_sum(
                            start,
                            end,
                            seconds,
                            &rev,
                            !fwd_is_sender,
                            has_retransmits,
                            is_udp,
                            blk,
                            omitted,
                        )
                    });
                    let interval = crate::json_report::Interval {
                        streams: collected_streams,
                        sum,
                        sum_bidir_reverse,
                    };
                    // `-J` collects intervals for the final batched blob; `--json-stream`
                    // emits each one live as `{"event":"interval","data":{...}}`.
                    if config.json_stream {
                        println!(
                            "{}",
                            crate::json_report::json_stream_event("interval", &interval)
                        );
                        // A json-stream SERVER additionally keeps them when
                        // the client requested --get-server-output: iperf3's
                        // discard_json exists precisely to retain the
                        // interval objects for the attached
                        // server_output_json (#168 r1 n2).
                        if config.keep_intervals {
                            collected.push(interval);
                        }
                    } else {
                        collected.push(interval);
                    }
                }

                // Flush after each interval if requested. --json-stream always flushes
                // so a piped consumer sees each event as it happens (the point of the
                // streaming format), regardless of --forceflush.
                if config.print && (config.forceflush || config.json_stream) {
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            } // do_emit

            // Omit boundary (#31): statistics reset, like iperf3's
            // iperf_reset_stats — interval counters drained (iperf3 zeroes the
            // per-interval bytes; the un-tick-aligned warm-up tail is
            // discarded, not emitted), byte baselines, UDP omitted_* counters
            // + delta prevs re-synced, a FRESH retransmit sample taken
            // (iperf3 does save_tcpinfo at reset — the last tick's value may
            // be stale), and the end-block extremes restart. Lives inside
            // this closure because prev_*/acc_extremes are its captures.
            if omit_boundary {
                // Order is load-bearing (review r3 blocker 3): baselines
                // FIRST, budget refill second. Refill-first let a sender wake
                // in the store→snapshot gap, claim post-refill budget, and
                // record bytes BEFORE the baseline — consumed budget excluded
                // from net, so net topped out below target and the forward
                // run hung. Baselines-first is safe: paused senders cannot
                // record new bytes until the refill lands.
                for (i, s) in streams.iter().enumerate() {
                    let _ = s.counters.take_sent_interval();
                    let _ = s.counters.take_received_interval();
                    s.counters.snapshot_omit();
                    if let Some(u) = &s.udp_recv_stats {
                        if let Ok(mut st) = u.lock() {
                            st.snapshot_omit();
                            prev_cnt_error[i] = st.cnt_error;
                            prev_packet_count[i] = st.packet_count;
                        }
                    }
                    if has_retransmits && s.is_sender {
                        if let Some(fd) = s.raw_fd {
                            if let Some(info) = tcp_info::get_tcp_info(fd) {
                                prev_retransmits[i] = info.total_retransmits;
                            }
                        }
                        // #171: the exchange subtracts this baseline from the
                        // sender's lifetime total, like iperf3's
                        // stream_prev_total_retrans at iperf_reset_stats.
                        s.counters.set_omit_retransmits(prev_retransmits[i] as i64);
                    }
                    omit_retransmits[i] = prev_retransmits[i];
                    acc_extremes[i] = StreamExtremes {
                        stream_id: s.id,
                        min_rtt: u32::MAX,
                        ..Default::default()
                    };
                }
                // -n/-k + -O (#31): refill the shared sender budget at the
                // boundary, where the byte baselines were just snapshotted,
                // so the limit and the net accounting share one boundary
                // instant (review r2).
                if let Some((b, target)) = &byte_budget {
                    b.store(*target, std::sync::atomic::Ordering::Relaxed);
                }
                // Wake the -n/-k driver LAST: its first post-warm-up check
                // must observe the baselines and the refill (review r3).
                if let Some(ob) = &boundary_signal {
                    ob.cross();
                }
            }
        };

        loop {
            // Wait for either the next interval boundary or the driver's
            // end-of-test signal. `biased` checks the end signal FIRST: the driver
            // sets `done` immediately after `finish` (to stop the senders at the
            // deadline), so the notify must win that race — otherwise the tick
            // branch would observe `done` and exit without flushing the final
            // interval. When `finish` and a boundary tick are both ready, that
            // boundary's data is folded into the recovered final interval below.
            tokio::select! {
                biased;
                _ = reporter_end.notify.notified() => {
                    // #55: the run ended part-way through an interval. Flush one
                    // final interval `[last_boundary, end_secs]` using the
                    // driver's authoritative end time, then stop.
                    //
                    // Skip a remainder that is zero-length (the run ended on a
                    // boundary — the sender driver passes the exact `-t`, so this
                    // is exact) OR carries no residual bytes (the receiver side:
                    // the peer has stopped, so its boundary-aligned tail is empty
                    // even though `end_secs` trails the boundary by the control
                    // round-trip).
                    let last_end = interval_num as f64 * config.interval_secs;
                    let end_secs = reporter_end.end_secs();
                    let residual_bytes: u64 = streams
                        .iter()
                        .map(|s| {
                            if s.is_sender {
                                s.counters.peek_sent_interval()
                            } else {
                                s.counters.peek_received_interval()
                            }
                        })
                        .sum();
                    if end_secs > last_end + 1e-3 && residual_bytes > 0 {
                        // Normal final partial flush; `in_warmup` is only true
                        // here when the run died before the boundary
                        // (error/abort path), tagging the flush omitted.
                        emit_interval(last_end, end_secs, in_warmup, false, true);
                    }
                    break;
                }
                _ = ticker.tick() => {
                    // `done` without a `finish` is the error/early-teardown path:
                    // stop without inventing a final interval.
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    interval_num += 1;
                    let start = (interval_num - 1) as f64 * config.interval_secs;
                    let end = interval_num as f64 * config.interval_secs;
                    emit_interval(start, end, in_warmup, false, true);
                }
                // Omit boundary (#31): ordered AFTER the ticker, matching
                // iperf3's coincident-timer behavior (its stats timer fires
                // before the omit timer, so -O 1 -i 1 emits the [0,1] omitted
                // line from the tick, then resets). iperf3 DISCARDS a warm-up
                // tail that isn't tick-aligned (stats are zeroed at reset, no
                // partial emission), so the closure's boundary block drains
                // and re-baselines without emitting. Its own sleep, so `-i 0`
                // (year-long ticker) still hits it.
                _ = tokio::time::sleep_until(omit_deadline), if in_warmup => {
                    emit_interval(0.0, 0.0, true, true, false);
                    // The interval timeline restarts at 0 and the ticker is
                    // re-phased (omit need not be a multiple of -i).
                    in_warmup = false;
                    interval_num = 0;
                    ticker = tokio::time::interval(interval_dur);
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    ticker.tick().await; // skip the immediate first tick
                }
            }
        }

        // Hand the collected samples + extremes to the client (#36 PR2).
        if let Some(c) = collector {
            if let Ok(mut g) = c.lock() {
                for (i, e) in acc_extremes.iter_mut().enumerate() {
                    if e.min_rtt == u32::MAX {
                        e.min_rtt = 0;
                    }
                    // End-block retransmit totals cover the post-omit window
                    // only (#31), like iperf3's boundary stats reset.
                    if let Some(t) = e.total_retransmits {
                        e.total_retransmits = Some(t.saturating_sub(omit_retransmits[i]));
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
            retransmits: None,
            snd_cwnd: None,
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
            role_tag: None,
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
            retransmits: Some(3),
            snd_cwnd: None,
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
            role_tag: None,
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

    // ---- direction_interval_sum (#54: bidir per-direction interval sums) ----

    #[test]
    fn direction_sum_tcp_sender_carries_retransmits() {
        let acc = DirAcc {
            count: 2,
            bytes: 2_000,
            retransmits: 3,
            ..Default::default()
        };
        let s = direction_interval_sum(0.0, 1.0, 1.0, &acc, true, true, false, 128, false);
        assert!(s.sender);
        assert_eq!(s.bytes, 2_000);
        assert_eq!(s.bits_per_second, 16_000.0);
        assert_eq!(s.retransmits, Some(3));
        assert!(
            s.jitter_ms.is_none() && s.packets.is_none(),
            "TCP sum has no UDP detail"
        );
    }

    #[test]
    fn direction_sum_tcp_receiver_omits_retransmits() {
        // Even when the test HAS retransmit info (the other direction sends),
        // a received-flow sum must not carry a retransmit count.
        let acc = DirAcc {
            count: 2,
            bytes: 4_000,
            retransmits: 0,
            ..Default::default()
        };
        let s = direction_interval_sum(0.0, 1.0, 1.0, &acc, false, true, false, 128, false);
        assert!(!s.sender);
        assert_eq!(s.retransmits, None);
    }

    #[test]
    fn direction_sum_udp_receiving_jitter_is_mean_across_streams() {
        // #142: iperf3's interval-sum jitter is the AVERAGE across the
        // direction's receiving streams (avg_jitter /= num_streams), not the
        // last stream's value.
        let acc = DirAcc {
            count: 2,
            bytes: 28_960,
            lost: 0,
            packets: 20,
            jitter_sum: 0.0030, // two receivers: 1ms + 2ms
            udp_recv_count: 2,
            ..Default::default()
        };
        let s = direction_interval_sum(0.0, 1.0, 1.0, &acc, false, false, true, 1448, false);
        assert_eq!(s.jitter_ms, Some(1.5), "mean of 1ms and 2ms");
    }

    #[test]
    fn direction_sum_udp_receiving_reports_measured_stats() {
        let acc = DirAcc {
            count: 1,
            bytes: 14_480,
            lost: 2,
            packets: 10,
            jitter_sum: 0.0015,
            udp_recv_count: 1,
            ..Default::default()
        };
        let s = direction_interval_sum(0.0, 1.0, 1.0, &acc, false, false, true, 1448, false);
        assert_eq!(s.jitter_ms, Some(1.5));
        assert_eq!(s.lost_packets, Some(2));
        assert_eq!(s.packets, Some(10));
        assert_eq!(s.lost_percent, Some(20.0));
    }

    #[test]
    fn direction_sum_udp_sending_reports_sent_packet_count_only() {
        let acc = DirAcc {
            count: 1,
            bytes: 14_480,
            ..Default::default()
        };
        let s = direction_interval_sum(0.0, 1.0, 1.0, &acc, true, false, true, 1448, false);
        assert_eq!(s.packets, Some(10), "sent packets = bytes / blksize");
        assert!(s.jitter_ms.is_none() && s.lost_packets.is_none() && s.lost_percent.is_none());
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
    fn sum_summaries_udp_aggregates_loss_and_mean_jitter() {
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
                role_tag: None,
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
                role_tag: None,
            },
        ];
        let sums = sum_summaries(&streams);
        assert_eq!(sums.len(), 1);
        let s = &sums[0];
        assert_eq!(s.bytes, 300_000);
        assert_eq!(s.lost, Some(7), "lost datagrams summed");
        assert_eq!(s.total_packets, Some(3000), "total datagrams summed");
        // iperf3's END-block SUM jitter is the MEAN across the direction's
        // streams (iperf_api.c: avg_jitter += sp->jitter per stream, then
        // avg_jitter /= test->num_streams), not the worst case (#169).
        assert_eq!(
            s.jitter,
            Some((0.010f64 + 0.025) / 2.0),
            "SUM jitter is the mean across streams"
        );
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

// ---------------------------------------------------------------------------
// Interval reporter edge cases (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod interval_reporter_tests {
    use crate::protocol::TransportProtocol;
    use crate::reporter::{spawn_interval_reporter, IntervalReporterConfig, ReporterEnd};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn zero_interval_spawns_whole_test_reporter() {
        // #107: `-i 0` means "one interval = the whole test" (iperf3 parity), so
        // the reporter IS spawned — it flushes a single [0, duration] interval at
        // end rather than ticking periodically. Only a negative interval is
        // rejected (see negative_interval_returns_none).
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: 0.0,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: true,
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        assert!(spawn_interval_reporter(
            config,
            vec![],
            done,
            Arc::new(ReporterEnd::new()),
            None,
            None,
            None
        )
        .is_some());
    }

    #[tokio::test]
    async fn negative_interval_returns_none() {
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: -1.0,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: true,
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        assert!(spawn_interval_reporter(
            config,
            vec![],
            done,
            Arc::new(ReporterEnd::new()),
            None,
            None,
            None
        )
        .is_none());
    }

    #[tokio::test]
    async fn zero_streams_doesnt_panic() {
        let done = Arc::new(AtomicBool::new(false));
        let config = IntervalReporterConfig {
            interval_secs: 0.5,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 0,
            forceflush: false,
            json_stream: false,
            print: true,
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        let handle = spawn_interval_reporter(
            config,
            vec![],
            done.clone(),
            Arc::new(ReporterEnd::new()),
            None,
            None,
            None,
        );
        assert!(handle.is_some());
        // Let it tick once then stop via the `done` fallback (no `finish`).
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        done.store(true, Ordering::Relaxed);
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    /// #55: a test that ends part-way through an interval must still emit a
    /// final partial interval carrying the residual bytes, rather than dropping
    /// it. Drives two full intervals, then a partial third, and asserts the last
    /// collected interval is the short partial (residual bytes, sub-interval
    /// duration) — not a full interval and not missing entirely.
    #[tokio::test]
    async fn final_partial_interval_is_emitted() {
        use crate::reporter::{CollectedIntervals, IntervalStreamRef};
        use crate::stream::StreamCounters;
        use std::sync::Mutex;
        use std::time::Duration;

        let interval = 0.5_f64;
        let counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let collector = Arc::new(Mutex::new(CollectedIntervals::default()));

        let stream_ref = IntervalStreamRef {
            id: 1,
            is_sender: true,
            counters: counters.clone(),
            udp_recv_stats: None,
            raw_fd: None,
        };
        let config = IntervalReporterConfig {
            interval_secs: interval,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: false, // collect-only; assert on the collector
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        let reporter_end = Arc::new(ReporterEnd::new());
        let report_start = std::time::Instant::now();
        let handle = spawn_interval_reporter(
            config,
            vec![stream_ref],
            done.clone(),
            reporter_end.clone(),
            Some(collector.clone()),
            None,
            None,
        )
        .expect("reporter spawns for a positive interval");

        // Two full intervals, each draining 1000 bytes at its tick. The 650ms
        // sleeps give ~150ms slack so each tick lands before the next batch.
        counters.record_sent(1000);
        tokio::time::sleep(Duration::from_millis(650)).await; // tick @0.5 -> [0,0.5]=1000
        counters.record_sent(1000);
        tokio::time::sleep(Duration::from_millis(650)).await; // tick @1.0 -> [0.5,1.0]=1000

        // Partial third interval: residual bytes, then end mid-interval (well
        // before the @1.5 tick) by signalling the authoritative end time, exactly
        // as the client/server driver does.
        counters.record_sent(500);
        tokio::time::sleep(Duration::from_millis(120)).await; // ~1.42s
        reporter_end.finish(report_start.elapsed().as_secs_f64());
        let _ = handle.await;

        let g = collector.lock().unwrap();
        let n = g.intervals.len();
        assert!(n >= 1, "expected at least one collected interval");
        let last = &g.intervals[n - 1];
        // The defining property of the fix: the last interval is the short
        // partial holding the 500 residual bytes — pre-fix it was dropped, so
        // the last interval would be a full [0.5,1.0]=1000 instead.
        assert_eq!(
            last.sum.bytes, 500,
            "final partial must carry the residual 500 bytes; got {} (intervals={n})",
            last.sum.bytes
        );
        let dur = last.sum.end - last.sum.start;
        assert!(
            dur > 0.0 && dur < interval,
            "final interval must be a sub-interval partial; start={} end={} dur={dur}",
            last.sum.start,
            last.sum.end
        );
    }

    /// #107: with `-i 0`, the reporter must emit exactly ONE interval covering
    /// the whole test (`[0, duration]`, all bytes) — not zero, and not periodic
    /// samples. Drives bytes across what would be several 1s intervals, then ends;
    /// no periodic tick may fire, and the single flushed interval carries the lot.
    #[tokio::test]
    async fn zero_interval_emits_single_whole_test_interval() {
        use crate::reporter::{CollectedIntervals, IntervalStreamRef};
        use crate::stream::StreamCounters;
        use std::sync::Mutex;
        use std::time::Duration;

        let counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let collector = Arc::new(Mutex::new(CollectedIntervals::default()));

        let stream_ref = IntervalStreamRef {
            id: 1,
            is_sender: true,
            counters: counters.clone(),
            udp_recv_stats: None,
            raw_fd: None,
        };
        let config = IntervalReporterConfig {
            interval_secs: 0.0, // -i 0
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: false, // collect-only; assert on the collector
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        let reporter_end = Arc::new(ReporterEnd::new());
        let report_start = std::time::Instant::now();
        let handle = spawn_interval_reporter(
            config,
            vec![stream_ref],
            done.clone(),
            reporter_end.clone(),
            Some(collector.clone()),
            None,
            None,
        )
        .expect("reporter spawns for -i 0 (#107)");

        // Accrue bytes across ~600ms (would be several 1s ticks); none must fire.
        counters.record_sent(1000);
        tokio::time::sleep(Duration::from_millis(300)).await;
        counters.record_sent(2000);
        tokio::time::sleep(Duration::from_millis(300)).await;
        reporter_end.finish(report_start.elapsed().as_secs_f64());
        let _ = handle.await;

        let g = collector.lock().unwrap();
        assert_eq!(
            g.intervals.len(),
            1,
            "-i 0 must emit exactly one whole-test interval, got {}",
            g.intervals.len()
        );
        let only = &g.intervals[0];
        assert_eq!(
            only.sum.bytes, 3000,
            "whole-test interval carries all bytes"
        );
        assert_eq!(only.sum.start, 0.0, "whole-test interval starts at 0");
        assert!(
            only.sum.end > 0.0,
            "whole-test interval ends at the measured duration; got {}",
            only.sum.end
        );
    }

    /// #55 guard: a run that ends exactly on an interval boundary must NOT emit a
    /// trailing partial — even when the sender is still writing into the socket
    /// at that instant (the saturating-TCP case). Records "slack" bytes after the
    /// last tick to model the live sender, then finishes on the boundary; the
    /// zero-length remainder must be dropped, so the slack bytes never surface as
    /// a spurious interval.
    #[tokio::test]
    async fn no_spurious_partial_when_ending_on_boundary() {
        use crate::reporter::{CollectedIntervals, IntervalStreamRef};
        use crate::stream::StreamCounters;
        use std::sync::Mutex;
        use std::time::Duration;

        let interval = 0.5_f64;
        let counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let collector = Arc::new(Mutex::new(CollectedIntervals::default()));

        let stream_ref = IntervalStreamRef {
            id: 1,
            is_sender: true,
            counters: counters.clone(),
            udp_recv_stats: None,
            raw_fd: None,
        };
        let config = IntervalReporterConfig {
            interval_secs: interval,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: false,
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        let reporter_end = Arc::new(ReporterEnd::new());
        let handle = spawn_interval_reporter(
            config,
            vec![stream_ref],
            done.clone(),
            reporter_end.clone(),
            Some(collector.clone()),
            None,
            None,
        )
        .expect("reporter spawns for a positive interval");

        // Two full intervals.
        counters.record_sent(1000);
        tokio::time::sleep(Duration::from_millis(650)).await; // tick @0.5
        counters.record_sent(1000);
        tokio::time::sleep(Duration::from_millis(650)).await; // tick @1.0, now ~1.3s

        // Live sender: bytes are still landing right as the run ends on the 1.0s
        // boundary. The authoritative end time is exactly 1.0, so the remainder is
        // zero-length and must be dropped despite these residual bytes.
        counters.record_sent(777);
        reporter_end.finish(1.0);
        let _ = handle.await;

        let g = collector.lock().unwrap();
        assert_eq!(
            g.intervals.len(),
            2,
            "no trailing partial when ending on a boundary; got {} intervals",
            g.intervals.len()
        );
        let last = g.intervals.last().unwrap();
        assert_eq!(
            last.sum.bytes, 1000,
            "last interval is the full [0.5,1.0]; the 777 slack bytes are not a new interval"
        );
    }

    /// #55 receiver-side guard: a receiver whose `end_secs` trails the last
    /// boundary (the control round-trip that delivers TEST_END) but whose tail
    /// has no residual bytes (the peer already stopped) must not emit a trailing
    /// empty interval. Mirrors the server's situation.
    #[tokio::test]
    async fn no_partial_for_receiver_with_no_residual() {
        use crate::reporter::{CollectedIntervals, IntervalStreamRef};
        use crate::stream::StreamCounters;
        use std::sync::Mutex;
        use std::time::Duration;

        let interval = 0.5_f64;
        let counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let collector = Arc::new(Mutex::new(CollectedIntervals::default()));

        let stream_ref = IntervalStreamRef {
            id: 1,
            is_sender: false, // receiver side
            counters: counters.clone(),
            udp_recv_stats: None,
            raw_fd: None,
        };
        let config = IntervalReporterConfig {
            interval_secs: interval,
            protocol: TransportProtocol::Tcp,
            format_char: 'a',
            omit_secs: 0,
            num_streams: 1,
            forceflush: false,
            json_stream: false,
            print: false,
            blksize: 128 * 1024,
            keep_intervals: false,
        };
        let reporter_end = Arc::new(ReporterEnd::new());
        let handle = spawn_interval_reporter(
            config,
            vec![stream_ref],
            done.clone(),
            reporter_end.clone(),
            Some(collector.clone()),
            None,
            None,
        )
        .expect("reporter spawns for a positive interval");

        // One full interval of received bytes, then the peer stops: no further
        // bytes arrive before the run ends.
        counters.record_received(1000);
        tokio::time::sleep(Duration::from_millis(650)).await; // tick @0.5 -> [0,0.5]=1000

        // end_secs trails the 0.5 boundary (as TEST_END would), but the tail is
        // empty — the residual-bytes guard must drop it.
        reporter_end.finish(0.62);
        let _ = handle.await;

        let g = collector.lock().unwrap();
        assert_eq!(
            g.intervals.len(),
            1,
            "no trailing empty interval for an idle receiver tail; got {} intervals",
            g.intervals.len()
        );
    }
}
