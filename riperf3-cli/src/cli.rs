use clap::{ArgGroup, Parser};

#[derive(Parser, Debug)]
#[command(about, author, long_about = None, version)]
#[command(group(
        ArgGroup::new("mode")
            .required(true)
            .args(&["server", "client"])
))]

// The main CLI struct for the riperf3-cli application.
pub struct Cli {
    /// Run in server mode
    #[arg(short, long, group = "mode")]
    pub server: bool,

    /// Run in client mode
    #[arg(short, long, group = "mode", value_name = "host")]
    pub client: Option<String>,

    /// Port number to connect to or listen on
    #[arg(
        short,
        long,
        default_value_t = 5201,
        help = "server port to listen on/connect to"
    )]
    pub port: u16,

    /// Emit debugging output (optional "=" and debug level: 1-4. Default is 4 - all messages)
    #[arg(
        short,
        long,
        value_name = "level",
        num_args = 0..=1,
        value_parser = clap::value_parser!(u8).range(1..=4),
        default_missing_value = "4",
        help = "emit debugging output (optional '=' and debug level: 1-4. Default is 4 - all messages)"
    )]
    pub debug: Option<u8>,
}
