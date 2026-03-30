use clap::Parser;
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

use riperf3::protocol::TransportProtocol;
use riperf3::utils::{parse_bitrate, parse_kmg, set_verbose};

mod cli;
use cli::Cli;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    set_verbose(cli.verbose);
    configure_log4rs(cli.debug.unwrap_or(0));

    if let Some(server_host) = cli.client {
        // ---- Client mode ----
        let mut builder = riperf3::ClientBuilder::new(&server_host);

        if let Some(port) = cli.port {
            builder = builder.port(Some(port));
        }
        if cli.udp {
            builder = builder.protocol(TransportProtocol::Udp);
        }
        if let Some(t) = cli.time {
            builder = builder.duration(t);
        }
        if let Some(ref s) = cli.bytes {
            builder = builder.bytes(parse_kmg(s)?);
        }
        if let Some(ref s) = cli.blockcount {
            builder = builder.blocks(parse_kmg(s)?);
        }
        if let Some(ref s) = cli.length {
            builder = builder.blksize(parse_kmg(s)? as usize);
        }
        if let Some(n) = cli.parallel {
            builder = builder.num_streams(n);
        }
        if cli.reverse {
            builder = builder.reverse(true);
        }
        if cli.bidir {
            builder = builder.bidir(true);
        }
        if let Some(ref s) = cli.window {
            builder = builder.window(parse_kmg(s)? as i32);
        }
        if let Some(ref algo) = cli.congestion {
            builder = builder.congestion(algo);
        }
        if let Some(mss) = cli.mss {
            builder = builder.mss(mss);
        }
        if cli.no_delay {
            builder = builder.no_delay(true);
        }
        if let Some(ref s) = cli.bitrate {
            let (rate, _burst) = parse_bitrate(s)?;
            builder = builder.bandwidth(rate);
        }
        if let Some(tos) = cli.tos {
            builder = builder.tos(tos);
        }
        if let Some(o) = cli.omit {
            builder = builder.omit(o);
        }
        if let Some(ref t) = cli.title {
            builder = builder.title(t);
        }
        if let Some(ref d) = cli.extra_data {
            builder = builder.extra_data(d);
        }
        if let Some(ms) = cli.connect_timeout {
            builder = builder.connect_timeout(std::time::Duration::from_millis(ms));
        }
        if cli.verbose {
            builder = builder.verbose(true);
        }
        if cli.json {
            builder = builder.json_output(true);
        }
        if cli.json_stream {
            builder = builder.json_stream(true);
        }
        if cli.udp_counters_64bit {
            builder = builder.udp_counters_64bit(true);
        }
        if cli.repeating_payload {
            builder = builder.repeating_payload(true);
        }
        if cli.dont_fragment {
            builder = builder.dont_fragment(true);
        }
        if let Some(port) = cli.cport {
            builder = builder.cport(port);
        }
        if cli.get_server_output {
            builder = builder.get_server_output(true);
        }
        if cli.forceflush {
            builder = builder.forceflush(true);
        }
        if let Some(ref fmt) = cli.timestamps {
            builder = builder.timestamps(fmt);
        }
        if let Some(ref addr) = cli.bind {
            builder = builder.bind_address(addr);
        }
        if let Some(ref dev) = cli.bind_dev {
            builder = builder.bind_dev(dev);
        }
        if let Some(ref s) = cli.fq_rate {
            builder = builder.fq_rate(parse_kmg(s)?);
        }
        if let Some(label) = cli.flowlabel {
            builder = builder.flowlabel(label);
        }
        if cli.version4 {
            builder = builder.ip_version(4);
        }
        if cli.version6 {
            builder = builder.ip_version(6);
        }
        if cli.mptcp {
            builder = builder.mptcp(true);
        }
        if cli.skip_rx_copy {
            builder = builder.skip_rx_copy(true);
        }
        if let Some(ms) = cli.rcv_timeout {
            builder = builder.rcv_timeout(ms);
        }
        if let Some(ms) = cli.snd_timeout {
            builder = builder.snd_timeout(ms);
        }
        if let Some(ref path) = cli.file {
            builder = builder.file(path);
        }
        if let Some(ref spec) = cli.affinity {
            builder = builder.affinity(spec);
        }
        if let Some(ref val) = cli.dscp {
            builder = builder.dscp(val);
        }
        if let Some(ref path) = cli.pidfile {
            builder = builder.pidfile(path);
        }
        if let Some(ref path) = cli.logfile {
            builder = builder.logfile(path);
        }

        let client = builder.build()?;
        client.run().await?;
    } else if cli.server {
        // ---- Server mode ----
        let mut builder = riperf3::ServerBuilder::new();

        if let Some(port) = cli.port {
            builder = builder.port(Some(port));
        }
        if cli.one_off {
            builder = builder.one_off(true);
        }
        if cli.verbose {
            builder = builder.verbose(true);
        }
        if cli.daemon {
            builder = builder.daemon(true);
        }
        if let Some(secs) = cli.idle_timeout {
            builder = builder.idle_timeout(secs);
        }
        if let Some(ref s) = cli.server_bitrate_limit {
            builder = builder.server_bitrate_limit(parse_kmg(s)?);
        }
        if let Some(secs) = cli.server_max_duration {
            builder = builder.server_max_duration(secs);
        }
        if let Some(ref path) = cli.pidfile {
            builder = builder.pidfile(path);
        }
        if let Some(ref path) = cli.logfile {
            builder = builder.logfile(path);
        }
        if cli.forceflush {
            builder = builder.forceflush(true);
        }
        if let Some(ref addr) = cli.bind {
            builder = builder.bind_address(addr);
        }
        if let Some(ref fmt) = cli.timestamps {
            builder = builder.timestamps(fmt);
        }

        let server = builder.build()?;
        server.run().await?;
    } else {
        eprintln!("No mode specified. Use -s for server or -c <host> for client.");
    }

    Ok(())
}

fn configure_log4rs(verbosity: u8) {
    let level = match verbosity {
        0 => LevelFilter::Error,
        1 => LevelFilter::Warn,
        2 => LevelFilter::Info,
        3 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    let stdout = ConsoleAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{d} - {l} - {m}{n}")))
        .build();

    let config = Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        .logger(Logger::builder().build("app::backend::db", LevelFilter::Info))
        .build(Root::builder().appender("stdout").build(level))
        .unwrap();

    log4rs::init_config(config).unwrap();
}
