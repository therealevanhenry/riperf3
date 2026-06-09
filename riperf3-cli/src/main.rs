use clap::Parser;
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

mod cli;
use cli::Cli;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    configure_log4rs(cli.debug.unwrap_or(0));

    // Reject client-only options on the server (#65) before any side effects
    // (pidfile/logfile writes, CPU affinity, runtime build), mirroring iperf3,
    // which raises IECLIENTONLY at parse time — before it applies affinity or
    // does any work. The message embeds iperf3's canonical IECLIENTONLY text as
    // a substring (so anything matching iperf3's string still matches) and adds
    // the offending flag name, which iperf3 omits.
    if cli.server {
        if let Some(flag) = cli.first_client_only_violation() {
            return Err(format!(
                "some option you are trying to set is client only: \
                 {flag} cannot be used with -s/--server"
            )
            .into());
        }
    }

    // Reject server-only options on the client (#100), symmetric to the
    // client-only check above. iperf3 raises IESERVERONLY at parse time for any
    // option whose parse arm sets `server_flag` (e.g. -D, -1, --idle-timeout,
    // --rsa-private-key-path, --use-pkcs1-padding); mirror that exact set so a
    // riperf3 client rejects the same options, before any side effects.
    if cli.client.is_some() {
        if let Some(flag) = cli.first_server_only_violation() {
            return Err(format!(
                "some option you are trying to set is server only: \
                 {flag} cannot be used with -c/--client"
            )
            .into());
        }
        // Conflicting end conditions (#140): iperf3 raises IEENDCONDITIONS in
        // parse_arguments — before pidfile/logfile/affinity — so this check
        // also runs ahead of the side effects below.
        if cli.end_conditions_conflict() {
            return Err(cli::END_CONDITIONS_MSG.into());
        }
    }

    // Daemonize BEFORE building the tokio runtime. `daemon()` forks, and forking
    // a process that already has a multi-threaded runtime leaves the child with
    // only the calling thread — no tokio workers — so the daemon would accept the
    // control connection but never serve (#81). Doing it here, in the binary,
    // also keeps the library's async `Server::run()` free of a process-level fork
    // it cannot perform safely from inside the runtime. Matches iperf3's
    // `daemon(1, 0)`: nochdir=true (keep CWD so a relative `-I`/`--logfile` path
    // resolves as given) and noclose=false (redirect std{in,out,err} to
    // /dev/null). The pidfile and logfile are set up just below — AFTER the fork —
    // so the pidfile records the daemon child's pid and the logfile redirect
    // survives daemon()'s stdout->/dev/null (iperf3 likewise creates its pidfile
    // after daemon()).
    if cli.server && cli.daemon {
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
        nix::unistd::daemon(true, false).map_err(|e| format!("failed to daemonize: {e}"))?;
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd")))]
        return Err("daemon mode is not supported on this platform".into());
    }

    // Write the PID file (after daemonizing, so it records the daemon child's
    // pid — the long-lived server — not the foreground process that forked away).
    if let Some(ref path) = cli.pidfile {
        std::fs::write(path, format!("{}\n", std::process::id()))?;
    }

    // Redirect stdout to logfile if requested. Must run after daemonizing so the
    // redirect isn't clobbered by daemon()'s stdout->/dev/null.
    if let Some(ref path) = cli.logfile {
        #[cfg(unix)]
        {
            use std::fs::OpenOptions;
            use std::os::unix::io::{AsRawFd, IntoRawFd};
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            let fd = file.into_raw_fd();
            nix::unistd::dup2(fd, std::io::stdout().as_raw_fd())?;
            nix::unistd::close(fd)?;
        }
        #[cfg(not(unix))]
        {
            eprintln!("warning: --logfile uses dup2 and is not supported on this platform");
            let _ = path;
        }
    }

    // Set CPU affinity BEFORE building the tokio runtime so worker threads
    // inherit the affinity mask from the main thread.
    if let Some(ref spec) = cli.affinity {
        if let Some(core_str) = spec.split(',').next() {
            if let Ok(core) = core_str.parse::<usize>() {
                riperf3::set_cpu_affinity(core)?;
            }
        }
    }

    // Build tokio runtime manually (instead of #[tokio::main]) so that
    // CPU affinity is set before worker threads are spawned.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if cli.client.is_some() {
        // Client mode. Client-only options on a server and server-only options
        // on a client are both rejected up front in `main` (#65/#100), before
        // any side effects. The arg→builder mapping lives in `Cli::build_client`
        // (cli.rs) so the wiring tests exercise the same code path (#124).
        cli.build_client()?.run().await?;
    } else if cli.server {
        // Server mode. See `Cli::build_server` (cli.rs). `-D`/`--daemon` is
        // handled before the runtime is built (daemonize block in `main`).
        cli.build_server()?.run().await?;
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
