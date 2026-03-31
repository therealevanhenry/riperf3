use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::ConfigError;

// ---------------------------------------------------------------------------
// Verbose flag
// ---------------------------------------------------------------------------

pub static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

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
            if p.is_empty() { None } else { p.parse().ok() }
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

/// Maximum UDP payload: 65535 - 8 (UDP header) - 20 (IP header)
pub const MAX_UDP_BLKSIZE: usize = 65507;

// ---------------------------------------------------------------------------
// KMG suffix parser
// ---------------------------------------------------------------------------

/// Parse a numeric string with an optional K/M/G/T suffix (case-insensitive).
/// K = 1024, M = 1024^2, G = 1024^3, T = 1024^4.
///
/// Examples: "128K" -> 131072, "1M" -> 1048576, "10G" -> 10737418240, "42" -> 42
pub fn parse_kmg(s: &str) -> std::result::Result<u64, ConfigError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ConfigError::InvalidValue("number", s.to_string()));
    }

    let (num_str, multiplier) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 1024_u64 * 1024),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 1024_u64 * 1024 * 1024),
        Some(b't' | b'T') => (&s[..s.len() - 1], 1024_u64 * 1024 * 1024 * 1024),
        _ => (s, 1),
    };

    let base: u64 = num_str
        .parse()
        .map_err(|_| ConfigError::InvalidValue("number", s.to_string()))?;

    base.checked_mul(multiplier)
        .ok_or_else(|| ConfigError::InvalidValue("number", format!("{s} overflows u64")))
}

/// Parse a bitrate string with optional KMG suffix and optional burst count.
/// Format: `<rate>[KMG][/<burst>]`
/// Returns (rate_bits_per_sec, burst_packets).
pub fn parse_bitrate(s: &str) -> std::result::Result<(u64, u32), ConfigError> {
    let s = s.trim();
    if let Some((rate_str, burst_str)) = s.split_once('/') {
        let rate = parse_kmg(rate_str)?;
        let burst: u32 = burst_str
            .parse()
            .map_err(|_| ConfigError::InvalidValue("burst count", burst_str.to_string()))?;
        Ok((rate, burst))
    } else {
        let rate = parse_kmg(s)?;
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
        assert_eq!(parse_bitrate("100M").unwrap(), (100 * 1024 * 1024, 0));
    }

    #[test]
    fn parse_bitrate_with_burst() {
        assert_eq!(parse_bitrate("100M/10").unwrap(), (100 * 1024 * 1024, 10));
    }
}
