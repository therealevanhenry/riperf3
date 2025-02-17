// The main entry point for the riperf3-cli application.

// Use clap to parse the command line arguments.
use clap::Parser;

// Use log and log4rs for logging.
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

// Use riperf3 for the necessary riper3 types and functions.
use riperf3::utils::set_verbose;
use riperf3::vprintln;

// riperf3 CLI module
mod cli;
use cli::Cli;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse the command line arguments
    let cli = Cli::parse();

    // Set the verbose flag
    set_verbose(cli.verbose);
    vprintln!("Verbose mode enabled.");

    // Configure log4rs
    let log_level = cli.debug.unwrap_or(0); // Default to 0 if not specified, which is ERROR
    configure_log4rs(log_level);
    log::debug!("Log level set to: {}", log_level);

    // Check the mode we are running in
    if cli.server {
        // If the server argument was passed, we are in server mode
        use riperf3::ServerBuilder;

        // Create a new ServerBuilder
        let mut server_builder = ServerBuilder::new();

        // Set the port if it was specified
        if cli.port.is_some() {
            server_builder = server_builder.port(cli.port);
        }

        // Build the Server
        let server = server_builder.build()?;

        // Run the server
        server.run().await?;
    } else {
        // Since server was false, we are in client mode so create a new ClientBuilder
        use riperf3::ClientBuilder;

        // Create a new ClientBuilder
        let mut client_builder = ClientBuilder::new(cli.client);

        // Set the port if it was specified
        if cli.port.is_some() {
            client_builder = client_builder.port(cli.port);
        }

        // Build the Client
        let client = client_builder.build()?;

        // Run the client
        client.run().await?;
    }

    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// Log4rs configuration ////////////////////////////////////////////////////////
////////////////////////////////////////////////////////////////////////////////
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
