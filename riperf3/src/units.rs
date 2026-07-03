/// Format a byte count for display with appropriate units.
///
/// Bytes use 1024-based units (KBytes, MBytes, GBytes, TBytes).
/// Bits use 1000-based units (Kbits, Mbits, Gbits, Tbits).
///
/// `format_char` selects the mode:
/// - `'a'` / `'A'` — adaptive (auto-pick best unit). Lowercase = bits, uppercase = bytes.
/// - `'k'` = Kbits, `'K'` = KBytes
/// - `'m'` = Mbits, `'M'` = MBytes
/// - `'g'` = Gbits, `'G'` = GBytes
/// - `'t'` = Tbits, `'T'` = TBytes
pub fn format_bytes(bytes: f64, format_char: char) -> String {
    match format_char {
        'A' => adaptive_bytes(bytes),
        'a' => adaptive_bits(bytes * 8.0),
        // 'B'/'b' mirror GT's lib-level unit_snprintf arms (units.c:305,
        // UNIT_CONV): the raw figure, no magnitude conversion. CLI-unreachable
        // in both tools — GT's getopt rejects them with IEBADFORMAT (#263).
        'B' => format!("{} Bytes", ladder(bytes)),
        'b' => format!("{} bits", ladder(bytes * 8.0)),
        'K' => format!("{} KBytes", ladder(bytes / 1024.0)),
        'k' => format!("{} Kbits", ladder(bytes * 8.0 / 1000.0)),
        'M' => format!("{} MBytes", ladder(bytes / (1024.0 * 1024.0))),
        'm' => format!("{} Mbits", ladder(bytes * 8.0 / 1_000_000.0)),
        'G' => format!("{} GBytes", ladder(bytes / (1024.0 * 1024.0 * 1024.0))),
        'g' => format!("{} Gbits", ladder(bytes * 8.0 / 1_000_000_000.0)),
        'T' => format!(
            "{} TBytes",
            ladder(bytes / (1024.0 * 1024.0 * 1024.0 * 1024.0))
        ),
        't' => format!("{} Tbits", ladder(bytes * 8.0 / 1_000_000_000_000.0)),
        _ => adaptive_bytes(bytes),
    }
}

/// iperf3's unit_snprintf precision ladder (#221): the printed figure keeps
/// ~3 significant digits — < 9.995 → 2 dp, < 99.95 → 1 dp, else 0 dp. The
/// boundaries are round-aware (units.c: "9.995 would be rounded to 10.0",
/// "99.95 would be rounded to 100"), and the figure is right-padded to four
/// characters (`%4.2f`/`%4.1f`/`%4.0f`) — GT's row templates drop the cell
/// in with no alignment of their own, so the pad IS the column alignment
/// (#264; only 3-digit `%.0f` figures actually gain a space).
fn ladder(n: f64) -> String {
    if n < 9.995 {
        format!("{n:4.2}")
    } else if n < 99.95 {
        format!("{n:4.1}")
    } else {
        format!("{n:4.0}")
    }
}

/// C's `%.2g`, for GT's UDP loss percent (report_bw_udp_format's `(%.2g%%)`,
/// #264): two significant digits, trailing zeros stripped, switching to
/// e-notation when the ROUNDED exponent falls outside [-4, 2) — C99
/// fprintf's %g rules, decided after rounding (99.6 → 100 → `1e+02`).
pub(crate) fn g2(v: f64) -> String {
    if !v.is_finite() {
        // C's %g renders these as words; {:.1e} has no exponent to split on.
        return if v.is_nan() {
            "nan".to_string()
        } else if v > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    if v == 0.0 {
        return "0".to_string();
    }
    // {:.1e} rounds to 2 significant digits and normalizes the mantissa,
    // giving the post-rounding exponent C uses for the notation switch.
    let sci = format!("{v:.1e}");
    let (mant, exp) = sci.split_once('e').expect("LowerExp always has an e");
    let exp: i32 = exp.parse().expect("integer exponent");
    if !(-4..2).contains(&exp) {
        let mant = mant.trim_end_matches('0').trim_end_matches('.');
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        let prec = (1 - exp).max(0) as usize;
        let s = format!("{v:.prec$}");
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
}

/// Format a bits-per-second rate for display.
///
/// Same char table as [`format_bytes`] (#241): lowercase = bit-rates with
/// 1000 divisors, UPPERCASE = byte-rates with 1024 divisors — GT's
/// unit_snprintf takes bytes and only multiplies by 8 for the lowercase
/// set (units.c:299-302), so `-f K` means KBytes/sec, never Kbits.
pub fn format_rate(bits_per_sec: f64, format_char: char) -> String {
    let bytes_per_sec = bits_per_sec / 8.0;
    match format_char {
        // Lib-level 'B'/'b' twins of the format_bytes arms (#263).
        'B' => format!("{} Bytes/sec", ladder(bytes_per_sec)),
        'b' => format!("{} bits/sec", ladder(bits_per_sec)),
        'k' => format!("{} Kbits/sec", ladder(bits_per_sec / 1000.0)),
        'm' => format!("{} Mbits/sec", ladder(bits_per_sec / 1_000_000.0)),
        'g' => format!("{} Gbits/sec", ladder(bits_per_sec / 1_000_000_000.0)),
        't' => format!("{} Tbits/sec", ladder(bits_per_sec / 1_000_000_000_000.0)),
        'K' => format!("{} KBytes/sec", ladder(bytes_per_sec / 1024.0)),
        'M' => format!("{} MBytes/sec", ladder(bytes_per_sec / (1024.0 * 1024.0))),
        'G' => format!(
            "{} GBytes/sec",
            ladder(bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
        ),
        'T' => format!(
            "{} TBytes/sec",
            ladder(bytes_per_sec / (1024.0 * 1024.0 * 1024.0 * 1024.0))
        ),
        'A' => adaptive_rate_bytes(bytes_per_sec),
        _ => adaptive_rate(bits_per_sec),
    }
}

fn adaptive_bytes(bytes: f64) -> String {
    const K: f64 = 1024.0;
    const M: f64 = K * 1024.0;
    const G: f64 = M * 1024.0;
    const T: f64 = G * 1024.0;

    if bytes >= T {
        format!("{} TBytes", ladder(bytes / T))
    } else if bytes >= G {
        format!("{} GBytes", ladder(bytes / G))
    } else if bytes >= M {
        format!("{} MBytes", ladder(bytes / M))
    } else if bytes >= K {
        format!("{} KBytes", ladder(bytes / K))
    } else {
        format!("{} Bytes", ladder(bytes))
    }
}

fn adaptive_bits(bits: f64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * 1000.0;
    const G: f64 = M * 1000.0;
    const T: f64 = G * 1000.0;

    if bits >= T {
        format!("{} Tbits", ladder(bits / T))
    } else if bits >= G {
        format!("{} Gbits", ladder(bits / G))
    } else if bits >= M {
        format!("{} Mbits", ladder(bits / M))
    } else if bits >= K {
        format!("{} Kbits", ladder(bits / K))
    } else {
        format!("{} bits", ladder(bits))
    }
}

/// The byte-rate twin of [`adaptive_rate`]: unit_snprintf's uppercase
/// adaptive arm, 1024-stepped (#241).
fn adaptive_rate_bytes(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    const M: f64 = K * 1024.0;
    const G: f64 = M * 1024.0;
    const T: f64 = G * 1024.0;

    if bytes_per_sec >= T {
        format!("{} TBytes/sec", ladder(bytes_per_sec / T))
    } else if bytes_per_sec >= G {
        format!("{} GBytes/sec", ladder(bytes_per_sec / G))
    } else if bytes_per_sec >= M {
        format!("{} MBytes/sec", ladder(bytes_per_sec / M))
    } else if bytes_per_sec >= K {
        format!("{} KBytes/sec", ladder(bytes_per_sec / K))
    } else {
        format!("{} Bytes/sec", ladder(bytes_per_sec))
    }
}

fn adaptive_rate(bits_per_sec: f64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * 1000.0;
    const G: f64 = M * 1000.0;
    const T: f64 = G * 1000.0;

    if bits_per_sec >= T {
        format!("{} Tbits/sec", ladder(bits_per_sec / T))
    } else if bits_per_sec >= G {
        format!("{} Gbits/sec", ladder(bits_per_sec / G))
    } else if bits_per_sec >= M {
        format!("{} Mbits/sec", ladder(bits_per_sec / M))
    } else if bits_per_sec >= K {
        format!("{} Kbits/sec", ladder(bits_per_sec / K))
    } else {
        format!("{} bits/sec", ladder(bits_per_sec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_adaptive() {
        // The ladder applies at the sub-unit floor too (r1 blocker —
        // iperf3's canonical stall row is "0.00 Bytes  0.00 bits/sec").
        assert_eq!(format_bytes(0.0, 'A'), "0.00 Bytes");
        assert_eq!(format_bytes(16.0, 'A'), "16.0 Bytes");
        assert_eq!(format_bytes(50.0, 'A'), "50.0 Bytes");
        assert_eq!(format_rate(0.0, 'a'), "0.00 bits/sec");
        assert_eq!(format_bytes(1.0, 'a'), "8.00 bits");
        assert_eq!(format_bytes(500.0, 'A'), " 500 Bytes");
        assert_eq!(format_bytes(1024.0, 'A'), "1.00 KBytes");
        assert_eq!(format_bytes(1024.0 * 1024.0 * 1.5, 'A'), "1.50 MBytes");
        assert_eq!(
            format_bytes(1024.0 * 1024.0 * 1024.0 * 2.5, 'A'),
            "2.50 GBytes"
        );
    }

    #[test]
    fn format_bytes_fixed_units() {
        let bytes = 1024.0 * 1024.0; // 1 MiB
                                     // iperf3's magnitude ladder applies to FIXED units too
                                     // (unit_snprintf formats after conversion): >=999.5 -> %.0f.
        assert_eq!(format_bytes(bytes, 'K'), "1024 KBytes");
        assert_eq!(format_bytes(bytes, 'M'), "1.00 MBytes");
        assert_eq!(format_bytes(bytes, 'G'), "0.00 GBytes");
    }

    /// iperf3's t_units.c expectations, adjusted only for riperf3's baked
    /// plural "s" (iperf3's row formats append it): the magnitude ladder is
    /// <9.995 -> %.2f, <99.95 -> %.1f, else %.0f (#221).
    #[test]
    fn iperf3_t_units_pins() {
        let gib4 = 4.0 * 1024.0 * 1024.0 * 1024.0;
        let tib4 = gib4 * 1024.0;
        let pib4 = tib4 * 1024.0;
        assert_eq!(format_bytes(1024.0, 'A'), "1.00 KBytes");
        assert_eq!(format_bytes(1024.0 * 1024.0, 'A'), "1.00 MBytes");
        assert_eq!(format_bytes(1000.0, 'k'), "8.00 Kbits");
        assert_eq!(format_bytes(1000.0 * 1000.0, 'a'), "8.00 Mbits");
        assert_eq!(format_bytes(gib4, 'A'), "4.00 GBytes");
        assert_eq!(format_bytes(gib4, 'a'), "34.4 Gbits");
        assert_eq!(format_bytes(tib4, 'A'), "4.00 TBytes");
        assert_eq!(format_bytes(tib4, 'a'), "35.2 Tbits");
        // Past the TERA cap the number grows; %.0f keeps it integral.
        assert_eq!(format_bytes(pib4, 'A'), "4096 TBytes");
        assert_eq!(format_bytes(pib4, 'a'), "36029 Tbits");
    }

    /// The ladder boundaries are ROUND-aware (iperf3's 9.995/99.95/999.5
    /// comments: "9.995 would be rounded to 10.0"), and the issue's own
    /// examples render correctly (#221: 12120.88 MBytes -> 11.8 GBytes
    /// class; the GT row was 11.7 GBytes / 101 Gbits/sec).
    #[test]
    fn ladder_boundaries_and_issue_examples() {
        // 11.7 GBytes (the live GT row figure)
        let b = 11.7 * 1024.0 * 1024.0 * 1024.0;
        assert_eq!(format_bytes(b, 'A'), "11.7 GBytes");
        // 101 Gbits/sec (>=99.95 -> %.0f)
        assert_eq!(format_rate(101.0e9, 'a'), " 101 Gbits/sec");
        // boundary: 9.994 stays 2dp, 9.996 promotes to 1dp ("10.0")
        assert_eq!(format_rate(9.994e9, 'a'), "9.99 Gbits/sec");
        assert_eq!(format_rate(9.996e9, 'a'), "10.0 Gbits/sec");
        // boundary: 99.94 stays 1dp, 99.96 promotes to 0dp ("100")
        assert_eq!(format_rate(99.94e9, 'a'), "99.9 Gbits/sec");
        assert_eq!(format_rate(99.96e9, 'a'), " 100 Gbits/sec");
    }

    #[test]
    fn format_bits_adaptive() {
        let bytes = 125_000.0; // 1 Mbit
        assert_eq!(format_bytes(bytes, 'a'), "1.00 Mbits");
    }

    #[test]
    fn format_rate_fixed() {
        assert_eq!(format_rate(1_000_000_000.0, 'g'), "1.00 Gbits/sec");
        // ladder: 1000 >= 999.5 -> %.0f
        assert_eq!(format_rate(1_000_000_000.0, 'm'), "1000 Mbits/sec");
    }

    /// #241: uppercase = BYTE-rates — GT's unit_snprintf takes bytes and
    /// only multiplies by 8 for lowercase (units.c:299-302), with 1024
    /// divisors for the byte side. Live GT pin (2026-06-11): a 10 Mbit/s
    /// run under `-f K` prints "1278 KBytes/sec", NOT Kbits.
    #[test]
    fn format_rate_uppercase_byte_rates() {
        assert_eq!(format_rate(8.0 * 1024.0, 'K'), "1.00 KBytes/sec");
        assert_eq!(format_rate(8.0 * 1024.0 * 1024.0, 'M'), "1.00 MBytes/sec");
        assert_eq!(
            format_rate(8.0 * 1024.0 * 1024.0 * 1024.0, 'G'),
            "1.00 GBytes/sec"
        );
        assert_eq!(
            format_rate(8.0 * 1024.0 * 1024.0 * 1024.0 * 1024.0, 'T'),
            "1.00 TBytes/sec"
        );
        // The precision ladder applies to byte-rates too: 10 Mbit/s ->
        // 1,310,720 bytes/s -> 1280 KiB/s -> %.0f.
        assert_eq!(format_rate(10_485_760.0, 'K'), "1280 KBytes/sec");
        // Case is semantic, not cosmetic.
        assert_eq!(format_rate(10_485_760.0, 'k'), "10486 Kbits/sec");
    }

    /// 'A' as a rate format = adaptive BYTE-rate (unit_snprintf's uppercase
    /// adaptive arm, 1024-stepped). Unreachable from the CLI (GT rejects
    /// -f A with IEBADFORMAT) but part of the lib's format_char surface.
    #[test]
    fn format_rate_adaptive_bytes() {
        assert_eq!(format_rate(16_384.0, 'A'), "2.00 KBytes/sec");
        assert_eq!(
            format_rate(8.0 * 1024.0 * 1024.0 * 1.5, 'A'),
            "1.50 MBytes/sec"
        );
        assert_eq!(format_rate(800.0, 'A'), " 100 Bytes/sec");
    }

    #[test]
    fn format_rate_adaptive() {
        assert_eq!(format_rate(500.0, 'a'), " 500 bits/sec");
        assert_eq!(format_rate(1_500_000.0, 'a'), "1.50 Mbits/sec");
        assert_eq!(format_rate(9_420_000_000.0, 'a'), "9.42 Gbits/sec");
    }

    /// #264: unit_snprintf right-pads the FIGURE to four characters
    /// (`%4.2f`/`%4.1f`/`%4.0f`) — the pad belongs to the cell, not the row
    /// template. Only 3-digit `%.0f` figures gain a space; 4+ digits and
    /// 2-dp/1-dp figures are already four wide.
    #[test]
    fn figures_pad_to_gt_width_four() {
        assert_eq!(format_rate(300_000_000.0, 'a'), " 300 Mbits/sec");
        assert_eq!(format_bytes(384.0 * 1024.0, 'A'), " 384 KBytes");
        // 4-digit and sub-100 figures carry no extra pad.
        assert_eq!(format_rate(1_000_000_000.0, 'm'), "1000 Mbits/sec");
        assert_eq!(format_rate(9_420_000_000.0, 'a'), "9.42 Gbits/sec");
    }

    /// #263: 'B'/'b' mirror GT's lib-level unit_snprintf arms — raw Bytes /
    /// bits with NO magnitude conversion (units.c:305, UNIT_CONV), ladder
    /// precision applied to the raw figure. CLI-unreachable in both tools
    /// (GT rejects them with IEBADFORMAT), lib-reachable via format_char.
    #[test]
    fn b_chars_mirror_gt_lib_behavior() {
        assert_eq!(format_bytes(500.0, 'B'), " 500 Bytes");
        assert_eq!(format_bytes(500.0, 'b'), "4000 bits");
        assert_eq!(format_bytes(1_000_000.0, 'b'), "8000000 bits");
        assert_eq!(format_rate(4000.0, 'b'), "4000 bits/sec");
        assert_eq!(format_rate(4096.0, 'B'), " 512 Bytes/sec");
    }

    /// #264: C's `%.2g` for the UDP loss percent — two significant digits,
    /// trailing zeros stripped, e-notation once the rounded exponent
    /// reaches the precision (C99 fprintf %g rules; cross-checked against
    /// coreutils printf %.2g).
    #[test]
    fn percent_g2_matches_c_printf() {
        assert_eq!(g2(0.0), "0");
        assert_eq!(g2(0.5), "0.5");
        assert_eq!(g2(2.5), "2.5");
        assert_eq!(g2(25.0), "25");
        assert_eq!(g2(8.333_333), "8.3");
        assert_eq!(g2(33.333), "33");
        assert_eq!(g2(0.033), "0.033");
        assert_eq!(g2(100.0), "1e+02");
        // Rounding happens BEFORE the notation switch: 99.6 → 100 → 1e+02.
        assert_eq!(g2(99.6), "1e+02");
        // Non-finite inputs render like C's %g instead of panicking on the
        // missing exponent (r1 n2 — unreachable from lost_percent, but g2
        // is pub(crate) and a future caller must not hit the expect()).
        assert_eq!(g2(f64::NAN), "nan");
        assert_eq!(g2(f64::INFINITY), "inf");
        assert_eq!(g2(f64::NEG_INFINITY), "-inf");
    }
}
