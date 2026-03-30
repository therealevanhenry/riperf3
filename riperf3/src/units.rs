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
        'K' => format!("{:.2} KBytes", bytes / 1024.0),
        'k' => format!("{:.2} Kbits", bytes * 8.0 / 1000.0),
        'M' => format!("{:.2} MBytes", bytes / (1024.0 * 1024.0)),
        'm' => format!("{:.2} Mbits", bytes * 8.0 / 1_000_000.0),
        'G' => format!("{:.2} GBytes", bytes / (1024.0 * 1024.0 * 1024.0)),
        'g' => format!("{:.2} Gbits", bytes * 8.0 / 1_000_000_000.0),
        'T' => format!("{:.2} TBytes", bytes / (1024.0 * 1024.0 * 1024.0 * 1024.0)),
        't' => format!("{:.2} Tbits", bytes * 8.0 / 1_000_000_000_000.0),
        _ => adaptive_bytes(bytes),
    }
}

/// Format a bits-per-second rate for display.
pub fn format_rate(bits_per_sec: f64, format_char: char) -> String {
    match format_char {
        'k' | 'K' => format!("{:.2} Kbits/sec", bits_per_sec / 1000.0),
        'm' | 'M' => format!("{:.2} Mbits/sec", bits_per_sec / 1_000_000.0),
        'g' | 'G' => format!("{:.2} Gbits/sec", bits_per_sec / 1_000_000_000.0),
        't' | 'T' => format!("{:.2} Tbits/sec", bits_per_sec / 1_000_000_000_000.0),
        _ => adaptive_rate(bits_per_sec),
    }
}

fn adaptive_bytes(bytes: f64) -> String {
    const K: f64 = 1024.0;
    const M: f64 = K * 1024.0;
    const G: f64 = M * 1024.0;
    const T: f64 = G * 1024.0;

    if bytes >= T {
        format!("{:.2} TBytes", bytes / T)
    } else if bytes >= G {
        format!("{:.2} GBytes", bytes / G)
    } else if bytes >= M {
        format!("{:.2} MBytes", bytes / M)
    } else if bytes >= K {
        format!("{:.2} KBytes", bytes / K)
    } else {
        format!("{:.0} Bytes", bytes)
    }
}

fn adaptive_bits(bits: f64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * 1000.0;
    const G: f64 = M * 1000.0;
    const T: f64 = G * 1000.0;

    if bits >= T {
        format!("{:.2} Tbits", bits / T)
    } else if bits >= G {
        format!("{:.2} Gbits", bits / G)
    } else if bits >= M {
        format!("{:.2} Mbits", bits / M)
    } else if bits >= K {
        format!("{:.2} Kbits", bits / K)
    } else {
        format!("{:.0} bits", bits)
    }
}

fn adaptive_rate(bits_per_sec: f64) -> String {
    const K: f64 = 1000.0;
    const M: f64 = K * 1000.0;
    const G: f64 = M * 1000.0;
    const T: f64 = G * 1000.0;

    if bits_per_sec >= T {
        format!("{:.2} Tbits/sec", bits_per_sec / T)
    } else if bits_per_sec >= G {
        format!("{:.2} Gbits/sec", bits_per_sec / G)
    } else if bits_per_sec >= M {
        format!("{:.2} Mbits/sec", bits_per_sec / M)
    } else if bits_per_sec >= K {
        format!("{:.2} Kbits/sec", bits_per_sec / K)
    } else {
        format!("{:.0} bits/sec", bits_per_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_adaptive() {
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
        assert_eq!(format_bytes(bytes, 'K'), "1024.00 KBytes");
        assert_eq!(format_bytes(bytes, 'M'), "1.00 MBytes");
        assert_eq!(format_bytes(bytes, 'G'), "0.00 GBytes");
    }

    #[test]
    fn format_bits_adaptive() {
        let bytes = 125_000.0; // 1 Mbit
        assert_eq!(format_bytes(bytes, 'a'), "1.00 Mbits");
    }

    #[test]
    fn format_rate_fixed() {
        assert_eq!(format_rate(1_000_000_000.0, 'g'), "1.00 Gbits/sec");
        assert_eq!(format_rate(1_000_000_000.0, 'm'), "1000.00 Mbits/sec");
    }

    #[test]
    fn format_rate_adaptive() {
        assert_eq!(format_rate(500.0, 'a'), "500 bits/sec");
        assert_eq!(format_rate(1_500_000.0, 'a'), "1.50 Mbits/sec");
        assert_eq!(format_rate(9_420_000_000.0, 'a'), "9.42 Gbits/sec");
    }
}
