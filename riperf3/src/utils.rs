use crate::error::ConfigError;

// ---------------------------------------------------------------------------
// Default values
// ---------------------------------------------------------------------------

pub const DEFAULT_PORT: u16 = 5201;
pub const DEFAULT_OMIT: u32 = 0;
pub const DEFAULT_DURATION: u32 = 10;
pub const DEFAULT_NUM_STREAMS: u32 = 1;
pub const DEFAULT_TCP_BLKSIZE: usize = 128 * 1024; // 128 KiB
pub const DEFAULT_UDP_BLKSIZE: usize = 1460;
pub const DEFAULT_UDP_RATE: u64 = 1024 * 1024; // 1 Mbit/sec in bits
pub const DEFAULT_TIMESTAMP_FORMAT: &str = "%c ";

/// Minimum UDP datagram size: 4 (sec) + 4 (usec) + 8 (64-bit counter)
pub const MIN_UDP_BLKSIZE: usize = 16;

/// Parse a DSCP value — either numeric (0-63, decimal/octal/hex) or symbolic.
/// Returns the TOS byte value (DSCP << 2).
pub fn parse_dscp(s: &str) -> std::result::Result<i32, ConfigError> {
    // Try symbolic names first
    let dscp_val = match s.to_lowercase().as_str() {
        "cs0" => 0,
        "cs1" => 8,
        "cs2" => 16,
        "cs3" => 24,
        "cs4" => 32,
        "cs5" => 40,
        "cs6" => 48,
        "cs7" => 56,
        "af11" => 10,
        "af12" => 12,
        "af13" => 14,
        "af21" => 18,
        "af22" => 20,
        "af23" => 22,
        "af31" => 26,
        "af32" => 28,
        "af33" => 30,
        "af41" => 34,
        "af42" => 36,
        "af43" => 38,
        "ef" => 46,
        "voice-admit" => 44,
        "le" => 1,
        _ => {
            // Numeric: supports decimal, 0x hex, 0 octal
            let val = if s.starts_with("0x") || s.starts_with("0X") {
                i32::from_str_radix(&s[2..], 16)
            } else if s.starts_with('0') && s.len() > 1 {
                i32::from_str_radix(&s[1..], 8)
            } else {
                s.parse::<i32>()
            };
            val.map_err(|_| ConfigError::InvalidValue("dscp", s.to_string()))?
        }
    };

    if !(0..=63).contains(&dscp_val) {
        return Err(ConfigError::InvalidValue(
            "dscp",
            format!("{dscp_val} out of range 0-63"),
        ));
    }

    // DSCP occupies the top 6 bits of the TOS byte
    Ok(dscp_val << 2)
}

/// Parse a `--cntl-ka` keepalive spec: `idle/interval/count`.
/// Each component is optional (uses system defaults if empty).
/// Examples: "10/5/3", "10//", "//3", ""
pub fn parse_keepalive(s: &str) -> (Option<u32>, Option<u32>, Option<u32>) {
    let parts: Vec<&str> = s.split('/').collect();
    let parse = |i: usize| -> Option<u32> {
        parts.get(i).and_then(|p| {
            let p = p.trim();
            if p.is_empty() {
                None
            } else {
                p.parse().ok()
            }
        })
    };
    (parse(0), parse(1), parse(2))
}

/// Create a send buffer of `size` bytes.
/// If `repeating_payload` is true, fills with a repeating 0x00..0xFF pattern (like iperf2).
/// Otherwise returns a zero-filled buffer.
pub fn make_send_buffer(size: usize, repeating_payload: bool) -> Vec<u8> {
    if repeating_payload {
        (0..size).map(|i| (i % 256) as u8).collect()
    } else {
        vec![0u8; size]
    }
}

/// Compute the stream ID for the given 0-based stream index.
/// Matches iperf3's `iperf_add_stream()` assignment: 1, 3, 4, 5, 6, ...
pub fn iperf3_stream_id(index: u32) -> i32 {
    if index == 0 {
        1
    } else {
        (index + 2) as i32
    }
}

/// iperf3-style `system_info`: the uname fields joined, e.g.
/// "Linux host 6.x #1 SMP ... x86_64". Empty on platforms without `uname`.
/// Shared by the client and server `-J` `start.system_info` (#36, #50).
#[cfg(unix)]
pub fn system_info() -> String {
    match nix::sys::utsname::uname() {
        Ok(u) => format!(
            "{} {} {} {} {}",
            u.sysname().to_string_lossy(),
            u.nodename().to_string_lossy(),
            u.release().to_string_lossy(),
            u.version().to_string_lossy(),
            u.machine().to_string_lossy(),
        ),
        Err(_) => String::new(),
    }
}

#[cfg(not(unix))]
pub fn system_info() -> String {
    String::new()
}

/// Maximum UDP payload: 65535 - 8 (UDP header) - 20 (IP header)
pub const MAX_UDP_BLKSIZE: usize = 65507;

/// Resolve the effective UDP datagram size.
///
/// Mirrors iperf3 (`iperf_client_api.c`): an explicit `-l` always wins;
/// otherwise the size tracks the control-connection MSS, so a jumbo-frame path
/// uses large datagrams instead of the conservative 1460 floor. The MSS is
/// clamped to the valid UDP payload range, and falls back to
/// [`DEFAULT_UDP_BLKSIZE`] when no usable MSS is available (e.g. non-Unix, or a
/// nonsensically small value).
pub fn resolve_udp_blksize(explicit: Option<usize>, ctrl_mss: Option<u32>) -> usize {
    if let Some(size) = explicit {
        return size;
    }
    match ctrl_mss {
        Some(mss) if (mss as usize) >= MIN_UDP_BLKSIZE => (mss as usize).min(MAX_UDP_BLKSIZE),
        _ => DEFAULT_UDP_BLKSIZE,
    }
}

// ---------------------------------------------------------------------------
// KMG suffix parser
// ---------------------------------------------------------------------------

/// Parse a numeric string with an optional K/M/G/T suffix (case-insensitive),
/// where each suffix level multiplies by `base` (K=base, M=base^2, G=base^3,
/// T=base^4). iperf3 splits this: *sizes* use 1024 (`unit_atof`), *rates* use
/// 1000 (`unit_atof_rate`).
fn parse_suffixed(s: &str, base: u64) -> std::result::Result<u64, ConfigError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ConfigError::InvalidValue("number", s.to_string()));
    }

    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], base),
        Some(b'm' | b'M') => (&s[..s.len() - 1], base * base),
        Some(b'g' | b'G') => (&s[..s.len() - 1], base * base * base),
        Some(b't' | b'T') => (&s[..s.len() - 1], base * base * base * base),
        _ => (s, 1),
    };

    // Parse the numeric part as f64 so fractional values work (e.g. "1.5M" ->
    // 1.5 * multiplier), matching iperf3's `sscanf("%lf", ...)`. Scale then
    // truncate to u64, like iperf3's implicit double->int. Reject non-finite,
    // negative, and overflow (a plain `as u64` cast would silently saturate).
    let n: f64 = num_str
        .parse()
        .map_err(|_| ConfigError::InvalidValue("number", s.to_string()))?;
    if !n.is_finite() || n < 0.0 {
        return Err(ConfigError::InvalidValue("number", s.to_string()));
    }
    let scaled = n * multiplier as f64;
    if scaled >= u64::MAX as f64 {
        return Err(ConfigError::InvalidValue(
            "number",
            format!("{s} overflows u64"),
        ));
    }
    Ok(scaled as u64)
}

/// Parse a SIZE with an optional K/M/G/T suffix — **binary** (1024-based),
/// matching iperf3's `unit_atof` for `-w`/`-l`/`-n`/`-k`.
///
/// Examples: "128K" -> 131072, "1M" -> 1048576, "10G" -> 10737418240, "42" -> 42
pub fn parse_kmg(s: &str) -> std::result::Result<u64, ConfigError> {
    parse_suffixed(s, 1024)
}

/// Parse a RATE (bits/sec) with an optional K/M/G/T suffix — **decimal**
/// (1000-based), matching iperf3's `unit_atof_rate` for `-b`/`--fq-rate`.
/// iperf3 parses sizes with 1024 but rates with 1000, so "1M" as a rate is
/// 1_000_000, not 1_048_576 (#56).
///
/// Examples: "1M" -> 1_000_000, "10G" -> 10_000_000_000, "42" -> 42
pub fn parse_rate(s: &str) -> std::result::Result<u64, ConfigError> {
    parse_suffixed(s, 1000)
}

/// Parse a bitrate string with optional KMG suffix and optional burst count.
/// Format: `<rate>[KMG][/<burst>]`
/// Returns (rate_bits_per_sec, burst_packets). The rate uses decimal (1000-based)
/// suffixes like iperf3 (`-b`), e.g. "100M" -> 100_000_000 (#56).
pub fn parse_bitrate(s: &str) -> std::result::Result<(u64, u32), ConfigError> {
    let s = s.trim();
    if let Some((rate_str, burst_str)) = s.split_once('/') {
        let rate = parse_rate(rate_str)?;
        let burst: u32 = burst_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("burst count", burst_str.to_string()))?;
        Ok((rate, burst))
    } else {
        let rate = parse_rate(s)?;
        Ok((rate, 0))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kmg_plain_number() {
        assert_eq!(parse_kmg("42").unwrap(), 42);
        assert_eq!(parse_kmg("0").unwrap(), 0);
        assert_eq!(parse_kmg("1000000").unwrap(), 1_000_000);
    }

    #[test]
    fn parse_kmg_kilo() {
        assert_eq!(parse_kmg("128K").unwrap(), 128 * 1024);
        assert_eq!(parse_kmg("128k").unwrap(), 128 * 1024);
    }

    #[test]
    fn parse_kmg_mega() {
        assert_eq!(parse_kmg("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_kmg("1m").unwrap(), 1024 * 1024);
    }

    #[test]
    fn parse_kmg_giga() {
        assert_eq!(parse_kmg("10G").unwrap(), 10 * 1024 * 1024 * 1024);
        assert_eq!(parse_kmg("10g").unwrap(), 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_kmg_tera() {
        assert_eq!(parse_kmg("1T").unwrap(), 1024_u64.pow(4));
    }

    #[test]
    fn parse_kmg_empty_is_error() {
        assert!(parse_kmg("").is_err());
    }

    #[test]
    fn parse_kmg_invalid_is_error() {
        assert!(parse_kmg("abc").is_err());
        assert!(parse_kmg("12X").is_err());
    }

    #[test]
    fn parse_bitrate_plain() {
        // Rates are decimal (1000-based), like iperf3: 100M = 100_000_000.
        assert_eq!(parse_bitrate("100M").unwrap(), (100 * 1_000_000, 0));
    }

    #[test]
    fn parse_bitrate_with_burst() {
        assert_eq!(parse_bitrate("100M/10").unwrap(), (100 * 1_000_000, 10));
    }

    #[test]
    fn parse_rate_is_decimal() {
        // Rates use 1000-based suffixes (iperf3's unit_atof_rate).
        assert_eq!(parse_rate("1K").unwrap(), 1_000);
        assert_eq!(parse_rate("1M").unwrap(), 1_000_000);
        assert_eq!(parse_rate("10G").unwrap(), 10_000_000_000);
        assert_eq!(parse_rate("42").unwrap(), 42);
        assert_eq!(parse_rate("1m").unwrap(), 1_000_000); // case-insensitive
    }

    #[test]
    fn rate_and_size_suffix_bases_differ() {
        // The crux of #56: the same "1M" is 1000^2 as a rate, 1024^2 as a size.
        assert_eq!(parse_rate("1M").unwrap(), 1_000_000);
        assert_eq!(parse_kmg("1M").unwrap(), 1_048_576);
        assert_ne!(parse_rate("1M").unwrap(), parse_kmg("1M").unwrap());
    }

    #[test]
    fn fractional_suffixes_accepted() {
        // #73: iperf3 parses the numeric part with %lf, so fractional values work.
        assert_eq!(parse_rate("1.5M").unwrap(), 1_500_000); // 1.5 * 1000^2
        assert_eq!(parse_kmg("1.5K").unwrap(), 1_536); // 1.5 * 1024
        assert_eq!(parse_kmg("0.5M").unwrap(), 524_288); // 0.5 * 1024^2
        assert_eq!(parse_bitrate("2.5M").unwrap(), (2_500_000, 0));
        // Plain integers and the exact-value tests above still hold (f64 is exact
        // for these magnitudes).
        assert_eq!(parse_rate("42").unwrap(), 42);
    }

    #[test]
    fn rejects_non_finite_negative_and_garbage() {
        assert!(parse_rate("-5M").is_err()); // negative
        assert!(parse_rate("inf").is_err()); // non-finite
        assert!(parse_kmg("nanK").is_err());
        assert!(parse_kmg("1.5.5M").is_err()); // not a number
        assert!(parse_kmg("abc").is_err());
        assert!(parse_kmg("K").is_err()); // suffix only, no number
    }

    // -- parse_keepalive --

    #[test]
    fn parse_keepalive_all_values() {
        assert_eq!(parse_keepalive("10/5/3"), (Some(10), Some(5), Some(3)));
    }

    #[test]
    fn parse_keepalive_partial() {
        assert_eq!(parse_keepalive("10//"), (Some(10), None, None));
        assert_eq!(parse_keepalive("//3"), (None, None, Some(3)));
        assert_eq!(parse_keepalive("/5/"), (None, Some(5), None));
        assert_eq!(parse_keepalive("10/5"), (Some(10), Some(5), None));
    }

    #[test]
    fn parse_keepalive_empty() {
        assert_eq!(parse_keepalive(""), (None, None, None));
    }

    #[test]
    fn parse_keepalive_single_value() {
        assert_eq!(parse_keepalive("30"), (Some(30), None, None));
    }

    #[test]
    fn parse_keepalive_invalid_ignored() {
        // Non-numeric values parse as None (not an error)
        assert_eq!(parse_keepalive("abc/def/ghi"), (None, None, None));
    }

    // -- parse_dscp edge cases --

    #[test]
    fn parse_dscp_all_classes() {
        assert_eq!(parse_dscp("cs0").unwrap(), 0);
        assert_eq!(parse_dscp("cs7").unwrap(), 56 << 2);
        assert_eq!(parse_dscp("le").unwrap(), 1 << 2);
        assert_eq!(parse_dscp("voice-admit").unwrap(), 44 << 2);
    }

    #[test]
    fn parse_dscp_case_insensitive() {
        assert_eq!(parse_dscp("EF").unwrap(), parse_dscp("ef").unwrap());
        assert_eq!(parse_dscp("AF11").unwrap(), parse_dscp("af11").unwrap());
    }

    // -- resolve_udp_blksize: MSS-derived default (issue #6) --

    #[test]
    fn resolve_udp_blksize_explicit_wins() {
        // An explicit -l always takes precedence, even when an MSS is known.
        assert_eq!(resolve_udp_blksize(Some(2000), Some(8928)), 2000);
        assert_eq!(resolve_udp_blksize(Some(1460), None), 1460);
    }

    #[test]
    fn resolve_udp_blksize_derives_from_mss() {
        // No -l: track the control-connection MSS, like iperf3. On a jumbo path
        // this yields large datagrams instead of the conservative 1460 floor.
        assert_eq!(resolve_udp_blksize(None, Some(8928)), 8928);
        assert_eq!(resolve_udp_blksize(None, Some(1448)), 1448);
    }

    #[test]
    fn resolve_udp_blksize_falls_back_without_mss() {
        // No -l and no MSS available (e.g. non-Unix): historical 1460 default.
        assert_eq!(resolve_udp_blksize(None, None), DEFAULT_UDP_BLKSIZE);
    }

    #[test]
    fn resolve_udp_blksize_clamps_to_udp_bounds() {
        // A bogus huge MSS is clamped to the max UDP payload; a sub-header MSS
        // falls back to the safe default rather than producing tiny datagrams.
        assert_eq!(resolve_udp_blksize(None, Some(70_000)), MAX_UDP_BLKSIZE);
        assert_eq!(resolve_udp_blksize(None, Some(8)), DEFAULT_UDP_BLKSIZE);
    }

    // -- make_send_buffer edge cases --

    #[test]
    fn make_send_buffer_wraps_at_256() {
        let buf = make_send_buffer(512, true);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[255], 255);
        assert_eq!(buf[256], 0); // wraps
        assert_eq!(buf[511], 255);
    }
}
