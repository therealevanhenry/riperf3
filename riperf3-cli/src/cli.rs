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

    /// Length of buffer to read or write (default 128 KB for TCP; UDP tracks the connection MSS, else 1460)
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

    /// Target bitrate in bits/sec (0 = unlimited; default: unlimited TCP, 1M UDP)
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
    // Capped at i32::MAX: the wire TestParams field is i32 (a larger u32
    // would wrap negative on the wire — review r1).
    #[arg(long, value_name = "usec", value_parser = clap::value_parser!(u32).range(..=i32::MAX as i64))]
    pub pacing_timer: Option<u32>,

    /// Only use IPv4
    #[arg(short = '4', long, conflicts_with = "version6")]
    pub version4: bool,

    /// Only use IPv6
    #[arg(short = '6', long)]
    pub version6: bool,

    /// Bind to a local source address (device binding is `--bind-dev`)
    #[arg(short = 'B', long, value_name = "host")]
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

impl Cli {
    /// Reject client-only options when running as a server (`-s`), matching
    /// iperf3 (#65). iperf3 sets an internal `client_flag` for each of these in
    /// `iperf_parse_arguments` and errors with IECLIENTONLY ("some option you are
    /// trying to set is client only") when `role == 's'`. riperf3 silently
    /// accepted and ignored them, which is a drop-in divergence.
    ///
    /// The set is adjudicated against iperf3's `iperf_api.c` (3.20) so riperf3
    /// rejects exactly what iperf3 rejects, no more:
    /// - Every option iperf3 marks `client_flag = 1`.
    /// - `--gsro`: iperf3 tracks this with a separate `gsro_flag` but applies the
    ///   same `role == 's'` -> IECLIENTONLY rejection, so it belongs here.
    /// - `--sendmmsg`: riperf3-only client transmit option with no iperf3
    ///   equivalent and no server meaning; rejecting it carries no drop-in risk.
    ///
    /// Deliberately NOT rejected, because iperf3 accepts them on a server:
    /// - `-m/--mptcp` and `--cport`: iperf3's `'m'` and `OPT_CLIENT_PORT` cases do
    ///   not set `client_flag`.
    /// - A bare `-A n`: iperf3 only sets `client_flag` for the two-arg `-A n,m`
    ///   form (where the first value is the *client* affinity); `-A n` alone is
    ///   the server's own CPU affinity. riperf3's server does not yet honor it,
    ///   but faithfulness means matching iperf3's accept/reject decision, not
    ///   erroring on a legal invocation.
    ///
    /// Returns the name of the first offending flag if one was given.
    pub fn first_client_only_violation(&self) -> Option<&'static str> {
        // iperf3 rejects `-A n,m` (the comma form carries a client affinity) but
        // accepts a bare `-A n` (server affinity). Mirror that distinction.
        let affinity_client_only = self
            .affinity
            .as_deref()
            .is_some_and(|spec| spec.contains(','));
        // (was-it-set, canonical flag name) — order is the report priority.
        let checks: [(bool, &'static str); 33] = [
            (self.udp, "-u/--udp"),
            (self.time.is_some(), "-t/--time"),
            (self.bytes.is_some(), "-n/--bytes"),
            (self.blockcount.is_some(), "-k/--blockcount"),
            (self.length.is_some(), "-l/--length"),
            (self.parallel.is_some(), "-P/--parallel"),
            (self.reverse, "-R/--reverse"),
            (self.bidir, "--bidir"),
            (self.window.is_some(), "-w/--window"),
            (self.congestion.is_some(), "-C/--congestion"),
            (self.mss.is_some(), "-M/--set-mss"),
            (self.no_delay, "-N/--no-delay"),
            (self.bitrate.is_some(), "-b/--bitrate"),
            (self.tos.is_some(), "-S/--tos"),
            (self.omit.is_some(), "-O/--omit"),
            (self.title.is_some(), "-T/--title"),
            (self.extra_data.is_some(), "--extra-data"),
            (self.connect_timeout.is_some(), "--connect-timeout"),
            (self.get_server_output, "--get-server-output"),
            (self.udp_counters_64bit, "--udp-counters-64bit"),
            (self.repeating_payload, "--repeating-payload"),
            (self.dont_fragment, "--dont-fragment"),
            (self.pacing_timer.is_some(), "--pacing-timer"),
            (self.zerocopy, "-Z/--zerocopy"),
            (self.skip_rx_copy, "--skip-rx-copy"),
            (self.fq_rate.is_some(), "--fq-rate"),
            (self.flowlabel.is_some(), "-L/--flowlabel"),
            (self.dscp.is_some(), "--dscp"),
            (affinity_client_only, "-A/--affinity"),
            (self.username.is_some(), "--username"),
            (self.rsa_public_key_path.is_some(), "--rsa-public-key-path"),
            (self.gsro, "--gsro"),
            (self.sendmmsg, "--sendmmsg"),
        ];
        checks.iter().find(|(set, _)| *set).map(|(_, name)| *name)
    }

    /// The first server-only option set on the command line, or `None`.
    ///
    /// iperf3 raises `IESERVERONLY` when a client (`-c`) is given an option that
    /// only makes sense on the server — every option whose parse arm sets
    /// `server_flag`, plus `--authorized-users-path` (caught by a separate role
    /// check). This mirrors that exact set so a riperf3 client rejects the same
    /// options iperf3 would, before any side effects. Companion to
    /// `first_client_only_violation` (#65); see #100.
    ///
    /// `--use-pkcs1-padding` is included deliberately: iperf3 marks it
    /// server-only (the server uses PKCS#1 v1.5 to decode tokens from legacy
    /// clients; modern clients always send OAEP), so a client passing it is an
    /// error in iperf3 too. The library `ClientBuilder::use_pkcs1_padding` stays
    /// available to embedders; only the CLI client path matches iperf3.
    pub fn first_server_only_violation(&self) -> Option<&'static str> {
        // (was-it-set, canonical flag name) — order is the report priority.
        // Cross-checked against iperf3's `server_flag` set in iperf_api.c.
        let checks: [(bool, &'static str); 9] = [
            (self.daemon, "-D/--daemon"),
            (self.one_off, "-1/--one-off"),
            (
                self.server_bitrate_limit.is_some(),
                "--server-bitrate-limit",
            ),
            (self.idle_timeout.is_some(), "--idle-timeout"),
            (self.server_max_duration.is_some(), "--server-max-duration"),
            (
                self.rsa_private_key_path.is_some(),
                "--rsa-private-key-path",
            ),
            (
                self.authorized_users_path.is_some(),
                "--authorized-users-path",
            ),
            (self.time_skew_threshold.is_some(), "--time-skew-threshold"),
            (self.use_pkcs1_padding, "--use-pkcs1-padding"),
        ];
        checks.iter().find(|(set, _)| *set).map(|(_, name)| *name)
    }

    /// Build a configured [`riperf3::Client`] from the parsed CLI.
    ///
    /// The single source of truth for the client arg→builder mapping, called by
    /// both `main` and the wiring tests (#124) so the tests exercise the real
    /// mapping rather than a hand-maintained copy. Process-level concerns
    /// (daemonize, pidfile, logfile, CPU affinity) stay in `main`; this is pure
    /// arg → builder → `build()`.
    pub fn build_client(&self) -> std::result::Result<riperf3::Client, Box<dyn std::error::Error>> {
        let host = self
            .client
            .as_deref()
            .ok_or("client mode requires a host (-c <host>)")?;
        let mut builder = riperf3::ClientBuilder::new(host);

        // Format: K/M/G/T → lowercase char for bits, uppercase for bytes
        let format_char = match self.format {
            Format::K => 'k',
            Format::M => 'm',
            Format::G => 'g',
            Format::T => 't',
        };
        builder = builder.format_char(format_char);

        if let Some(port) = self.port {
            builder = builder.port(Some(port));
        }
        if self.udp {
            builder = builder.protocol(riperf3::TransportProtocol::Udp);
        }
        if let Some(t) = self.time {
            builder = builder.duration(t);
        }
        if let Some(us) = self.pacing_timer {
            builder = builder.pacing_timer(us);
        }
        if let Some(ref s) = self.bytes {
            builder = builder.bytes_str(s)?;
        }
        if let Some(ref s) = self.blockcount {
            builder = builder.blocks_str(s)?;
        }
        if let Some(ref s) = self.length {
            builder = builder.blksize_str(s)?;
        }
        if let Some(n) = self.parallel {
            builder = builder.num_streams(n);
        }
        if self.reverse {
            builder = builder.reverse(true);
        }
        if self.bidir {
            builder = builder.bidir(true);
        }
        if let Some(ref s) = self.window {
            builder = builder.window_str(s)?;
        }
        if let Some(ref algo) = self.congestion {
            builder = builder.congestion(algo);
        }
        if let Some(mss) = self.mss {
            builder = builder.mss(mss);
        }
        if self.no_delay {
            builder = builder.no_delay(true);
        }
        if let Some(ref s) = self.bitrate {
            builder = builder.bandwidth_str(s)?;
        }
        if let Some(tos) = self.tos {
            builder = builder.tos(tos);
        }
        if let Some(o) = self.omit {
            builder = builder.omit(o);
        }
        if let Some(i) = self.interval {
            builder = builder.interval(i);
        }
        if let Some(ref t) = self.title {
            builder = builder.title(t);
        }
        if let Some(ref d) = self.extra_data {
            builder = builder.extra_data(d);
        }
        if let Some(ms) = self.connect_timeout {
            builder = builder.connect_timeout(std::time::Duration::from_millis(ms));
        }
        if self.verbose {
            builder = builder.verbose(true);
        }
        if self.json {
            builder = builder.json_output(true);
        }
        if self.json_stream {
            builder = builder.json_stream(true);
        }
        if self.udp_counters_64bit {
            builder = builder.udp_counters_64bit(true);
        }
        if self.repeating_payload {
            builder = builder.repeating_payload(true);
        }
        if self.zerocopy {
            builder = builder.zerocopy(true);
        }
        if self.gsro {
            builder = builder.gsro(true);
        }
        if self.sendmmsg {
            builder = builder.sendmmsg(true);
        }
        if self.dont_fragment {
            builder = builder.dont_fragment(true);
        }
        if let Some(port) = self.cport {
            builder = builder.cport(port);
        }
        if self.get_server_output {
            builder = builder.get_server_output(true);
        }
        if self.forceflush {
            builder = builder.forceflush(true);
        }
        if let Some(ref fmt) = self.timestamps {
            builder = builder.timestamps(fmt);
        }
        if let Some(ref addr) = self.bind {
            builder = builder.bind_address(addr);
        }
        if let Some(ref dev) = self.bind_dev {
            builder = builder.bind_dev(dev);
        }
        if let Some(ref s) = self.fq_rate {
            builder = builder.fq_rate_str(s)?;
        }
        if let Some(label) = self.flowlabel {
            builder = builder.flowlabel(label);
        }
        if self.version4 {
            builder = builder.ip_version(4);
        }
        if self.version6 {
            builder = builder.ip_version(6);
        }
        if self.mptcp {
            builder = builder.mptcp(true);
        }
        if self.skip_rx_copy {
            builder = builder.skip_rx_copy(true);
        }
        if let Some(ms) = self.rcv_timeout {
            builder = builder.rcv_timeout(ms);
        }
        if let Some(ms) = self.snd_timeout {
            builder = builder.snd_timeout(ms);
        }
        if let Some(ref path) = self.file {
            builder = builder.file(path);
        }
        if let Some(ref val) = self.dscp {
            builder = builder.dscp(val);
        }
        if let Some(ref spec) = self.cntl_ka {
            builder = builder.cntl_ka(spec);
        }
        if let Some(ref name) = self.username {
            builder = builder.username(name);
        }
        if let Some(ref path) = self.rsa_public_key_path {
            builder = builder.rsa_public_key_path(path);
        }
        // `--use-pkcs1-padding` is server-only and is rejected for clients in
        // `main` (#100), mirroring iperf3, so it is intentionally not wired on
        // the client here. The library `ClientBuilder::use_pkcs1_padding` stays
        // available to embedders.

        Ok(builder.build()?)
    }

    /// Build a configured [`riperf3::Server`] from the parsed CLI.
    ///
    /// Companion to [`Cli::build_client`] — the single source of truth for the
    /// server arg→builder mapping (#124). `-D`/`--daemon` is intentionally not
    /// here: the binary daemonizes before building the runtime, since the
    /// library cannot fork safely from inside the async runtime (#81).
    pub fn build_server(&self) -> std::result::Result<riperf3::Server, Box<dyn std::error::Error>> {
        let mut builder = riperf3::ServerBuilder::new();

        if let Some(port) = self.port {
            builder = builder.port(Some(port));
        }
        if self.one_off {
            builder = builder.one_off(true);
        }
        if self.verbose {
            builder = builder.verbose(true);
        }
        if self.json {
            builder = builder.json_output(true);
        }
        if self.json_stream {
            builder = builder.json_stream(true);
        }
        if let Some(secs) = self.idle_timeout {
            builder = builder.idle_timeout(secs);
        }
        if let Some(ref s) = self.server_bitrate_limit {
            builder = builder.server_bitrate_limit_str(s)?;
        }
        if let Some(secs) = self.server_max_duration {
            builder = builder.server_max_duration(secs);
        }
        if self.forceflush {
            builder = builder.forceflush(true);
        }
        if let Some(ref addr) = self.bind {
            builder = builder.bind_address(addr);
        }
        if self.version4 {
            builder = builder.ip_version(4);
        } else if self.version6 {
            builder = builder.ip_version(6);
        }
        if let Some(ref fmt) = self.timestamps {
            builder = builder.timestamps(fmt);
        }
        if let Some(ref path) = self.rsa_private_key_path {
            builder = builder.rsa_private_key_path(path);
        }
        if let Some(ref path) = self.authorized_users_path {
            builder = builder.authorized_users_path(path);
        }
        if let Some(secs) = self.time_skew_threshold {
            builder = builder.time_skew_threshold(secs);
        }
        if self.use_pkcs1_padding {
            builder = builder.use_pkcs1_padding(true);
        }

        Ok(builder.build()?)
    }
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

        // #65: the server rejects client-only options, like iperf3.
        #[test]
        fn server_flags_client_only_violation() {
            // A bare server, and a server with only server/common-valid options,
            // are clean.
            assert_eq!(
                Cli::parse_from(["riperf3", "-s"]).first_client_only_violation(),
                None
            );
            let clean = Cli::parse_from([
                "riperf3",
                "-s",
                "-1",
                "-D",
                "-p",
                "5201",
                "-B",
                "0.0.0.0",
                "-i",
                "1",
                "-J",
                "-V",
                "--idle-timeout",
                "30",
                "--server-max-duration",
                "60",
                "--forceflush",
            ]);
            assert_eq!(clean.first_client_only_violation(), None);

            // Options iperf3 ACCEPTS on a server must not be flagged: a bare
            // `-A n` (server's own affinity), `--cport`, and `-m/--mptcp` all map
            // to iperf3 cases that do not set `client_flag`.
            for args in [
                vec!["-A", "3"],
                vec!["--affinity", "3"],
                vec!["--cport", "12345"],
                vec!["-m"],
                vec!["--mptcp"],
            ] {
                let mut argv = vec!["riperf3", "-s"];
                argv.extend(args.iter().copied());
                let cli = Cli::parse_from(&argv);
                assert_eq!(
                    cli.first_client_only_violation(),
                    None,
                    "expected {args:?} accepted on the server (iperf3 parity)"
                );
            }

            // Each client-only option is flagged on the server. `-A n,m` is
            // client-only (the comma form carries a client affinity); `--gsro`
            // maps to iperf3's `gsro_flag` reject; `--sendmmsg` is riperf3-only.
            for (args, want) in [
                (vec!["-t", "5"], "-t/--time"),
                (vec!["-u"], "-u/--udp"),
                (vec!["-R"], "-R/--reverse"),
                (vec!["--bidir"], "--bidir"),
                (vec!["-P", "4"], "-P/--parallel"),
                (vec!["-b", "100M"], "-b/--bitrate"),
                (vec!["--extra-data", "x"], "--extra-data"),
                (vec!["-w", "1M"], "-w/--window"),
                (vec!["--get-server-output"], "--get-server-output"),
                (vec!["-Z"], "-Z/--zerocopy"),
                (vec!["-A", "3,4"], "-A/--affinity"),
                (vec!["--gsro"], "--gsro"),
                (vec!["--sendmmsg"], "--sendmmsg"),
            ] {
                let mut argv = vec!["riperf3", "-s"];
                argv.extend(args.iter().copied());
                let cli = Cli::parse_from(&argv);
                assert_eq!(
                    cli.first_client_only_violation(),
                    Some(want),
                    "expected {want} flagged for args {args:?}"
                );
            }
        }

        // #100: the client rejects server-only options, like iperf3 (IESERVERONLY).
        #[test]
        fn client_flags_server_only_violation() {
            // A bare client, and a client with only client/common-valid
            // options (including the client-side auth options), are clean.
            assert_eq!(
                Cli::parse_from(["riperf3", "-c", "host"]).first_server_only_violation(),
                None
            );
            let clean = Cli::parse_from([
                "riperf3",
                "-c",
                "host",
                "-t",
                "5",
                "-u",
                "-R",
                "-P",
                "4",
                "-b",
                "100M",
                "-p",
                "5201",
                "-i",
                "1",
                "-J",
                "--username",
                "alice",
                "--rsa-public-key-path",
                "/tmp/key.pem",
            ]);
            assert_eq!(clean.first_server_only_violation(), None);

            // Each server-only option is flagged on the client. Cross-checked
            // against iperf3's `server_flag` set (raises IESERVERONLY).
            for (args, want) in [
                (vec!["-D"], "-D/--daemon"),
                (vec!["-1"], "-1/--one-off"),
                (
                    vec!["--server-bitrate-limit", "100M"],
                    "--server-bitrate-limit",
                ),
                (vec!["--idle-timeout", "30"], "--idle-timeout"),
                (vec!["--server-max-duration", "60"], "--server-max-duration"),
                (
                    vec!["--rsa-private-key-path", "/tmp/priv.pem"],
                    "--rsa-private-key-path",
                ),
                (
                    vec!["--authorized-users-path", "/tmp/users"],
                    "--authorized-users-path",
                ),
                (vec!["--time-skew-threshold", "5"], "--time-skew-threshold"),
                (vec!["--use-pkcs1-padding"], "--use-pkcs1-padding"),
            ] {
                let mut argv = vec!["riperf3", "-c", "host"];
                argv.extend(args.iter().copied());
                let cli = Cli::parse_from(&argv);
                assert_eq!(
                    cli.first_server_only_violation(),
                    Some(want),
                    "expected {want} flagged for args {args:?}"
                );
            }
        }
    }

    /// Tests that CLI flags are correctly wired through to Client/Server
    /// fields, matching the logic in main.rs. This is the test class that
    /// would have caught the -J flag not being wired up.
    mod cli_wiring_tests {
        use super::*;
        use riperf3::TransportProtocol;

        /// Build a `Client` through the real production mapping
        /// (`Cli::build_client`). Tests call this so they exercise the same code
        /// path as `main`, instead of a hand-maintained copy that can drift (the
        /// blind spot the `-J` bug exploited). See #124.
        fn build_client_from_cli(cli: &Cli) -> riperf3::Client {
            cli.build_client().unwrap()
        }

        /// Server counterpart of [`build_client_from_cli`] — exercises the real
        /// `Cli::build_server` mapping (#124).
        fn build_server_from_cli(cli: &Cli) -> riperf3::Server {
            cli.build_server().unwrap()
        }

        /// A `ClientBuilder` pre-seeded with the CLI's unconditional defaults, so
        /// wiring-test expectations match what `build_client` produces. The CLI
        /// always sets the report format (default `-f m`), whereas the bare
        /// library builder defaults to 'a' (auto); without this baseline the
        /// comparisons would differ only in `format_char`.
        fn expected_client(host: &str) -> riperf3::ClientBuilder {
            riperf3::ClientBuilder::new(host).format_char('m')
        }

        #[test]
        fn json_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-J"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host").json_output(true).build().unwrap()
            );
        }

        #[test]
        fn udp_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-u"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .protocol(TransportProtocol::Udp)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn duration_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-t", "30"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").duration(30).build().unwrap());
        }

        #[test]
        fn bytes_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-n", "1M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .bytes_str("1M")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn blockcount_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-k", "100"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .blocks_str("100")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn length_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-l", "64K"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .blksize_str("64K")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn parallel_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-P", "8"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").num_streams(8).build().unwrap());
        }

        #[test]
        fn reverse_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-R"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").reverse(true).build().unwrap());
        }

        #[test]
        fn bidir_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--bidir"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").bidir(true).build().unwrap());
        }

        #[test]
        fn window_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-w", "512K"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .window_str("512K")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        // build() rejects -C/--congestion (TCP congestion control) on non-Unix
        // (cfg(not(unix)) → Unsupported), so gate the wiring test to match.
        #[cfg(unix)]
        #[test]
        fn congestion_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-C", "bbr"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host").congestion("bbr").build().unwrap()
            );
        }

        #[test]
        fn mss_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-M", "1400"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").mss(1400).build().unwrap());
        }

        #[test]
        fn no_delay_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-N"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").no_delay(true).build().unwrap());
        }

        #[test]
        fn bitrate_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-b", "100M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .bandwidth_str("100M")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn tos_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-S", "16"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").tos(16).build().unwrap());
        }

        #[test]
        fn omit_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-O", "3"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").omit(3).build().unwrap());
        }

        #[test]
        fn title_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-T", "my test"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").title("my test").build().unwrap());
        }

        #[test]
        fn extra_data_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--extra-data", "abc"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host").extra_data("abc").build().unwrap()
            );
        }

        #[test]
        fn connect_timeout_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--connect-timeout", "500"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("host")
                    .connect_timeout(std::time::Duration::from_millis(500))
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn verbose_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-V"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").verbose(true).build().unwrap());
        }

        // Combines the Unix-only flags above (congestion/bind-dev/affinity),
        // which build() rejects on non-Unix, so gate to match.
        #[cfg(unix)]
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
            assert_eq!(
                c,
                expected_client("host")
                    .protocol(TransportProtocol::Udp)
                    .duration(30)
                    .num_streams(4)
                    .reverse(true)
                    .bidir(true)
                    .no_delay(true)
                    .blksize_str("1460")
                    .unwrap()
                    .bandwidth_str("100M")
                    .unwrap()
                    .json_output(true)
                    .verbose(true)
                    .window_str("512K")
                    .unwrap()
                    .mss(1400)
                    .congestion("bbr")
                    .tos(16)
                    .omit(2)
                    .title("test")
                    .extra_data("x")
                    .connect_timeout(std::time::Duration::from_millis(500))
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_one_off_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-1"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new().one_off(true).build().unwrap()
            );
        }

        // -- New flag wiring tests --

        #[test]
        fn udp_counters_64bit_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--udp-counters-64bit"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .udp_counters_64bit(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn repeating_payload_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--repeating-payload"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .repeating_payload(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn dont_fragment_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--dont-fragment"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").dont_fragment(true).build().unwrap());
        }

        #[test]
        fn cport_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cport", "12345"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").cport(12345).build().unwrap());
        }

        #[test]
        fn get_server_output_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--get-server-output"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .get_server_output(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn forceflush_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--forceflush"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").forceflush(true).build().unwrap());
        }

        #[test]
        fn timestamps_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--timestamps", "%H:%M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").timestamps("%H:%M").build().unwrap());
        }

        #[test]
        fn version4_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-4"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").ip_version(4).build().unwrap());
        }

        #[test]
        fn version6_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-6"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").ip_version(6).build().unwrap());
        }

        #[test]
        fn version4_and_version6_conflict() {
            // -4 and -6 are mutually exclusive (matches iperf3).
            let err = Cli::try_parse_from(["riperf3", "-c", "h", "-4", "-6"]);
            assert!(err.is_err(), "-4 -6 together should be rejected");
        }

        #[test]
        fn bind_address_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-B", "10.0.0.1"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .bind_address("10.0.0.1")
                    .build()
                    .unwrap()
            );
        }

        // build() rejects --bind-dev (SO_BINDTODEVICE/IP_BOUND_IF) on non-Unix,
        // so gate the wiring test to match.
        #[cfg(unix)]
        #[test]
        fn bind_dev_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--bind-dev", "eth0"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").bind_dev("eth0").build().unwrap());
        }

        #[test]
        fn fq_rate_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--fq-rate", "1G"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .fq_rate_str("1G")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn flowlabel_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-L", "42"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").flowlabel(42).build().unwrap());
        }

        #[test]
        fn dscp_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--dscp", "46"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").dscp("46").build().unwrap());
        }

        #[test]
        fn mptcp_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-m"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").mptcp(true).build().unwrap());
        }

        #[test]
        fn skip_rx_copy_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--skip-rx-copy"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").skip_rx_copy(true).build().unwrap());
        }

        #[test]
        fn rcv_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--rcv-timeout", "5000"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").rcv_timeout(5000).build().unwrap());
        }

        #[test]
        fn snd_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--snd-timeout", "3000"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").snd_timeout(3000).build().unwrap());
        }

        #[test]
        fn file_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-F", "/tmp/data"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").file("/tmp/data").build().unwrap());
        }

        // affinity/pidfile/logfile builder setters were pruned (#122) — the CLI
        // realizes those at the process level, not via the builder.

        #[test]
        fn json_stream_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--json-stream"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").json_stream(true).build().unwrap());
        }

        // sendmmsg(2) is Linux/FreeBSD/NetBSD-only (stream.rs); build() rejects
        // --sendmmsg on other platforms, so gate the wiring test to match (#76).
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
        #[test]
        fn sendmmsg_flag_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-u", "--sendmmsg"]);
            assert!(cli.sendmmsg);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .protocol(TransportProtocol::Udp)
                    .sendmmsg(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn sendmmsg_default_false() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-u"]);
            assert!(!cli.sendmmsg);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .protocol(TransportProtocol::Udp)
                    .build()
                    .unwrap()
            );
        }

        // Server new flags
        #[test]
        fn server_daemon_flag_parses() {
            // `-D`/`--daemon` is consumed by the binary (it daemonizes before the
            // tokio runtime is built — see main.rs / #81), not by ServerBuilder,
            // so there's nothing to wire through the builder; just assert parse.
            let cli = Cli::parse_from(["riperf3", "-s", "-D"]);
            assert!(cli.daemon);
            let long = Cli::parse_from(["riperf3", "-s", "--daemon"]);
            assert!(long.daemon);
        }

        #[test]
        fn server_idle_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--idle-timeout", "30"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new()
                    .idle_timeout(30)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_max_duration_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--server-max-duration", "60"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new()
                    .server_max_duration(60)
                    .build()
                    .unwrap()
            );
        }

        // -- #124: flags that previously had no dedicated wiring test (or were
        // silently omitted by the old hand-copied helper). Now covered via the
        // real `build_client` mapping.

        #[test]
        fn port_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-p", "5201"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").port(Some(5201)).build().unwrap());
        }

        #[test]
        fn interval_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-i", "0.5"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").interval(0.5).build().unwrap());
        }

        // zerocopy (sendfile) and gsro (UDP GSO/GRO) are rejected by `build()` on
        // non-unix (cfg(not(unix)) → Unsupported), so gate these wiring tests to
        // unix to match — same as congestion_flag_wired / bind_dev_wired.
        #[cfg(unix)]
        #[test]
        fn zerocopy_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-Z"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").zerocopy(true).build().unwrap());
        }

        #[cfg(unix)]
        #[test]
        fn gsro_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--gsro"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").gsro(true).build().unwrap());
        }

        #[test]
        fn cntl_ka_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cntl-ka", "10/5/3"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").cntl_ka("10/5/3").build().unwrap());
        }

        // #32: --pacing-timer was parsed but never wired — a silent no-op.
        #[test]
        fn pacing_timer_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--pacing-timer", "500"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").pacing_timer(500).build().unwrap());
        }

        #[test]
        fn format_wired() {
            // The CLI always sets a format (default 'm'); `-f g` must propagate.
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-f", "g"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").format_char('g').build().unwrap());
        }

        #[test]
        fn username_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--username", "alice"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").username("alice").build().unwrap());
        }

        #[test]
        fn rsa_public_key_path_wired() {
            let cli =
                Cli::parse_from(["riperf3", "-c", "h", "--rsa-public-key-path", "/tmp/k.pem"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .rsa_public_key_path("/tmp/k.pem")
                    .build()
                    .unwrap()
            );
        }

        // -- #124: server-side wiring coverage. `build_server` is now the single
        // source of truth, so cover its setters too (previously only one_off,
        // idle_timeout, and server_max_duration had dedicated tests). `build_server`
        // has no unconditional defaults, so the bare `ServerBuilder::new()` is the
        // correct baseline (unlike the client's `expected_client`).

        #[test]
        fn server_port_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-p", "5201"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .port(Some(5201))
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_verbose_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-V"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new().verbose(true).build().unwrap()
            );
        }

        #[test]
        fn server_json_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-J"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .json_output(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_json_stream_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--json-stream"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .json_stream(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_forceflush_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--forceflush"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .forceflush(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_bind_address_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-B", "0.0.0.0"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .bind_address("0.0.0.0")
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_ip_version4_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-4"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new().ip_version(4).build().unwrap()
            );
        }

        #[test]
        fn server_ip_version6_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-6"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new().ip_version(6).build().unwrap()
            );
        }

        #[test]
        fn server_timestamps_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--timestamps", "%H"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .timestamps("%H")
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_bitrate_limit_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--server-bitrate-limit", "100M"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .server_bitrate_limit_str("100M")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_authorized_users_path_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--authorized-users-path", "/tmp/users"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .authorized_users_path("/tmp/users")
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_rsa_private_key_path_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--rsa-private-key-path", "/tmp/priv.pem"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .rsa_private_key_path("/tmp/priv.pem")
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_time_skew_threshold_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--time-skew-threshold", "5"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .time_skew_threshold(5)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_use_pkcs1_padding_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--use-pkcs1-padding"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .use_pkcs1_padding(true)
                    .build()
                    .unwrap()
            );
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
