use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinHandle;

use crate::error::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// UDP header size with 32-bit sequence counter: sec(4) + usec(4) + seq(4).
pub const UDP_HEADER_SIZE_32: usize = 12;

/// UDP header size with 64-bit sequence counter: sec(4) + usec(4) + seq(8).
pub const UDP_HEADER_SIZE_64: usize = 16;

// ---------------------------------------------------------------------------
// Stream counters (lock-free, shared between data task and stats collector)
// ---------------------------------------------------------------------------

/// Atomic byte counters shared between the data-plane task and the stats
/// collector. The interval counters use swap-and-reset semantics so the
/// stats collector reads and clears them atomically each tick.
pub struct StreamCounters {
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    bytes_sent_interval: AtomicU64,
    bytes_received_interval: AtomicU64,
    // `-O/--omit` warm-up baselines (#31): cumulative counts at the omit
    // boundary. The net getters subtract them so summaries cover only the
    // post-omit window, like iperf3's stats reset in iperf_reset_stats.
    bytes_sent_omit: AtomicU64,
    bytes_received_omit: AtomicU64,
    /// #156: the sender's end-of-test retransmit total, snapshotted by the
    /// sender task while its socket is still open. The results exchange runs
    /// after the task has dropped (closed) the socket, so an exchange-time
    /// TCP_INFO read hits a dead fd. -1 = not captured (receiver, UDP, or no
    /// platform support).
    final_retransmits: AtomicI64,
    /// #171: the cumulative retransmit count at the omit boundary, stored by
    /// the reporter's boundary block — iperf3's iperf_reset_stats records
    /// stream_prev_total_retrans the same way, so the exchanged total covers
    /// the post-omit window only. -1 = no boundary crossed.
    omit_retransmits: AtomicI64,
}

impl Default for StreamCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamCounters {
    pub fn new() -> Self {
        Self {
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent_interval: AtomicU64::new(0),
            bytes_received_interval: AtomicU64::new(0),
            bytes_sent_omit: AtomicU64::new(0),
            bytes_received_omit: AtomicU64::new(0),
            final_retransmits: AtomicI64::new(-1),
            omit_retransmits: AtomicI64::new(-1),
        }
    }

    /// Record the omit boundary (#31): cumulative counts so far become the
    /// warm-up baseline the net getters subtract.
    pub fn snapshot_omit(&self) {
        self.bytes_sent_omit
            .store(self.bytes_sent.load(Ordering::Relaxed), Ordering::Relaxed);
        self.bytes_received_omit.store(
            self.bytes_received.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// Bytes sent since the omit boundary (the whole run when `-O` is unused).
    pub fn bytes_sent_net(&self) -> u64 {
        self.bytes_sent
            .load(Ordering::Relaxed)
            .saturating_sub(self.bytes_sent_omit.load(Ordering::Relaxed))
    }

    /// Bytes received since the omit boundary (see [`Self::bytes_sent_net`]).
    pub fn bytes_received_net(&self) -> u64 {
        self.bytes_received
            .load(Ordering::Relaxed)
            .saturating_sub(self.bytes_received_omit.load(Ordering::Relaxed))
    }

    /// Store the end-of-test retransmit total (#156; sender task only, while
    /// the socket is still open).
    pub fn set_final_retransmits(&self, n: i64) {
        self.final_retransmits.store(n, Ordering::Relaxed);
    }

    /// The snapshotted end-of-test retransmit total, or -1 if never captured.
    pub fn final_retransmits(&self) -> i64 {
        self.final_retransmits.load(Ordering::Relaxed)
    }

    /// Record the omit-boundary retransmit baseline (#171; reporter boundary
    /// block only).
    pub fn set_omit_retransmits(&self, n: i64) {
        self.omit_retransmits.store(n, Ordering::Relaxed);
    }

    /// Adjust a connection-lifetime retransmit total to the post-omit window
    /// (#171): subtract the boundary baseline when one was recorded, exactly
    /// iperf3's stream_retrans accounting after iperf_reset_stats.
    pub fn omit_adjusted_retransmits(&self, lifetime: i64) -> i64 {
        let base = self.omit_retransmits.load(Ordering::Relaxed);
        if base >= 0 {
            (lifetime - base).max(0)
        } else {
            lifetime
        }
    }

    /// Record bytes sent (called from the send loop hot path).
    pub fn record_sent(&self, n: u64) {
        self.bytes_sent.fetch_add(n, Ordering::Relaxed);
        self.bytes_sent_interval.fetch_add(n, Ordering::Relaxed);
    }

    /// Record bytes received (called from the recv loop hot path).
    pub fn record_received(&self, n: u64) {
        self.bytes_received.fetch_add(n, Ordering::Relaxed);
        self.bytes_received_interval.fetch_add(n, Ordering::Relaxed);
    }

    /// Atomically read and reset the sent interval counter.
    pub fn take_sent_interval(&self) -> u64 {
        self.bytes_sent_interval.swap(0, Ordering::Relaxed)
    }

    /// Atomically read and reset the received interval counter.
    pub fn take_received_interval(&self) -> u64 {
        self.bytes_received_interval.swap(0, Ordering::Relaxed)
    }

    /// Read the sent interval counter without clearing it, to test whether a
    /// final partial interval carries any residual bytes (#55).
    pub fn peek_sent_interval(&self) -> u64 {
        self.bytes_sent_interval.load(Ordering::Relaxed)
    }

    /// Read the received interval counter without clearing it (see
    /// [`Self::peek_sent_interval`]).
    pub fn peek_received_interval(&self) -> u64 {
        self.bytes_received_interval.load(Ordering::Relaxed)
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// UDP packet header
// ---------------------------------------------------------------------------

/// Header prepended to each UDP datagram on the wire.
/// Contains the sender's timestamp and a monotonically increasing sequence number.
#[derive(Debug, Clone, Copy)]
pub struct UdpHeader {
    pub sec: u32,
    pub usec: u32,
    pub seq: u64,
}

impl UdpHeader {
    /// Wire size in bytes.
    pub fn wire_size(use_64bit: bool) -> usize {
        if use_64bit {
            UDP_HEADER_SIZE_64
        } else {
            UDP_HEADER_SIZE_32
        }
    }

    /// Create a header stamped with the current time and the given sequence.
    pub fn new(seq: u64) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            sec: now.as_secs() as u32,
            usec: now.subsec_micros(),
            seq,
        }
    }

    /// Serialize into the first bytes of `buf`.
    pub fn write_to(&self, buf: &mut [u8], use_64bit: bool) {
        buf[0..4].copy_from_slice(&self.sec.to_be_bytes());
        buf[4..8].copy_from_slice(&self.usec.to_be_bytes());
        if use_64bit {
            buf[8..16].copy_from_slice(&self.seq.to_be_bytes());
        } else {
            buf[8..12].copy_from_slice(&(self.seq as u32).to_be_bytes());
        }
    }

    /// Deserialize from the first bytes of `buf`.
    pub fn read_from(buf: &[u8], use_64bit: bool) -> Option<Self> {
        if buf.len() < Self::wire_size(use_64bit) {
            return None;
        }
        let sec = u32::from_be_bytes(buf[0..4].try_into().ok()?);
        let usec = u32::from_be_bytes(buf[4..8].try_into().ok()?);
        let seq = if use_64bit {
            u64::from_be_bytes(buf[8..16].try_into().ok()?)
        } else {
            u32::from_be_bytes(buf[8..12].try_into().ok()?) as u64
        };
        Some(Self { sec, usec, seq })
    }
}

// ---------------------------------------------------------------------------
// UDP receiver statistics (jitter, loss, out-of-order)
// ---------------------------------------------------------------------------

/// Receiver-side UDP statistics tracking jitter (RFC 1889), packet loss,
/// and out-of-order delivery.
#[derive(Debug, Clone)]
pub struct UdpRecvStats {
    /// Highest sequence number seen so far.
    pub packet_count: i64,
    /// Cumulative packet loss count.
    pub cnt_error: i64,
    /// Out-of-order packets received.
    pub outoforder_packets: i64,
    /// Smoothed jitter estimate in seconds (RFC 1889 EWMA with 1/16 gain).
    pub jitter: f64,
    /// Previous one-way transit time for jitter calculation.
    prev_transit: f64,

    // Snapshots taken at the omit boundary (#31): the results exchange carries
    // gross packets/errors plus these omitted_* baselines, and the reading side
    // subtracts — exactly iperf3's accounting.
    pub omitted_packet_count: i64,
    pub omitted_cnt_error: i64,
    pub omitted_outoforder_packets: i64,
}

impl Default for UdpRecvStats {
    fn default() -> Self {
        Self::new()
    }
}

impl UdpRecvStats {
    pub fn new() -> Self {
        Self {
            packet_count: 0,
            cnt_error: 0,
            outoforder_packets: 0,
            jitter: 0.0,
            prev_transit: 0.0,
            omitted_packet_count: 0,
            omitted_cnt_error: 0,
            omitted_outoforder_packets: 0,
        }
    }

    /// Process a received datagram's header and update jitter/loss/OOO stats.
    pub fn update(&mut self, header: &UdpHeader, arrival_secs: f64) {
        let sent = header.sec as f64 + header.usec as f64 / 1_000_000.0;
        let transit = arrival_secs - sent;
        let pcount = header.seq as i64;

        // Jitter: RFC 1889 exponential moving average
        if self.packet_count > 0 {
            let d = (transit - self.prev_transit).abs();
            self.jitter += (d - self.jitter) / 16.0;
        }
        self.prev_transit = transit;

        // Loss and out-of-order detection
        if pcount > self.packet_count {
            if pcount > self.packet_count + 1 {
                self.cnt_error += (pcount - 1) - self.packet_count;
            }
            self.packet_count = pcount;
        } else {
            self.outoforder_packets += 1;
            if self.cnt_error > 0 {
                self.cnt_error -= 1;
            }
        }
    }

    /// Snapshot current values as the omit-period baseline.
    /// Called by the reporter at the omit boundary (#31).
    pub fn snapshot_omit(&mut self) {
        self.omitted_packet_count = self.packet_count;
        self.omitted_cnt_error = self.cnt_error;
        self.omitted_outoforder_packets = self.outoforder_packets;
        // iperf3's iperf_reset_stats also zeroes the jitter EWMA at the
        // boundary so warm-up influence doesn't bleed into the measurement.
        self.jitter = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Rate limiter (cumulative-average throttle for `-b` pacing, TCP path)
// ---------------------------------------------------------------------------

/// Cumulative-average rate limiter for application-level send pacing.
pub struct RateLimiter {
    rate_bytes_per_sec: f64,
    /// Wakeup quantum when behind schedule (`--pacing-timer`, iperf3 default
    /// 1000 µs).
    pacing: Duration,
    start: tokio::time::Instant,
    sent: u64,
    /// `-b rate/burst` (#160): blocks allowed per green light (0 = per-block).
    burst: u32,
    /// Blocks remaining in the current burst window (skip the throttle check).
    in_burst: u32,
}

impl RateLimiter {
    /// Create a rate limiter using iperf3's cumulative-average throttle
    /// (`iperf_check_throttle`): a send is green-lit whenever the cumulative
    /// bytes are at or below `elapsed * rate`, so total overshoot is bounded
    /// by ONE in-flight block at any rate — the old token bucket's burst floor
    /// (max(rate*0.1, 4*blksize), granted up front) overshot a low `-b` by 2x
    /// with TCP's 128 KiB default block (#116). High rates self-correct the
    /// other way: after an oversleep the average is behind, so blocks go out
    /// back-to-back with no sleep — burstiness ≈ rate × pacing quantum,
    /// matching the documented `--pacing-timer` semantics (iperf3 <= 3.17's
    /// timer-driven throttle; 3.18+ deprecated the quantum and sleeps exactly
    /// to the green-light instant — same long-run average either way).
    ///
    /// - `rate_bits_per_sec`: target send rate
    /// - `pacing_timer_us`: wakeup quantum when behind (`--pacing-timer`,
    ///   0 → iperf3's 1000 µs default)
    /// - `burst`: `-b rate/burst` block count (0 = unset → per-block checks)
    pub fn new(rate_bits_per_sec: u64, pacing_timer_us: u32, burst: u32) -> Self {
        let pacing_us = if pacing_timer_us == 0 {
            crate::utils::DEFAULT_PACING_TIMER_US
        } else {
            pacing_timer_us
        };
        Self {
            rate_bytes_per_sec: rate_bits_per_sec as f64 / 8.0,
            pacing: Duration::from_micros(pacing_us as u64),
            // tokio's Instant so the accuracy tests can run under start_paused.
            start: tokio::time::Instant::now(),
            sent: 0,
            burst,
            in_burst: 0,
        }
    }

    /// Wait until the cumulative average is at or below the target rate, then
    /// account `bytes` as sent. With a `-b rate/burst` count, blocks
    /// 2..=burst of a batch skip the check entirely — iperf3's multisend
    /// loop sends the whole burst per green light and only then re-checks
    /// the throttle (#160).
    pub async fn acquire(&mut self, bytes: u64) {
        if self.in_burst > 0 {
            self.in_burst -= 1;
            self.sent += bytes;
            return;
        }
        loop {
            let allowed = self.start.elapsed().as_secs_f64() * self.rate_bytes_per_sec;
            let behind = self.sent as f64 - allowed;
            if behind <= 0.0 {
                break;
            }
            // Sleep to the green-light instant, but never less than the pacing
            // quantum — the documented --pacing-timer semantics (iperf3
            // <= 3.17's minimum wakeup was one tick; 3.18+ deprecated the
            // quantum). The cumulative math absorbs any oversleep.
            let to_green = Duration::from_secs_f64(behind / self.rate_bytes_per_sec);
            tokio::time::sleep(to_green.max(self.pacing)).await;
        }
        self.sent += bytes;
        self.in_burst = self.burst.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// DataStream: a live data stream backed by a tokio task
// ---------------------------------------------------------------------------

/// A running data stream with its background task handle and shared state.
pub struct DataStream {
    /// Stream identifier (shown as `[ ID]` in output).
    pub id: i32,
    pub is_sender: bool,
    pub counters: Arc<StreamCounters>,
    /// UDP-only: receiver-side jitter/loss stats behind a mutex.
    pub udp_recv_stats: Option<Arc<Mutex<UdpRecvStats>>>,
    /// The background send/recv task.
    pub task: JoinHandle<Result<()>>,
    /// Raw TCP socket fd for TCP_INFO queries. `None` for UDP streams.
    pub raw_fd: Option<i32>,
    /// This stream's actual local/peer socket addresses, captured at creation
    /// for the `-J` `start.connected` block (issue #36). `None` if unavailable.
    pub local_addr: Option<std::net::SocketAddr>,
    pub peer_addr: Option<std::net::SocketAddr>,
    /// Actual SO_SNDBUF/SO_RCVBUF on this stream's socket, captured at creation
    /// for the `-J` `start.sndbuf_actual`/`rcvbuf_actual` fields (#36 PR3).
    pub sndbuf_actual: Option<u64>,
    pub rcvbuf_actual: Option<u64>,
    /// The TCP congestion-control algorithm actually in effect on this stream's
    /// socket (read back via `getsockopt(TCP_CONGESTION)`), for the `congestion_used`
    /// report field (#37). `None` for UDP and on platforms without TCP_CONGESTION.
    pub congestion_used: Option<String>,
}

impl DataStream {
    /// The omit-adjusted lifetime retransmit total to render/exchange for a TCP
    /// sending stream, or `None` for a receiver, a UDP stream, or a platform
    /// without TCP_INFO retransmits (Windows) — where iperf3 omits the `Retr`
    /// column entirely. The sender task snapshots the count while the socket is
    /// open (#156); a live fd read is only a fallback. With `-O` the boundary
    /// baseline is subtracted (#171), like iperf3's `stream_retrans` after
    /// `iperf_reset_stats`. `Some(-1)` means info exists but the value was
    /// unavailable (iperf3's sentinel, rendered literally).
    pub(crate) fn sender_retransmits(&self, is_udp: bool) -> Option<i64> {
        if !self.is_sender || is_udp || !crate::tcp_info::has_retransmit_info() {
            return None;
        }
        let lifetime = match self.counters.final_retransmits() {
            n if n >= 0 => Some(n),
            _ => self
                .raw_fd
                .and_then(crate::tcp_info::get_tcp_info)
                .map(|i| i.total_retransmits as i64),
        };
        Some(
            lifetime
                .map(|t| self.counters.omit_adjusted_retransmits(t))
                .unwrap_or(-1),
        )
    }
}

/// Build the shared `-n`/`-k` byte budget the sending streams decrement, or
/// `None` when there is no limit. A `0` limit means **unlimited** — iperf3 sends
/// `num`/`blocks` = 0 for a plain `-t` run, so it must be filtered out or a
/// reverse iperf3 client would make the server build a 0-budget and send
/// nothing. An absurd N is clamped to `i64::MAX` so the budget can never start
/// non-positive and stall every sender.
pub(crate) fn make_byte_budget(
    bytes: Option<u64>,
    blocks: Option<u64>,
    blksize: usize,
) -> Option<Arc<AtomicI64>> {
    bytes
        .filter(|&n| n > 0)
        .or_else(|| {
            blocks
                .filter(|&k| k > 0)
                .map(|k| k.saturating_mul(blksize as u64))
        })
        .map(|n| Arc::new(AtomicI64::new(i64::try_from(n).unwrap_or(i64::MAX))))
}

// ---------------------------------------------------------------------------
// TCP send / recv loops
// ---------------------------------------------------------------------------

/// #156: snapshot the sender's final retransmit total into its counters while
/// the socket is still open. Called by every TCP sender just before it
/// returns (and drops the socket): the results exchange runs after that drop,
/// so an exchange-time TCP_INFO read would hit a closed fd and ship the -1
/// sentinel beside `sender_has_retransmits=1` — which an iperf3 peer renders
/// as a bogus Retr count (u64::MAX on 3.12).
fn snapshot_final_retransmits(stream: &TcpStream, counters: &StreamCounters) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(info) = crate::tcp_info::get_tcp_info(stream.as_raw_fd()) {
            counters.set_final_retransmits(info.total_retransmits as i64);
        }
    }
    #[cfg(not(unix))]
    let _ = (stream, counters);
}

/// TCP sender: writes full blocks as fast as the kernel will accept them.
/// If `file_path` is set, reads from the file into the buffer each iteration.
#[allow(clippy::too_many_arguments)] // hot-path sender; knobs map 1:1 to CLI flags
pub async fn run_tcp_sender(
    mut stream: TcpStream,
    counters: Arc<StreamCounters>,
    mut buf: Vec<u8>,
    done: Arc<AtomicBool>,
    file_path: Option<std::path::PathBuf>,
    rate: u64,
    pacing_timer_us: u32,
    burst: u32,
    byte_budget: Option<Arc<AtomicI64>>,
) -> Result<()> {
    use std::io::Read;
    let mut file = file_path.as_ref().map(std::fs::File::open).transpose()?;
    // `-b` pacing: iperf3's cumulative-average throttle caps the application
    // send rate, waking on the `--pacing-timer` quantum (#32/#116). 0 =
    // unlimited → no limiter, so the default TCP path is unchanged (#102;
    // mirrors UDP's `-b 0` per #17). A `-b rate/burst` count lets `burst`
    // blocks through per green light (#160).
    let mut limiter = (rate > 0).then(|| RateLimiter::new(rate, pacing_timer_us, burst));

    while !done.load(Ordering::Relaxed) {
        // `-n`/`-k` byte/block limit: claim this block from the shared budget and
        // stop when it is exhausted, so the sender stops at ~N instead of
        // free-running until the controller's next 100ms poll. The budget is
        // shared across the sending streams — iperf3's `-n` is the test-wide
        // total, consumed across streams as each sends. The claim has NO undo:
        // a fetch_sub + compensating fetch_add could interleave with the omit
        // boundary's refill store and land the undo on the fresh budget,
        // leaking a block past the target (review r3). fetch_update claims
        // only from a positive budget, so only one claim can drive it
        // non-positive — BUDGET overshoot is bounded to less than one block.
        // The recorded post-omit net can additionally exceed N by one
        // in-flight block per sending stream (claimed pre-refill, recorded
        // post-snapshot), so a paced `-P k` run can land at N + k blocks —
        // don't pin the 1-block figure in tests (review r4).
        if let Some(b) = &byte_budget {
            let len = buf.len() as i64;
            if b.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                (v > 0).then_some(v - len)
            })
            .is_err()
            {
                // iperf3's sender IDLES at the limit instead of exiting (its
                // mt sender checks bytes_sent >= N per burst, including during
                // an -O warm-up, and resumes when the boundary resets the
                // counter). Wait for a refill (#31) or the driver's `done`
                // (the normal end, set once the post-omit target is reached).
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        }

        // Refill buffer from file if specified
        if let Some(ref mut f) = file {
            let n = f.read(&mut buf).unwrap_or(0);
            if n == 0 {
                // EOF — rewind and retry
                use std::io::Seek;
                f.seek(std::io::SeekFrom::Start(0))?;
                let _ = f.read(&mut buf);
            }
        }

        if let Some(rl) = limiter.as_mut() {
            rl.acquire(buf.len() as u64).await;
            // The green-light sleep can outlast the test at very low -b:
            // re-check `done` after waking instead of writing one more block
            // past the end, like modern iperf3's send worker (#160).
            if done.load(Ordering::Relaxed) {
                break;
            }
        }

        match stream.write_all(&buf).await {
            Ok(()) => counters.record_sent(buf.len() as u64),
            Err(e)
                if e.kind() == std::io::ErrorKind::BrokenPipe
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }
    snapshot_final_retransmits(&stream, &counters);
    Ok(())
}

/// TCP sender using sendfile() for zero-copy transmission (Linux only).
/// Creates a temp file with the send buffer content, then uses sendfile()
/// to transfer directly from the page cache to the socket.
#[cfg(target_os = "linux")]
pub async fn run_tcp_sender_zerocopy(
    stream: TcpStream,
    counters: Arc<StreamCounters>,
    buf: Vec<u8>,
    done: Arc<AtomicBool>,
) -> Result<()> {
    use std::io::Write;

    // Create temp file with buffer content
    let mut tmpfile = tempfile()?;
    tmpfile.write_all(&buf)?;
    let blksize = buf.len();

    loop {
        // Wait for socket to be writable
        stream.writable().await?;

        if done.load(Ordering::Relaxed) {
            break;
        }

        let result = stream.try_io(tokio::io::Interest::WRITABLE, || {
            let mut offset: libc::off_t = 0;
            nix::sys::sendfile::sendfile(&stream, &tmpfile, Some(&mut offset), blksize)
                .map_err(std::io::Error::from)
        });

        match result {
            Ok(n) if n > 0 => counters.record_sent(n as u64),
            Ok(_) => break, // 0 = closed
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::BrokenPipe
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }
    snapshot_final_retransmits(&stream, &counters);
    Ok(())
}

/// Zerocopy sender for macOS — uses macOS sendfile (reversed fd order, returns bytes via tuple).
#[cfg(target_os = "macos")]
pub async fn run_tcp_sender_zerocopy(
    stream: TcpStream,
    counters: Arc<StreamCounters>,
    buf: Vec<u8>,
    done: Arc<AtomicBool>,
) -> Result<()> {
    use std::io::Write;

    let mut tmpfile = tempfile()?;
    tmpfile.write_all(&buf)?;
    let blksize = buf.len();

    loop {
        stream.writable().await?;

        if done.load(Ordering::Relaxed) {
            break;
        }

        let result = stream.try_io(tokio::io::Interest::WRITABLE, || {
            let (res, bytes_sent) = nix::sys::sendfile::sendfile(
                &tmpfile,
                &stream,
                0, // offset
                Some(blksize as libc::off_t),
                None, // headers
                None, // trailers
            );
            match res {
                Ok(()) => Ok(bytes_sent as usize),
                Err(e) => {
                    if bytes_sent > 0 {
                        Ok(bytes_sent as usize)
                    } else {
                        Err(std::io::Error::from(e))
                    }
                }
            }
        });

        match result {
            Ok(n) if n > 0 => counters.record_sent(n as u64),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::BrokenPipe
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }
    snapshot_final_retransmits(&stream, &counters);
    Ok(())
}

/// Zerocopy sender for FreeBSD — uses FreeBSD sendfile (reversed fd order, SfFlags).
#[cfg(target_os = "freebsd")]
pub async fn run_tcp_sender_zerocopy(
    stream: TcpStream,
    counters: Arc<StreamCounters>,
    buf: Vec<u8>,
    done: Arc<AtomicBool>,
) -> Result<()> {
    use std::io::Write;

    let mut tmpfile = tempfile()?;
    tmpfile.write_all(&buf)?;
    let blksize = buf.len();

    loop {
        stream.writable().await?;

        if done.load(Ordering::Relaxed) {
            break;
        }

        let result = stream.try_io(tokio::io::Interest::WRITABLE, || {
            let (res, bytes_sent) = nix::sys::sendfile::sendfile(
                &tmpfile,
                &stream,
                0, // offset
                Some(blksize),
                None, // headers
                None, // trailers
                nix::sys::sendfile::SfFlags::empty(),
                0, // readahead
            );
            match res {
                Ok(()) => Ok(bytes_sent as usize),
                Err(e) => {
                    if bytes_sent > 0 {
                        Ok(bytes_sent as usize)
                    } else {
                        Err(std::io::Error::from(e))
                    }
                }
            }
        });

        match result {
            Ok(n) if n > 0 => counters.record_sent(n as u64),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::BrokenPipe
                    || e.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break
            }
            Err(e) => return Err(e.into()),
        }
    }
    snapshot_final_retransmits(&stream, &counters);
    Ok(())
}

/// Create a temporary file for zerocopy sends. Gated to the targets whose
/// `run_tcp_sender_zerocopy` impls call it; a broader `#[cfg(unix)]` would be
/// dead code (and warn) on other-Unix, which has no zerocopy sender (#78).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
fn tempfile() -> std::io::Result<std::fs::File> {
    use std::io::Seek;
    let mut f = tempfile_in(std::env::temp_dir())?;
    f.seek(std::io::SeekFrom::Start(0))?;
    Ok(f)
}

/// A unique temp-file name for one zerocopy sender. The `<pid>-<seq>` form keeps
/// every sender's backing file distinct: with `-Z -P >1` each sender opened its
/// own temp file under the same fixed `.riperf3-zc-<pid>` name and raced
/// create+truncate on that shared path (#42). A monotonic per-process sequence
/// removes the shared name entirely, so the senders can never collide.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
fn zc_tempfile_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(".riperf3-zc-{}-{}", std::process::id(), seq)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
fn tempfile_in(dir: std::path::PathBuf) -> std::io::Result<std::fs::File> {
    let path = dir.join(zc_tempfile_name());
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    // Unlink immediately — file stays open but invisible
    let _ = std::fs::remove_file(&path);
    Ok(f)
}

/// Hard cap on how long the receiver drains the data socket after its test
/// duration ends, waiting for the peer to close it (issue #23). A well-behaved
/// peer (iperf3/riperf3) closes its data socket at teardown, so EOF normally
/// arrives long before this — even on a high-RTT link where result exchange runs
/// for seconds, or one lossy enough to stall the stream past a retransmit
/// timeout. The cap is *only* a hang guard: it bounds a peer that stops sending
/// but never closes, so the receiver task — and the client's join on it — can't
/// block forever. Deliberately generous, since closing early is what reopens the
/// EPIPE this fix exists to prevent.
const RECEIVER_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a receive loop returned: the test duration ended (`done`), or the peer
/// closed/reset the socket first. Only `Done` needs a post-loop drain — a peer
/// that already closed has nothing left to send and cannot be EPIPE'd.
enum RecvStop {
    Done,
    PeerClosed,
}

/// TCP receiver: reads until the peer closes the connection or `done` is set.
/// If `skip_rx_copy` is true, uses MSG_TRUNC to avoid copying data (Linux only).
/// If `file_path` is set, writes received data to the file.
pub async fn run_tcp_receiver(
    mut stream: TcpStream,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    skip_rx_copy: bool,
    file_path: Option<std::path::PathBuf>,
) -> Result<()> {
    let mut buf = vec![0u8; blksize];
    let mut file = file_path
        .as_ref()
        .map(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(p)
        })
        .transpose()?;

    let stop = if skip_rx_copy {
        #[cfg(target_os = "linux")]
        {
            run_tcp_receiver_msgtrunc(&mut stream, &counters, &mut buf, &done).await?
        }
        #[cfg(not(target_os = "linux"))]
        {
            run_tcp_receiver_normal(&mut stream, &counters, &mut buf, &done, &mut file).await?
        }
    } else {
        run_tcp_receiver_normal(&mut stream, &counters, &mut buf, &done, &mut file).await?
    };

    // Reverse/bidir teardown (issue #23): once our duration ends we must not slam
    // the data socket shut while the peer sender is still writing — a remote
    // iperf3 (<= 3.12) treats the resulting EPIPE as fatal and aborts the whole
    // control connection, which we'd see as `PeerDisconnected`. iperf3 keeps data
    // sockets open through result exchange and lets the peer initiate the close.
    // Mirror that: drain (read and discard, without counting) and let the peer
    // close first, so the teardown is always clean from its side.
    if matches!(stop, RecvStop::Done) {
        drain_until_peer_closes(&mut stream, &mut buf).await;
    }
    Ok(())
}

/// After the test ends, hold the data socket open and drain (read and discard)
/// until the peer closes it (EOF) — never closing first, so we can't EPIPE a peer
/// still finishing its send (issue #23). Waiting on the peer's close, rather than
/// on a silence window, is what makes this robust on slow or lossy links: a
/// mid-stream stall longer than any fixed grace would otherwise look like "peer
/// done" and trip an early close. `RECEIVER_DRAIN_TIMEOUT` is only a hang guard
/// for a peer that goes silent but never closes.
async fn drain_until_peer_closes(stream: &mut TcpStream, buf: &mut [u8]) {
    let _ = tokio::time::timeout(RECEIVER_DRAIN_TIMEOUT, async {
        loop {
            match stream.read(buf).await {
                Ok(0) => break,    // peer closed — fully drained
                Ok(_) => continue, // in-flight data — discard, keep the socket open
                Err(_) => break,   // socket error — nothing left to drain
            }
        }
    })
    .await;
}

#[cfg(target_os = "linux")]
async fn run_tcp_receiver_msgtrunc(
    stream: &mut TcpStream,
    counters: &StreamCounters,
    buf: &mut [u8],
    done: &AtomicBool,
) -> Result<RecvStop> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    loop {
        if done.load(Ordering::Relaxed) {
            return Ok(RecvStop::Done);
        }
        stream.readable().await?;
        let n = stream.try_io(tokio::io::Interest::READABLE, || {
            nix::sys::socket::recv(fd, buf, nix::sys::socket::MsgFlags::MSG_TRUNC)
                .map_err(std::io::Error::from)
        });
        match n {
            Ok(0) => return Ok(RecvStop::PeerClosed),
            Ok(n) => counters.record_received(n as u64),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                return Ok(RecvStop::PeerClosed)
            }
            Err(e) => return Err(e.into()),
        }
    }
}

async fn run_tcp_receiver_normal(
    stream: &mut TcpStream,
    counters: &StreamCounters,
    buf: &mut [u8],
    done: &AtomicBool,
    file: &mut Option<std::fs::File>,
) -> Result<RecvStop> {
    loop {
        if done.load(Ordering::Relaxed) {
            return Ok(RecvStop::Done);
        }
        match stream.read(buf).await {
            Ok(0) => return Ok(RecvStop::PeerClosed),
            Ok(n) => {
                counters.record_received(n as u64);
                if let Some(ref mut f) = file {
                    use std::io::Write;
                    let _ = f.write_all(&buf[..n]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                return Ok(RecvStop::PeerClosed)
            }
            Err(e) => return Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// UDP send / recv loops
// ---------------------------------------------------------------------------

/// UDP sender: sends datagrams with a timestamp+sequence header, paced by the
/// rate limiter if present.
// Unwired async variant (prod uses the blocking/sendmmsg senders); kept in
// parity with run_udp_receiver. Retract/gate decision tracked in #125.
#[allow(dead_code)]
pub async fn run_udp_sender(
    socket: UdpSocket,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    mut rate_limiter: Option<RateLimiter>,
    use_64bit: bool,
) -> Result<()> {
    let mut buf = vec![0u8; blksize];
    let mut seq: u64 = 0;

    while !done.load(Ordering::Relaxed) {
        if let Some(ref mut limiter) = rate_limiter {
            limiter.acquire(blksize as u64).await;
        }

        seq += 1;
        UdpHeader::new(seq).write_to(&mut buf, use_64bit);

        match socket.send(&buf).await {
            Ok(n) => counters.record_sent(n as u64),
            Err(e) => {
                log::debug!("UDP send error: {e}");
                seq -= 1; // allow retry with the same sequence
            }
        }
    }
    Ok(())
}

/// UDP receiver: receives datagrams, counts bytes, and tracks jitter/loss/OOO.
// Unwired async variant (see the parity note in its drain block); kept
// alongside run_udp_sender. Retract/gate decision tracked in #125.
#[allow(dead_code)]
pub async fn run_udp_receiver(
    socket: UdpSocket,
    counters: Arc<StreamCounters>,
    udp_stats: Arc<Mutex<UdpRecvStats>>,
    blksize: usize,
    done: Arc<AtomicBool>,
    use_64bit: bool,
) -> Result<()> {
    // Buffer large enough for the negotiated block size or a jumbo datagram
    let mut buf = vec![0u8; blksize.max(65536)];

    let mut drain = false;
    loop {
        if done.load(Ordering::Relaxed) {
            drain = true;
            break;
        }

        // Short timeout so we can periodically re-check the done flag.
        let recv = tokio::time::timeout(Duration::from_millis(500), socket.recv(&mut buf)).await;

        match recv {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                counters.record_received(n as u64);

                if let Some(header) = UdpHeader::read_from(&buf[..n], use_64bit) {
                    let arrival = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();

                    if let Ok(mut stats) = udp_stats.lock() {
                        stats.update(&header, arrival);
                    }
                }
            }
            Ok(Err(e)) => log::debug!("UDP recv error: {e}"),
            Err(_) => { /* timeout — re-check done flag */ }
        }
    }
    // Hold the socket open and drain late datagrams until the peer goes quiet, so
    // a still-sending iperf3 <=3.12 peer isn't reset at teardown (issue #48). The
    // async sibling of the blocking receiver's drain; a recv that times out with
    // no datagram means the peer has stopped. (This async variant is currently
    // unwired — the client/server use run_udp_receiver_blocking — but kept in
    // parity so the two don't diverge.)
    if drain {
        let deadline = tokio::time::Instant::now() + UDP_RECEIVER_DRAIN_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), socket.recv(&mut buf)).await {
                // any datagram (incl. 0-byte — UDP has no EOF): discard, keep open
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => break,
                Err(_) => break, // silence: the peer has stopped
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Blocking UDP send / recv (high-performance, no async overhead)
// ---------------------------------------------------------------------------

/// Block until the start barrier is released so a UDP sender does not transmit
/// during stream setup (issue #5). The UDP create-streams handshake is sent on
/// the same port as the data and is silently lost under a high-rate flood, so
/// if early streams start blasting while later streams are still handshaking,
/// setup stalls forever. Holding all senders until every stream is created
/// keeps the handshake clean. Returns `false` if the test was torn down before
/// starting (the caller should just return).
fn await_start(start: &AtomicBool, done: &AtomicBool) -> bool {
    while !start.load(Ordering::Relaxed) {
        if done.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// How long the control plane waits for spawned stream data threads to check
/// in before opening the test window anyway (#178). Generous: the worst stall
/// observed on loaded windows-latest runners was ~3.5 s; a barrier timeout
/// only recreates the pre-fix behavior, so erring long costs nothing in the
/// normal case (the wait ends the moment the threads are up). Note a blocking
/// pool persistently sized below the stream count (an embedder's custom
/// runtime) burns this in full on every setup — the queued threads cannot
/// enter until earlier streams finish, which is the whole test.
pub(crate) const STREAM_THREAD_START_TIMEOUT: Duration = Duration::from_secs(10);

/// Readiness gate for stream data threads (#178).
///
/// The UDP data plane runs on `spawn_blocking` OS threads while the `-t` test
/// clock is driven by the async control plane. On a loaded host (2-core CI
/// runners), OS-thread creation can stall for seconds — the whole test window
/// can elapse before a single data thread runs, completing a run "normally"
/// with zero bytes (the late receiver goes straight to its post-test drain and
/// discards everything the peer sent). Spawning through the gate makes each
/// task release a permit as its first action; the control plane awaits the
/// full permit count before letting the test window open, so the duration
/// clock cannot start ahead of the data plane. The gate owns both the check-in
/// and the expected count, so a future spawn site cannot desynchronize them.
///
/// UDP-only by design: TCP data tasks are `tokio::spawn` async tasks (no
/// OS-thread creation to stall), and a late TCP reader still gets its bytes
/// from the kernel buffer instead of discarding them like the late UDP drain.
pub(crate) struct StreamThreadGate {
    sem: Arc<tokio::sync::Semaphore>,
    expected: u32,
}

impl StreamThreadGate {
    pub(crate) fn new() -> Self {
        Self {
            sem: Arc::new(tokio::sync::Semaphore::new(0)),
            expected: 0,
        }
    }

    /// `tokio::task::spawn_blocking` with a check-in: the spawned closure
    /// releases its gate permit the moment its OS thread actually runs.
    pub(crate) fn spawn<T, F>(&mut self, f: F) -> tokio::task::JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.expected += 1;
        let sem = self.sem.clone();
        tokio::task::spawn_blocking(move || {
            sem.add_permits(1);
            f()
        })
    }

    /// Wait (bounded) until every gate-spawned thread has checked in.
    ///
    /// Returns whether all threads checked in. On timeout the caller proceeds
    /// anyway — a degraded run (pre-#178 behavior) beats failing a test that
    /// would have moved most of its bytes.
    pub(crate) async fn wait(&self, timeout: Duration) -> bool {
        if self.expected == 0 {
            return true;
        }
        match tokio::time::timeout(timeout, self.sem.acquire_many(self.expected)).await {
            Ok(Ok(_permits)) => true,
            _ => {
                // No late/total breakdown: a thread can check in between the
                // timeout firing and any count we'd read, making a precise
                // number a lie exactly when it matters (review r2).
                log::warn!(
                    "not all of {} stream data thread(s) started within {timeout:?}; \
                     proceeding degraded (#178)",
                    self.expected
                );
                false
            }
        }
    }
}

/// Sets `done` when dropped, so every exit path of a test handler — including
/// early `?` error returns before the normal `done.store(true)` — signals the
/// data tasks to stop. Without this, a UDP sender parked in `await_start`
/// (start=false, done=false) on a failed/aborted setup would never observe a
/// stop and would leak on a long-running server (issue #5 follow-up).
pub struct DoneOnDrop(pub Arc<AtomicBool>);

impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// High-performance UDP sender using blocking I/O on a dedicated OS thread.
/// No `unsafe` code — uses `std::net::UdpSocket` and batch pacing with
/// `std::thread::sleep` + spin-loop for sub-microsecond precision.
///
/// Batched UDP sender using sendmmsg — one kernel crossing per batch.
/// Safe Rust only (nix wraps sendmmsg). Available on Linux, FreeBSD, NetBSD.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
#[allow(clippy::too_many_arguments)] // hot-path sender: socket + tuning + lifecycle
pub fn run_udp_sender_sendmmsg(
    socket: std::net::UdpSocket,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    pacing_timer_us: u32,
    use_64bit: bool,
    start: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> Result<()> {
    use nix::sys::socket::{self, MsgFlags, MultiHeaders, SockaddrIn};
    use std::io::IoSlice;
    use std::os::unix::io::AsRawFd;

    if !await_start(&start, &done) {
        return Ok(());
    }
    // Deadline measured from the actual start of data (issue #5): at a high
    // `-b` the CPU-bound senders can starve the async runtime so `done` is
    // never set; the sender must stop itself at `-t`. The deadline is checked
    // once per batch — between blocking sendmmsg calls — so overshoot is bounded
    // by how long one batch can block. On a draining link that's sub-ms; on a
    // wedged link it's the SO_SNDTIMEO set by configure_udp_sender (~1s) before
    // sendmmsg returns EAGAIN and the loop re-checks the deadline.
    let deadline = max_duration.map(|d| Instant::now() + d);

    // Larger ceiling than the per-packet sender: sendmmsg amortizes syscall
    // overhead so bigger batches help more than with individual send() calls.
    // Still bounded to about one pacing quantum at a low rate so a paced run
    // doesn't stage one huge burst then sleep past a short test (#185); an
    // unlimited run (rate 0, -b 0) keeps the full 128.
    let batch_size: usize =
        udp_pacing_batch(rate_bits_per_sec, blksize, pacing_timer_us, 128) as usize;

    // Switch to blocking I/O — tokio's into_std() leaves the socket
    // non-blocking, which makes sendmmsg busy-spin on EAGAIN once SO_SNDBUF
    // fills (the batch is far larger than wmem_max), redundantly re-staging the
    // whole batch and starving the async runtime. Blocking lets the kernel
    // backpressure this thread instead; best-effort enlarge the buffer to a
    // batch and bound a wedged link with SO_SNDTIMEO (issue #6).
    crate::net::configure_udp_sender(&socket, batch_size * blksize)?;

    let fd = socket.as_raw_fd();

    // Pre-allocate everything outside the hot loop
    let mut bufs: Vec<Vec<u8>> = (0..batch_size).map(|_| vec![0u8; blksize]).collect();
    let addrs: Vec<Option<SockaddrIn>> = vec![None; batch_size];
    let cmsgs: Vec<socket::ControlMessage> = vec![];
    let mut headers = MultiHeaders::<SockaddrIn>::preallocate(batch_size, None);

    let mut seq: u64 = 0;

    let pacing = if rate_bits_per_sec > 0 {
        let rate_bytes = rate_bits_per_sec as f64 / 8.0;
        let per_packet = Duration::from_secs_f64(blksize as f64 / rate_bytes);
        Some(per_packet * batch_size as u32)
    } else {
        None
    };

    let mut next_send = Instant::now();

    while !done.load(Ordering::Relaxed) {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        // Cache timestamp once per batch
        let cached_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let cached_sec = cached_time.as_secs() as u32;
        let cached_usec = cached_time.subsec_micros();

        // Write headers into each buffer
        for buf in bufs.iter_mut() {
            seq += 1;
            let header = UdpHeader {
                sec: cached_sec,
                usec: cached_usec,
                seq,
            };
            header.write_to(buf, use_64bit);
        }

        // Build IoSlice refs (points into pre-allocated bufs — no heap alloc)
        let slices: Vec<[IoSlice; 1]> = bufs.iter().map(|b| [IoSlice::new(b)]).collect();

        // Send all packets in one kernel crossing
        match socket::sendmmsg(fd, &mut headers, &slices, &addrs, &cmsgs, MsgFlags::empty()) {
            Ok(results) => {
                let sent_count = results.count(); // consumes iterator, releases borrow
                let batch_bytes = sent_count as u64 * blksize as u64;
                counters.record_sent(batch_bytes);
                // Rewind seq for unsent packets
                let unsent = batch_size - sent_count;
                seq -= unsent as u64;
            }
            Err(nix::errno::Errno::EAGAIN) => {
                // On a blocking socket this means the SO_SNDTIMEO set by
                // configure_udp_sender fired (a wedged link) — send nothing,
                // rewind the batch, and loop to re-check `done`/`deadline`.
                seq -= batch_size as u64;
            }
            Err(e) => {
                // A non-EAGAIN error (e.g. ECONNREFUSED from an ICMP
                // port-unreachable on the connected socket) can persist; back
                // off briefly so we don't spin a core re-trying until the
                // deadline, while still recovering if it clears (#18).
                log::debug!("sendmmsg error: {e}");
                seq -= batch_size as u64;
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        // Rate pacing: one clock check per batch
        if let Some(batch_interval) = pacing {
            next_send += batch_interval;
            let now = Instant::now();
            if next_send > now {
                let remaining = next_send - now;
                if remaining > Duration::from_micros(50) {
                    std::thread::sleep(remaining - Duration::from_micros(20));
                }
                while Instant::now() < next_send {
                    std::hint::spin_loop();
                }
            }
        }
    }
    Ok(())
}

/// Fallback for platforms without sendmmsg.
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
#[allow(clippy::too_many_arguments)] // hot-path sender: socket + tuning + lifecycle
pub fn run_udp_sender_sendmmsg(
    socket: std::net::UdpSocket,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    pacing_timer_us: u32,
    use_64bit: bool,
    start: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> Result<()> {
    run_udp_sender_blocking(
        socket,
        counters,
        blksize,
        done,
        rate_bits_per_sec,
        pacing_timer_us,
        use_64bit,
        start,
        max_duration,
    )
}

/// Datagrams to send between pacing clock-checks (#185). The old fixed batch
/// represented a fixed packet COUNT regardless of rate, so at a low `-b` over a
/// large datagram one batch was many seconds of send budget: the sender emitted
/// a single burst, then slept past the end of a short test and reported near-zero
/// throughput (e.g. default 1 Mbit/s on loopback, blksize ~32 KiB → a 8.4 s batch
/// interval). Size the batch so its paced interval is about one pacing quantum
/// (`--pacing-timer`, default 1 ms) instead, clamped to `[1, max_batch]`.
///
/// Whenever a quantum's worth of budget is at least `max_batch` packets — every
/// unlimited (`-b 0`, short-circuited) run, and any paced run above
/// `max_batch * blksize / quantum` bits/s (≈1.5 Gbit/s at `max_batch` 128, a
/// 1448-byte datagram, and the 1 ms default) — this saturates at `max_batch`,
/// leaving the high-rate path at the old fixed value. BELOW that, a paced run
/// deliberately uses a smaller batch (the fix): the cost is fewer packets per
/// `send`/`sendmmsg`, but the rate is the cap so throughput is unaffected and
/// the amortization is still ample (e.g. ~43 packets at `-b 500M --sendmmsg`).
fn udp_pacing_batch(
    rate_bits_per_sec: u64,
    blksize: usize,
    pacing_timer_us: u32,
    max_batch: u32,
) -> u32 {
    if rate_bits_per_sec == 0 {
        return max_batch;
    }
    let rate_bytes = rate_bits_per_sec as f64 / 8.0;
    let pacing_us = if pacing_timer_us == 0 {
        crate::utils::DEFAULT_PACING_TIMER_US
    } else {
        pacing_timer_us
    };
    let quantum = Duration::from_micros(pacing_us as u64).as_secs_f64();
    let per_quantum = (quantum * rate_bytes / blksize.max(1) as f64).floor() as u32;
    per_quantum.clamp(1, max_batch)
}

/// Batch pacing: sends N packets in a tight loop, then does a single clock
/// check and sleep/spin for the aggregate interval. This amortizes the cost
/// of `Instant::now()` (~50ns) and atomic operations across multiple packets.
///
/// `target` selects the destination model. `None` sends on a *connected* socket
/// (the per-stream client/Unix-server path: one socket per stream, kernel 4-tuple
/// demux). `Some(addr)` uses `send_to(addr)` on a *shared, unconnected* socket —
/// the single-socket UDP server demux (#80), where one server socket fans out to
/// each client by address. The loop body is otherwise identical, so both paths
/// share the same pacing/batching/error handling.
#[allow(clippy::too_many_arguments)] // hot-path sender: socket + tuning + lifecycle
fn udp_send_loop(
    socket: &std::net::UdpSocket,
    target: Option<std::net::SocketAddr>,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    pacing_timer_us: u32,
    use_64bit: bool,
    start: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> Result<()> {
    if !await_start(&start, &done) {
        return Ok(());
    }
    // Deadline measured from the actual start of data — see the note in
    // run_udp_sender_sendmmsg (issue #5).
    let deadline = max_duration.map(|d| Instant::now() + d);

    let mut buf = vec![0u8; blksize];
    let mut seq: u64 = 0;

    // Packets between clock checks: enough to amortize Instant::now() and the
    // atomic counters, but bounded so one batch is about a pacing quantum of
    // send budget, not a fixed count that becomes seconds at a low rate (#185).
    // 32 is the high-rate ceiling (the old fixed value).
    let batch_size: u32 = udp_pacing_batch(rate_bits_per_sec, blksize, pacing_timer_us, 32);

    // Blocking I/O so send() backpressures in-kernel instead of returning
    // WouldBlock and truncating the batch once SO_SNDBUF fills (issue #6).
    crate::net::configure_udp_sender(socket, batch_size as usize * blksize)?;

    let pacing = if rate_bits_per_sec > 0 {
        let rate_bytes = rate_bits_per_sec as f64 / 8.0;
        let per_packet = Duration::from_secs_f64(blksize as f64 / rate_bytes);
        Some(per_packet * batch_size)
    } else {
        None
    };

    let mut next_send = Instant::now();

    while !done.load(Ordering::Relaxed) {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        // Cache timestamp once per batch (sufficient for jitter calculation)
        let cached_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let cached_sec = cached_time.as_secs() as u32;
        let cached_usec = cached_time.subsec_micros();

        let mut batch_bytes: u64 = 0;

        for _ in 0..batch_size {
            seq += 1;
            let header = UdpHeader {
                sec: cached_sec,
                usec: cached_usec,
                seq,
            };
            header.write_to(&mut buf, use_64bit);

            let sent = match target {
                Some(addr) => socket.send_to(&buf, addr),
                None => socket.send(&buf),
            };
            match sent {
                Ok(n) => batch_bytes += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    seq -= 1;
                    break;
                }
                Err(e) => {
                    // Persistent fatal error (e.g. ECONNREFUSED): stop the
                    // batch and back off briefly rather than retrying every
                    // packet in a tight loop until the deadline (#18).
                    log::debug!("UDP send error: {e}");
                    seq -= 1;
                    std::thread::sleep(Duration::from_millis(1));
                    break;
                }
            }
        }

        counters.record_sent(batch_bytes);

        // Rate pacing: one clock check per batch
        if let Some(batch_interval) = pacing {
            next_send += batch_interval;
            let now = Instant::now();
            if next_send > now {
                let remaining = next_send - now;
                if remaining > Duration::from_micros(50) {
                    std::thread::sleep(remaining - Duration::from_micros(20));
                }
                while Instant::now() < next_send {
                    std::hint::spin_loop();
                }
            }
            // If behind schedule, next_send stays in the past — sends
            // immediately until caught up (no accumulating debt)
        }
    }
    Ok(())
}

/// UDP sender on a *connected* socket — the per-stream client/Unix-server path.
/// Thin wrapper over [`udp_send_loop`] with no `send_to` target; the public API
/// (owned socket, `send`) is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn run_udp_sender_blocking(
    socket: std::net::UdpSocket,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    pacing_timer_us: u32,
    use_64bit: bool,
    start: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> Result<()> {
    udp_send_loop(
        &socket,
        None,
        counters,
        blksize,
        done,
        rate_bits_per_sec,
        pacing_timer_us,
        use_64bit,
        start,
        max_duration,
    )
}

/// UDP sender on a *shared, unconnected* server socket, addressing one client by
/// `target` via `send_to` — the reverse/bidir half of the single-socket UDP
/// server demux (#80). Multiple senders share one `Arc<UdpSocket>` (UDP `send_to`
/// is per-datagram atomic and thread-safe), each fanning out to its own client.
/// The socket must already be in blocking mode (the demux setup sets it once).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_udp_server_demux_sender(
    socket: Arc<std::net::UdpSocket>,
    target: std::net::SocketAddr,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    pacing_timer_us: u32,
    use_64bit: bool,
    start: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> Result<()> {
    udp_send_loop(
        &socket,
        Some(target),
        counters,
        blksize,
        done,
        rate_bits_per_sec,
        pacing_timer_us,
        use_64bit,
        start,
        max_duration,
    )
}

/// Where one client's datagrams are accounted in the single-socket UDP server
/// demux: the receiving stream's byte counters and its jitter/loss stats.
pub(crate) struct UdpDemuxRoute {
    pub(crate) counters: Arc<StreamCounters>,
    pub(crate) stats: Arc<Mutex<UdpRecvStats>>,
}

/// Single-socket UDP server receiver demux (#80). On native winsock a connected
/// UDP socket sharing a port with a wildcard socket silently drops a new source's
/// datagrams, so the per-stream connected-socket design hangs `-P > 1` setup on
/// Windows. This path instead binds **one** unconnected server socket for the
/// whole test and demultiplexes incoming datagrams to the right receiving stream
/// by source address in userspace — exactly what the kernel does on Linux/BSD,
/// done explicitly so it is correct on every platform.
///
/// One dedicated blocking thread owns `recv_from` (a datagram can be consumed
/// only once, so a single consumer must route every packet — N threads each
/// filtering by source would lose each other's data). Datagrams from an unknown
/// source — a late retransmit of the connect magic, or a stray — are dropped.
/// Teardown mirrors the connected receiver: keep the socket open and drain late
/// datagrams until the peer goes quiet (issue #48), so a still-sending iperf3
/// <=3.12 peer isn't reset. The socket must already be in blocking mode.
pub(crate) fn run_udp_server_demux_receiver(
    socket: Arc<std::net::UdpSocket>,
    routes: std::collections::HashMap<std::net::SocketAddr, UdpDemuxRoute>,
    done: Arc<AtomicBool>,
    use_64bit: bool,
) -> Result<()> {
    // 65536 >= MAX_UDP_BLKSIZE (65507), so a full UDP datagram never truncates;
    // this is the same floor run_udp_receiver_blocking caps to via blksize.max().
    let mut buf = vec![0u8; 65536];
    // Match run_udp_receiver_blocking: blocking + a read timeout so the thread
    // parks between datagrams instead of busy-spinning, and so `done` is observed
    // promptly during idle gaps.
    socket
        .set_nonblocking(false)
        .map_err(crate::error::RiperfError::Io)?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(crate::error::RiperfError::Io)?;

    let mut drain = false;
    loop {
        if done.load(Ordering::Relaxed) {
            drain = true;
            break;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                // Route by source address. An unknown source is a late connect
                // retransmit or stray — drop it (do not count it against any
                // stream). Unlike the connected receiver, a 0-byte datagram is
                // NOT a loop-exit here: it must not tear down an N-stream demux
                // (iperf3 never sends empty data datagrams anyway); it just routes
                // and records 0 bytes with no header.
                if let Some(route) = routes.get(&src) {
                    route.counters.record_received(n as u64);
                    if let Some(header) = UdpHeader::read_from(&buf[..n], use_64bit) {
                        let arrival = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64();
                        if let Ok(mut stats) = route.stats.lock() {
                            stats.update(&header, arrival);
                        }
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            // #178: reset-class errors on a UDP socket are port-unreachable
            // noise from something WE sent — not EOF. This demux socket is
            // UNCONNECTED, so on Linux it never even surfaces ICMP errors
            // (that needs IP_RECVERR); the arm is live on Windows, where
            // winsock latches WSAECONNRESET per send_to target. A break here
            // would silently end reception for EVERY stream at once.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                continue
            }
            Err(_) => break,
        }
    }
    if drain {
        drain_udp_demux_after_done(&socket, &mut buf);
    }
    Ok(())
}

/// `recv_from` analogue of [`drain_udp_after_done`] for the single-socket demux:
/// after the test ends, keep the shared socket open and discard late datagrams
/// (from any source) until one read-timeout of silence, bounded by
/// [`UDP_RECEIVER_DRAIN_TIMEOUT`]. See [`drain_udp_after_done`] for the why.
fn drain_udp_demux_after_done(socket: &std::net::UdpSocket, buf: &mut [u8]) {
    let deadline = Instant::now() + UDP_RECEIVER_DRAIN_TIMEOUT;
    while Instant::now() < deadline {
        match socket.recv_from(buf) {
            Ok(_) => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => break,
        }
    }
}

/// Hard cap on the post-test UDP drain (issue #48). The normal exit is a single
/// read-timeout of silence (the peer has stopped); this only bounds a peer that
/// floods forever without ever stopping, so the receiver thread can't block
/// teardown. Deliberately generous — closing early is what reopens the reset.
const UDP_RECEIVER_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// After the test ends, keep the UDP socket OPEN and discard late datagrams until
/// the peer goes quiet (issue #48 — the UDP analog of the TCP teardown race #23).
/// In UDP reverse/bidir the peer is the sender and keeps transmitting for a brief
/// window after our duration ends, until our control-plane TestEnd reaches it.
/// Closing the socket while it's still sending draws an ICMP port-unreachable, and
/// an iperf3 <=3.12 sender takes the resulting ECONNRESET as fatal and aborts the
/// whole control connection (which we'd see as `PeerDisconnected`). There is no
/// UDP EOF to wait on, so drain until one read-timeout passes with no datagram —
/// the peer has stopped — bounded by [`UDP_RECEIVER_DRAIN_TIMEOUT`]. Late datagrams
/// are discarded, not counted: they're outside the test window. The caller must
/// have set a read timeout on the socket (so a silent recv returns rather than
/// blocking forever); the blocking receiver sets `SO_RCVTIMEO` for exactly this.
fn drain_udp_after_done(socket: &std::net::UdpSocket, buf: &mut [u8]) {
    let deadline = Instant::now() + UDP_RECEIVER_DRAIN_TIMEOUT;
    while Instant::now() < deadline {
        match socket.recv(buf) {
            // Any datagram — including a 0-byte one (UDP has no EOF) — is late
            // traffic: discard it and keep the socket open.
            Ok(_) => continue,
            // A read-timeout with no datagram means the peer has stopped sending:
            // safe to return now and let the socket close.
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => break,
        }
    }
}

/// High-performance UDP receiver using blocking I/O on a dedicated OS thread.
/// No `unsafe` code — uses `std::net::UdpSocket` with a read timeout.
pub fn run_udp_receiver_blocking(
    socket: std::net::UdpSocket,
    counters: Arc<StreamCounters>,
    udp_stats: Arc<Mutex<UdpRecvStats>>,
    blksize: usize,
    done: Arc<AtomicBool>,
    use_64bit: bool,
) -> Result<()> {
    let mut buf = vec![0u8; blksize.max(65536)];
    // tokio's into_std() leaves the socket non-blocking, which makes the
    // SO_RCVTIMEO below a no-op: recv() returns WouldBlock immediately and the
    // loop busy-spins at 100% CPU. Switch to blocking so the read timeout
    // actually parks the thread between datagrams (issue #6).
    socket
        .set_nonblocking(false)
        .map_err(crate::error::RiperfError::Io)?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(crate::error::RiperfError::Io)?;

    let mut drain = false;
    loop {
        if done.load(Ordering::Relaxed) {
            drain = true;
            break;
        }
        match socket.recv(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                counters.record_received(n as u64);
                if let Some(header) = UdpHeader::read_from(&buf[..n], use_64bit) {
                    let arrival = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    if let Ok(mut stats) = udp_stats.lock() {
                        stats.update(&header, arrival);
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            // #178: ConnectionReset on a UDP socket is ICMP port-unreachable
            // noise from something WE sent (Windows: WSAECONNRESET after a
            // connect-magic retransmit hit a closed port; Linux: ECONNREFUSED
            // for the same class) — not EOF. Breaking
            // here silently ended the reverse flow: a bidir run completed
            // "normally" with sum_bidir_reverse 0 throughout (windows-latest
            // CI signature).
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                continue
            }
            Err(_) => break,
        }
    }
    // Stopping because the test ended (not because the peer already closed/
    // errored): hold the socket open and drain late datagrams so a still-sending
    // iperf3 <=3.12 peer isn't reset (issue #48). See drain_udp_after_done.
    if drain {
        drain_udp_after_done(&socket, &mut buf);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // #185: the paced UDP batch is sized to ~one pacing quantum of send budget,
    // not a fixed packet count. A low rate over a large datagram must drop to a
    // small batch (else one batch is many seconds and the sender bursts then
    // starves); a high rate saturates at the ceiling; unlimited keeps it.
    #[test]
    fn udp_pacing_batch_scales_with_rate() {
        // Default 1 Mbit/s over a loopback-MSS datagram: one packet already
        // exceeds a 1 ms quantum's budget, so the batch floors at 1 (the bug
        // was a fixed 32 → an ~8 s batch interval).
        assert_eq!(udp_pacing_batch(1_000_000, 32_741, 1000, 32), 1);
        // A high rate over a small datagram saturates at the ceiling.
        assert_eq!(udp_pacing_batch(10_000_000_000, 1448, 1000, 32), 32);
        assert_eq!(udp_pacing_batch(10_000_000_000, 1448, 1000, 128), 128);
        // Unlimited (-b 0) is unpaced and keeps the full ceiling.
        assert_eq!(udp_pacing_batch(0, 32_741, 1000, 32), 32);
        // A larger --pacing-timer admits a larger batch at the same rate.
        let small_q = udp_pacing_batch(100_000_000, 1448, 1000, 64);
        let big_q = udp_pacing_batch(100_000_000, 1448, 10_000, 64);
        assert!(big_q > small_q, "{big_q} should exceed {small_q}");
        // pacing_timer 0 resolves to iperf3's 1 ms default, not a div-by-zero.
        assert_eq!(
            udp_pacing_batch(100_000_000, 1448, 0, 64),
            udp_pacing_batch(100_000_000, 1448, 1000, 64),
        );
        // A degenerate blksize cannot divide by zero — it yields a valid
        // clamped batch (here the max, since 1-byte packets are tiny), not a
        // panic or a garbage value.
        assert_eq!(udp_pacing_batch(1_000_000, 0, 1000, 32), 32);
    }

    // #178 readiness barrier: wait() is trivially true with nothing spawned,
    // true once every gate-spawned thread checks in (even when the threads
    // start slowly), and false — without hanging — when a thread can't start.
    #[tokio::test]
    async fn stream_thread_gate_waits_and_times_out() {
        let gate = StreamThreadGate::new();
        assert!(
            gate.wait(Duration::from_millis(50)).await,
            "an empty gate must not wait"
        );

        let mut gate = StreamThreadGate::new();
        let h1 = gate.spawn(|| std::thread::sleep(Duration::from_millis(20)));
        let h2 = gate.spawn(|| ());
        assert!(
            gate.wait(Duration::from_secs(5)).await,
            "gate-spawned threads must satisfy the barrier once running"
        );
        h1.await.unwrap();
        h2.await.unwrap();

        // A check-in that never comes (in real life: a spawn whose thread is
        // queued behind a saturated blocking pool): an expectation with no
        // permit ever released.
        let starved = StreamThreadGate {
            sem: Arc::new(tokio::sync::Semaphore::new(0)),
            expected: 1,
        };
        assert!(
            !starved.wait(Duration::from_millis(40)).await,
            "missing threads must time out, not hang"
        );
    }

    // #178: a connection-reset-class error on a UDP socket is ICMP
    // port-unreachable noise from something WE sent — Windows surfaces it as
    // WSAECONNRESET/ConnectionReset on a connected socket (e.g. after a
    // connect-magic retransmit hits a closed port); Linux connected UDP
    // surfaces the same class as ECONNREFUSED/ConnectionRefused, which is
    // what makes this test portable. It is NOT EOF, and the receiver must
    // keep receiving. Pre-fix, the blocking receiver's `Err(_) => break`
    // silently ended reception on the first such event: a complete bidir run
    // with `sum_bidir_reverse: 0` throughout, the windows-latest CI
    // signature.
    #[test]
    // Runs where the poison injection is verified: Linux (loopback ICMP →
    // ECONNREFUSED on the connected socket) and Windows (winsock latches
    // WSAECONNRESET locally for loopback sends to closed ports — no ICMP
    // path needed; it is the canonical home of the mechanism AND the only
    // platform exercising the ConnectionReset arm). macOS/FreeBSD are
    // ignored as unverified, not impossible.
    #[cfg_attr(not(any(target_os = "linux", target_os = "windows")), ignore)]
    fn udp_receiver_survives_connection_reset() {
        use std::net::UdpSocket as StdUdp;
        use std::sync::atomic::AtomicBool;

        // Receiver socket, connected to a peer that immediately vanishes.
        let receiver = StdUdp::bind("127.0.0.1:0").expect("bind receiver");
        let doomed = StdUdp::bind("127.0.0.1:0").expect("bind doomed peer");
        let peer_addr = doomed.local_addr().unwrap();
        receiver.connect(peer_addr).expect("connect");
        drop(doomed);

        // Poison: send to the now-closed port → ICMP unreachable → the next
        // recv on this connected socket reports ConnectionReset.
        receiver.send(b"poison").expect("send to dead port");
        std::thread::sleep(Duration::from_millis(50));

        // The peer "comes back" on the same port (as a still-sending peer
        // would simply keep existing) and delivers real datagrams.
        let revived = StdUdp::bind(peer_addr).expect("rebind peer port");
        let dest = receiver.local_addr().unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(StreamCounters::new());
        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));

        let sender = std::thread::spawn(move || {
            for _ in 0..20 {
                let _ = revived.send_to(&[0u8; 64], dest);
                std::thread::sleep(Duration::from_millis(25));
            }
        });

        let recv_counters = counters.clone();
        let recv_done = done.clone();
        let receiver_thread = std::thread::spawn(move || {
            run_udp_receiver_blocking(receiver, recv_counters, stats, 1024, recv_done, false)
        });

        sender.join().unwrap();
        done.store(true, Ordering::Relaxed);
        receiver_thread.join().unwrap().unwrap();

        assert!(
            counters.bytes_received() >= 10 * 64,
            "#178: the receiver must survive a ConnectionReset (ICMP noise) \
             and KEEP receiving (>= half the 20 x 64B datagrams); got {}",
            counters.bytes_received()
        );
    }

    #[test]
    fn byte_budget_zero_is_unlimited() {
        let bs = 128 * 1024;
        // iperf3 sends num=0 / blocks=0 for a plain `-t` run → no budget, or the
        // server would build a 0-budget and send nothing (regression guard).
        assert!(make_byte_budget(Some(0), Some(0), bs).is_none());
        assert!(make_byte_budget(None, None, bs).is_none());
        // `-n N` → an N-byte budget.
        let b = make_byte_budget(Some(5_000_000), Some(0), bs).unwrap();
        assert_eq!(b.load(Ordering::Relaxed), 5_000_000);
        // `-k K` (num=0 filtered, blocks path) → K*blksize.
        let b = make_byte_budget(Some(0), Some(10), bs).unwrap();
        assert_eq!(b.load(Ordering::Relaxed), (10 * bs) as i64);
        // An absurd N clamps to a positive budget, never negative.
        let b = make_byte_budget(Some(u64::MAX), None, bs).unwrap();
        assert!(b.load(Ordering::Relaxed) > 0);
    }

    // #42: zerocopy senders must get distinct temp-file names so `-Z -P >1`
    // can't race create+truncate on a shared path. The race needs concurrency a
    // sequential test can't reproduce, so we assert the mechanism directly: the
    // name generator never repeats and carries the pid.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn zc_tempfile_names_are_unique() {
        let names: Vec<String> = (0..16).map(|_| zc_tempfile_name()).collect();
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "names collided: {names:?}");
        let pid = std::process::id().to_string();
        assert!(names.iter().all(|n| n.contains(&pid)));
    }

    // -- StreamCounters --

    #[test]
    fn counters_basic() {
        let c = StreamCounters::new();
        c.record_sent(100);
        c.record_sent(200);
        assert_eq!(c.bytes_sent(), 300);
        assert_eq!(c.take_sent_interval(), 300);
        // After take, interval resets but cumulative stays
        assert_eq!(c.take_sent_interval(), 0);
        assert_eq!(c.bytes_sent(), 300);
    }

    #[test]
    fn counters_received() {
        let c = StreamCounters::new();
        c.record_received(50);
        c.record_received(75);
        assert_eq!(c.bytes_received(), 125);
        assert_eq!(c.take_received_interval(), 125);
        assert_eq!(c.take_received_interval(), 0);
    }

    /// Regression: verify that interval swap-and-reset does NOT
    /// affect cumulative counters. This is the invariant that
    /// prevents the interval reporter from stealing bytes from
    /// the final summary.
    #[test]
    fn interval_swap_does_not_affect_cumulative() {
        let c = StreamCounters::new();
        c.record_sent(1000);
        c.record_sent(2000);
        assert_eq!(c.bytes_sent(), 3000);

        // Simulate interval reporter draining the interval counter
        assert_eq!(c.take_sent_interval(), 3000);

        // Cumulative counter must be unaffected
        assert_eq!(c.bytes_sent(), 3000);

        // Record more data
        c.record_sent(500);
        assert_eq!(c.bytes_sent(), 3500);
        assert_eq!(c.take_sent_interval(), 500);
        assert_eq!(c.bytes_sent(), 3500); // still unaffected

        // Same for received
        c.record_received(100);
        assert_eq!(c.take_received_interval(), 100);
        assert_eq!(c.bytes_received(), 100);
    }

    // -- UdpHeader --

    #[test]
    fn udp_header_round_trip_32() {
        let h = UdpHeader {
            sec: 1000,
            usec: 500_000,
            seq: 42,
        };
        let mut buf = [0u8; 64];
        h.write_to(&mut buf, false);
        let h2 = UdpHeader::read_from(&buf, false).unwrap();
        assert_eq!(h2.sec, 1000);
        assert_eq!(h2.usec, 500_000);
        assert_eq!(h2.seq, 42);
    }

    #[test]
    fn udp_header_round_trip_64() {
        let h = UdpHeader {
            sec: 1000,
            usec: 500_000,
            seq: u64::MAX - 1,
        };
        let mut buf = [0u8; 64];
        h.write_to(&mut buf, true);
        let h2 = UdpHeader::read_from(&buf, true).unwrap();
        assert_eq!(h2.seq, u64::MAX - 1);
    }

    #[test]
    fn udp_header_too_short() {
        let buf = [0u8; 8];
        assert!(UdpHeader::read_from(&buf, false).is_none());
        assert!(UdpHeader::read_from(&buf, true).is_none());
    }

    #[test]
    fn udp_header_wire_sizes() {
        assert_eq!(UdpHeader::wire_size(false), 12);
        assert_eq!(UdpHeader::wire_size(true), 16);
    }

    // -- UdpRecvStats --

    #[test]
    fn udp_stats_sequential_no_loss() {
        let mut stats = UdpRecvStats::new();
        let t = 1000.0;
        for i in 1..=5 {
            let h = UdpHeader {
                sec: 1000,
                usec: 0,
                seq: i,
            };
            stats.update(&h, t + i as f64 * 0.001);
        }
        assert_eq!(stats.packet_count, 5);
        assert_eq!(stats.cnt_error, 0);
        assert_eq!(stats.outoforder_packets, 0);
    }

    #[test]
    fn udp_stats_detects_loss() {
        let mut stats = UdpRecvStats::new();
        let t = 1000.0;
        // Receive packets 1, 2, 5 (missing 3, 4)
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 1,
            },
            t,
        );
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 2,
            },
            t + 0.001,
        );
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 5,
            },
            t + 0.002,
        );
        assert_eq!(stats.packet_count, 5);
        assert_eq!(stats.cnt_error, 2); // packets 3 and 4 missing
    }

    #[test]
    fn udp_stats_detects_ooo() {
        let mut stats = UdpRecvStats::new();
        let t = 1000.0;
        // Receive 1, 3, 2
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 1,
            },
            t,
        );
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 3,
            },
            t + 0.001,
        );
        // At this point: packet_count=3, cnt_error=1 (packet 2 "lost")
        assert_eq!(stats.cnt_error, 1);
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 2,
            },
            t + 0.002,
        );
        // Packet 2 arrives late: OOO incremented, loss decremented
        assert_eq!(stats.outoforder_packets, 1);
        assert_eq!(stats.cnt_error, 0);
    }

    #[test]
    fn udp_stats_jitter_accumulates() {
        let mut stats = UdpRecvStats::new();
        // Packets sent at t=1000.000, 1000.001, 1000.002
        // Received at t=1000.010, 1000.012, 1000.013
        // Transit times: 0.010, 0.011, 0.011
        // d values: -, 0.001, 0.000
        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 0,
                seq: 1,
            },
            1000.010,
        );
        assert_eq!(stats.jitter, 0.0); // first packet, no jitter yet

        stats.update(
            &UdpHeader {
                sec: 1000,
                usec: 1000,
                seq: 2,
            },
            1000.012,
        );
        // d = |0.011 - 0.010| = 0.001
        // jitter = 0.0 + (0.001 - 0.0) / 16 = 0.0000625
        assert!((stats.jitter - 0.0000625).abs() < 1e-10);
    }

    #[test]
    fn udp_stats_omit_snapshot() {
        let mut stats = UdpRecvStats::new();
        let t = 1000.0;
        for i in 1..=3 {
            stats.update(
                &UdpHeader {
                    sec: 1000,
                    usec: 0,
                    seq: i,
                },
                t,
            );
        }
        stats.snapshot_omit();
        assert_eq!(stats.omitted_packet_count, 3);

        for i in 4..=6 {
            stats.update(
                &UdpHeader {
                    sec: 1000,
                    usec: 0,
                    seq: i,
                },
                t,
            );
        }
        // Effective (post-omit) packet count: 6 - 3 = 3
        assert_eq!(stats.packet_count - stats.omitted_packet_count, 3);
    }

    // -- RateLimiter --

    // #116: with TCP's 128 KiB default block, the old token bucket's burst
    // floor (max(rate*0.1, 4*blksize) = 512 KiB, granted instantly) overshoots
    // a low -b by ~2x at 1 Mbit/s. iperf3's cumulative-average throttle bounds
    // total sent to elapsed*rate + one in-flight block at any rate/blksize.
    #[tokio::test]
    async fn rate_limiter_total_accuracy_low_rate_large_blocks() {
        let rate_bits: u64 = 1_000_000; // 1 Mbit/s
        let blk: u64 = 128 * 1024; // TCP default block
        let mut limiter = RateLimiter::new(rate_bits, 0, 0);
        let start = Instant::now();
        let mut sent: u64 = 0;
        while start.elapsed() < Duration::from_millis(1000) {
            limiter.acquire(blk).await;
            sent += blk;
        }
        let budget = (rate_bits as f64 / 8.0) * start.elapsed().as_secs_f64();
        let bound = budget * 1.25 + blk as f64;
        assert!(
            (sent as f64) <= bound,
            "sent {sent} bytes in {:?}; budget {budget:.0} (+25% + one block = {bound:.0}) — \
             initial burst overshoots the target rate (#116)",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn rate_limiter_first_acquire_is_instant() {
        let mut limiter = RateLimiter::new(1_000_000, 0, 0); // 1 Mbit/s
        let start = Instant::now();
        // Cumulative average: nothing sent yet → always green-lit.
        limiter.acquire(1000).await;
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn rate_limiter_paces_from_the_second_block() {
        // 80_000 bits/sec = 10_000 bytes/sec, 1000-byte blocks: after block 1
        // the average is 1000 bytes ahead of schedule, so block 2 waits ~100ms
        // — there is no up-front burst grant (#116).
        let mut limiter = RateLimiter::new(80_000, 0, 0);
        limiter.acquire(1000).await; // instant
        let start = Instant::now();
        limiter.acquire(1000).await; // must wait ~100ms
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(50)); // generous lower bound
        assert!(elapsed < Duration::from_millis(300)); // generous upper bound
    }

    #[tokio::test]
    async fn rate_limiter_high_rate_precision() {
        // At 10 Gbps the cumulative average is almost always at-or-behind
        // schedule, so acquires release back-to-back with no sleep; catch-up
        // after any oversleep keeps throughput within 50% of target.
        let rate = 10_000_000_000u64; // 10 Gbps
        let blksize = 1460u64;
        let mut limiter = RateLimiter::new(rate, 0, 0);

        let start = Instant::now();
        let mut total_bytes: u64 = 0;
        while start.elapsed() < Duration::from_millis(100) {
            limiter.acquire(blksize).await;
            total_bytes += blksize;
        }

        let elapsed = start.elapsed().as_secs_f64();
        let achieved_bps = total_bytes as f64 * 8.0 / elapsed;
        assert!(
            achieved_bps > rate as f64 * 0.5,
            "high-rate pacing too slow: {:.1} Gbps achieved, 10.0 Gbps target",
            achieved_bps / 1e9,
        );
    }

    #[tokio::test]
    async fn rate_limiter_low_rate_still_works() {
        let mut limiter = RateLimiter::new(1_000_000, 0, 0); // 1 Mbps
        let start = Instant::now();
        let mut total_bytes: u64 = 0;

        // 10 × 1000-byte blocks at 125 KB/s ≈ 72ms of pacing after block 1.
        for _ in 0..10 {
            limiter.acquire(1000).await;
            total_bytes += 1000;
        }

        let elapsed = start.elapsed();
        assert!(total_bytes == 10_000);
        // Should complete in under 1 second (generously)
        assert!(elapsed < Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limiter_burst_allows_burst_blocks_per_green_light() {
        // -b 8K/4 → 1000 bytes/s, bursts of 4 (#160). iperf3's multisend loop
        // sends `burst` blocks per throttle check: after the batch head is
        // green-lit, the next 3 blocks pass with NO check even though the
        // cumulative average is far ahead of schedule.
        let mut limiter = RateLimiter::new(8_000, 0, 4);
        let t0 = tokio::time::Instant::now();
        for _ in 0..4 {
            limiter.acquire(1000).await;
        }
        assert_eq!(
            t0.elapsed(),
            Duration::ZERO,
            "burst window must not sleep: blocks 2..=4 ride block 1's green light"
        );
        // Block 5 is the next batch head: 4000 B sent against a 1000 B/s
        // schedule → it must wait to the green-light instant (~4 s).
        limiter.acquire(1000).await;
        assert!(
            t0.elapsed() >= Duration::from_secs(4),
            "next batch head waits to green light, got {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn tcp_sender_rechecks_done_after_pacing_sleep() {
        // At very low -b the green-light sleep can outlast the test; the
        // sender must re-check `done` after the throttle wakes instead of
        // writing one more block past the end (#160; modern iperf3's worker
        // re-checks before sending).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let blksize = 10_000u64;

        let sc = counters.clone();
        let d = done.clone();
        let sender = tokio::spawn(async move {
            let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            let buf = vec![0u8; blksize as usize];
            // 8000 bits/s = 1000 bytes/s: block 1 is green-lit immediately,
            // block 2's acquire sleeps ~10 virtual seconds.
            run_tcp_sender(stream, sc, buf, d, None, 8_000, 0, 0, None).await
        });
        let (peer, _) = listener.accept().await.unwrap();

        // Flip `done` mid-sleep (5 s < the ~10 s green-light instant).
        tokio::time::sleep(Duration::from_secs(5)).await;
        done.store(true, Ordering::Relaxed);

        sender.await.unwrap().unwrap();
        drop(peer);
        assert_eq!(
            counters.bytes_sent(),
            blksize,
            "sender wrote a block after `done` despite the post-sleep re-check"
        );
    }

    // -- TCP send/recv integration --

    #[tokio::test]
    async fn tcp_send_recv_counts_bytes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let send_counters = Arc::new(StreamCounters::new());
        let recv_counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));

        let sc = send_counters.clone();
        let d = done.clone();
        let sender = tokio::spawn(async move {
            let stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            let buf = vec![0u8; 1024];
            run_tcp_sender(stream, sc, buf, d, None, 0, 1000, 0, None).await
        });

        let rc = recv_counters.clone();
        let d2 = done.clone();
        let receiver = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            run_tcp_receiver(stream, rc, 1024, d2, false, None).await
        });

        // Let data flow for a short time
        tokio::time::sleep(Duration::from_millis(100)).await;
        done.store(true, Ordering::Relaxed);

        // Give receiver time to see the done flag / connection close
        tokio::time::sleep(Duration::from_millis(50)).await;

        let _ = sender.await;
        let _ = receiver.await;

        assert!(send_counters.bytes_sent() > 0);
        assert!(recv_counters.bytes_received() > 0);
    }

    // ---- issue #23: receiver drains after `done` instead of resetting peer --

    /// Reverse-mode teardown race (issue #23): when the local receiver's test
    /// duration ends (`done` is set), it must NOT immediately close its data
    /// socket while the peer sender is still writing. A remote iperf3 (<= 3.12)
    /// treats the resulting EPIPE as fatal and aborts the whole control
    /// connection, which surfaces to riperf3 as `PeerDisconnected`. The receiver
    /// must keep draining until the peer stops/closes, so in-flight writes after
    /// `done` still land rather than resetting the peer.
    async fn receiver_drains_after_done(skip_rx_copy: bool) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut sender = TcpStream::connect(addr).await.unwrap();
        let (recv_sock, _) = listener.accept().await.unwrap();

        let recv_counters = Arc::new(StreamCounters::new());
        let done = Arc::new(AtomicBool::new(false));
        let d = done.clone();
        let rc = recv_counters.clone();
        let receiver = tokio::spawn(async move {
            run_tcp_receiver(recv_sock, rc, 64 * 1024, d, skip_rx_copy, None).await
        });

        let block = vec![0u8; 64 * 1024];

        // Data phase: peer is actively sending.
        sender.write_all(&block).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Duration ends — the receiver observes `done`.
        done.store(true, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(20)).await;

        // The peer keeps writing in-flight blocks after `done`. Every write must
        // succeed: the receiver holds the socket open and drains rather than
        // closing, so none of these surface as BrokenPipe/ConnectionReset.
        for i in 0..20 {
            sender
                .write_all(&block)
                .await
                .unwrap_or_else(|e| panic!("post-done write #{i} must not fail (peer reset): {e}"));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // Peer finishes and closes; the receiver drains to EOF and exits cleanly.
        drop(sender);
        let res = tokio::time::timeout(Duration::from_secs(2), receiver)
            .await
            .expect("receiver must finish after peer closes")
            .expect("receiver task panicked");
        assert!(res.is_ok(), "receiver returned error: {res:?}");
    }

    #[tokio::test]
    async fn tcp_receiver_drains_after_done_normal_path() {
        receiver_drains_after_done(false).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tcp_receiver_drains_after_done_msgtrunc_path() {
        receiver_drains_after_done(true).await;
    }

    // ---- issue #5: UDP senders self-enforce the wall-clock deadline ---------

    /// Connected UDP socket pair on loopback. The receiver is never drained,
    /// but UDP send() doesn't block on a full receive buffer (datagrams are
    /// dropped), so the sender loops freely — exactly like the real bug where
    /// senders spin at a high `-b`.
    fn udp_pair() -> (std::net::UdpSocket, std::net::UdpSocket) {
        let recv = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let send = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        send.connect(recv.local_addr().unwrap()).unwrap();
        (send, recv)
    }

    /// A released start barrier.
    fn started() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }

    /// The core regression: with `done` never set, the max-duration alone must
    /// stop the loop. Before the fix this spun forever (issue #5).
    #[test]
    fn udp_sender_blocking_honors_deadline_without_done() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false)); // intentionally never set
        let counters = Arc::new(StreamCounters::new());

        let t0 = Instant::now();
        run_udp_sender_blocking(
            send,
            counters.clone(),
            1400,
            done.clone(),
            0,
            1000,
            false,
            started(),
            Some(Duration::from_millis(200)),
        )
        .unwrap();
        let elapsed = t0.elapsed();

        assert!(
            !done.load(Ordering::Relaxed),
            "done was never set — the deadline alone must terminate the loop"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "sender should stop near its 200ms deadline, took {elapsed:?}"
        );
        assert!(
            counters.bytes_sent() > 0,
            "should have sent before stopping"
        );
    }

    /// Same deadline guarantee, but with a non-zero rate so `batch_size > 1`
    /// and the paced send loop runs — production never uses rate 0, so this
    /// covers the batched regime where the deadline is checked once per batch
    /// rather than per packet.
    #[test]
    fn udp_sender_blocking_honors_deadline_when_paced() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false)); // never set
        let counters = Arc::new(StreamCounters::new());

        let t0 = Instant::now();
        run_udp_sender_blocking(
            send,
            counters.clone(),
            1400,
            done.clone(),
            100_000_000, // 100 Mbps → rate>0 → batched + paced
            1000,
            false,
            started(),
            Some(Duration::from_millis(200)),
        )
        .unwrap();

        assert!(!done.load(Ordering::Relaxed));
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "paced sender should stop near its 200ms deadline, took {:?}",
            t0.elapsed()
        );
        assert!(counters.bytes_sent() > 0);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
    #[test]
    fn udp_sender_sendmmsg_honors_deadline_without_done() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false)); // intentionally never set
        let counters = Arc::new(StreamCounters::new());

        let t0 = Instant::now();
        run_udp_sender_sendmmsg(
            send,
            counters.clone(),
            1400,
            done.clone(),
            0,
            1000,
            false,
            started(),
            Some(Duration::from_millis(200)),
        )
        .unwrap();
        let elapsed = t0.elapsed();

        assert!(!done.load(Ordering::Relaxed));
        assert!(
            elapsed < Duration::from_secs(2),
            "sendmmsg sender should stop near its 200ms deadline, took {elapsed:?}"
        );
        assert!(counters.bytes_sent() > 0);
    }

    /// `max_duration = None` preserves the original behavior: the loop runs
    /// until `done` is set (byte/block-limited tests and the control path).
    #[test]
    fn udp_sender_blocking_no_deadline_stops_on_done() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(StreamCounters::new());
        let d2 = done.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            d2.store(true, Ordering::Relaxed);
        });

        let t0 = Instant::now();
        run_udp_sender_blocking(send, counters, 1400, done, 0, 1000, false, started(), None)
            .unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "should stop shortly after done is set"
        );
    }

    /// The start barrier (issue #5): a sender must not transmit until `start`
    /// is released, so the create-streams handshake isn't flooded.
    #[test]
    fn udp_sender_blocking_waits_for_start_barrier() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(AtomicBool::new(false)); // held closed
        let counters = Arc::new(StreamCounters::new());

        let c2 = counters.clone();
        let d2 = done.clone();
        let s2 = start.clone();
        let h = std::thread::spawn(move || {
            run_udp_sender_blocking(
                send,
                c2,
                1400,
                d2,
                0,
                1000,
                false,
                s2,
                Some(Duration::from_secs(10)),
            )
        });

        // While the barrier is closed, nothing should be sent.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            counters.bytes_sent(),
            0,
            "sender must not transmit before the start barrier is released"
        );

        // Release, let it run briefly, then stop it.
        start.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        done.store(true, Ordering::Relaxed);
        h.join().unwrap().unwrap();
        assert!(
            counters.bytes_sent() > 0,
            "sender should transmit after the barrier opens"
        );
    }

    /// If torn down before the barrier opens, the sender exits without sending.
    #[test]
    fn udp_sender_blocking_exits_if_done_before_start() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(true)); // already done
        let start = Arc::new(AtomicBool::new(false)); // never released
        let counters = Arc::new(StreamCounters::new());

        let t0 = Instant::now();
        run_udp_sender_blocking(
            send,
            counters.clone(),
            1400,
            done,
            0,
            1000,
            false,
            start,
            Some(Duration::from_secs(10)),
        )
        .unwrap();
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert_eq!(counters.bytes_sent(), 0);
    }

    /// DoneOnDrop releases a sender parked on the start barrier: on a failed
    /// setup the guard drops, sets `done`, and the parked sender exits instead
    /// of leaking (issue #5 follow-up).
    #[test]
    fn done_on_drop_releases_parked_sender() {
        let (send, _recv) = udp_pair();
        let done = Arc::new(AtomicBool::new(false));
        let start = Arc::new(AtomicBool::new(false)); // never released
        let counters = Arc::new(StreamCounters::new());

        let c = counters.clone();
        let d = done.clone();
        let s = start.clone();
        let h = std::thread::spawn(move || {
            run_udp_sender_blocking(
                send,
                c,
                1400,
                d,
                0,
                1000,
                false,
                s,
                Some(Duration::from_secs(10)),
            )
        });

        // Parked on the barrier — nothing transmitted.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(counters.bytes_sent(), 0);

        // Simulate a handler tearing down: the guard drops and sets `done`.
        drop(DoneOnDrop(done.clone()));

        // The parked sender must observe `done` and return (no leak, no send).
        let t0 = Instant::now();
        h.join().unwrap().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "sender should exit promptly"
        );
        assert!(done.load(Ordering::Relaxed));
        assert_eq!(counters.bytes_sent(), 0);
    }

    // ---- issue #48: UDP receiver drains after `done` instead of closing -------

    /// UDP teardown race (issue #48, the UDP analog of #23): when the local
    /// receiver's duration ends (`done`), it must NOT immediately close its data
    /// socket while the peer sender is still transmitting. In reverse/bidir the
    /// peer keeps sending until our control-plane TestEnd reaches it; a datagram
    /// landing on our closed port draws an ICMP port-unreachable, and an iperf3
    /// <=3.12 sender treats the resulting ECONNRESET as fatal and aborts the whole
    /// control connection (surfacing to us as `PeerDisconnected`). The receiver
    /// must hold the socket open and drain until the peer goes quiet.
    #[test]
    fn udp_receiver_drains_after_done_instead_of_closing() {
        let (send, recv) = udp_pair();
        let counters = Arc::new(StreamCounters::new());
        let stats = Arc::new(Mutex::new(UdpRecvStats::new()));
        let done = Arc::new(AtomicBool::new(false));

        let c = counters.clone();
        let s = stats.clone();
        let d = done.clone();
        let h = std::thread::spawn(move || run_udp_receiver_blocking(recv, c, s, 1400, d, false));

        let buf = vec![0u8; 1400];
        // Active phase: datagrams flow and are counted.
        for _ in 0..10 {
            let _ = send.send(&buf);
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            counters.bytes_received() > 0,
            "should count during the test"
        );

        // Duration ends.
        done.store(true, Ordering::Relaxed);

        // The peer keeps sending in-flight datagrams after `done` (it hasn't seen
        // our control TestEnd yet). With the fix the receiver holds its socket open
        // and drains them, so every send keeps succeeding; without it the socket
        // closes and a later send draws ECONNREFUSED (the ICMP port-unreachable
        // that resets a <=3.12 peer).
        let loop_start = Instant::now();
        let mut send_err = None;
        for i in 0..30 {
            if let Err(e) = send.send(&buf) {
                send_err = Some((i, e));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let elapsed = loop_start.elapsed();

        // These assertions are only valid if the send loop ran on schedule. If the
        // process was descheduled long enough that a gap between sends could exceed
        // the receiver's 500ms silence window (e.g. CI build contention), the drain
        // may legitimately have seen "silence" and exited — an environmental stall,
        // not a regression. The loop staying under that window guarantees no single
        // gap reached the timeout, so a stall can't false-fail this test.
        if elapsed < Duration::from_millis(400) {
            assert!(
                send_err.is_none(),
                "post-done send #{:?} failed — receiver closed its socket and would reset the peer (#48): {:?}",
                send_err.as_ref().map(|(i, _)| *i),
                send_err.as_ref().map(|(_, e)| e),
            );
            assert!(
                !h.is_finished(),
                "receiver exited while the peer was still sending — would ECONNRESET an iperf3 <=3.12 peer (#48)"
            );
        } else {
            eprintln!(
                "SKIP timing assertion: post-done send loop took {elapsed:?} (process stalled past the 500ms drain window); reset path not exercised this run"
            );
        }

        // Peer stops; the receiver drains to silence and exits cleanly within the cap.
        let start = Instant::now();
        let res = loop {
            if h.is_finished() {
                break h.join().expect("receiver thread panicked");
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "receiver did not exit after the peer went quiet (drain cap exceeded)"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(res.is_ok(), "receiver returned error: {res:?}");
    }
}

// ---------------------------------------------------------------------------
// UDP edge cases (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod udp_edge_tests {
    use crate::stream::{UdpHeader, UdpRecvStats};

    #[test]
    fn udp_header_32bit_sequence_max() {
        let h = UdpHeader {
            sec: 0,
            usec: 0,
            seq: u32::MAX as u64,
        };
        let mut buf = [0u8; 16];
        h.write_to(&mut buf, false);
        let h2 = UdpHeader::read_from(&buf, false).unwrap();
        assert_eq!(h2.seq, u32::MAX as u64);
    }

    #[test]
    fn udp_header_64bit_sequence_max() {
        let h = UdpHeader {
            sec: 0,
            usec: 0,
            seq: u64::MAX,
        };
        let mut buf = [0u8; 16];
        h.write_to(&mut buf, true);
        let h2 = UdpHeader::read_from(&buf, true).unwrap();
        assert_eq!(h2.seq, u64::MAX);
    }

    #[test]
    fn udp_stats_massive_gap() {
        // Simulate losing 1000 packets at once
        let mut stats = UdpRecvStats::new();
        stats.update(
            &UdpHeader {
                sec: 0,
                usec: 0,
                seq: 1,
            },
            0.0,
        );
        stats.update(
            &UdpHeader {
                sec: 0,
                usec: 0,
                seq: 1002,
            },
            1.0,
        );
        assert_eq!(stats.cnt_error, 1000);
        assert_eq!(stats.packet_count, 1002);
    }

    #[test]
    fn udp_stats_duplicate_packet() {
        let mut stats = UdpRecvStats::new();
        stats.update(
            &UdpHeader {
                sec: 0,
                usec: 0,
                seq: 1,
            },
            0.0,
        );
        stats.update(
            &UdpHeader {
                sec: 0,
                usec: 0,
                seq: 2,
            },
            0.001,
        );
        // Duplicate of packet 1
        stats.update(
            &UdpHeader {
                sec: 0,
                usec: 0,
                seq: 1,
            },
            0.002,
        );
        assert_eq!(stats.outoforder_packets, 1);
        assert_eq!(stats.packet_count, 2);
    }
}
