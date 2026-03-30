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
