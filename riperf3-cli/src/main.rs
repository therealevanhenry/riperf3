// The main entry point for the riperf3-cli application.

// Use clap to parse the command line arguments.
use clap::Parser;

// Use log and log4rs for logging.
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

// riperf3 CLI module
mod cli;
use cli::Cli;

fn main() {
    // Parse the command line arguments
    let cli = Cli::parse();

    // Configure log4rs
    let log_level = cli.debug.unwrap_or(0); // Default to 0 if not specified, which is ERROR
    configure_log4rs(log_level);
    log::trace!("log4rs configured with verbosity: {}", log_level);

    if let Some(server_addr) = cli.client {
        log::info!(
            "Running in client mode, connecting to server at {} on port {}",
            server_addr,
            cli.port
        );
    } else if cli.server {
        log::info!("Running in server mode, listening on port {}", cli.port);
    } else {
        log::error!("No mode specified, exiting.");
        return;
    }
}

// ////////////////////////////////////////////////////////////////////////////
// Log4rs configuration
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
