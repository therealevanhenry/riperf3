// Module: iperf_api
// Path: riperf3/src/iperf_api.rs
// This module is used to define the API for the riperf3 crate.

#[derive(Debug, PartialEq)]
pub enum DebugLevel {
    Error,
    Warn,
    Info,
    Debug,
    Max, // TODO: what is different between Debug and Max?
}

#[derive(Debug, PartialEq)]
pub enum IperfRole {
    Server,
    Client,
}

#[derive(Debug, PartialEq)]
pub enum IperfMode {
    Sender,
    Receiver,
    Bidirectional,
}

#[derive(Debug, PartialEq)]
pub struct IperfSettings {
    pub domain: i32,         // AF_INET or AF_INET6
    pub socket_bufsize: i32, // window size for TCP
    pub blksize: i32,        // size of read/write blocks for '-l' flag
    // TODO: iperf_size_t rate;    // target data rate for the test
    // TODO: iperf_size_t bitrate_limit;    // server's total bitrate limit
    pub bitrate_limit_interval: i32, // interval for averaging server's total bitrate limit
    pub bitrate_limit_stats_per_interval: i32, // number of stats periods to accumulate for
    // server's total bitrate average
    pub fqrate: u64,       // target data rate for FQ pacing
    pub pacing_timer: i32, // pacing timer for FQ pacing in microseconds
    pub burst: i32,        // number of packets to send in a burst
    pub mss: i32,          // TCP maximum segment size
    pub ttl: i32,          // IP TTL (Time to Live)
    pub tos: i32,          // IP TOS (Type of Service)
    pub flowlabel: i32,    // IPv6 flow label
    // TODO: iperf_size_t bytes;    // number of bytes to transmit/receive
    // TODO: iperf_size_t blocks;    // number of blocks (packets) to transmit/receive
    pub unit_format: char, // 'k', 'K', 'm', 'M', 'g', 'G' for Kbits/sec, Mbits/sec, Gbits/sec
    // KBytes/sec, MBytes/sec, GBytes/sec '-f' flag
    pub num_ostreams: i32,   // SCTP initmsg settings
    pub dont_fragment: bool, // set/clear IP Don't Fragment flag

    // TODO:    if defined(HAVE_SSL)
    //              char *authtoken;
    //              char *client_username;
    //              char *client_password;
    //              EVP_PKEY *client_rsa_pubkey;
    //          #endif
    pub connect_timeout: i32, // timeout for control connection setup, in milliseconds
    pub idle_timeout: i32,    // server idle time timeout, in seconds
    pub snd_timeout: u32,     // Timeout for sending TCP messages in active mode
                              // in microseconds
                              // TODO: struct iperf_time rcv_timeout;   // Timeout for receiving TCP messages in
                              //                                        // active mode in microseconds
}

impl Default for IperfSettings {
    fn default() -> Self {
        IperfSettings {
            domain: 0,
            socket_bufsize: 0,
            blksize: 0,
            bitrate_limit_interval: 0,
            bitrate_limit_stats_per_interval: 0,
            fqrate: 0,
            pacing_timer: 0,
            burst: 0,
            mss: 0,
            ttl: 0,
            tos: 0,
            flowlabel: 0,
            unit_format: ' ',
            num_ostreams: 0,
            dont_fragment: false,
            connect_timeout: 0,
            idle_timeout: 0,
            snd_timeout: 0,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct IperfTest {
    pub role: IperfRole, // '-s' (server) or '-c' (client) flags
    pub mode: IperfMode, // defaults to Sender unless '-R' (reverse) or '--bidir' (bidirectional)
    // flags are specified
    pub sender_has_retransmits: bool,
    pub other_side_has_retransmits: bool, // used if mode is Bidirectional

    // TODO: struct protocol *protocol;

    // TODO: signed char state;
    pub server_hostname: String, // hostname of test server, used with '-c' flag
    pub tmp_template: String,
    pub bind_address: String, // bind to the interface at this address
    pub bind_dev: String,     // bind to the interface at this device

    // TODO: TAILQ_HEAD(xbind_addrhead, xbind_entry) xbind_addrs;
    pub bind_port: i32, // '--cport' flag
    pub server_port: i32,
    pub omit: i32,             // duration of omit period '-O' flag
    pub duration: i32,         // total duration of test '-t' flag
    pub diskfile_name: String, // name of file to write test output to '-F' flag

    // TODO: Can this be refactored to just use affinity?
    pub affinity: i32,        // cpu affinity '-A' flag
    pub server_affinity: i32, // server cpu affinity '-A' flag

    // TODO:    #if defined (HAVE_CPUSET_SETAFFINITY)
    //          cpuset_t cpumask;
    //          #endif
    pub title: String,                  // title to include in JSON output '-T' flag
    pub extra_data: String,             // '--extra-data' flag
    pub congestion: String, // '-C' flag to specify preferred congestion control algorithm
    pub congestion_used: String, // The congestion control algorithm used
    pub remote_congestion_used: String, // The congestion control algorithm used on the remote side

    // TODO: Are we sure this is the '-P' flag?
    pub pidfile: String, // '-P' flag

    pub logfile: String, // '--logfile' flag

    // TODO: FILE *outfile;

    // TODO: should this be a socket object instead of an int32?
    pub ctrl_sck: i32,      // control socket
    pub mapped_v4: i32,     // test uses mapped address
    pub listener: i32,      // listener socket
    pub prot_listener: i32, // listener socket for protocol

    pub ctrl_sck_mss: i32, // MSS for control socket

    // TODO:    #if defined(HAVE_SSL)
    //              char *server_authorized_users;
    //              EVP_PKEY *server_rsa_private_key;
    //              int server_skew_threshold;
    //              int use_pkcs1_padding;
    //          #endif

    // boolean variables for flags
    pub daemon: bool,             // '-D' flag
    pub one_off: bool,            // '-1' flag
    pub no_delay: bool,           // '-N' flag
    pub reverse: bool,            // '-R' flag
    pub bidir: bool,              // '--bidir' flag
    pub verbose: bool,            // '-V' flag
    pub json_output: bool,        // '-J' flag
    pub json_stream: bool,        // '--json-stream' flag
    pub zerocopy: bool,           // '-Z' flag uses sendfile
    pub debug: bool,              // '-d' flag
    pub debug_level: DebugLevel,  // '-d' flag with explicit level
    pub get_server_output: bool,  // '--get-server-output' flag
    pub udp_counters_64bit: bool, // '--udp-counters-64bit' flag
    pub forceflush: bool,         // '--forceflush' flag to flush output after every interval
    pub multisend: bool,
    pub repeating_payload: bool, // '--repeating-payload' flag
    pub timestamps: bool,        // '--timestamps' flag
    pub timestamp_format: String,

    pub json_output_string: String, // JSON output string used with '--json-output' flag

    pub max_fd: i32,
    // TODO: fd_set read_set;   // set of read sockets
    // TODO: fd_set write_set;  // set of write sockets

    // interval related variables
    pub omitting: bool, // '-O' flag
    pub stats_interval: f64,
    pub reporter_interval: f64,
    // TODO: void (*stats_callback)(struct iperf_test *test);
    // TODO: void (*reporter_callback)(struct iperf_test *test);
    // TODO: Timer *omit_timer;
    // TODO: Timer *timer;
    pub done: bool,
    // TODO: Timer *stats_timer;
    // TODO: Timer *reporter_timer;

    // TODO: double cpu_util[3];    // cpu utilization of system - total, user, system
    // TODO: double remote_cpu_util[3];    // cpu utilization for the remote host/client - total,
    // user, system
    pub num_streams: i32, // number of parallel streams to run '-P' flag

    // TODO: atomic_iperf_size_t bytes_sent;
    // TODO: atomic_iperf_size_t blocks_sent;
    //
    // TODO: atomic_iperf_size_t bytes_received;
    // TODO: atomic_iperf_size_t blocks_received;
    //
    // TODO: iperf_size_t bitrate_limit_stats_count;    // Number of stats periods accumuldated for
    // server's total bitrate average
    //
    // TODO: iperf_size_t *bitrate_limit_intervals_traffic_bytes;   // Pointer to cyclic array that
    // includes the last interval's bytes transferred

    // TODO: iperf_size_t bitrate_limit_last_interval_index;    // Index of the last interval into
    // the cyclic array
    pub bitrate_limit_exceeded: bool, // Set by callback routine when average data rate exceeded
    // the server's bitrate limit
    pub server_last_run_rc: i32, // Save the last server run return code for the next test
    pub server_forced_idle_restarts_count: u32, // count number of forced server restarts due
    // to idle to make sure it is not stuck
    pub server_forced_no_msg_restarts_count: u32, // count number of forced server restarts due
    // to no messages receiveda to make sure it
    // is not stuck
    pub server_test_number: u32, // count number of tests run by the server

    pub cookie: String, // cookie string for authentication

    // Presumably, the SLIST_HEAD macro replaced the `struct iperf_stream *streams;` pointer
    // struct iperf_stream *streams;    /* pointer to list of struct stream */
    // TODO: SLIST_HEAD(slisthead, iperf_stream) streams;
    pub settings: IperfSettings, // TODO: implement the struct

    // TODO: SLIST_HEAD(plisthead, protocl) protocols;

    // TODO: void (*on_new_stream)(struct iperf_stream *);
    // TODO: void (*on_test_start)(struct iperf_test *);
    // TODO: void (*on_connect)(struct iperf_test *);
    // TODO: void (*on_test_finish)(struct iperf_test *);

    // cJSON handles for use when in JSON output mode
    // TODO: cJSON *json_top;
    // TODO: cJSON *json_start;
    // TODO: cJSON *json_connected;
    // TODO: cJSON *json_intervals;
    // TODO: cJSON *json_end;

    // server output (use on client-side only)
    pub server_output_text: String,
    // TODO: cJSON *json_server_output;

    // server output (use on server-side only)
    // TODO: TAILQ_HEAD(iperf_textlisthead, iperf_textline) server_output_list;
}

impl Default for IperfTest {
    fn default() -> Self {
        IperfTest {
            role: IperfRole::Server,
            mode: IperfMode::Sender,
            sender_has_retransmits: false,
            other_side_has_retransmits: false,
            server_hostname: String::from("localhost"),
            tmp_template: String::from(""),
            bind_address: String::from(""),
            bind_dev: String::from(""),
            bind_port: DEFAULT_PORT,
            server_port: DEFAULT_PORT,
            omit: DEFAULT_OMIT,
            duration: DEFAULT_DURATION,
            diskfile_name: String::from(""),
            affinity: 0,
            server_affinity: 0,
            // TODO: cpumask
            title: String::from(""),
            extra_data: String::from(""),
            congestion: String::from(""),
            congestion_used: String::from(""),
            remote_congestion_used: String::from(""),
            pidfile: String::from(""),
            logfile: String::from(""),
            // TODO: outfile
            ctrl_sck: 0,
            mapped_v4: 0,
            listener: 0,
            prot_listener: 0,
            ctrl_sck_mss: 0,
            // TODO: all the SLL stuff
            daemon: false,
            one_off: false,
            no_delay: false,
            reverse: false,
            bidir: false,
            verbose: false,
            json_output: false,
            json_stream: false,
            zerocopy: false,
            debug: false,
            debug_level: DebugLevel::Warn,
            get_server_output: false,
            udp_counters_64bit: false,
            forceflush: false,
            multisend: false,
            repeating_payload: false,
            timestamps: false,
            timestamp_format: String::from(DEFAULT_TIMESTAMP_FORMAT),
            json_output_string: String::from(""),
            max_fd: 0,
            // TODO: read_set
            // TODO: write_set
            omitting: false,
            stats_interval: 0.0,
            reporter_interval: 0.0,
            // TODO: stats_callback
            // TODO: reporter_callback
            done: false,
            // TODO: stats_timer
            // TODO: reporter_timer
            // TODO: cpu_util
            // TODO: remote_cpu_util
            num_streams: 1,
            // TODO: all the atomic_iperf_size_t bytes and blocks sent and received stuff
            // TODO: all the iperf_size_t bitrate_limit stuff
            bitrate_limit_exceeded: false,
            server_last_run_rc: 0,
            server_forced_idle_restarts_count: 0,
            server_forced_no_msg_restarts_count: 0,
            server_test_number: 0,
            cookie: String::from(""),
            // TODO: the streams stuff with SLIST_HEAD
            settings: IperfSettings::default(),
            // TODO: the protocols stuff with SLIST_HEAD
            // TODO: all the callback pointer functions
            // TODO: all the cJSON stuff
            server_output_text: String::from(""),
            // TODO: json_server_output
            // TODO: server_output_list
        }
    }
}

// TODO: should we create the cookie string to be a fixed size?
//pub const COOKIE_SIZE: u8 = 37;     // ASCII UUID size is 36 characters + null terminator
pub const DEFAULT_PORT: i32 = 5201; // default port number is 5201
pub const DEFAULT_OMIT: i32 = 0; // default omit period is 0 seconds
pub const DEFAULT_DURATION: i32 = 10; // default duration is 10 seconds
pub const DEFAULT_TIMESTAMP_FORMAT: &str = "%c "; // default timestamp format

//
// Unit Tests for the iperf_api module
//
#[cfg(test)]
mod iperf_api_tests {
    use super::*;

    // This module is a collection of tests for the DebugLevel enum.
    mod debug_level_tests {
        use super::*;

        // This function is used to test the Error variant of the DebugLevel enum.
        #[test]
        fn test_debug_level_error() {
            let level = DebugLevel::Error;
            assert_eq!(level, DebugLevel::Error);
        }

        // This function is used to test the Warn variant of the DebugLevel enum.
        #[test]
        fn test_debug_level_warn() {
            let level = DebugLevel::Warn;
            assert_eq!(level, DebugLevel::Warn);
        }

        // This function is used to test the Info variant of the DebugLevel enum.
        #[test]
        fn test_debug_level_info() {
            let level = DebugLevel::Info;
            assert_eq!(level, DebugLevel::Info);
        }

        // This function is used to test the Debug variant of the DebugLevel enum.
        #[test]
        fn test_debug_level_debug() {
            let level = DebugLevel::Debug;
            assert_eq!(level, DebugLevel::Debug);
        }

        // This function is used to test the Max variant of the DebugLevel enum.
        #[test]
        fn test_debug_level_max() {
            let level = DebugLevel::Max;
            assert_eq!(level, DebugLevel::Max);
        }
    }

    // This module is a collection of tests for the IperfRole enum.
    mod iperf_role_tests {
        use super::*;

        // This function is used to test the Server variant of the IperfRole enum.
        #[test]
        fn test_iperf_role() {
            let role = IperfRole::Server;
            assert_eq!(role, IperfRole::Server);
        }

        // This function is used to test the Client variant of the IperfRole enum.
        #[test]
        fn test_iperf_role_client() {
            let role = IperfRole::Client;
            assert_eq!(role, IperfRole::Client);
        }
    }

    // This module is a collection of tests for the IperfMode enum.
    mod iperf_mode_tests {
        use super::*;

        // This function is used to test the Sender variant of the IperfMode enum.
        #[test]
        fn test_iperf_mode_sender() {
            let mode = IperfMode::Sender;
            assert_eq!(mode, IperfMode::Sender);
        }

        // This function is used to test the Receiver variant of the IperfMode enum.
        #[test]
        fn test_iperf_mode_receiver() {
            let mode = IperfMode::Receiver;
            assert_eq!(mode, IperfMode::Receiver);
        }

        // This function is used to test the Bidirectional variant of the IperfMode enum.
        #[test]
        fn test_iperf_mode_bidirectional() {
            let mode = IperfMode::Bidirectional;
            assert_eq!(mode, IperfMode::Bidirectional);
        }
    }

    // This module is a collection of tests for the IperfSettings struct.
    mod iperf_settings_tests {
        use super::*;

        // This function is used to test the default values of the IperfSettings struct.
        #[test]
        fn test_default_iperf_settings() {
            let settings = IperfSettings::default();
            assert_eq!(settings.domain, 0);
            assert_eq!(settings.socket_bufsize, 0);
            assert_eq!(settings.blksize, 0);
            assert_eq!(settings.bitrate_limit_interval, 0);
            assert_eq!(settings.bitrate_limit_stats_per_interval, 0);
            assert_eq!(settings.fqrate, 0);
            assert_eq!(settings.pacing_timer, 0);
            assert_eq!(settings.burst, 0);
            assert_eq!(settings.mss, 0);
            assert_eq!(settings.ttl, 0);
            assert_eq!(settings.tos, 0);
            assert_eq!(settings.flowlabel, 0);
            assert_eq!(settings.unit_format, ' ');
            assert_eq!(settings.num_ostreams, 0);
            assert_eq!(settings.dont_fragment, false);
            assert_eq!(settings.connect_timeout, 0);
            assert_eq!(settings.idle_timeout, 0);
            assert_eq!(settings.snd_timeout, 0);
        }
    }

    // This module is a collection of tests for the IperfTest struct.
    mod iperf_test_tests {
        use super::*;

        // This function is used to test the default values of the IperfTest struct.
        #[test]
        fn test_default_iperf_test() {
            let test = IperfTest::default();
            assert_eq!(test.role, IperfRole::Server);
            assert_eq!(test.mode, IperfMode::Sender);
            assert_eq!(test.sender_has_retransmits, false);
            assert_eq!(test.other_side_has_retransmits, false);
            assert_eq!(test.server_hostname, "localhost");
            assert_eq!(test.tmp_template, "");
            assert_eq!(test.bind_address, "");
            assert_eq!(test.bind_dev, "");
            assert_eq!(test.bind_port, DEFAULT_PORT);
            assert_eq!(test.server_port, DEFAULT_PORT);
            assert_eq!(test.omit, DEFAULT_OMIT);
            assert_eq!(test.duration, DEFAULT_DURATION);
            assert_eq!(test.diskfile_name, "");
            assert_eq!(test.affinity, 0);
            assert_eq!(test.server_affinity, 0);
            assert_eq!(test.title, "");
            assert_eq!(test.extra_data, "");
            assert_eq!(test.congestion, "");
            assert_eq!(test.congestion_used, "");
            assert_eq!(test.remote_congestion_used, "");
            assert_eq!(test.pidfile, "");
            assert_eq!(test.logfile, "");
            assert_eq!(test.ctrl_sck, 0);
            assert_eq!(test.mapped_v4, 0);
            assert_eq!(test.listener, 0);
            assert_eq!(test.prot_listener, 0);
            assert_eq!(test.ctrl_sck_mss, 0);
            assert_eq!(test.daemon, false);
            assert_eq!(test.one_off, false);
            assert_eq!(test.no_delay, false);
            assert_eq!(test.reverse, false);
            assert_eq!(test.bidir, false);
            assert_eq!(test.verbose, false);
            assert_eq!(test.json_output, false);
            assert_eq!(test.json_stream, false);
            assert_eq!(test.zerocopy, false);
            assert_eq!(test.debug, false);
            assert_eq!(test.debug_level, DebugLevel::Warn);
            assert_eq!(test.get_server_output, false);
            assert_eq!(test.udp_counters_64bit, false);
            assert_eq!(test.forceflush, false);
            assert_eq!(test.multisend, false);
            assert_eq!(test.repeating_payload, false);
            assert_eq!(test.timestamps, false);
            assert_eq!(test.timestamp_format, DEFAULT_TIMESTAMP_FORMAT);
            assert_eq!(test.json_output_string, "");
            assert_eq!(test.max_fd, 0);
            assert_eq!(test.omitting, false);
            assert_eq!(test.stats_interval, 0.0);
            assert_eq!(test.reporter_interval, 0.0);
            assert_eq!(test.done, false);
            assert_eq!(test.num_streams, 1);
            assert_eq!(test.bitrate_limit_exceeded, false);
            assert_eq!(test.server_last_run_rc, 0);
            assert_eq!(test.server_forced_idle_restarts_count, 0);
            assert_eq!(test.server_forced_no_msg_restarts_count, 0);
            assert_eq!(test.server_test_number, 0);
            assert_eq!(test.cookie, "");
            assert_eq!(test.settings, IperfSettings::default());
            assert_eq!(test.server_output_text, "");
        }
    }
}
