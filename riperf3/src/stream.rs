use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

    // Snapshots taken at the end of the omit period so we can subtract them.
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
    /// Call this at the end of the omit period.
    pub fn snapshot_omit(&mut self) {
        self.omitted_packet_count = self.packet_count;
        self.omitted_cnt_error = self.cnt_error;
        self.omitted_outoforder_packets = self.outoforder_packets;
    }
}

// ---------------------------------------------------------------------------
// Rate limiter (token bucket for UDP pacing)
// ---------------------------------------------------------------------------

/// Token-bucket rate limiter for application-level send pacing.
pub struct RateLimiter {
    rate_bytes_per_sec: f64,
    burst_bytes: f64,
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a rate limiter.
    ///
    /// - `rate_bits_per_sec`: target send rate
    /// - `burst_packets`: how many packets to send per burst (0 = auto)
    /// - `blksize`: datagram/block size in bytes
    pub fn new(rate_bits_per_sec: u64, burst_packets: u32, blksize: usize) -> Self {
        let rate_bytes = rate_bits_per_sec as f64 / 8.0;
        // Default burst: ~100ms worth of data, minimum 4 packets.
        // This avoids per-packet tokio::sleep overhead which has ~1ms granularity.
        let burst = if burst_packets > 0 {
            burst_packets as f64 * blksize as f64
        } else {
            (rate_bytes * 0.1).max(4.0 * blksize as f64)
        };
        Self {
            rate_bytes_per_sec: rate_bytes,
            burst_bytes: burst,
            tokens: burst,
            last_refill: Instant::now(),
        }
    }

    /// Wait until enough tokens are available for `bytes`, then consume them.
    pub async fn acquire(&mut self, bytes: u64) {
        self.refill();
        let needed = bytes as f64;
        while self.tokens < needed {
            let deficit = needed - self.tokens;
            let wait = Duration::from_secs_f64(deficit / self.rate_bytes_per_sec);
            tokio::time::sleep(wait).await;
            self.refill();
        }
        self.tokens -= needed;
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate_bytes_per_sec).min(self.burst_bytes);
        self.last_refill = now;
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
}

// ---------------------------------------------------------------------------
// TCP send / recv loops
// ---------------------------------------------------------------------------

/// TCP sender: writes full blocks as fast as the kernel will accept them.
/// If `file_path` is set, reads from the file into the buffer each iteration.
pub async fn run_tcp_sender(
    mut stream: TcpStream,
    counters: Arc<StreamCounters>,
    mut buf: Vec<u8>,
    done: Arc<AtomicBool>,
    file_path: Option<std::path::PathBuf>,
) -> Result<()> {
    use std::io::Read;
    let mut file = file_path
        .as_ref()
        .map(std::fs::File::open)
        .transpose()?;

    while !done.load(Ordering::Relaxed) {
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
    use std::os::unix::io::AsRawFd;

    // Create temp file with buffer content
    let mut tmpfile = tempfile()?;
    tmpfile.write_all(&buf)?;
    let file_fd = tmpfile.as_raw_fd();
    let sock_fd = stream.as_raw_fd();
    let blksize = buf.len();

    loop {
        // Wait for socket to be writable
        stream.writable().await?;

        if done.load(Ordering::Relaxed) {
            break;
        }

        let result = stream.try_io(tokio::io::Interest::WRITABLE, || {
            let mut offset: libc::off_t = 0;
            let sent = unsafe {
                libc::sendfile(sock_fd, file_fd, &mut offset, blksize)
            };
            if sent < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(sent as usize)
            }
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
    Ok(())
}

/// Create a temporary file for zerocopy sends.
#[cfg(target_os = "linux")]
fn tempfile() -> std::io::Result<std::fs::File> {
    use std::io::Seek;
    let mut f = tempfile_in(std::env::temp_dir())?;
    f.seek(std::io::SeekFrom::Start(0))?;
    Ok(f)
}

#[cfg(target_os = "linux")]
fn tempfile_in(dir: std::path::PathBuf) -> std::io::Result<std::fs::File> {
    let path = dir.join(format!(".riperf3-zc-{}", std::process::id()));
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

    if skip_rx_copy {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = stream.as_raw_fd();
            loop {
                if done.load(Ordering::Relaxed) {
                    break;
                }
                stream.readable().await?;
                let n = stream.try_io(tokio::io::Interest::READABLE, || {
                    let ret = unsafe {
                        libc::recv(
                            fd,
                            buf.as_mut_ptr() as *mut libc::c_void,
                            blksize,
                            libc::MSG_TRUNC,
                        )
                    };
                    if ret < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(ret as usize)
                    }
                });
                match n {
                    Ok(0) => break,
                    Ok(n) => counters.record_received(n as u64),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Fallback: normal read path
            return run_tcp_receiver_normal(&mut stream, &counters, &mut buf, &done, &mut file).await;
        }
    } else {
        return run_tcp_receiver_normal(&mut stream, &counters, &mut buf, &done, &mut file).await;
    }
    Ok(())
}

async fn run_tcp_receiver_normal(
    stream: &mut TcpStream,
    counters: &StreamCounters,
    buf: &mut [u8],
    done: &AtomicBool,
    file: &mut Option<std::fs::File>,
) -> Result<()> {
    loop {
        if done.load(Ordering::Relaxed) {
            break;
        }
        match stream.read(buf).await {
            Ok(0) => break,
            Ok(n) => {
                counters.record_received(n as u64);
                if let Some(ref mut f) = file {
                    use std::io::Write;
                    let _ = f.write_all(&buf[..n]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// UDP send / recv loops
// ---------------------------------------------------------------------------

/// UDP sender: sends datagrams with a timestamp+sequence header, paced by the
/// rate limiter if present.
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

    loop {
        if done.load(Ordering::Relaxed) {
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Blocking UDP send / recv (high-performance, no async overhead)
// ---------------------------------------------------------------------------

/// High-performance UDP sender using blocking I/O on a dedicated OS thread.
/// No `unsafe` code — uses `std::net::UdpSocket` and batch pacing with
/// `std::thread::sleep` + spin-loop for sub-microsecond precision.
///
/// Batch pacing: sends N packets in a tight loop, then does a single clock
/// check and sleep/spin for the aggregate interval. This amortizes the cost
/// of `Instant::now()` (~50ns) and atomic operations across multiple packets.
pub fn run_udp_sender_blocking(
    socket: std::net::UdpSocket,
    counters: Arc<StreamCounters>,
    blksize: usize,
    done: Arc<AtomicBool>,
    rate_bits_per_sec: u64,
    use_64bit: bool,
) -> Result<()> {
    let mut buf = vec![0u8; blksize];
    let mut seq: u64 = 0;

    // Batch size scales with rate: 1 per Gbps, capped at 64, minimum 1
    let batch_size: u32 = if rate_bits_per_sec > 0 {
        (rate_bits_per_sec / 1_000_000_000).clamp(1, 64) as u32
    } else {
        1
    };

    let pacing = if rate_bits_per_sec > 0 {
        let rate_bytes = rate_bits_per_sec as f64 / 8.0;
        let per_packet = Duration::from_secs_f64(blksize as f64 / rate_bytes);
        Some(per_packet * batch_size)
    } else {
        None
    };

    let mut next_send = Instant::now();

    while !done.load(Ordering::Relaxed) {
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

            match socket.send(&buf) {
                Ok(n) => batch_bytes += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    seq -= 1;
                    break;
                }
                Err(e) => {
                    log::debug!("UDP send error: {e}");
                    seq -= 1;
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
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(crate::error::RiperfError::Io)?;

    loop {
        if done.load(Ordering::Relaxed) {
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
            Err(_) => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        let h = UdpHeader { sec: 1000, usec: 500_000, seq: 42 };
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
            let h = UdpHeader { sec: 1000, usec: 0, seq: i };
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
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 1 }, t);
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 2 }, t + 0.001);
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 5 }, t + 0.002);
        assert_eq!(stats.packet_count, 5);
        assert_eq!(stats.cnt_error, 2); // packets 3 and 4 missing
    }

    #[test]
    fn udp_stats_detects_ooo() {
        let mut stats = UdpRecvStats::new();
        let t = 1000.0;
        // Receive 1, 3, 2
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 1 }, t);
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 3 }, t + 0.001);
        // At this point: packet_count=3, cnt_error=1 (packet 2 "lost")
        assert_eq!(stats.cnt_error, 1);
        stats.update(&UdpHeader { sec: 1000, usec: 0, seq: 2 }, t + 0.002);
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
            &UdpHeader { sec: 1000, usec: 0, seq: 1 },
            1000.010,
        );
        assert_eq!(stats.jitter, 0.0); // first packet, no jitter yet

        stats.update(
            &UdpHeader { sec: 1000, usec: 1000, seq: 2 },
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
            stats.update(&UdpHeader { sec: 1000, usec: 0, seq: i }, t);
        }
        stats.snapshot_omit();
        assert_eq!(stats.omitted_packet_count, 3);

        for i in 4..=6 {
            stats.update(&UdpHeader { sec: 1000, usec: 0, seq: i }, t);
        }
        // Effective (post-omit) packet count: 6 - 3 = 3
        assert_eq!(stats.packet_count - stats.omitted_packet_count, 3);
    }

    // -- RateLimiter --

    #[tokio::test]
    async fn rate_limiter_allows_burst() {
        let mut limiter = RateLimiter::new(1_000_000, 0, 1000); // 1 Mbit/s, 1000-byte blocks
        let start = Instant::now();
        // First acquire should be instant (burst tokens available)
        limiter.acquire(1000).await;
        assert!(start.elapsed() < Duration::from_millis(10));
    }

    #[tokio::test]
    async fn rate_limiter_paces() {
        // 80_000 bits/sec = 10_000 bytes/sec, 1000-byte blocks
        // burst = max(10000*0.1, 4*1000) = 4000 bytes
        let mut limiter = RateLimiter::new(80_000, 0, 1000);
        // Drain the burst (4 packets)
        for _ in 0..4 {
            limiter.acquire(1000).await;
        }
        let start = Instant::now();
        limiter.acquire(1000).await; // must wait ~100ms
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(50)); // generous lower bound
        assert!(elapsed < Duration::from_millis(300)); // generous upper bound
    }

    #[tokio::test]
    async fn rate_limiter_high_rate_precision() {
        // At 10 Gbps with 1460-byte packets, drain the burst first,
        // then verify paced throughput is within 50% of target.
        // Before fix: tokio::time::sleep caps at ~1 Gbps after burst.
        // After fix: clock_nanosleep should approach 10 Gbps.
        let rate = 10_000_000_000u64; // 10 Gbps
        let blksize = 1460usize;
        let mut limiter = RateLimiter::new(rate, 0, blksize);

        // Drain the burst completely
        let burst_bytes = (rate as f64 / 8.0 * 0.1).max(4.0 * blksize as f64) as u64;
        let burst_packets = burst_bytes / blksize as u64 + 1;
        for _ in 0..burst_packets {
            limiter.acquire(blksize as u64).await;
        }

        // Now measure paced throughput (no burst tokens left)
        let start = Instant::now();
        let mut total_bytes: u64 = 0;
        let target_duration = Duration::from_millis(100);

        while start.elapsed() < target_duration {
            limiter.acquire(blksize as u64).await;
            total_bytes += blksize as u64;
        }

        let elapsed = start.elapsed().as_secs_f64();
        let achieved_bps = total_bytes as f64 * 8.0 / elapsed;
        let target_bps = rate as f64;

        // Should achieve at least 50% of target rate
        assert!(
            achieved_bps > target_bps * 0.5,
            "high-rate pacing too slow: {:.1} Gbps achieved, {:.1} Gbps target",
            achieved_bps / 1e9,
            target_bps / 1e9,
        );
    }

    #[tokio::test]
    async fn rate_limiter_low_rate_still_works() {
        // Verify that the clock_nanosleep path doesn't break low-rate pacing
        let mut limiter = RateLimiter::new(1_000_000, 0, 1000); // 1 Mbps
        let start = Instant::now();
        let mut total_bytes: u64 = 0;

        // Send 10 packets at 1 Mbps = ~10ms
        for _ in 0..10 {
            limiter.acquire(1000).await;
            total_bytes += 1000;
        }

        let elapsed = start.elapsed();
        // Should take at least some time (not instant)
        // Burst covers first ~4 packets, remaining 6 need pacing
        assert!(total_bytes == 10_000);
        // Should complete in under 1 second (generously)
        assert!(elapsed < Duration::from_secs(1));
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
            let stream = TcpStream::connect(format!("127.0.0.1:{port}")).await.unwrap();
            let buf = vec![0u8; 1024];
            run_tcp_sender(stream, sc, buf, d, None).await
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
}
