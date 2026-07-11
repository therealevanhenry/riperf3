use clap::{ArgGroup, Parser};

/// iperf3's IEENDCONDITIONS text, verbatim (#140).
pub const END_CONDITIONS_MSG: &str = "only one test end condition (-t, -n, -k) may be specified";

#[derive(Parser, Debug)]
#[command(about, author, long_about = None, name = "riperf3", version, disable_version_flag = true)]
// Last-wins for repeated flags, like iperf3's getopt: wrapper scripts that
// append override flags to a base command line (`-b 0 ... -b 100M`) must not
// be rejected by the drop-in (#205).
#[command(args_override_self = true)]
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
    // #328: GT parses -p with atoi (iperf_api.c:1229) — `17299x` is 17299,
    // garbage is 0 — then range-checks 1..=65535 (IEBADPORT); the pre-sink
    // chain in main.rs rejects with GT's wording.
    #[arg(short, long, allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub port: Option<i64>,

    /// [kmgtKMGT] format to report: Kbits, Mbits, Gbits, Tbits
    // iperf3 has NO default -f: absent, every figure auto-scales
    // (unit_snprintf 'a'/'A'). The old forced "m" default printed
    // 12120.88 MBytes where iperf3 prints 11.8 GBytes (#221). NB: a ///
    // here would leak into clap's --help text (r1 blocker).
    // Case-sensitive like iperf3's [kmgtKMGT] (#241): lowercase = bit-rates,
    // UPPERCASE = byte-rates — ignore_case used to collapse `-f K` into
    // Kbits, silently a different unit.
    // A plain String, not a ValueEnum (#263): GT parses only `*optarg`, so
    // `-f kilobits` means `-f k` — validation happens post-parse via
    // [`parse_format_char`], rejecting with GT's IEBADFORMAT sentence
    // instead of a clap invalid-value error.
    #[arg(short, long, value_name = "format")]
    pub format: Option<String>,

    /// Seconds between periodic throughput reports
    // #328: GT parses -i with C atof (iperf_api.c:1260) — strtod's longest
    // prefix, garbage → 0.0, so `-i 2x` is 2.0 and `-i x` is 0.0 — then the
    // IEINTERVAL range check (in main.rs). allow_hyphen: `-i -1` takes the
    // range sentence like GT, not a clap error.
    #[arg(short, long, value_name = "interval", allow_hyphen_values = true, value_parser = atof_like_os())]
    pub interval: Option<f64>,

    /// Enable verbose output
    #[arg(short = 'V', long)]
    pub verbose: bool,

    /// Output in JSON format
    #[arg(short = 'J', long)]
    pub json: bool,

    /// Debug level 0-4 (default 4)
    // #328: GT's level is C atoi with negative → DEBUG_LEVEL_MAX
    // (iperf_api.c:1692-1697) and NO upper clamp (`--debug=100` runs), so
    // the old 1..=4 clap range rejected GT-accepted forms (`--debug=abc` is
    // level 0 in GT). KNOWN-DIVERGENT: GT's short `-d` takes no value ever
    // (getopt "d" without a colon — `-d3` is "invalid option -- '3'" and
    // `-d 3` leaves 3 as an ignored operand); clap's optional-value form
    // consumes both. Levels above 4 clamp to the max log verbosity.
    #[arg(short, long, value_name = "level", num_args = 0..=1,
          value_parser = debug_level_like_os(),
          default_missing_value = "4")]
    pub debug: Option<i64>,

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
    // i64 + negatives accepted at parse (#303): GT's atoi wraps them into
    // its range checks; the pre-sink chain rejects with GT's wording.
    // (#328: value_parser moved to the OsString level so raw invalid-UTF-8
    // argv bytes parse like C atoi — garbage → 0 — instead of dying at
    // clap's UTF-8 conversion. Same for -t/-O/--server-max-duration.)
    #[arg(long, value_name = "secs", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub idle_timeout: Option<i64>,

    /// Server's total bit rate limit
    // #328: GT parses `rate[/interval]` (iperf_api.c:1366-1385) — the
    // interval piece with C atof + IETOTALINTERVAL, then the rate with
    // unit_atof_rate (1000-based) + IEUNITVAL, both in main.rs. allow_hyphen:
    // a negative rate (uint64)-wraps huge and GT proceeds.
    #[arg(
        long = "server-bitrate-limit",
        value_name = "rate",
        allow_hyphen_values = true
    )]
    pub server_bitrate_limit: Option<std::ffi::OsString>,

    /// Max time a test can run on the server
    #[arg(
        long = "server-max-duration",
        value_name = "secs",
        allow_hyphen_values = true,
        value_parser = atoi_like_os()
    )]
    pub server_max_duration: Option<i64>,

    // -----------------------------------------------------------------------
    // Client-specific arguments
    // -----------------------------------------------------------------------
    /// Use UDP rather than TCP
    #[arg(short = 'u', long)]
    pub udp: bool,

    /// Time in seconds to transmit for (default 10 secs)
    #[arg(short = 't', long, value_name = "secs", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub time: Option<i64>,

    /// Number of bytes to transmit (instead of -t)
    // #328: raw OsString — GT parses -n with unit_atoi (iperf_api.c:1394);
    // the pre-sink chain in main.rs validates and IEUNITVAL echoes the raw
    // argv bytes. allow_hyphen: `-n -5` (uint64)-wraps huge and GT RUNS it.
    #[arg(short = 'n', long, value_name = "bytes", allow_hyphen_values = true)]
    pub bytes: Option<std::ffi::OsString>,

    /// Number of blocks (packets) to transmit (instead of -t or -n)
    // #328: unit_atoi like -n (iperf_api.c:1401).
    #[arg(short = 'k', long, value_name = "count", allow_hyphen_values = true)]
    pub blockcount: Option<std::ffi::OsString>,

    /// Length of buffer to read or write (default 128 KB for TCP; UDP tracks the connection MSS, else 1460)
    // #328: unit_atoi through an int (iperf_api.c:1408) — 3G wraps
    // negative; GT's post-loop IEBLOCKSIZE/IEUDPBLOCKSIZE range checks
    // (:1926-1944) live in main.rs.
    #[arg(short = 'l', long, value_name = "size", allow_hyphen_values = true)]
    pub length: Option<std::ffi::OsString>,

    /// Number of parallel client streams to run
    // #328: GT parses -P with atoi (iperf_api.c:1415) and checks ONLY the
    // upper bound (> MAX_STREAMS → IENUMSTREAMS, in main.rs); there is no
    // lower-bound check at parse — live-probed: GT runs `-P 0` as an
    // instantly-complete 0-stream test and `-P -1` proceeds too.
    #[arg(short = 'P', long, value_name = "num", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub parallel: Option<i64>,

    /// Reverse mode (server sends, client receives)
    #[arg(short = 'R', long)]
    pub reverse: bool,

    /// Bidirectional mode: client and server send and receive
    #[arg(long)]
    pub bidir: bool,

    /// Set socket buffer sizes (indirectly sets TCP window size)
    // #334: GT parses -w with unit_atof (iperf_api.c:1438-1452) — a 1024-based
    // size returning a double, then `> MAX_TCP_BUFFER` (536870912) → IEBUFSIZE,
    // else `(int) farg`. The IEUNITVAL/IEBUFSIZE surface runs pre-sink in
    // main.rs; raw OsString so invalid-UTF-8 argv echoes byte-for-byte like GT.
    // allow_hyphen: a negative casts straight through like GT's (int) farg.
    #[arg(short = 'w', long, value_name = "size", allow_hyphen_values = true)]
    pub window: Option<std::ffi::OsString>,

    /// Set TCP congestion control algorithm
    #[arg(short = 'C', long, value_name = "algo")]
    pub congestion: Option<String>,

    /// Set TCP/SCTP maximum segment size (MTU - 40 bytes)
    // #328: GT parses -M with atoi (iperf_api.c:1487) and checks ONLY the
    // upper bound (> MAX_MSS → IEMSS, in main.rs); negatives proceed to a
    // runtime setsockopt failure (live-probed: `-M -100` connects first).
    #[arg(short = 'M', long = "set-mss", value_name = "mss", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub mss: Option<i64>,

    /// Disable Nagle's algorithm (set TCP_NODELAY)
    #[arg(short = 'N', long = "no-delay")]
    pub no_delay: bool,

    /// Target bitrate in bits/sec (0 = unlimited; default: unlimited TCP, 1M UDP)
    // #334: GT parses -b `rate[/burst]` (iperf_api.c:1347-1365) — slash-split
    // FIRST: if a '/' is present, burst = atoi(after) with `<= 0 || >
    // MAX_BURST` (1000) → IEBURST; THEN rate = unit_atof_rate(before)
    // (1000-based) → IEUNITVAL. Both run pre-sink in main.rs. allow_hyphen: a
    // negative rate wraps huge (iperf_size_t) and GT proceeds.
    #[arg(
        short = 'b',
        long,
        value_name = "rate[/burst]",
        allow_hyphen_values = true
    )]
    pub bitrate: Option<std::ffi::OsString>,

    /// Set the IP type of service (0-255; decimal, 0x hex, or 0 octal)
    // String, parsed by the builder's `tos_str` with iperf3's strtol-base-0
    // semantics + IEBADTOS range check (#167).
    #[arg(short = 'S', long, value_name = "tos")]
    pub tos: Option<String>,

    /// Omit the first N seconds of the test
    #[arg(short = 'O', long, value_name = "secs", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub omit: Option<i64>,

    /// Prefix every output line with this string
    #[arg(short = 'T', long, value_name = "title")]
    pub title: Option<String>,

    /// Extra data string to include in JSON output
    #[arg(long, value_name = "str")]
    pub extra_data: Option<String>,

    /// Timeout for control connection setup (ms)
    // #328: GT parses --connect-timeout with unit_atoi through an int
    // (iperf_api.c:1787) — `1K` is 1024 ms — and hands it to netdial;
    // poll(2) with a negative timeout waits forever (net.c:272-289), so
    // negatives mean "no timeout" (live-probed: `--connect-timeout -100`
    // connects fine).
    #[arg(long, value_name = "ms", allow_hyphen_values = true)]
    pub connect_timeout: Option<std::ffi::OsString>,

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
    // #328: GT parses --cport with atoi (iperf_api.c:1479) then the same
    // 1..=65535 IEBADPORT range check as -p (in main.rs).
    #[arg(long, value_name = "port", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub cport: Option<i64>,

    /// Set the server timing for pacing in microseconds (deprecated)
    // #328: GT parses --pacing-timer with unit_atoi through an int
    // (iperf_api.c:1780), so KMG suffixes (1024-based) work and `3G` wraps
    // negative — GT proceeds (live-probed); see build_client for the
    // recorded deviation on the wrapped values. (#160's lib-level
    // `pacing_timer_str` cap remains for embedders.)
    #[arg(long, value_name = "usec", allow_hyphen_values = true)]
    pub pacing_timer: Option<std::ffi::OsString>,

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
    // #334: GT parses --fq-rate with unit_atof_rate (iperf_api.c:1726-1737),
    // 1000-based → IEUNITVAL pre-sink in main.rs. allow_hyphen: a negative rate
    // wraps huge (iperf_size_t) and GT proceeds.
    #[arg(long, value_name = "rate", allow_hyphen_values = true)]
    pub fq_rate: Option<std::ffi::OsString>,

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
    // #328: GT parses --rcv-timeout with atoi (iperf_api.c:1603), then
    // MIN_NO_MSG_RCVD_TIMEOUT..=MAX_TIME*1000 → IERCVTIMEOUT (in main.rs).
    #[arg(long, value_name = "ms", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub rcv_timeout: Option<i64>,

    /// Timeout for unacknowledged TCP data (ms)
    // #328: GT parses --snd-timeout with atoi (iperf_api.c:1614), then
    // 0..=MAX_TIME*1000 → IESNDTIMEOUT (in main.rs).
    #[arg(long, value_name = "ms", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub snd_timeout: Option<i64>,

    /// Transmit/receive the specified file
    #[arg(short = 'F', long, value_name = "name")]
    pub file: Option<String>,

    /// Set CPU affinity
    #[arg(short = 'A', long, value_name = "n[,m]")]
    pub affinity: Option<String>,

    /// Output in line-delimited JSON format
    #[arg(long)]
    pub json_stream: bool,

    /// With --json-stream, also print the complete monolithic JSON document
    /// after the stream ends
    #[arg(long = "json-stream-full-output")]
    pub json_stream_full_output: bool,

    /// enable UDP GSO/GRO on both client and server (client-only option)
    #[arg(long, verbatim_doc_comment)]
    pub gsro: bool,

    /// Use sendmmsg for batched UDP sends (experimental, Linux/FreeBSD/NetBSD)
    #[arg(long)]
    pub sendmmsg: bool,

    /// Use control connection TCP keepalive
    // #328: GT's --cntl-ka is optional_argument (iperf_api.c:1191) — bare
    // `--cntl-ka` enables keepalive with the 0-defaults (:3311-3313, filled
    // at socket time :5590-5600); the spec pieces are each C atoi and the
    // sanity check → IECNTLKA lives in main.rs. Raw OsString: garbage
    // pieces atoi to 0 like GT. require_equals (r1 F3): getopt's
    // optional_argument only ever attaches via `=` — GT treats a separate
    // `--cntl-ka 5/5/1` token as a stray operand it silently ignores
    // (live-probed: keepalive defaults, spec dropped, run proceeds), so the
    // spec must never be consumed from the next token. KNOWN-DIVERGENT:
    // the stray token then hits riperf3's pre-existing stray-operand
    // rejection (clap unexpected-argument) instead of being ignored.
    #[arg(
        long,
        value_name = "idle/intv/cnt",
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    pub cntl_ka: Option<std::ffi::OsString>,

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
    // #328: GT parses --time-skew-threshold with atoi (iperf_api.c:1761)
    // then rejects <= 0 with IESKEWTHRESHOLD (in main.rs) — so `abc` (atoi
    // 0) takes the skew sentence, in-loop, BEFORE the post-loop
    // server-only role check (live-probed).
    #[arg(long, value_name = "secs", allow_hyphen_values = true, value_parser = atoi_like_os())]
    pub time_skew_threshold: Option<i64>,

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
    /// `server_flag`. This mirrors that exact set so a riperf3 client rejects
    /// the same options iperf3 would, before any side effects. Companion to
    /// `first_client_only_violation` (#65); see #100.
    ///
    /// `--authorized-users-path` is deliberately ABSENT (#395 r1 F2): its
    /// getopt case never sets `server_flag` (iperf_api.c:1757-1759), so GT
    /// catches it only at the post-loop :1874 leg, AFTER the client-auth
    /// checks — the dedicated late leg in `parse_class_rejection` mirrors
    /// that slot (live-probed: `-c --username u --authorized-users-path f`
    /// is IESETCLIENTAUTH on GT, not IESERVERONLY).
    ///
    /// `--use-pkcs1-padding` is included deliberately: iperf3 marks it
    /// server-only (the server uses PKCS#1 v1.5 to decode tokens from legacy
    /// clients; modern clients always send OAEP), so a client passing it is an
    /// error in iperf3 too. The library `ClientBuilder::use_pkcs1_padding` stays
    /// available to embedders; only the CLI client path matches iperf3.
    pub fn first_server_only_violation(&self) -> Option<&'static str> {
        // (was-it-set, canonical flag name) — order is the report priority.
        // Cross-checked against iperf3's `server_flag` set in iperf_api.c.
        let checks: [(bool, &'static str); 8] = [
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
            (self.time_skew_threshold.is_some(), "--time-skew-threshold"),
            (self.use_pkcs1_padding, "--use-pkcs1-padding"),
        ];
        checks.iter().find(|(set, _)| *set).map(|(_, name)| *name)
    }

    /// Whether a `-n`/`-k` argument carries a non-zero value. iperf3's
    /// IEENDCONDITIONS legs test the PARSED value, not flag presence — so
    /// `-n 0`, `-n 0K`, and even `-n 0.5` (truncates to 0) all mean "not
    /// set" and run a plain duration test (review r2). #328: the value is
    /// GT's own unit_atoi + (uint64) conversion, so `-n -5` (huge wrap)
    /// counts as set. An arg that doesn't parse returns false so the real
    /// IEUNITVAL error surfaces first, not a bogus conflict.
    fn end_condition_set(arg: Option<&std::ffi::OsStr>) -> bool {
        arg.is_some_and(|s| {
            unit_atoi_like_bytes(s.as_encoded_bytes())
                .map(|n| c_double_to_u64(n) != 0)
                .unwrap_or(false)
        })
    }

    /// iperf3 permits only ONE test end condition (IEENDCONDITIONS,
    /// `parse_arguments`): `-t` explicit with a non-zero `-n` or `-k`, or
    /// non-zero `-n` with non-zero `-k`. Only `-t` is flag-based; the `-n`/`-k`
    /// legs are value-based (#140). Called from `main` BEFORE any side effects
    /// (pidfile/logfile/affinity), like iperf3, and again from `build_client`
    /// so the library-path mapping is covered by the wiring tests (#124).
    pub fn end_conditions_conflict(&self) -> bool {
        let bytes_set = Self::end_condition_set(self.bytes.as_deref());
        let blocks_set = Self::end_condition_set(self.blockcount.as_deref());
        (self.time.is_some() && (bytes_set || blocks_set)) || (bytes_set && blocks_set)
    }

    /// Build a configured [`riperf3::Client`] from the parsed CLI.
    ///
    /// The single source of truth for the client arg→builder mapping, called by
    /// both `main` and the wiring tests (#124) so the tests exercise the real
    /// mapping rather than a hand-maintained copy. Process-level concerns
    /// (daemonize, pidfile, logfile, CPU affinity) stay in `main`; this is pure
    /// arg → builder → `build()`.
    pub fn build_client(
        &self,
        auth_password: Option<&str>,
    ) -> std::result::Result<riperf3::Client, Box<dyn std::error::Error>> {
        let host = self
            .client
            .as_deref()
            .ok_or("client mode requires a host (-c <host>)")?;

        if self.end_conditions_conflict() {
            return Err(END_CONDITIONS_MSG.into());
        }

        // #294: the CLI is the faithful-output surface, so it opts INTO the
        // full iperf3 text/JSON output explicitly. The library default is now
        // quiet (a bare `run()` returns the Report and prints nothing).
        let mut builder = riperf3::ClientBuilder::new(host).emit_output(true);

        // Format (#241): case-sensitive [kmgtKMGT], uppercase = byte-rates;
        // absent → the library's adaptive default ('a'), like iperf3 (#221).
        if let Some(c) = self.format.as_deref().and_then(parse_format_char) {
            builder = builder.format_char(c);
        }

        if let Some(port) = self.port {
            // 1..=65535 enforced pre-sink in main.rs (IEBADPORT, #328);
            // saturating glue for direct callers, like duration below.
            builder = builder.port(Some(u16::try_from(port).unwrap_or(u16::MAX)));
        }
        if self.udp {
            builder = builder.protocol(riperf3::TransportProtocol::Udp);
        }
        if let Some(t) = self.time {
            builder = builder.duration(u32::try_from(t).unwrap_or(u32::MAX));
        }
        if let Some(ref s) = self.pacing_timer {
            // #328: GT stores unit_atoi through an int (iperf_api.c:1780) —
            // `3G` wraps negative and GT proceeds, pacing effectively
            // continuously. RECORDED DEVIATION: the u32 builder cannot hold
            // negatives, so wrapped/zero values keep riperf3's default
            // pacing instead (the flag is deprecated; sane inputs match).
            let v = c_u64_to_int(c_double_to_u64(unit_atoi_os(s)?));
            if v > 0 {
                builder = builder.pacing_timer(v as u32);
            }
        }
        if let Some(ref s) = self.bytes {
            // #328: GT's `settings->bytes = unit_atoi(optarg)`
            // (iperf_api.c:1394) — an iperf_size_t (uint64), so negatives
            // wrap huge and RUN (live-probed `-n -5`); 0 means "not set"
            // (the end-condition legs test the value).
            builder = builder.bytes(c_double_to_u64(unit_atoi_os(s)?));
        }
        if let Some(ref s) = self.blockcount {
            // #328: unit_atoi like -n (iperf_api.c:1401).
            builder = builder.blocks(c_double_to_u64(unit_atoi_os(s)?));
        }
        if let Some(ref s) = self.length {
            // #328: GT's `blksize = unit_atoi(optarg)` through an int
            // (iperf_api.c:1408); 0 keeps the protocol default
            // (:1926-1933). Ranges are enforced pre-sink in main.rs with
            // GT's IEBLOCKSIZE/IEUDPBLOCKSIZE sentences; the guard here
            // covers direct callers (negatives cannot become a usize).
            let v = c_u64_to_int(c_double_to_u64(unit_atoi_os(s)?));
            match v.cmp(&0) {
                std::cmp::Ordering::Greater => builder = builder.blksize(v as usize),
                std::cmp::Ordering::Equal => {}
                std::cmp::Ordering::Less => {
                    return Err("block size too large (maximum = 1048576 bytes)".into())
                }
            }
        }
        if let Some(n) = self.parallel {
            // RECORDED DEVIATION (#328): GT's num_streams is a plain int with
            // no lower bound at parse — live-probed, `-P 0` runs an
            // instantly-complete 0-stream test while `-P -1` WEDGES (the test
            // never finishes). riperf3's builder takes u32, so negatives fold
            // to 0 — the 0-stream instant-complete behavior — rather than
            // reproducing GT's hang.
            builder = builder.num_streams(u32::try_from(n).unwrap_or(0));
        }
        if self.reverse {
            builder = builder.reverse(true);
        }
        if self.bidir {
            builder = builder.bidir(true);
        }
        if let Some(ref s) = self.window {
            // #334: GT's -w is unit_atof (1024-based) → `(int) farg`
            // (iperf_api.c:1438-1452). The IEUNITVAL parse error and the
            // `> MAX_TCP_BUFFER` IEBUFSIZE check run pre-sink in main.rs; this
            // maps the accepted double to the socket_bufsize int. GT's cast is
            // a DIRECT double→int32 (cvttsd2si), so `farg as i32` matches it
            // exactly on every reachable value: `-w -5` → -5, and an
            // out-of-i32-range negative (`-w -3G`) saturates to i32::MIN on
            // both (GT's cvttsd2si also yields INT_MIN); positive overflow is
            // unreachable (IEBUFSIZE catches anything > MAX_TCP_BUFFER first).
            // RECORDED DEVIATION: the sole literal that diverges is `-w nan`
            // — `nan as i32` = 0, GT's `(int)nan` = INT_MIN. Both tools DO
            // connect and complete (#432 r2 probe): GT renders
            // `sock_bufsize: -2147483648` with the negative applied (the
            // kernel max-clamps it), riperf3 renders 0 and runs fully unset
            // (#415 normalizes the 0 to kernel autotuning). Observable in
            // `-J`, tolerated: a NaN-cast literal is not worth a
            // special-case for an absurd input.
            let farg = unit_atoi_os(s)?;
            builder = builder.window(farg as i32);
        }
        if let Some(ref algo) = self.congestion {
            builder = builder.congestion(algo);
        }
        if let Some(mss) = self.mss {
            // atoi_like output always fits i32 (the (int) truncation); the
            // MAX_MSS upper bound is enforced pre-sink in main.rs (#328).
            builder = builder.mss(mss as i32);
        }
        if self.no_delay {
            builder = builder.no_delay(true);
        }
        if let Some(ref s) = self.bitrate {
            // #334: GT's -b `rate[/burst]` (iperf_api.c:1347-1365) — burst =
            // atoi(after the FIRST '/') then rate = unit_atof_rate(before),
            // 1000-based. The IEBURST/IEUNITVAL surface is enforced pre-sink
            // in main.rs; here we wire the resolved rate (iperf_size_t) and
            // burst. A negative rate wraps huge via c_double_to_u64 like GT.
            let (rate, burst) = split_rate_interval(s);
            let bps = unit_atof_rate_like_bytes(rate).map_err(|()| {
                format!(
                    "invalid unit value or suffix: '{}'",
                    String::from_utf8_lossy(rate)
                )
            })?;
            builder = builder.bandwidth(c_double_to_u64(bps));
            if let Some(burst) = burst {
                // The IEBURST range (1..=1000) is enforced pre-sink; a direct
                // caller's out-of-range burst folds through u32, and the lib's
                // build() re-checks `> MAX_BURST` for those callers.
                let n = atoi_like_bytes(burst);
                builder = builder.burst(u32::try_from(n).unwrap_or(0));
            }
        }
        if let Some(ref s) = self.tos {
            builder = builder.tos_str(s)?;
        }
        if let Some(o) = self.omit {
            builder = builder.omit(u32::try_from(o).unwrap_or(u32::MAX));
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
        if let Some(ref s) = self.connect_timeout {
            // #328: GT stores unit_atoi through an int (iperf_api.c:1787)
            // and hands it to netdial → timeout_connect (net.c:272-289),
            // where poll(2) treats a NEGATIVE timeout as infinite — so
            // negatives (incl. 3G's int wrap) mean "no timeout" and are
            // skipped here; 0 keeps riperf3's existing zero-duration
            // semantics.
            let v = c_u64_to_int(c_double_to_u64(unit_atoi_os(s)?));
            if v >= 0 {
                builder = builder.connect_timeout(std::time::Duration::from_millis(v as u64));
            }
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
        if self.json_stream_full_output {
            builder = builder.json_stream_full_output(true);
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
            // 1..=65535 enforced pre-sink in main.rs (IEBADPORT, #328).
            builder = builder.cport(u16::try_from(port).unwrap_or(u16::MAX));
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
            // #334: GT's --fq-rate is unit_atof_rate (1000-based),
            // iperf_api.c:1728 → IEUNITVAL pre-sink in main.rs; wire the
            // resolved rate (iperf_size_t). A negative wraps huge like GT.
            let bps = unit_atof_rate_like_bytes(s.as_encoded_bytes())
                .map_err(|()| format!("invalid unit value or suffix: '{}'", s.to_string_lossy()))?;
            builder = builder.fq_rate(c_double_to_u64(bps));
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
            // IERCVTIMEOUT range (100..=86_400_000) enforced pre-sink in
            // main.rs (#328); negatives cannot reach here via the binary.
            builder = builder.rcv_timeout(u64::try_from(ms).unwrap_or(0));
        }
        if let Some(ms) = self.snd_timeout {
            // IESNDTIMEOUT range (0..=86_400_000) enforced pre-sink (#328).
            builder = builder.snd_timeout(u64::try_from(ms).unwrap_or(0));
        }
        if let Some(ref path) = self.file {
            builder = builder.file(path);
        }
        if let Some(ref val) = self.dscp {
            builder = builder.dscp(val);
        }
        if let Some(ref spec) = self.cntl_ka {
            // #328: the IECNTLKA sanity check runs pre-sink in main.rs; the
            // lib takes the spec string (empty = bare --cntl-ka = keepalive
            // with defaults, like GT's optional_argument). Lossy is safe:
            // invalid-UTF-8 pieces atoi to 0 either way.
            builder = builder.cntl_ka(&spec.to_string_lossy());
        }
        if let Some(ref name) = self.username {
            builder = builder.username(name);
        }
        // #395: the password is resolved at PARSE time (GT's getpass slot in
        // the argument post-loop) and handed in here, so the lib's own
        // runtime prompt fallback never fires from the CLI path.
        if let Some(pw) = auth_password {
            builder = builder.password(pw);
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
        // #294: the CLI opts into iperf3's full output; the library default is
        // now quiet (see build_client).
        let mut builder = riperf3::ServerBuilder::new().emit_output(true);

        // Format (#242): the server-side -f was silently dropped — never
        // wired through the builder, every render site hardcoded adaptive.
        // Same case-sensitive mapping as the client (#241).
        if let Some(c) = self.format.as_deref().and_then(parse_format_char) {
            builder = builder.format_char(c);
        }
        if let Some(port) = self.port {
            // 1..=65535 enforced pre-sink in main.rs (IEBADPORT, #328).
            builder = builder.port(Some(u16::try_from(port).unwrap_or(u16::MAX)));
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
        if self.json_stream_full_output {
            builder = builder.json_stream_full_output(true);
        }
        if let Some(secs) = self.idle_timeout {
            builder = builder.idle_timeout(u32::try_from(secs).unwrap_or(u32::MAX));
        }
        if let Some(ref s) = self.server_bitrate_limit {
            // #328: GT's `bitrate_limit = unit_atof_rate(rate_part)` — an
            // iperf_size_t (uint64), so a negative rate wraps huge and GT
            // proceeds (iperf_api.c:1379-1383). The IETOTALINTERVAL check
            // on the interval part runs pre-sink in main.rs.
            let (rate, interval) = split_rate_interval(s);
            let n = unit_atof_rate_like_bytes(rate).map_err(|()| {
                format!(
                    "invalid unit value or suffix: '{}'",
                    String::from_utf8_lossy(rate)
                )
            })?;
            builder = builder.server_bitrate_limit(c_double_to_u64(n));
            // #410: the `/N` averaging window rides into the lib (GT's
            // bitrate_limit_interval, iperf_api.c:1371 — un-recording the
            // old "validated but not wired" deviation). RECORDED DEVIATION
            // (`/0` only, r1 F5 live probe): GT's interval=0 leaves
            // per_interval at 0 → 0.0/0.0 = NaN → uint64 conversion UB —
            // on x86-64 that reads 2^63 bps and GT SELF-TERMINATES every
            // test at the FIRST tick (aarch64's NaN→0 would never breach).
            // Platform-divergent UB is not mirrorable; riperf3 reads 0 as
            // the default 5 s window.
            if let Some(iv) = interval.map(atof_like_bytes) {
                if iv != 0.0 {
                    builder = builder.server_bitrate_limit_interval(iv);
                }
            }
        }
        if let Some(secs) = self.server_max_duration {
            builder = builder.server_max_duration(u32::try_from(secs).unwrap_or(u32::MAX));
        }
        if let Some(ms) = self.rcv_timeout {
            // #338: the server half of --rcv-timeout (GT uses it as the
            // no-progress bound on both roles). IERCVTIMEOUT range enforced
            // pre-sink in main.rs (#328).
            builder = builder.rcv_timeout(u64::try_from(ms).unwrap_or(0));
        }
        if self.forceflush {
            builder = builder.forceflush(true);
        }
        if let Some(ref addr) = self.bind {
            builder = builder.bind_address(addr);
        }
        if let Some(ref dev) = self.bind_dev {
            // iperf3's server applies --bind-dev in netannounce (#149).
            builder = builder.bind_dev(dev);
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
            // IESKEWTHRESHOLD (> 0) enforced pre-sink in main.rs (#328);
            // atoi_like output fits u32 once positive.
            builder = builder.time_skew_threshold(u32::try_from(secs).unwrap_or(0));
        }
        if self.use_pkcs1_padding {
            builder = builder.use_pkcs1_padding(true);
        }

        Ok(builder.build()?)
    }
}

/// #317/#328: GT parses its integer flags with `atoi` — mirror it so the
/// drop-in accepts what GT accepts: leading whitespace skipped, optional
/// sign, leading digits (0 when none — `abc`, `-abc`), and C's overflow
/// shape (`strtol` saturates at LONG_MAX, then the `(int)` cast truncates:
/// 2^32 → 0 runs, 2^31 → INT_MIN → the range error). Never a parse error.
///
/// Works on raw bytes (#328 issue comment): C atoi never converts to UTF-8,
/// so an invalid-UTF-8 argv byte like 0xA0 is simply "garbage" → 0 and GT
/// proceeds (live-probed: `iperf3 -P $'\xa0'` runs a 0-stream test). The
/// C-locale whitespace/sign/digit sets are all ASCII, so byte-wise scanning
/// is exact.
pub fn atoi_like_bytes(s: &[u8]) -> i64 {
    // C-locale isspace only (GT never calls setlocale): Rust's trim_start
    // would also eat NBSP/NEL/U+2028/U+3000, which glibc atoi does not
    // (#317 r1 F2 — live: GT rejects `--idle-timeout <NBSP>5`, we accepted).
    let mut t = s;
    while let [b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r', rest @ ..] = t {
        t = rest;
    }
    let (neg, rest) = match t {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, t),
    };
    let end = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let digits = std::str::from_utf8(&rest[..end]).expect("ASCII digits");
    if digits.is_empty() {
        return 0;
    }
    // strtol saturates DIRECTIONALLY (#317 r1 F1: magnitude-then-negate gave
    // LONG_MIN+1 — (int) 1 — where C parses -2^63 exactly and (int)s to 0).
    let long = if neg {
        format!("-{digits}").parse::<i64>().unwrap_or(i64::MIN)
    } else {
        digits.parse::<i64>().unwrap_or(i64::MAX)
    };
    i64::from(long as i32) // the (int) truncation
}

/// A clap value parser applying [`atoi_like_bytes`] at the `OsString` level,
/// so invalid-UTF-8 argv bytes parse like GT's C atoi instead of dying at
/// clap's UTF-8 conversion (#328).
fn atoi_like_os() -> impl clap::builder::TypedValueParser<Value = i64> {
    use clap::builder::TypedValueParser as _;
    clap::builder::OsStringValueParser::new()
        .map(|s: std::ffi::OsString| atoi_like_bytes(s.as_encoded_bytes()))
}

/// #328: the longest C-double prefix of `s`, consumed the way GT's
/// `sscanf(s, "%lf%c", ...)` (units.c:196) reads it: C-locale leading
/// whitespace, optional sign, then a decimal mantissa with optional
/// fraction/e-exponent, a strtod 0x hex mantissa with optional p-exponent,
/// or inf/infinity/nan. Returns the value and the byte index just past the
/// number (where `%c` reads the suffix); `None` on a matching failure.
///
/// scanf can't push back more than one byte, so it FAILS outright where
/// strtod would back up (`1e`, `0x`); modeling with strtod's backed-up
/// prefix instead is outcome-identical for unit_atoi, because the leftover
/// byte at the prefix boundary ('e', 'x', '.') is never in `[tTgGmMkK]` —
/// both roads end at IEUNITVAL (live-probed: `-n 1e`, `-n 1ex`, `-n 0x`
/// all reject; `-n 0x10` parses as 16).
fn c_strtod_prefix(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    while matches!(
        s.get(i),
        Some(b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
    ) {
        i += 1;
    }
    let sign_start = i;
    let neg = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let rest = &s[i..];
    let starts = |p: &[u8]| rest.len() >= p.len() && rest[..p.len()].eq_ignore_ascii_case(p);
    // strtod's special forms (C99 7.20.1.3), case-insensitive.
    if starts(b"infinity") {
        let v = if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Some((v, i + 8));
    }
    if starts(b"inf") {
        let v = if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Some((v, i + 3));
    }
    if starts(b"nan") {
        return Some((f64::NAN, i + 3));
    }
    // strtod hex floats: 0x hexdigits [. hexdigits] [pP [sign] digits].
    if rest.len() >= 2 && rest[0] == b'0' && (rest[1] | 0x20) == b'x' {
        let mut j = 2;
        let mut mant = 0f64;
        let mut any = false;
        while let Some(d) = rest.get(j).and_then(|b| (*b as char).to_digit(16)) {
            mant = mant * 16.0 + f64::from(d);
            any = true;
            j += 1;
        }
        if rest.get(j) == Some(&b'.') {
            let mut scale = 1.0 / 16.0;
            let mut k = j + 1;
            while let Some(d) = rest.get(k).and_then(|b| (*b as char).to_digit(16)) {
                mant += f64::from(d) * scale;
                scale /= 16.0;
                any = true;
                k += 1;
            }
            if k > j + 1 {
                j = k;
            }
        }
        if !any {
            // "0x<junk>": strtod backs up to the plain "0".
            return Some((0.0, i + 1));
        }
        if matches!(rest.get(j), Some(b'p' | b'P')) {
            let mut k = j + 1;
            let eneg = match rest.get(k) {
                Some(b'-') => {
                    k += 1;
                    true
                }
                Some(b'+') => {
                    k += 1;
                    false
                }
                _ => false,
            };
            let estart = k;
            let mut e = 0i32;
            while let Some(d) = rest.get(k).and_then(|b| (*b as char).to_digit(10)) {
                e = e.saturating_mul(10).saturating_add(d as i32);
                k += 1;
            }
            if k > estart {
                mant *= 2f64.powi(if eneg { -e } else { e });
                j = k;
            }
        }
        return Some((if neg { -mant } else { mant }, i + j));
    }
    // Decimal: digits [. digits] [eE [sign] digits].
    let mut j = 0;
    let mut any = false;
    while rest.get(j).is_some_and(u8::is_ascii_digit) {
        any = true;
        j += 1;
    }
    if rest.get(j) == Some(&b'.') {
        j += 1;
        while rest.get(j).is_some_and(u8::is_ascii_digit) {
            any = true;
            j += 1;
        }
    }
    if !any {
        return None;
    }
    let mant_end = j;
    if matches!(rest.get(j), Some(b'e' | b'E')) {
        let mut k = j + 1;
        if matches!(rest.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let estart = k;
        while rest.get(k).is_some_and(u8::is_ascii_digit) {
            k += 1;
        }
        j = if k > estart { k } else { mant_end };
    }
    let text = std::str::from_utf8(&s[sign_start..i + j]).expect("ASCII number");
    Some((text.parse::<f64>().expect("valid double prefix"), i + j))
}

/// The shared suffix step of GT's unit parsers: the number's C-double
/// prefix, then AT MOST ONE suffix char in `[tTgGmMkK]` scaling by base^n;
/// end-of-string means no scaling; junk AFTER a valid suffix is IGNORED
/// (`10Kx` is 10240: sscanf never reads the x); any OTHER byte right after
/// the number — or an unparseable number — is Err (IEUNITVAL).
fn unit_suffix_scaled(s: &[u8], base: f64) -> Result<f64, ()> {
    let (n, consumed) = c_strtod_prefix(s).ok_or(())?;
    match s.get(consumed) {
        None => Ok(n),
        Some(b't' | b'T') => Ok(n * base.powi(4)),
        Some(b'g' | b'G') => Ok(n * base.powi(3)),
        Some(b'm' | b'M') => Ok(n * base.powi(2)),
        Some(b'k' | b'K') => Ok(n * base),
        Some(_) => Err(()),
    }
}

/// #328: GT's `unit_atoi` (units.c:190-227, 1024-based) minus the final
/// integer cast — the scaled double, so each call site can apply ITS C
/// target-type conversion (`iperf_size_t` for -n/-k, `int` for
/// -l/--pacing-timer/--connect-timeout).
pub fn unit_atoi_like_bytes(s: &[u8]) -> Result<f64, ()> {
    unit_suffix_scaled(s, 1024.0)
}

/// #328: GT's `unit_atof_rate` (units.c:136-173) — the RATE sibling with
/// 1000-based suffixes (--server-bitrate-limit's rate part).
pub fn unit_atof_rate_like_bytes(s: &[u8]) -> Result<f64, ()> {
    unit_suffix_scaled(s, 1000.0)
}

/// #328: C `atof` — strtod's longest-prefix value, garbage → 0.0 (GT's -i
/// at iperf_api.c:1260 and --server-bitrate-limit's interval at :1372).
pub fn atof_like_bytes(s: &[u8]) -> f64 {
    c_strtod_prefix(s).map_or(0.0, |(n, _)| n)
}

/// A clap value parser applying [`atof_like_bytes`] at the `OsString` level
/// (#328): raw invalid-UTF-8 argv bytes are strtod garbage → 0.0, like GT.
fn atof_like_os() -> impl clap::builder::TypedValueParser<Value = f64> {
    use clap::builder::TypedValueParser as _;
    clap::builder::OsStringValueParser::new()
        .map(|s: std::ffi::OsString| atof_like_bytes(s.as_encoded_bytes()))
}

/// #328: GT's -d/--debug level parse (iperf_api.c:1692-1697): C atoi, and a
/// NEGATIVE level means DEBUG_LEVEL_MAX (4, iperf.h:300). No upper clamp —
/// `--debug=100` is level 100 in GT too.
fn debug_level_like_os() -> impl clap::builder::TypedValueParser<Value = i64> {
    use clap::builder::TypedValueParser as _;
    clap::builder::OsStringValueParser::new().map(|s: std::ffi::OsString| {
        let v = atoi_like_bytes(s.as_encoded_bytes());
        if v < 0 {
            4
        } else {
            v
        }
    })
}

/// #328: --server-bitrate-limit's `rate[/interval]` split
/// (iperf_api.c:1366-1385): GT strchr's the FIRST '/', atof's everything
/// after it as the averaging interval, and unit_atof_rate's the part
/// before it.
pub fn split_rate_interval(spec: &std::ffi::OsStr) -> (&[u8], Option<&[u8]>) {
    let b = spec.as_encoded_bytes();
    match b.iter().position(|&c| c == b'/') {
        Some(i) => (&b[..i], Some(&b[i + 1..])),
        None => (b, None),
    }
}

/// #328: GT's --cntl-ka sanity check (iperf_api.c:1626-1653): the optarg
/// is slash-separated `keepidle[/interval[/count]]`, each non-empty piece
/// C atoi (empty pieces keep the 0 defaults, iperf_api.c:3311-3313; the
/// third piece's atoi absorbs any further slashes as trailing junk), then
/// `keepidle != 0 && keepidle <= count * interval` → IECNTLKA. The product
/// is C int arithmetic (wrapping).
pub fn cntl_ka_violation(spec: &std::ffi::OsStr) -> bool {
    let b = spec.as_encoded_bytes();
    let (p0, rest) = match b.iter().position(|&c| c == b'/') {
        Some(i) => (&b[..i], Some(&b[i + 1..])),
        None => (b, None),
    };
    let (p1, p2) = match rest {
        None => (None, None),
        Some(r) => match r.iter().position(|&c| c == b'/') {
            Some(i) => (Some(&r[..i]), Some(&r[i + 1..])),
            None => (Some(r), None),
        },
    };
    let piece = |p: Option<&[u8]>| -> i32 {
        p.filter(|p| !p.is_empty())
            .map_or(0, |p| atoi_like_bytes(p) as i32)
    };
    let keepidle = piece(Some(p0));
    let interval = piece(p1);
    let count = piece(p2);
    // C signed-int overflow in `count * interval` is UB; wrapping_mul pins
    // the observed GT binary's two's-complement wrap deterministically,
    // like c_double_to_u64 does for its conversion UB.
    keepidle != 0 && keepidle <= count.wrapping_mul(interval)
}

/// #328: C's `(iperf_size_t)` — i.e. `(uint64_t)` — conversion of a double,
/// as gcc emits it on x86-64 (the GT reference build): values below 2^63 go
/// through cvttsd2si (truncate toward zero; unrepresentable → INT64_MIN,
/// reinterpreted unsigned — so `-n -5` becomes a huge byte target, which GT
/// RUNS, live-probed); values at/above 2^63 are converted with the top bit
/// folded, so 2^63..2^64 map exactly and +inf/1e300 land on 0. NaN → 2^63.
/// (The C conversion is UB for negatives/overflow; this pins the observed
/// GT binary's arithmetic deterministically on every platform.)
pub fn c_double_to_u64(n: f64) -> u64 {
    const TWO63: f64 = 9_223_372_036_854_775_808.0;
    if n.is_nan() {
        return 1 << 63;
    }
    if n < TWO63 {
        // Rust's saturating `as i64` matches cvttsd2si's INT64_MIN floor.
        (n as i64) as u64
    } else {
        let d = n - TWO63;
        let i = if d < TWO63 { d as i64 } else { i64::MIN };
        (i as u64) ^ (1 << 63)
    }
}

/// #328: C's `int` conversion of the `iperf_size_t` unit_atoi result (the
/// `-l`/`--pacing-timer`/`--connect-timeout` assignments): uint64 → int32
/// keeps the low 32 bits, reinterpreted signed — `-l 3G` wraps negative,
/// exactly the value GT's post-loop range checks then see.
pub fn c_u64_to_int(v: u64) -> i32 {
    v as u32 as i32
}

/// unit_atoi over an argv value, erring with IEUNITVAL's sentence (lossy
/// rendering — the binary's pre-sink path in main.rs writes the RAW bytes
/// instead; this face serves direct `build_client`/`build_server` callers).
fn unit_atoi_os(arg: &std::ffi::OsStr) -> Result<f64, String> {
    unit_atoi_like_bytes(arg.as_encoded_bytes())
        .map_err(|()| format!("invalid unit value or suffix: '{}'", arg.to_string_lossy()))
}

/// #263: GT's `-f` parse (iperf_api.c:1236-1256) reads `*optarg` — the
/// FIRST character only, so `-f kilobits` is `-f k` — and accepts exactly
/// `[kmgtKMGT]`; anything else is IEBADFORMAT. 'B'/'b' stay lib-only in both
/// tools (GT's getopt rejects them; unit_snprintf supports them).
pub fn parse_format_char(arg: &str) -> Option<char> {
    arg.chars().next().filter(|c| "kmgtKMGT".contains(*c))
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
            // #221: no -f means ADAPTIVE (None) — iperf3 has no default format.
            assert_eq!(cli.format, None);
            assert_eq!(cli.interval, None);
            assert!(!cli.verbose);
            assert_eq!(cli.debug, None);
            assert_eq!(cli.version, None);

            let cli = Cli::parse_from(["riperf3", "--client", "localhost"]);
            assert!(!cli.server);
            assert_eq!(cli.client, Some("localhost".to_string()));
            assert_eq!(cli.port, None);
            // #221: no -f means ADAPTIVE (None) — iperf3 has no default format.
            assert_eq!(cli.format, None);
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
            // #241: case is semantic — all 8 of iperf3's [kmgtKMGT] parse on
            // both roles; #263: the raw string is kept (GT's *optarg rule
            // applies at validation, not parse).
            for letter in ["k", "m", "g", "t", "K", "M", "G", "T"] {
                let cli = Cli::parse_from(["riperf3", "--server", "--format", letter]);
                assert_eq!(cli.format.as_deref(), Some(letter), "-f {letter} (server)");
                let cli = Cli::parse_from(["riperf3", "--client", "localhost", "--format", letter]);
                assert_eq!(cli.format.as_deref(), Some(letter), "-f {letter} (client)");
            }
        }

        #[test]
        fn atoi_like_matches_glibc_atoi() {
            // #317 r1 F3: exact-value table pinned against real glibc atoi
            // (the r1 differential harness), incl. the two r1 mismatch
            // classes: DIRECTIONAL strtol saturation (-2^63 parses exactly,
            // (int)-truncates to 0 — magnitude-negate gave 1) and C-locale
            // isspace (NBSP must NOT be trimmed).
            for (input, want) in [
                ("", 0),
                ("+", 0),
                ("-", 0),
                ("+-5", 0),
                ("  -12x", -12),
                ("\t-12x", -12),
                ("0x10", 0),
                ("abc", 0),
                ("-abc", 0),
                ("1e3", 1),
                ("5x", 5),
                ("+5", 5),
                ("007", 7),
                (" 7", 7),
                ("9999999999999999999999", -1),
                ("9223372036854775807", -1),
                ("-9223372036854775808", 0),
                ("-9223372036854775809", 0),
                ("2147483647", 2147483647),
                ("-2147483648", -2147483648),
                ("2147483648", -2147483648),
                ("-2147483649", 2147483647),
                ("4294967295", -1),
                ("4294967296", 0),
                ("6442450941", 2147483645),
                ("\u{00a0}5", 0), // NBSP is not C-locale isspace
                ("\u{3000}5", 0),
                ("\x0b5", 5),
                ("\r5", 5),
                ("--bidir", 0),
                ("-R", 0),
            ] {
                assert_eq!(
                    atoi_like_bytes(input.as_bytes()),
                    want,
                    "atoi_like({input:?})"
                );
            }
        }

        #[test]
        fn atoi_like_bytes_handles_raw_non_utf8() {
            // #328 (issue comment): C atoi never converts to UTF-8 — raw
            // invalid bytes are garbage (0) or trailing junk, exactly like
            // any other non-digit (live-probed: GT runs `-P $'\xa0'` as 0
            // streams). The C-locale space/sign/digit sets are pure ASCII.
            for (input, want) in [
                (&b"\xa0"[..], 0),
                (&b"\xa05"[..], 0),
                (&b" \xff"[..], 0),
                (&b"5\xa0"[..], 5),
                (&b"-12\xff"[..], -12),
                (&b"\t-12\xff"[..], -12),
                (&b"+\xa0"[..], 0),
            ] {
                assert_eq!(atoi_like_bytes(input), want, "atoi_like_bytes({input:?})");
            }
        }

        #[test]
        fn unit_atoi_like_matches_units_c() {
            // #328: exact-value table pinned from units.c:190-227 + live GT
            // probes (2026-07-03), with the (uint64) conversion applied like
            // the -n/-k call sites. Suffix rules: ONE char, [tTgGmMkK],
            // 1024-based; junk AFTER a valid suffix is ignored (sscanf never
            // reads past %c); junk INSTEAD of a suffix — or an unparseable
            // number — is IEUNITVAL (the Err rows).
            let ok: &[(&[u8], u64)] = &[
                (b"10", 10),
                (b"0", 0),
                (b"10K", 10240),
                (b"10k", 10240),
                (b"10Kx", 10240), // live: GT runs -n 10Kx
                (b"10KK", 10240), // the second K is junk AFTER the suffix
                (b"1.5K", 1536),
                (b"1M", 1 << 20),
                (b"1G", 1 << 30),
                (b"1T", 1 << 40),
                (b".5m", 524_288), // live: GT runs -n .5m
                (b"1e3", 1000),    // live: GT runs -n 1e3
                (b"1.5e2K", 153_600),
                (b"0x10", 16), // strtod hex — live: GT runs -n 0x10
                (b"0x1p3", 8), // strtod hex p-exponent
                (b" 10K", 10240),
                (b"\t7", 7),
                (b"007", 7),
                (b"+5", 5),
                // The C (uint64) conversion edges (the x86-64 GT build):
                (b"-5", u64::MAX - 4), // live: GT runs -n -5 as a huge target
                (b"-1K", u64::MAX - 1023),
                (b"1e300", 0),                     // overflow folds to 0
                (b"inf", 0),                       // (uint64)inf is 0 too
                (b"nan", 1 << 63),                 // live: GT runs -n nan
                (b"9223372036854775808", 1 << 63), // 2^63 maps exactly
                (b"8388608T", 1 << 63),            // 2^23 * 1024^4 = 2^63
                (b"18446744073709551616", 0),      // 2^64 overflows to 0
            ];
            for (input, want) in ok {
                assert_eq!(
                    unit_atoi_like_bytes(input).map(c_double_to_u64),
                    Ok(*want),
                    "unit_atoi({input:?})"
                );
            }
            let err: &[&[u8]] = &[
                b"10x",  // live: IEUNITVAL '10x'
                b"abc",  // live: IEUNITVAL 'abc'
                b"1e",   // live: IEUNITVAL (scanf %lf fails outright)
                b"1ex",  // live: IEUNITVAL (prefix "1", suffix 'e')
                b"",     // live: IEUNITVAL ''
                b".",    // live: IEUNITVAL '.'
                b"0x",   // live: IEUNITVAL (strtod backs up to "0", 'x' left)
                b"10 K", // live: IEUNITVAL (%c reads the space, not the K)
                b"+", b"-", b"\xa0", // raw invalid UTF-8 is garbage like any other byte
            ];
            for input in err {
                assert_eq!(
                    unit_atoi_like_bytes(input),
                    Err(()),
                    "unit_atoi({input:?}) is IEUNITVAL"
                );
            }
        }

        #[test]
        fn c_u64_to_int_wraps_like_c() {
            // #328: the -l/--pacing-timer/--connect-timeout assignments run
            // unit_atoi's uint64 through a C int — low 32 bits, signed.
            for (input, want) in [
                (1024u64, 1024i32),
                (3_221_225_472, -1_073_741_824), // 3G wraps negative
                (2_147_483_648, i32::MIN),       // 2^31
                (u64::MAX - 4, -5),              // -n -5's wrap round-trips
                (1 << 32, 0),
            ] {
                assert_eq!(c_u64_to_int(input), want, "(int)({input})");
            }
        }

        #[test]
        fn test_format_chars_match_unit_snprintf() {
            // The CLI→lib glue: each accepted letter maps to the exact
            // unit_snprintf char; uppercase survives (the old ignore_case +
            // 4-variant enum collapsed K to 'k'). #263: only the FIRST char
            // counts (GT's *optarg), and non-[kmgtKMGT] leads are rejected —
            // including lib-only 'b'/'B'.
            for (arg, ch) in [
                ("k", 'k'),
                ("m", 'm'),
                ("g", 'g'),
                ("t", 't'),
                ("K", 'K'),
                ("M", 'M'),
                ("G", 'G'),
                ("T", 'T'),
                ("kilobits", 'k'),
                ("MBytes", 'M'),
            ] {
                assert_eq!(parse_format_char(arg), Some(ch), "-f {arg}");
            }
            for bad in ["x", "b", "B", "a", "A", "", "9k"] {
                assert_eq!(parse_format_char(bad), None, "-f {bad} is IEBADFORMAT");
            }
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
            assert_eq!(cli.length, Some("1460".into()));
            assert_eq!(cli.bitrate, Some("100M".into()));
        }

        #[test]
        fn test_client_bytes_and_blocks() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-n", "1G"]);
            assert_eq!(cli.bytes, Some("1G".into()));

            let cli = Cli::parse_from(["riperf3", "-c", "host", "-k", "100K"]);
            assert_eq!(cli.blockcount, Some("100K".into()));
        }

        #[test]
        fn test_client_window_mss_congestion() {
            let cli = Cli::parse_from([
                "riperf3", "-c", "host", "-w", "512K", "-M", "1400", "-C", "bbr",
            ]);
            assert_eq!(cli.window, Some("512K".into()));
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

            // #395 r1 F2: `--authorized-users-path` is NOT in the generic
            // set — its getopt case never sets GT's server_flag
            // (iperf_api.c:1757-1759); the post-loop :1874 slot handles it
            // in `parse_class_rejection`, AFTER the client-auth legs (the
            // ordering pin lives in error_format.rs).
            let cli = Cli::parse_from(["riperf3", "-c", "host", "--authorized-users-path", "/f"]);
            assert_eq!(
                cli.first_server_only_violation(),
                None,
                "--authorized-users-path is the late leg, not the generic set"
            );
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
            cli.build_client(None).unwrap()
        }

        /// Server counterpart of [`build_client_from_cli`] — exercises the real
        /// `Cli::build_server` mapping (#124).
        fn build_server_from_cli(cli: &Cli) -> riperf3::Server {
            cli.build_server().unwrap()
        }

        /// A `ClientBuilder` matching the CLI's no-flag defaults. Since #221
        /// the CLI no longer forces a format: absent -f, the library's
        /// adaptive default ('a') stands, like iperf3. #294: the CLI opts into
        /// output (the library default is now quiet), so the expected builder
        /// carries `emit_output(true)` too.
        fn expected_client(host: &str) -> riperf3::ClientBuilder {
            riperf3::ClientBuilder::new(host).emit_output(true)
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

        // #328: the atoi set parses suffixed garbage to the leading digits
        // and wires the SAME value the plain form would (GT: atoi).
        #[test]
        fn atoi_suffixed_values_wire_like_plain() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-M", "1400x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").mss(1400).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-P", "5x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").num_streams(5).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cport", "12345x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").cport(12345).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-R", "--rcv-timeout", "5000x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h")
                    .reverse(true)
                    .rcv_timeout(5000)
                    .build()
                    .unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--snd-timeout", "3000x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").snd_timeout(3000).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-p", "5201x"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").port(Some(5201)).build().unwrap()
            );
        }

        // #328 RECORDED DEVIATION: GT's -P has no lower bound at parse
        // (live-probed: `-P 0` completes an empty test instantly; `-P -1`
        // wedges). riperf3's u32 builder folds negatives to 0 — the
        // instant-empty-test behavior — instead of reproducing the hang.
        #[test]
        fn parallel_negative_folds_to_zero_streams() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-P", "-1"]);
            assert_eq!(cli.parallel, Some(-1), "atoi keeps the negative");
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").num_streams(0).build().unwrap()
            );
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

        // #167: -S takes iperf3's strtol-base-0 forms (hex/octal), and bad
        // values fail the build with IEBADTOS wording instead of parsing.
        #[test]
        fn tos_flag_accepts_hex_and_octal() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-S", "0x20"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").tos(0x20).build().unwrap());
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-S", "020"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("host").tos(16).build().unwrap());
        }

        #[test]
        fn tos_flag_rejects_out_of_range() {
            let cli = Cli::parse_from(["riperf3", "-c", "host", "-S", "256"]);
            let err = cli.build_client(None).unwrap_err().to_string();
            assert!(err.contains("bad TOS value"), "got: {err}");
        }

        // #205: repeated flags are last-wins, like iperf3's getopt — wrapper
        // scripts append override flags to a base line. Pins the value args,
        // the previously-rejected repeated BOOLS, and the value-optional
        // reset-to-default (C's bare --timestamps else-leg explicitly resets
        // to "%c " — iperf_api.c:1566-1573) (review r1 n8).
        #[test]
        fn repeated_flags_are_last_wins() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-b", "0", "-b", "100M"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .bandwidth_str("100M")
                    .unwrap()
                    .build()
                    .unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-t", "5", "-t", "30"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").duration(30).build().unwrap());
            // Repeated bools parse (previously a hard clap error).
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-V", "-V"]);
            assert!(cli.verbose);
            // Bare repeat of a value-optional flag resets to the default.
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--timestamps=%H", "--timestamps"]);
            assert_eq!(cli.timestamps.as_deref(), Some("%c "));
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
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .one_off(true)
                    .build()
                    .unwrap()
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

        // build() rejects --bind-dev where there is no SO_BINDTODEVICE (Linux)
        // or IP_BOUND_IF (macOS) — including FreeBSD/NetBSD since #149 closed
        // the silent-no-op fallback — so gate the wiring tests to match.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        #[test]
        fn bind_dev_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--bind-dev", "eth0"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").bind_dev("eth0").build().unwrap());
        }

        // #149: the server applies --bind-dev too (iperf3's netannounce —
        // SO_BINDTODEVICE-only, so the server side is Linux-only; iperf3's
        // macOS IP_BOUND_IF covers only the client path).
        #[cfg(target_os = "linux")]
        #[test]
        fn bind_dev_wired_server() {
            let cli = Cli::parse_from(["riperf3", "-s", "--bind-dev", "eth0"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .bind_dev("eth0")
                    .build()
                    .unwrap()
            );
        }

        // #149: where it cannot be honored, --bind-dev is a config-time error
        // (iperf3 without CAN_BIND_TO_DEVICE doesn't recognize the option at
        // all; the old behavior on FreeBSD/NetBSD was a silent no-op).
        // Exercised by the FreeBSD and Windows CI jobs (NetBSD is xcheck
        // compile-only).
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        #[test]
        fn bind_dev_rejected_on_unsupported_platforms() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--bind-dev", "eth0"]);
            let err = cli.build_client(None).unwrap_err().to_string();
            assert!(err.contains("--bind-dev"), "got: {err}");
        }

        // #149 review r1: the SERVER rejects --bind-dev everywhere but Linux
        // — including macOS, where real iperf3's netannounce (SO_BINDTODEVICE
        // -only) fails the listen. The macOS native CI job exercises this.
        #[cfg(not(target_os = "linux"))]
        #[test]
        fn bind_dev_rejected_on_server_off_linux() {
            let cli = Cli::parse_from(["riperf3", "-s", "--bind-dev", "eth0"]);
            let err = cli.build_server().unwrap_err().to_string();
            assert!(err.contains("--bind-dev"), "got: {err}");
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

        // #213: the full-output leg rides json-stream.
        #[test]
        fn json_stream_full_output_wired() {
            let cli = Cli::parse_from([
                "riperf3",
                "-c",
                "h",
                "--json-stream",
                "--json-stream-full-output",
            ]);
            let c = build_client_from_cli(&cli);
            assert_eq!(
                c,
                expected_client("h")
                    .json_stream(true)
                    .json_stream_full_output(true)
                    .build()
                    .unwrap()
            );
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
        fn server_rcv_timeout_wired() {
            // #356 r1 F10: the SERVER glue for --rcv-timeout (#338), the
            // builder-compare convention like the client's rcv_timeout_wired.
            let cli = Cli::parse_from(["riperf3", "-s", "--rcv-timeout", "3000"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .rcv_timeout(3000)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_idle_timeout_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--idle-timeout", "30"]);
            let s = build_server_from_cli(&cli);
            assert_eq!(
                s,
                riperf3::ServerBuilder::new()
                    .emit_output(true)
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
                    .emit_output(true)
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

        // #328: -i is C atof — strtod's longest prefix, garbage → 0.0
        // (iperf_api.c:1260; live-probed: GT runs `-i 2x` at 2.0 and
        // `-i x` as 0.0).
        #[test]
        fn interval_atof_values_wire_like_gt() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-i", "2x"]);
            assert_eq!(cli.interval, Some(2.0));
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").interval(2.0).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-i", "x"]);
            assert_eq!(cli.interval, Some(0.0));
        }

        // #328: -d/--debug's level is C atoi with negative → 4
        // (iperf_api.c:1692-1697); garbage is level 0 and there is no
        // upper clamp, all GT-accepted (live-probed).
        #[test]
        fn debug_level_parses_like_gt() {
            assert_eq!(Cli::parse_from(["riperf3", "-s", "-d"]).debug, Some(4));
            assert_eq!(
                Cli::parse_from(["riperf3", "-s", "--debug=abc"]).debug,
                Some(0)
            );
            assert_eq!(
                Cli::parse_from(["riperf3", "-s", "--debug=-1"]).debug,
                Some(4)
            );
            assert_eq!(
                Cli::parse_from(["riperf3", "-s", "--debug=100"]).debug,
                Some(100)
            );
        }

        // zerocopy (sendfile) is rejected by `build()` on non-unix
        // (cfg(not(unix)) → Unsupported), so its wiring test is gated to
        // unix to match — same as congestion_flag_wired / bind_dev_wired.
        #[cfg(unix)]
        #[test]
        fn zerocopy_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-Z"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").zerocopy(true).build().unwrap());
        }

        // --gsro builds on EVERY platform (#316): GT keeps the flag
        // available regardless of local support so the request still
        // reaches the server (iperf_api.c:1799-1804).
        #[test]
        fn gsro_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--gsro"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").gsro(true).build().unwrap());
        }

        #[test]
        fn cntl_ka_wired() {
            // 20 > 5*3, so the spec also passes GT's IECNTLKA sanity check.
            // =-attached: the only spec form GT's optional_argument takes
            // (require_equals, #328 r1 F3).
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cntl-ka=20/5/3"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").cntl_ka("20/5/3").build().unwrap());
        }

        // #328: bare --cntl-ka (GT optional_argument, iperf_api.c:1191)
        // enables keepalive with the all-defaults spec.
        #[test]
        fn cntl_ka_bare_wires_defaults() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--cntl-ka"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").cntl_ka("").build().unwrap()
            );
        }

        // #140: iperf3 permits only ONE test end condition; -t with -n/-k (or
        // -n with -k) fails with IEENDCONDITIONS. riperf3 silently let the
        // byte/block condition win, so a script relying on iperf3's rejection
        // got a different test instead of an error.
        #[test]
        fn conflicting_end_conditions_rejected() {
            for args in [
                ["riperf3", "-c", "h", "-t", "5", "-n", "1G"],
                ["riperf3", "-c", "h", "-t", "5", "-k", "100"],
                ["riperf3", "-c", "h", "-n", "1G", "-k", "100"],
                ["riperf3", "-c", "h", "-t", "5", "-n", "0.5K"],
            ] {
                let cli = Cli::parse_from(args);
                let err = cli
                    .build_client(None)
                    .expect_err("conflicting end conditions must be rejected")
                    .to_string();
                assert!(
                    err.contains("only one test end condition (-t, -n, -k) may be specified"),
                    "iperf3 IEENDCONDITIONS wording expected, got: {err}"
                );
            }
        }

        // Each end condition alone stays valid, and — like iperf3, whose -n/-k
        // legs test the PARSED VALUE (bytes != 0), not flag presence — a
        // zero-valued -n/-k never conflicts: iperf3 runs `-t 5 -n 0` as a plain
        // 5-second duration test (0 means "not set").
        #[test]
        fn single_end_conditions_accepted() {
            for args in [
                &["riperf3", "-c", "h", "-t", "5"][..],
                &["riperf3", "-c", "h", "-n", "1G"][..],
                &["riperf3", "-c", "h", "-k", "100"][..],
                &["riperf3", "-c", "h", "-t", "5", "-n", "0"][..],
                &["riperf3", "-c", "h", "-t", "5", "-n", "0.5"][..],
                &["riperf3", "-c", "h", "-t", "5", "-k", "0"][..],
                &["riperf3", "-c", "h", "-n", "0", "-k", "100"][..],
            ] {
                let cli = Cli::parse_from(args);
                assert!(
                    cli.build_client(None).is_ok(),
                    "iperf3 accepts {args:?}; riperf3 must not reject it"
                );
            }
        }

        // `-n 0` means "no byte limit" in iperf3 (the value gates everything);
        // it must wire to a plain duration test, not an instant-end Bytes(0).
        #[test]
        fn zero_byte_limit_is_unset() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-n", "0"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").build().unwrap());
        }

        // #32: --pacing-timer was parsed but never wired — a silent no-op.
        #[test]
        fn pacing_timer_accepts_kmg_suffix() {
            // #160: iperf3 parses --pacing-timer with unit_atoi (1024-based).
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--pacing-timer", "1K"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").pacing_timer(1024).build().unwrap());
        }

        #[test]
        fn pacing_timer_wired() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--pacing-timer", "500"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").pacing_timer(500).build().unwrap());
        }

        // #328: the unit_atoi family wires GT's parsed VALUE — junk after a
        // valid suffix is ignored (`10Kx` = 10240), exponents and hex parse
        // (units.c:196 sscanf %lf), and `-n -5` takes the (uint64) wrap GT
        // runs with (live-probed).
        #[test]
        fn unit_atoi_values_wire_like_gt() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-n", "10Kx"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").bytes(10240).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-n", "-5"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").bytes(u64::MAX - 4).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-k", "1e3"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").blocks(1000).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-l", "10Kx"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").blksize(10240).build().unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--connect-timeout", "1K"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h")
                    .connect_timeout(std::time::Duration::from_millis(1024))
                    .build()
                    .unwrap()
            );
        }

        // #328: `-l 0` keeps the protocol default, like GT's post-loop
        // `if (blksize == 0)` step (iperf_api.c:1926-1933) — NOT an explicit
        // 0-byte block size.
        #[test]
        fn length_zero_keeps_protocol_default() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-l", "0"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").build().unwrap()
            );
        }

        // #328: a NEGATIVE --connect-timeout means "no timeout" — GT hands
        // the int to poll(2), where negative waits forever (net.c:272-289;
        // live-probed: `--connect-timeout -100` connects fine).
        #[test]
        fn connect_timeout_negative_is_no_timeout() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--connect-timeout", "-100"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").build().unwrap()
            );
        }

        // #328 RECORDED DEVIATION: GT runs `--pacing-timer 3G` with the
        // int-wrapped NEGATIVE value (continuous pacing); riperf3's u32
        // builder cannot hold it, so wrapped values keep the default pacing.
        #[test]
        fn pacing_timer_wrapped_keeps_default() {
            let cli = Cli::parse_from(["riperf3", "-c", "h", "--pacing-timer", "3G"]);
            assert_eq!(
                build_client_from_cli(&cli),
                expected_client("h").build().unwrap()
            );
        }

        #[test]
        fn format_wired() {
            // An explicit `-f g` must propagate (absent -f, the lib default
            // 'a' stands — #221). #263: a full word rides its first char.
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-f", "g"]);
            let c = build_client_from_cli(&cli);
            assert_eq!(c, expected_client("h").format_char('g').build().unwrap());
            let cli = Cli::parse_from(["riperf3", "-c", "h", "-f", "gigabits"]);
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
        // idle_timeout, and server_max_duration had dedicated tests). #294:
        // `build_server` sets `emit_output(true)` unconditionally, so each
        // expected baseline carries `.emit_output(true)` (like the client's
        // `expected_client`).

        #[test]
        fn server_port_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-p", "5201"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
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
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .verbose(true)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_json_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-J"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
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
                    .emit_output(true)
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
                    .emit_output(true)
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
                    .emit_output(true)
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
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .ip_version(4)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_ip_version6_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "-6"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .ip_version(6)
                    .build()
                    .unwrap()
            );
        }

        #[test]
        fn server_timestamps_wired() {
            let cli = Cli::parse_from(["riperf3", "-s", "--timestamps", "%H"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
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
                    .emit_output(true)
                    .server_bitrate_limit_str("100M")
                    .unwrap()
                    .build()
                    .unwrap()
            );
        }

        // #328: the rate part is GT's unit_atof_rate (1000-based) with the
        // /interval piece split off first (iperf_api.c:1366-1385) — junk
        // after the suffix is ignored, and a negative rate (uint64)-wraps
        // huge like GT's iperf_size_t assignment. #410: the `/N` interval
        // half wires into the lib's averaging window (GT's
        // bitrate_limit_interval); `/0` stays unwired — GT's 0 leaves its
        // derivation degenerate (recorded at the build_server site).
        #[test]
        fn server_bitrate_limit_rate_parses_like_gt() {
            let cli = Cli::parse_from(["riperf3", "-s", "--server-bitrate-limit", "10M/2"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .server_bitrate_limit(10_000_000)
                    .server_bitrate_limit_interval(2.0)
                    .build()
                    .unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-s", "--server-bitrate-limit", "10M/0"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .server_bitrate_limit(10_000_000)
                    .build()
                    .unwrap(),
                "the /0 edge keeps the default window (recorded deviation)"
            );
            let cli = Cli::parse_from(["riperf3", "-s", "--server-bitrate-limit", "10Kx"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .server_bitrate_limit(10_000)
                    .build()
                    .unwrap()
            );
            let cli = Cli::parse_from(["riperf3", "-s", "--server-bitrate-limit", "-5"]);
            assert_eq!(
                build_server_from_cli(&cli),
                riperf3::ServerBuilder::new()
                    .emit_output(true)
                    .server_bitrate_limit(u64::MAX - 4)
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
                    .emit_output(true)
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
                    .emit_output(true)
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
                    .emit_output(true)
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
                    .emit_output(true)
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
