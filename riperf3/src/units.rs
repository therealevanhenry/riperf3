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
/// "99.95 would be rounded to 100"). iperf3 also left-pads to a 4-char
/// minimum (%4.Xf); riperf3's row templates own column alignment, so the
/// pad is theirs, not this function's.
fn ladder(n: f64) -> String {
    if n < 9.995 {
        format!("{n:.2}")
    } else if n < 99.95 {
        format!("{n:.1}")
    } else {
        format!("{n:.0}")
    }
}

/// Format a bits-per-second rate for display.
pub fn format_rate(bits_per_sec: f64, format_char: char) -> String {
    match format_char {
        'k' | 'K' => format!("{} Kbits/sec", ladder(bits_per_sec / 1000.0)),
        'm' | 'M' => format!("{} Mbits/sec", ladder(bits_per_sec / 1_000_000.0)),
        'g' | 'G' => format!("{} Gbits/sec", ladder(bits_per_sec / 1_000_000_000.0)),
        't' | 'T' => format!("{} Tbits/sec", ladder(bits_per_sec / 1_000_000_000_000.0)),
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
        assert_eq!(format_bytes(500.0, 'A'), "500 Bytes");
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
        assert_eq!(format_rate(101.0e9, 'a'), "101 Gbits/sec");
        // boundary: 9.994 stays 2dp, 9.996 promotes to 1dp ("10.0")
        assert_eq!(format_rate(9.994e9, 'a'), "9.99 Gbits/sec");
        assert_eq!(format_rate(9.996e9, 'a'), "10.0 Gbits/sec");
        // boundary: 99.94 stays 1dp, 99.96 promotes to 0dp ("100")
        assert_eq!(format_rate(99.94e9, 'a'), "99.9 Gbits/sec");
        assert_eq!(format_rate(99.96e9, 'a'), "100 Gbits/sec");
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

    #[test]
    fn format_rate_adaptive() {
        assert_eq!(format_rate(500.0, 'a'), "500 bits/sec");
        assert_eq!(format_rate(1_500_000.0, 'a'), "1.50 Mbits/sec");
        assert_eq!(format_rate(9_420_000_000.0, 'a'), "9.42 Gbits/sec");
    }
}
