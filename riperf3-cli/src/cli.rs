use clap::{ArgGroup, Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(about, author, long_about = None, name = "riperf3", version, disable_version_flag = true)]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .args(&["server", "client"])
))]
#[command(group(
    ArgGroup::new("meta")
        .required(false)
        .args(&["help", "version"])
))]
pub struct Cli {
    // -----------------------------------------------------------------------
    // Common arguments
    // -----------------------------------------------------------------------

    /// Run in server mode
    #[arg(short, long, group = "mode")]
    pub server: bool,

    /// Run in client mode, connecting to <host>
    #[arg(short, long, group = "mode", value_name = "host")]
    pub client: Option<String>,

    /// Server port to listen on/connect to
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Format to report: Kbits, Mbits, Gbits, Tbits
    #[arg(short, long, ignore_case = true, value_enum, value_name = "format", default_value = "m")]
    pub format: Format,

    /// Seconds between periodic throughput reports
    #[arg(short, long, value_name = "interval")]
    pub interval: Option<u8>,

    /// Enable verbose output
    #[arg(short = 'V', long)]
    pub verbose: bool,

    /// Output in JSON format
    #[arg(short = 'J', long)]
    pub json: bool,

    /// Debug level 1-4 (default 4)
    #[arg(short, long, value_name = "level", num_args = 0..=1,
          value_parser = clap::value_parser!(u8).range(1..=4),
          default_missing_value = "4")]
    pub debug: Option<u8>,

    /// Print version
    #[arg(short = 'v', long, group = "meta", action = clap::ArgAction::Version)]
    pub version: Option<bool>,

    // -----------------------------------------------------------------------
    // Server-specific arguments
    // -----------------------------------------------------------------------

    /// Handle one client connection then exit
    #[arg(short = '1', long)]
    pub one_off: bool,

    // -----------------------------------------------------------------------
    // Client-specific arguments
    // -----------------------------------------------------------------------

    /// Use UDP rather than TCP
    #[arg(short = 'u', long)]
    pub udp: bool,

    /// Time in seconds to transmit for (default 10 secs)
    #[arg(short = 't', long, value_name = "secs")]
    pub time: Option<u32>,

    /// Number of bytes to transmit (instead of -t)
    #[arg(short = 'n', long, value_name = "bytes")]
    pub bytes: Option<String>,

    /// Number of blocks (packets) to transmit (instead of -t or -n)
    #[arg(short = 'k', long, value_name = "count")]
    pub blockcount: Option<String>,

    /// Length of buffer to read or write (default 128 KB for TCP, 1460 for UDP)
    #[arg(short = 'l', long, value_name = "size")]
    pub length: Option<String>,

    /// Number of parallel client streams to run
    #[arg(short = 'P', long, value_name = "num")]
    pub parallel: Option<u32>,

    /// Reverse mode (server sends, client receives)
    #[arg(short = 'R', long)]
    pub reverse: bool,

    /// Bidirectional mode: client and server send and receive
    #[arg(long)]
    pub bidir: bool,

    /// Set socket buffer sizes (indirectly sets TCP window size)
    #[arg(short = 'w', long, value_name = "size")]
    pub window: Option<String>,

    /// Set TCP congestion control algorithm
    #[arg(short = 'C', long, value_name = "algo")]
    pub congestion: Option<String>,

    /// Set TCP/SCTP maximum segment size (MTU - 40 bytes)
    #[arg(short = 'M', long = "set-mss", value_name = "mss")]
    pub mss: Option<i32>,

    /// Disable Nagle's algorithm (set TCP_NODELAY)
    #[arg(short = 'N', long = "no-delay")]
    pub no_delay: bool,

    /// Target bitrate in bits/sec (0 = unlimited for TCP, 1M default for UDP)
    #[arg(short = 'b', long, value_name = "rate[/burst]")]
    pub bitrate: Option<String>,

    /// Set the IP type of service (0-255)
    #[arg(short = 'S', long, value_name = "tos")]
    pub tos: Option<i32>,

    /// Omit the first N seconds of the test
    #[arg(short = 'O', long, value_name = "secs")]
    pub omit: Option<u32>,

    /// Prefix every output line with this string
    #[arg(short = 'T', long, value_name = "title")]
    pub title: Option<String>,

    /// Extra data string to include in JSON output
    #[arg(long, value_name = "str")]
    pub extra_data: Option<String>,

    /// Timeout for control connection setup (ms)
    #[arg(long, value_name = "ms")]
    pub connect_timeout: Option<u64>,

    /// Get results from server
    #[arg(long)]
    pub get_server_output: bool,
}

#[derive(Debug, Clone, PartialEq, ValueEnum)]
pub enum Format {
    K,
    M,
    G,
    T,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod cli_tests {
    use super::*;

    mod common_arg_tests {
        use super::*;

        #[test]
        fn test_common_defaults() {
            let cli = Cli::parse_from(["riperf3", "--server"]);
            assert!(cli.server);
            assert!(cli.client.is_none());
            assert_eq!(cli.port, None);
            assert_eq!(cli.format, Format::M);
            assert_eq!(cli.interval, None);
            assert!(!cli.verbose);
            assert_eq!(cli.debug, None);
            assert_eq!(cli.version, None);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost"]);
            assert!(!cli.server);
            assert_eq!(cli.client, Some("localhost".to_string()));
            assert_eq!(cli.port, None);
            assert_eq!(cli.format, Format::M);
            assert_eq!(cli.interval, None);
            assert!(!cli.verbose);
            assert_eq!(cli.debug, None);
            assert_eq!(cli.version, None);
        }

        #[test]
        fn test_common_port() {
            let cli = Cli::parse_from(["riperf3", "--server", "--port", "1234"]);
            assert_eq!(cli.port, Some(1234));

            let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--port", "1234"]);
            assert_eq!(cli.port, Some(1234));
        }

        #[test]
        fn test_common_format() {
            let cli = Cli::parse_from(["riperf3", "--server", "--format", "k"]);
            assert_eq!(cli.format, Format::K);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--format", "g"]);
            assert_eq!(cli.format, Format::G);

            let cli = Cli::parse_from(["riperf3", "--server", "--format", "t"]);
            assert_eq!(cli.format, Format::T);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--format", "m"]);
            assert_eq!(cli.format, Format::M);

            let cli = Cli::parse_from(["riperf3", "--server", "--format", "M"]);
            assert_eq!(cli.format, Format::M);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--format", "T"]);
            assert_eq!(cli.format, Format::T);

            let cli = Cli::parse_from(["riperf3", "--server", "--format", "G"]);
            assert_eq!(cli.format, Format::G);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--format", "K"]);
            assert_eq!(cli.format, Format::K);
        }
    }

    mod client_arg_tests {
        use super::*;

        #[test]
        fn test_client_flags() {
            let cli = Cli::parse_from([
                "riperf3", "-c", "10.0.0.1",
                "-u", "-t", "30", "-P", "4", "-R", "--bidir",
                "-N", "-l", "1460", "-b", "100M",
            ]);
            assert!(cli.udp);
            assert_eq!(cli.time, Some(30));
            assert_eq!(cli.parallel, Some(4));
            assert!(cli.reverse);
            assert!(cli.bidir);
            assert!(cli.no_delay);
            assert_eq!(cli.length, Some("1460".to_string()));
            assert_eq!(cli.bitrate, Some("100M".to_string()));
        }

        #[test]
        fn test_client_bytes_and_blocks() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-n", "1G"]);
            assert_eq!(cli.bytes, Some("1G".to_string()));

            let cli = Cli::parse_from(["riperf3", "-c", "host", "-k", "100K"]);
            assert_eq!(cli.blockcount, Some("100K".to_string()));
        }

        #[test]
        fn test_client_window_mss_congestion() {
            let cli = Cli::parse_from([
                "riperf3", "-c", "host",
                "-w", "512K", "-M", "1400", "-C", "bbr",
            ]);
            assert_eq!(cli.window, Some("512K".to_string()));
            assert_eq!(cli.mss, Some(1400));
            assert_eq!(cli.congestion, Some("bbr".to_string()));
        }
    }

    mod server_arg_tests {
        use super::*;

        #[test]
        fn test_one_off() {
            let cli = Cli::parse_from(["riperf3", "-s", "-1"]);
            assert!(cli.one_off);
        }
    }
}
