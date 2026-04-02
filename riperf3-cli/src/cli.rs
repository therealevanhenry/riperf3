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
    #[arg(
        short,
        long,
        ignore_case = true,
        value_enum,
        value_name = "format",
        default_value = "m"
    )]
    pub format: Format,

    /// Seconds between periodic throughput reports
    #[arg(short, long, value_name = "interval")]
    pub interval: Option<f64>,

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
    /// Write PID to file
    #[arg(short = 'I', long, value_name = "file")]
    pub pidfile: Option<String>,

    /// Send output to a log file
    #[arg(long, value_name = "file")]
    pub logfile: Option<String>,

    /// Force flushing output at every interval
    #[arg(long)]
    pub forceflush: bool,

    /// Emit a timestamp at the start of each output line
    #[arg(long, value_name = "format", num_args = 0..=1, default_missing_value = "%c ")]
    pub timestamps: Option<String>,

    // -----------------------------------------------------------------------
    // Server-specific arguments
    // -----------------------------------------------------------------------
    /// Handle one client connection then exit
    #[arg(short = '1', long)]
    pub one_off: bool,

    /// Run the server as a daemon
    #[arg(short = 'D', long)]
    pub daemon: bool,

    /// Restart idle server after # seconds
    #[arg(long, value_name = "secs")]
    pub idle_timeout: Option<u32>,

    /// Server's total bit rate limit
    #[arg(long = "server-bitrate-limit", value_name = "rate")]
    pub server_bitrate_limit: Option<String>,

    /// Max time a test can run on the server
    #[arg(long = "server-max-duration", value_name = "secs")]
    pub server_max_duration: Option<u32>,

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

    /// Use 64-bit counters in UDP test packets
    #[arg(long)]
    pub udp_counters_64bit: bool,

    /// Use repeating pattern in payload instead of zeros
    #[arg(long)]
    pub repeating_payload: bool,

    /// Set IPv4 Don't Fragment flag
    #[arg(long)]
    pub dont_fragment: bool,

    /// Bind to a specific client port
    #[arg(long, value_name = "port")]
    pub cport: Option<u16>,

    /// Set the server timing for pacing in microseconds (deprecated)
    #[arg(long, value_name = "usec")]
    pub pacing_timer: Option<u32>,

    /// Only use IPv4
    #[arg(short = '4', long)]
    pub version4: bool,

    /// Only use IPv6
    #[arg(short = '6', long)]
    pub version6: bool,

    /// Bind to the interface associated with the address
    #[arg(short = 'B', long, value_name = "host[%dev]")]
    pub bind: Option<String>,

    /// Bind to the network interface with SO_BINDTODEVICE
    #[arg(long, value_name = "dev")]
    pub bind_dev: Option<String>,

    /// Enable fair-queuing based socket pacing (bits/sec, Linux only)
    #[arg(long, value_name = "rate")]
    pub fq_rate: Option<String>,

    /// Set the IPv6 flow label (Linux only)
    #[arg(short = 'L', long, value_name = "N")]
    pub flowlabel: Option<i32>,

    /// Set the IP DSCP value (0-63 or symbolic)
    #[arg(long, value_name = "val")]
    pub dscp: Option<String>,

    /// Use MPTCP rather than plain TCP
    #[arg(short = 'm', long)]
    pub mptcp: bool,

    /// Use zero copy method of sending data
    #[arg(short = 'Z', long)]
    pub zerocopy: bool,

    /// Ignore received messages using MSG_TRUNC
    #[arg(long)]
    pub skip_rx_copy: bool,

    /// Idle timeout for receiving data (ms)
    #[arg(long, value_name = "ms")]
    pub rcv_timeout: Option<u64>,

    /// Timeout for unacknowledged TCP data (ms)
    #[arg(long, value_name = "ms")]
    pub snd_timeout: Option<u64>,

    /// Transmit/receive the specified file
    #[arg(short = 'F', long, value_name = "name")]
    pub file: Option<String>,

    /// Set CPU affinity
    #[arg(short = 'A', long, value_name = "n[,m]")]
    pub affinity: Option<String>,

    /// Output in line-delimited JSON format
    #[arg(long)]
    pub json_stream: bool,

    /// Enable UDP GSO/GRO
    #[arg(long)]
    pub gsro: bool,

    /// Use sendmmsg for batched UDP sends (experimental, Linux/FreeBSD/NetBSD)
    #[arg(long)]
    pub sendmmsg: bool,

    /// Use control connection TCP keepalive
    #[arg(long, value_name = "idle/intv/cnt")]
    pub cntl_ka: Option<String>,

    /// Username for authentication
    #[arg(long)]
    pub username: Option<String>,

    /// Path to RSA public key for authentication
    #[arg(long, value_name = "file")]
    pub rsa_public_key_path: Option<String>,

    /// Path to RSA private key for authentication (server)
    #[arg(long, value_name = "file")]
    pub rsa_private_key_path: Option<String>,

    /// Path to authorized users file (server)
    #[arg(long, value_name = "file")]
    pub authorized_users_path: Option<String>,

    /// Time skew threshold for authentication (seconds)
    #[arg(long, value_name = "secs")]
    pub time_skew_threshold: Option<u32>,

    /// Use PKCS#1 padding for authentication
    #[arg(long)]
    pub use_pkcs1_padding: bool,
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
                "riperf3", "-c", "10.0.0.1", "-u", "-t", "30", "-P", "4", "-R", "--bidir", "-N",
                "-l", "1460", "-b", "100M",
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
                "riperf3", "-c", "host", "-w", "512K", "-M", "1400", "-C", "bbr",
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

    /// Tests that CLI flags are correctly wired through to Client/Server
    /// fields, matching the logic in main.rs. This is the test class that
    /// would have caught the -J flag not being wired up.
    mod cli_wiring_tests {
        use super::*;
        use riperf3::TransportProtocol;

        /// Simulate the main.rs client builder wiring for a parsed CLI.
        fn build_client_from_cli(cli: &Cli) -> riperf3::Client {
            let host = cli.client.as_ref().unwrap();
            let mut b = riperf3::ClientBuilder::new(host);
            if let Some(port) = cli.port {
                b = b.port(Some(port));
            }
            if cli.udp {
                b = b.protocol(TransportProtocol::Udp);
            }
            if let Some(t) = cli.time {
                b = b.duration(t);
            }
            if let Some(ref s) = cli.bytes {
                b = b.bytes_str(s).unwrap();
            }
            if let Some(ref s) = cli.blockcount {
                b = b.blocks_str(s).unwrap();
            }
            if let Some(ref s) = cli.length {
                b = b.blksize_str(s).unwrap();
            }
            if let Some(n) = cli.parallel {
                b = b.num_streams(n);
            }
            if cli.reverse {
                b = b.reverse(true);
            }
            if cli.bidir {
                b = b.bidir(true);
            }
            if let Some(ref s) = cli.window {
                b = b.window_str(s).unwrap();
            }
            if let Some(ref algo) = cli.congestion {
                b = b.congestion(algo);
            }
            if let Some(mss) = cli.mss {
                b = b.mss(mss);
            }
            if cli.no_delay {
                b = b.no_delay(true);
            }
            if let Some(ref s) = cli.bitrate {
                b = b.bandwidth_str(s).unwrap();
            }
            if let Some(tos) = cli.tos {
                b = b.tos(tos);
            }
            if let Some(o) = cli.omit {
                b = b.omit(o);
            }
            if let Some(i) = cli.interval {
                b = b.interval(i);
            }
            if let Some(ref t) = cli.title {
                b = b.title(t);
            }
            if let Some(ref d) = cli.extra_data {
                b = b.extra_data(d);
            }
            if let Some(ms) = cli.connect_timeout {
                b = b.connect_timeout(std::time::Duration::from_millis(ms));
            }
            if cli.verbose {
                b = b.verbose(true);
            }
            if cli.json {
                b = b.json_output(true);
            }
            if cli.json_stream {
                b = b.json_stream(true);
            }
            if cli.udp_counters_64bit {
                b = b.udp_counters_64bit(true);
            }
            if cli.repeating_payload {
                b = b.repeating_payload(true);
            }
            if cli.zerocopy {
                b = b.zerocopy(true);
            }
            if cli.gsro {
                b = b.gsro(true);
            }
            if cli.sendmmsg {
                b = b.sendmmsg(true);
            }
            if cli.dont_fragment {
                b = b.dont_fragment(true);
            }
            if let Some(port) = cli.cport {
                b = b.cport(port);
            }
            if cli.get_server_output {
                b = b.get_server_output(true);
            }
            if cli.forceflush {
                b = b.forceflush(true);
            }
            if let Some(ref fmt) = cli.timestamps {
                b = b.timestamps(fmt);
            }
            if let Some(ref addr) = cli.bind {
                b = b.bind_address(addr);
            }
            if let Some(ref dev) = cli.bind_dev {
                b = b.bind_dev(dev);
            }
            if let Some(ref s) = cli.fq_rate {
                b = b.fq_rate_str(s).unwrap();
            }
            if let Some(label) = cli.flowlabel {
                b = b.flowlabel(label);
            }
            if cli.version4 {
                b = b.ip_version(4);
            }
            if cli.version6 {
                b = b.ip_version(6);
            }
            if cli.mptcp {
                b = b.mptcp(true);
            }
            if cli.skip_rx_copy {
                b = b.skip_rx_copy(true);
            }
            if let Some(ms) = cli.rcv_timeout {
                b = b.rcv_timeout(ms);
            }
            if let Some(ms) = cli.snd_timeout {
                b = b.snd_timeout(ms);
            }
            if let Some(ref path) = cli.file {
                b = b.file(path);
            }
            if let Some(ref spec) = cli.affinity {
                b = b.affinity(spec);
            }
            if let Some(ref spec) = cli.cntl_ka {
                b = b.cntl_ka(spec);
            }
            if let Some(ref val) = cli.dscp {
                b = b.dscp(val);
            }
            if let Some(ref path) = cli.pidfile {
                b = b.pidfile(path);
            }
            if let Some(ref path) = cli.logfile {
                b = b.logfile(path);
            }
            b.build().unwrap()
        }

        #[test]
        fn json_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-J"]);
            let c = build_client_from_cli(&cli);
            assert!(c.json_output);
        }

        #[test]
        fn udp_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-u"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.protocol, TransportProtocol::Udp);
        }

        #[test]
        fn duration_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-t", "30"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.duration, 30);
        }

        #[test]
        fn bytes_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-n", "1M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.bytes_to_send, Some(1024 * 1024));
        }

        #[test]
        fn blockcount_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-k", "100"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.blocks_to_send, Some(100));
        }

        #[test]
        fn length_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-l", "64K"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.blksize, 64 * 1024);
        }

        #[test]
        fn parallel_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-P", "8"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.num_streams, 8);
        }

        #[test]
        fn reverse_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-R"]);
            let c = build_client_from_cli(&cli);
            assert!(c.reverse);
        }

        #[test]
        fn bidir_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--bidir"]);
            let c = build_client_from_cli(&cli);
            assert!(c.bidir);
        }

        #[test]
        fn window_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-w", "512K"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.window, Some(512 * 1024));
        }

        #[test]
        fn congestion_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-C", "bbr"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.congestion, Some("bbr".to_string()));
        }

        #[test]
        fn mss_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-M", "1400"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.mss, Some(1400));
        }

        #[test]
        fn no_delay_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-N"]);
            let c = build_client_from_cli(&cli);
            assert!(c.no_delay);
        }

        #[test]
        fn bitrate_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-b", "100M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.bandwidth, 100 * 1024 * 1024);
        }

        #[test]
        fn tos_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-S", "16"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.tos, 16);
        }

        #[test]
        fn omit_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-O", "3"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.omit, 3);
        }

        #[test]
        fn title_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-T", "my test"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.title, Some("my test".to_string()));
        }

        #[test]
        fn extra_data_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--extra-data", "abc"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.extra_data, Some("abc".to_string()));
        }

        #[test]
        fn connect_timeout_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--connect-timeout", "500"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c.connect_timeout,
                Some(std::time::Duration::from_millis(500))
            );
        }

        #[test]
        fn verbose_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-V"]);
            let c = build_client_from_cli(&cli);
            assert!(c.verbose);
        }

        #[test]
        fn all_flags_combined() {
            let cli = Cli::parse_from([
                "riperf3",
                "-c",
                "host",
                "-u",
                "-t",
                "30",
                "-P",
                "4",
                "-R",
                "--bidir",
                "-N",
                "-l",
                "1460",
                "-b",
                "100M",
                "-J",
                "-V",
                "-w",
                "512K",
                "-M",
                "1400",
                "-C",
                "bbr",
                "-S",
                "16",
                "-O",
                "2",
                "-T",
                "test",
                "--extra-data",
                "x",
                "--connect-timeout",
                "500",
            ]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.protocol, TransportProtocol::Udp);
            assert_eq!(c.duration, 30);
            assert_eq!(c.num_streams, 4);
            assert!(c.reverse);
            assert!(c.bidir);
            assert!(c.no_delay);
            assert_eq!(c.blksize, 1460);
            assert_eq!(c.bandwidth, 100 * 1024 * 1024);
            assert!(c.json_output);
            assert_eq!(c.window, Some(512 * 1024));
            assert_eq!(c.mss, Some(1400));
            assert_eq!(c.congestion, Some("bbr".to_string()));
            assert_eq!(c.tos, 16);
            assert_eq!(c.omit, 2);
            assert_eq!(c.title, Some("test".to_string()));
            assert_eq!(c.extra_data, Some("x".to_string()));
            assert!(c.verbose);
        }

        #[test]
        fn server_one_off_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-1"]);
            let mut b = riperf3::ServerBuilder::new();
            if cli.one_off {
                b = b.one_off(true);
            }
            let s = b.build().unwrap();
            assert!(s.one_off);
        }

        // -- New flag wiring tests --

        #[test]
        fn udp_counters_64bit_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--udp-counters-64bit"]);
            let c = build_client_from_cli(&cli);
            assert!(c.udp_counters_64bit);
        }

        #[test]
        fn repeating_payload_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--repeating-payload"]);
            let c = build_client_from_cli(&cli);
            assert!(c.repeating_payload);
        }

        #[test]
        fn dont_fragment_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--dont-fragment"]);
            let c = build_client_from_cli(&cli);
            assert!(c.dont_fragment);
        }

        #[test]
        fn cport_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cport", "12345"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.cport, Some(12345));
        }

        #[test]
        fn get_server_output_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--get-server-output"]);
            let c = build_client_from_cli(&cli);
            assert!(c.get_server_output);
        }

        #[test]
        fn forceflush_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--forceflush"]);
            let c = build_client_from_cli(&cli);
            assert!(c.forceflush);
        }

        #[test]
        fn timestamps_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--timestamps", "%H:%M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.timestamps, Some("%H:%M".to_string()));
        }

        #[test]
        fn version4_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-4"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.ip_version, Some(4));
        }

        #[test]
        fn version6_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-6"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.ip_version, Some(6));
        }

        #[test]
        fn bind_address_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-B", "10.0.0.1"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.bind_address, Some("10.0.0.1".to_string()));
        }

        #[test]
        fn bind_dev_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--bind-dev", "eth0"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.bind_dev, Some("eth0".to_string()));
        }

        #[test]
        fn fq_rate_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--fq-rate", "1G"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.fq_rate, Some(1024 * 1024 * 1024));
        }

        #[test]
        fn flowlabel_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-L", "42"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.flowlabel, Some(42));
        }

        #[test]
        fn dscp_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--dscp", "46"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.dscp, Some("46".to_string()));
        }

        #[test]
        fn mptcp_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-m"]);
            let c = build_client_from_cli(&cli);
            assert!(c.mptcp);
        }

        #[test]
        fn skip_rx_copy_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--skip-rx-copy"]);
            let c = build_client_from_cli(&cli);
            assert!(c.skip_rx_copy);
        }

        #[test]
        fn rcv_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--rcv-timeout", "5000"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.rcv_timeout, Some(5000));
        }

        #[test]
        fn snd_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--snd-timeout", "3000"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.snd_timeout, Some(3000));
        }

        #[test]
        fn file_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-F", "/tmp/data"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.file, Some("/tmp/data".to_string()));
        }

        #[test]
        fn affinity_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-A", "2,3"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.affinity, Some("2,3".to_string()));
        }

        #[test]
        fn json_stream_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--json-stream"]);
            let c = build_client_from_cli(&cli);
            assert!(c.json_stream);
        }

        #[test]
        fn pidfile_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-I", "/tmp/pid"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.pidfile, Some("/tmp/pid".to_string()));
        }

        #[test]
        fn logfile_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--logfile", "/tmp/log"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c.logfile, Some("/tmp/log".to_string()));
        }

        #[test]
        fn sendmmsg_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-u", "--sendmmsg"]);
            assert!(cli.sendmmsg);
            let c = build_client_from_cli(&cli);
            assert!(c.sendmmsg);
        }

        #[test]
        fn sendmmsg_default_false() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-u"]);
            assert!(!cli.sendmmsg);
            let c = build_client_from_cli(&cli);
            assert!(!c.sendmmsg);
        }

        // Server new flags
        #[test]
        fn server_daemon_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-D"]);
            let mut b = riperf3::ServerBuilder::new();
            if cli.daemon {
                b = b.daemon(true);
            }
            let s = b.build().unwrap();
            assert!(s.daemon);
        }

        #[test]
        fn server_idle_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--idle-timeout", "30"]);
            let mut b = riperf3::ServerBuilder::new();
            if let Some(secs) = cli.idle_timeout {
                b = b.idle_timeout(secs);
            }
            let s = b.build().unwrap();
            assert_eq!(s.idle_timeout, Some(30));
        }

        #[test]
        fn server_max_duration_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--server-max-duration", "60"]);
            let mut b = riperf3::ServerBuilder::new();
            if let Some(secs) = cli.server_max_duration {
                b = b.server_max_duration(secs);
            }
            let s = b.build().unwrap();
            assert_eq!(s.server_max_duration, Some(60));
        }

        /// Verify every short flag alias parses identically to its long form.
        /// Clap guarantees this, but this test catches typos in the arg declarations.
        #[test]
        fn short_and_long_flags_equivalent() {
            // (short_args, long_args, description)
            let pairs: Vec<(&[&str], &[&str], &str)> = vec![
                // Boolean client flags
                (&["-c", "h", "-u"], &["-c", "h", "--udp"], "udp"),
                (&["-c", "h", "-R"], &["-c", "h", "--reverse"], "reverse"),
                (&["-c", "h", "-N"], &["-c", "h", "--no-delay"], "no-delay"),
                (&["-c", "h", "-Z"], &["-c", "h", "--zerocopy"], "zerocopy"),
                (&["-c", "h", "-m"], &["-c", "h", "--mptcp"], "mptcp"),
                (&["-c", "h", "-J"], &["-c", "h", "--json"], "json"),
                (&["-c", "h", "-V"], &["-c", "h", "--verbose"], "verbose"),
                (&["-c", "h", "-4"], &["-c", "h", "--version4"], "version4"),
                (&["-c", "h", "-6"], &["-c", "h", "--version6"], "version6"),
                // Value client flags
                (&["-c", "h", "-t", "5"], &["-c", "h", "--time", "5"], "time"),
                (
                    &["-c", "h", "-n", "1M"],
                    &["-c", "h", "--bytes", "1M"],
                    "bytes",
                ),
                (
                    &["-c", "h", "-k", "10"],
                    &["-c", "h", "--blockcount", "10"],
                    "blockcount",
                ),
                (
                    &["-c", "h", "-l", "8K"],
                    &["-c", "h", "--length", "8K"],
                    "length",
                ),
                (
                    &["-c", "h", "-P", "4"],
                    &["-c", "h", "--parallel", "4"],
                    "parallel",
                ),
                (
                    &["-c", "h", "-w", "1M"],
                    &["-c", "h", "--window", "1M"],
                    "window",
                ),
                (
                    &["-c", "h", "-C", "bbr"],
                    &["-c", "h", "--congestion", "bbr"],
                    "congestion",
                ),
                (
                    &["-c", "h", "-M", "1400"],
                    &["-c", "h", "--set-mss", "1400"],
                    "set-mss",
                ),
                (
                    &["-c", "h", "-b", "1G"],
                    &["-c", "h", "--bitrate", "1G"],
                    "bitrate",
                ),
                (&["-c", "h", "-S", "16"], &["-c", "h", "--tos", "16"], "tos"),
                (&["-c", "h", "-O", "3"], &["-c", "h", "--omit", "3"], "omit"),
                (
                    &["-c", "h", "-T", "hi"],
                    &["-c", "h", "--title", "hi"],
                    "title",
                ),
                (
                    &["-c", "h", "-B", "lo"],
                    &["-c", "h", "--bind", "lo"],
                    "bind",
                ),
                (
                    &["-c", "h", "-L", "42"],
                    &["-c", "h", "--flowlabel", "42"],
                    "flowlabel",
                ),
                (
                    &["-c", "h", "-F", "/tmp/f"],
                    &["-c", "h", "--file", "/tmp/f"],
                    "file",
                ),
                (
                    &["-c", "h", "-A", "0"],
                    &["-c", "h", "--affinity", "0"],
                    "affinity",
                ),
                (
                    &["-c", "h", "-I", "/tmp/p"],
                    &["-c", "h", "--pidfile", "/tmp/p"],
                    "pidfile",
                ),
                // Server flags
                (&["-s", "-1"], &["-s", "--one-off"], "one-off"),
                (&["-s", "-D"], &["-s", "--daemon"], "daemon"),
            ];

            for (short_args, long_args, desc) in &pairs {
                let mut s: Vec<&str> = vec!["riperf3"];
                s.extend_from_slice(short_args);
                let mut l: Vec<&str> = vec!["riperf3"];
                l.extend_from_slice(long_args);

                let short = Cli::parse_from(&s);
                let long = Cli::parse_from(&l);

                // Compare the fields that matter for each flag
                assert_eq!(
                    format!("{:?}", short),
                    format!("{:?}", long),
                    "short vs long mismatch for '{desc}': {short_args:?} vs {long_args:?}"
                );
            }
        }
    }
}
