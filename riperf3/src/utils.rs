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
/// iperf3's default `--pacing-timer` interval (µs): the wakeup quantum of its
/// cumulative-average `-b` throttle (`iperf_check_throttle`).
pub const DEFAULT_PACING_TIMER_US: u32 = 1000;

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
        _ => parse_int_base0(s).map_err(|_| ConfigError::InvalidValue("dscp", s.to_string()))?,
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

/// Parse an integer with C `strtol(s, _, 0)` base selection: `0x`/`0X` hex,
/// leading-`0` octal, decimal otherwise. Shared by the `--dscp` and `-S/--tos`
/// numeric arms (#167).
fn parse_int_base0(s: &str) -> std::result::Result<i32, std::num::ParseIntError> {
    if s.starts_with("0x") || s.starts_with("0X") {
        i32::from_str_radix(&s[2..], 16)
    } else if s.starts_with('0') && s.len() > 1 {
        i32::from_str_radix(&s[1..], 8)
    } else {
        s.parse::<i32>()
    }
}

/// Parse a `-S/--tos` value like iperf3: `strtol(optarg, &endptr, 0)` (decimal,
/// `0x` hex, or leading-`0` octal) with the IEBADTOS range check (#167).
pub fn parse_tos(s: &str) -> std::result::Result<i32, ConfigError> {
    // Stricter than C strtol on purpose: iperf3 only checks endptr == optarg,
    // so it ACCEPTS partial parses like `32abc` (→32), `08` (→0), `0x` (→0).
    // Full-string parsing rejects those; recorded as a deliberate divergence
    // (#167 review r1 n4).
    let bad = || {
        ConfigError::InvalidValue(
            "tos",
            // iperf3's IEBADTOS wording, for the unparsable and the
            // out-of-range case alike (iperf3 raises IEBADTOS for both).
            format!("bad TOS value (must be between 0 and 255 inclusive): {s}"),
        )
    };
    let val = parse_int_base0(s.trim()).map_err(|_| bad())?;
    if !(0..=255).contains(&val) {
        return Err(bad());
    }
    Ok(val)
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
/// If `repeating_payload` is true, fills with GT's repeating pattern —
/// ASCII digits `'0'..'9'`, period 10 (`fill_with_repeating_pattern`,
/// iperf_util.c:85-99; #441 r1 fixed the old 0x00..0xFF ramp, which was a
/// wire-payload divergence live-probed against a real GT reverse round).
/// Otherwise returns a zero-filled buffer (the zeros-vs-GT-entropy
/// deviation is tracked in #440).
pub fn make_send_buffer(size: usize, repeating_payload: bool) -> Vec<u8> {
    if repeating_payload {
        (0..size).map(|i| b'0' + (i % 10) as u8).collect()
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

/// iperf3's `MAX_BLOCKSIZE` (1 MiB): the `-l` upper bound for TCP (IEBLOCKSIZE).
pub const MAX_BLOCKSIZE: usize = 1024 * 1024;

/// iperf3's `MAX_BURST`: the `-b rate/burst` upper bound (IEBURST).
pub const MAX_BURST: u32 = 1000;

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
    // 1.5 * multiplier), matching iperf3's `sscanf("%lf", ...)` for normal
    // decimal input (incl. scientific / leading-sign / leading-dot). Scale then
    // truncate to u64 like iperf3's double->int. The non-finite/negative/overflow
    // guards are a deliberate improvement, NOT iperf3 parity: iperf3 doesn't guard
    // these and silently emits garbage (inf->0, nan->i64::MAX, overflow->junk) —
    // we error cleanly instead. (One niche input iperf3 accepts that we don't: hex
    // like "0x10"; immaterial for a bitrate/size.)
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
        // A present burst must be 1..=MAX_BURST, like iperf3's IEBURST check
        // (`burst <= 0 || burst > MAX_BURST`) — "/0" is an error, distinct
        // from no slash at all (#160).
        if burst == 0 || burst > MAX_BURST {
            return Err(ConfigError::InvalidValue(
                "burst count",
                format!("invalid burst count (maximum = {MAX_BURST}): {burst_str}"),
            ));
        }
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
    fn parse_bitrate_burst_range_matches_ieburst() {
        // iperf3 rejects a present burst outside 1..=MAX_BURST (IEBURST,
        // iperf_api.c case 'b': burst <= 0 || burst > MAX_BURST). Note a
        // PRESENT "/0" is an error there, distinct from no slash at all
        // (burst stays 0 = unset). #160.
        assert_eq!(parse_bitrate("100M/1").unwrap(), (100 * 1_000_000, 1));
        assert_eq!(parse_bitrate("100M/1000").unwrap(), (100 * 1_000_000, 1000));
        assert!(parse_bitrate("100M/0").is_err());
        assert!(parse_bitrate("100M/1001").is_err());
        assert!(parse_bitrate("100M/-1").is_err());
    }

    #[test]
    fn parse_tos_strtol_base0() {
        // iperf3 parses -S with strtol(optarg, &endptr, 0): decimal, 0x hex,
        // leading-0 octal all accepted (#167).
        assert_eq!(parse_tos("32").unwrap(), 32);
        assert_eq!(parse_tos("0x20").unwrap(), 0x20);
        assert_eq!(parse_tos("0X20").unwrap(), 0x20);
        assert_eq!(parse_tos("020").unwrap(), 16);
        assert_eq!(parse_tos("0").unwrap(), 0);
        assert_eq!(parse_tos("255").unwrap(), 255);
    }

    #[test]
    fn parse_tos_rejects_out_of_range_and_garbage() {
        // IEBADTOS: "bad TOS value (must be between 0 and 255 inclusive)".
        assert!(parse_tos("256").is_err());
        assert!(parse_tos("-1").is_err());
        assert!(parse_tos("zzz").is_err());
        assert!(parse_tos("0x").is_err());
        assert!(parse_tos("").is_err());
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

    /// GT's pattern: ASCII '0'..'9' with period 10 from offset 0
    /// (fill_with_repeating_pattern, iperf_util.c:85-99) — live-probed
    /// byte-for-byte against a GT 3.21 reverse round (#441 r1).
    #[test]
    fn make_send_buffer_matches_gt_digit_pattern() {
        let buf = make_send_buffer(25, true);
        assert_eq!(&buf[..12], b"012345678901");
        assert_eq!(buf[24], b'4');
    }
}

// ---------------------------------------------------------------------------
// Stream ID assignment (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod stream_id_tests {
    use crate::utils::iperf3_stream_id;

    #[test]
    fn matches_iperf3_pattern() {
        assert_eq!(iperf3_stream_id(0), 1);
        assert_eq!(iperf3_stream_id(1), 3);
        assert_eq!(iperf3_stream_id(2), 4);
        assert_eq!(iperf3_stream_id(3), 5);
        assert_eq!(iperf3_stream_id(4), 6);
        assert_eq!(iperf3_stream_id(9), 11);
    }
}

// ---------------------------------------------------------------------------
// Error path tests (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod error_tests {
    use crate::utils::{parse_bitrate, parse_kmg};
    use crate::ClientBuilder;
    use crate::ConfigError;

    #[test]
    fn build_without_host_fails() {
        let r = ClientBuilder::default().build();
        assert!(matches!(r, Err(ConfigError::MissingField("host"))));
    }

    #[test]
    fn parse_kmg_negative() {
        assert!(parse_kmg("-1").is_err());
    }

    #[test]
    fn parse_kmg_fractional() {
        // #73: fractional suffixes are now accepted (iperf3 parses with %lf).
        // 1.5 * 1024^2 = 1_572_864.
        assert_eq!(parse_kmg("1.5M").unwrap(), 1_572_864);
        // Still rejects genuinely-malformed numbers.
        assert!(parse_kmg("1.5.5M").is_err());
    }

    #[test]
    fn parse_kmg_empty_suffix() {
        assert!(parse_kmg("K").is_err());
    }

    #[test]
    fn parse_bitrate_empty() {
        assert!(parse_bitrate("").is_err());
    }

    #[test]
    fn parse_bitrate_bad_burst() {
        assert!(parse_bitrate("100M/abc").is_err());
    }

    // -- Error type display messages --

    #[test]
    fn error_display_variants() {
        use crate::RiperfError;
        assert_eq!(
            format!("{}", RiperfError::CookieMismatch),
            "cookie mismatch"
        );
        // #362: the GT accept/configure classes — the strerror rides via
        // io::Error's "(os error N)" form, the recorded #151 convention
        // (GT's perr prints strerror alone; the suffix is the house
        // deviation every raw-os class already carries). The strerror
        // TEXT is platform-specific (the macOS CI red on the first pin
        // form), so the pins hold the GT sentence + the suffix only.
        for (rendered, sentence) in [
            (
                format!(
                    "{}",
                    RiperfError::AcceptFailed(std::io::Error::from_raw_os_error(24))
                ),
                "unable to accept connection from client: ",
            ),
            (
                format!(
                    "{}",
                    RiperfError::StreamConnectFailed(std::io::Error::from_raw_os_error(24))
                ),
                "unable to connect stream: ",
            ),
            (
                format!(
                    "{}",
                    RiperfError::SetNoDelayFailed(std::io::Error::from_raw_os_error(24))
                ),
                "unable to set TCP/SCTP NODELAY: ",
            ),
        ] {
            assert!(
                rendered.starts_with(sentence),
                "GT sentence prefix: {rendered}"
            );
            assert!(
                rendered.ends_with("(os error 24)"),
                "the #151 os-error suffix: {rendered}"
            );
        }
        assert_eq!(
            format!("{}", RiperfError::AccessDenied),
            "access denied by server"
        );
        assert_eq!(
            format!("{}", RiperfError::PeerDisconnected),
            "control socket has closed unexpectedly"
        );
        assert!(format!("{}", RiperfError::Aborted("test".into())).contains("test"));
        assert_eq!(
            format!("{}", RiperfError::ConnectionTimeout),
            "connection timed out"
        );
        assert!(format!("{}", RiperfError::Protocol("bad".into())).contains("bad"));
        assert!(format!("{}", RiperfError::Aborted("reason".into())).contains("reason"));
    }

    // -- Edge cases --

    #[tokio::test]
    async fn connect_to_wrong_port_fails() {
        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(1)) // port 1 — almost certainly not listening
            .duration(1)
            .connect_timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let result = client.run().await;
        assert!(result.is_err(), "connecting to port 1 should fail");
    }
}

// ---------------------------------------------------------------------------
// utils unit tests pulled out of implemented_flag_tests
// (migrated in-crate from tests/integration.rs, #67)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod implemented_flag_unit_tests {
    #[test]
    fn repeating_payload_buffer() {
        let buf = crate::utils::make_send_buffer(256, true);
        assert_eq!(buf.len(), 256);
        assert_eq!(buf[0], b'0');
        assert_eq!(buf[1], b'1');
        assert_eq!(buf[255], b'5'); // 255 % 10

        let zeros = crate::utils::make_send_buffer(256, false);
        assert!(zeros.iter().all(|&b| b == 0));
    }

    #[test]
    fn dscp_symbolic_and_numeric() {
        use crate::utils::parse_dscp;
        // Symbolic names
        assert_eq!(parse_dscp("ef").unwrap(), 46 << 2); // EF = 184
        assert_eq!(parse_dscp("af11").unwrap(), 10 << 2); // AF11 = 40
        assert_eq!(parse_dscp("cs1").unwrap(), 8 << 2); // CS1 = 32
                                                        // Numeric
        assert_eq!(parse_dscp("46").unwrap(), 46 << 2);
        assert_eq!(parse_dscp("0x2e").unwrap(), 46 << 2); // 0x2e = 46
        assert_eq!(parse_dscp("056").unwrap(), 46 << 2); // 056 octal = 46
                                                         // Out of range
        assert!(parse_dscp("64").is_err());
        assert!(parse_dscp("abc").is_err());
    }
}
