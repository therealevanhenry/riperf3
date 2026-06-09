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

    // Install the termination-signal handlers BEFORE writing the pidfile,
    // matching iperf3's run() order (iperf_catch_sigend first, create_pidfile
    // after): once the pidfile exists, a SIGTERM is guaranteed to take the
    // clean unlink path — there is no startup window where it gets default
    // disposition and strands a fresh pidfile (#105 review). Stream creation
    // registers the OS sigactions immediately; it needs the runtime context.
    #[cfg(unix)]
    let mut sigend = Sigend::install(&rt).map_err(|e| format!("signal handler setup: {e}"))?;

    // Write the PID file LAST before entering the runtime: after daemonizing
    // (it must record the daemon child's pid), after the logfile/affinity
    // fallible setup (their `?` exits can no longer leak a fresh pidfile),
    // and after the signal handlers above.
    if let Some(ref path) = cli.pidfile {
        std::fs::write(path, format!("{}\n", std::process::id()))?;
    }

    // The pidfile must be unlinked on EVERY exit path — normal completion,
    // error, or termination signal — like iperf3, which deletes it after the
    // server/client loop and in its sigend path (#105). One cleanup point,
    // after the runtime returns.
    let pidfile = cli.pidfile.clone();
    let is_server = cli.server;
    #[cfg(unix)]
    let outcome = rt.block_on(async {
        tokio::select! {
            r = async_main(cli) => r,
            sig = sigend.recv() => Ok(Exit::Signal(sig)),
        }
    });
    #[cfg(not(unix))]
    let outcome = rt.block_on(async_main(cli));
    if let Some(ref path) = pidfile {
        let _ = std::fs::remove_file(path);
    }
    match outcome {
        // iperf3 treats SIGTERM/SIGINT/SIGHUP as a NORMAL exit
        // (iperf_got_sigend → iperf_signormalexit → exit 0), printing an
        // interrupt notice first.
        Ok(Exit::Signal(sig)) => {
            let role = if is_server { "server" } else { "client" };
            eprintln!("riperf3: interrupt - the {role} has terminated by signal {sig}");
            Ok(())
        }
        Ok(Exit::Normal) => Ok(()),
        Err(e) => Err(e),
    }
}

/// How the async role future ended: normally, or cut short by a termination
/// signal (which iperf3 reports and treats as a clean exit — see `main`).
enum Exit {
    Normal,
    #[cfg_attr(not(unix), allow(dead_code))]
    Signal(&'static str),
}

/// The termination-signal set iperf3 catches in `iperf_catch_sigend`
/// (SIGTERM/SIGINT/SIGHUP). The streams are created — and the OS sigactions
/// registered — in `install`, so handler installation can be ordered before
/// pidfile creation; `recv` resolves with iperf3's `strsignal(sig)(num)`
/// rendering for the interrupt notice.
#[cfg(unix)]
struct Sigend {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Sigend {
    fn install(rt: &tokio::runtime::Runtime) -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        let _guard = rt.enter();
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
            hup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.term.recv() => "Terminated(15)",
            _ = self.int.recv() => "Interrupt(2)",
            _ = self.hup.recv() => "Hangup(1)",
        }
    }
}

async fn async_main(cli: Cli) -> std::result::Result<Exit, Box<dyn std::error::Error>> {
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
    Ok(Exit::Normal)
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
