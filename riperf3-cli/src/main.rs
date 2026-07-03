use clap::Parser;
use log::LevelFilter;
use log4rs::append::console::ConsoleAppender;
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

mod cli;
use cli::Cli;

fn main() -> std::process::ExitCode {
    // iperf3's getopt path exits 1 on usage errors (clap defaults to 2), and
    // a bare invocation raises its exact parameter-error sentence (#198).
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    let _ = e.print();
                    return std::process::ExitCode::SUCCESS;
                }
                // Wording-coupled detection (r1 n5): holds while the mode
                // group is the only required argument; the usage_errors test
                // pins it against a clap bump changing the rendering.
                ErrorKind::MissingRequiredArgument
                    if e.to_string().contains("--server") && e.to_string().contains("--client") =>
                {
                    eprintln!(
                        "riperf3: parameter error - must either be a client (-c) or server (-s)"
                    );
                    print_usage_trailer();
                    return std::process::ExitCode::FAILURE;
                }
                ErrorKind::ArgumentConflict
                    if e.to_string().contains("--server") && e.to_string().contains("--client") =>
                {
                    // iperf3's IESERVCLIENT (live: exit 1 + usage trailer).
                    eprintln!("riperf3: parameter error - cannot be both server and client");
                    print_usage_trailer();
                    return std::process::ExitCode::FAILURE;
                }
                _ => {
                    let _ = e.print();
                    return std::process::ExitCode::FAILURE;
                }
            }
        }
    };
    // Parse-class rejections (#65/#100/#140) resolve BEFORE the sink
    // dispatch below: iperf3's iperf_exit only emits the JSON document when
    // json_top exists and only writes the logfile when outfile is open —
    // both post-parse — so parse-time errors go to STDERR in every mode
    // (#198 review r1 f1, live-verified: `iperf3 -s -t 5 -J` errors in
    // plain text on stderr with empty stdout).
    // #259: GT's post-parse range validations (iperf_api.c:1386/1588/1596,
    // MAX_TIME = 86400) — parameter-error class: GT wording + the usage
    // trailer + exit 1, in every mode (parse-time precedes the sink choice).
    // Ordered BEFORE parse_class_rejection: GT's range checks fire inside
    // its getopt loop, ahead of the client-flag-on-server class (r1 F5).
    // RECORDED DEVIATION (r1 F4): with TWO violating flags GT reports the
    // command-line-FIRST one (per-flag getopt checks); riperf3 checks in a
    // fixed order (duration, idle-timeout, format, reverse+bidir) — clap's
    // derive parse has no
    // cheap arg-position access, and the divergence needs two simultaneously
    // invalid flags.
    const MAX_TIME_SECS: i64 = 86_400;
    // #303 item 3: the args parse as i64 with negatives allowed, so GT's
    // atoi-wrapped negative arms land in the SAME range checks and wordings
    // (live-probed: `-t -1` → the duration sentence, `--idle-timeout -5` →
    // the idle sentence, `-O -3`/`-O 700` → IEOMIT's bogus-value sentence).
    let range_violation = if cli.time.is_some_and(|t| !(0..=MAX_TIME_SECS).contains(&t))
        || cli
            .server_max_duration
            .is_some_and(|d| !(0..=MAX_TIME_SECS).contains(&d))
    {
        Some("test duration valid values are 0 to 86400 seconds")
    } else if cli
        .idle_timeout
        .is_some_and(|t| !(1..=MAX_TIME_SECS).contains(&t))
    {
        Some("idle timeout parameter is not positive or larger than allowed limit")
    } else if cli.omit.is_some_and(|o| !(0..=600).contains(&o)) {
        // GT's IEOMIT (MAX_OMIT_TIME 600, iperf.h:473), in-loop like the rest.
        Some("bogus value for --omit (maximum = 600 seconds)")
    } else if cli
        .format
        .as_deref()
        .is_some_and(|f| cli::parse_format_char(f).is_none())
    {
        // #263: GT's IEBADFORMAT — only the FIRST character of the argument
        // is inspected (iperf_api.c:1241), and [kmgtKMGT] is the whole set.
        Some("bad format specifier (valid formats are in the set [kmgtKMGT])")
    } else if cli.reverse && cli.bidir {
        // #309: GT's IEREVERSEBIDIR — the second of the pair is rejected
        // inside the getopt loop (iperf_api.c:1423/:1431), either order.
        Some("cannot be both reverse and bidirectional")
    } else {
        None
    };
    if let Some(msg) = range_violation {
        eprintln!("riperf3: parameter error - {msg}");
        print_usage_trailer();
        return std::process::ExitCode::FAILURE;
    }

    if let Some(msg) = parse_class_rejection(&cli) {
        // #270: GT routes the parse-error class through 'parameter error - '
        // with the usage trailer (live-probed for all three classes here:
        // end-conditions, client-only, server-only).
        eprintln!("riperf3: parameter error - {msg}");
        print_usage_trailer();
        return std::process::ExitCode::FAILURE;
    }

    // #263: GT warns when an explicit -f rides JSON output — end of
    // iperf_parse_arguments (iperf_api.c:2015-2017), both roles, and
    // --json-stream sets json_output too (:1281). GT's warning() is a bare
    // `warning: %s` fprintf to stderr, bypassing every sink (the -J document
    // and --logfile included).
    if (cli.json || cli.json_stream) && cli.format.is_some() {
        eprintln!("warning: Report format (-f) flag ignored with JSON output (-J)");
    }

    // The error SINK is chosen by mode, like iperf_errexit (#198): -J puts
    // the message in a JSON document on stdout (nothing on stderr),
    // --json-stream emits an error event + empty end event, --logfile gets
    // the text line when set, plain text goes to stderr (#151).
    let json = cli.json;
    let json_stream = cli.json_stream;
    let logfile = cli.logfile.clone();
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            // #225: on ServerTerminated the lib has ALREADY rendered the
            // partial report — error key inside — into the active JSON sink
            // before returning; re-rendering here concatenated a second
            // document (or a second error+end event pair), which breaks
            // every -J consumer. iperf3 emits exactly one. Text mode is
            // exempt: its stderr line is iperf3's errexit shape, and the
            // lib's text dump carries no error line.
            let lib_already_rendered = e.downcast_ref::<riperf3::RiperfError>().is_some_and(|le| {
                matches!(
                    le,
                    riperf3::RiperfError::ServerTerminated
                        | riperf3::RiperfError::ServerErrorRelayed(_)
                        // #267: the lib emits the populated ctrl-closed doc
                        // (bare end{}) before returning this class.
                        | riperf3::RiperfError::ControlSocketClosed
                )
            });
            // --json-stream wins over -J when combined: iperf3's
            // --json-stream gates iperf_json_finish into stream events
            // (review r1 f2, live-verified).
            if json_stream {
                if !lib_already_rendered {
                    println!(
                        "{}",
                        riperf3::json_report::error_stream_events(&e.to_string())
                    );
                }
            } else if json {
                if !lib_already_rendered {
                    println!("{}", riperf3::json_report::error_document(&e.to_string()));
                }
            } else {
                let line = format!("riperf3: error - {e}");
                let logged = logfile.as_deref().and_then(|path| {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .and_then(|mut f| writeln!(f, "{line}"))
                        .ok()
                });
                if logged.is_none() {
                    // iperf3's iperf_errexit shape ("iperf3: error - <text>",
                    // exit 1) instead of Rust's Debug rendering; the Display
                    // strings mirror iperf3's IE* wording (#151). Also the
                    // fallback when the logfile cannot be opened.
                    eprintln!("{line}");
                }
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// iperf3's parameter-error usage trailer (usage_shortstr + the --help hint).
fn print_usage_trailer() {
    eprintln!();
    eprintln!("Usage: riperf3 [-s|-c host] [options]");
    eprintln!("Try `riperf3 --help' for more information.");
}

/// The parse-class rejections (#65 client-only-on-server, #100
/// server-only-on-client, #140 conflicting end conditions): iperf3 raises
/// these in parse_arguments, before any output sink exists, so they print to
/// stderr in every mode. The messages embed iperf3's canonical IE* text as a
/// substring and add the offending flag name, which iperf3 omits.
fn parse_class_rejection(cli: &Cli) -> Option<String> {
    if cli.server {
        if let Some(flag) = cli.first_client_only_violation() {
            return Some(format!(
                "some option you are trying to set is client only: \
                 {flag} cannot be used with -s/--server"
            ));
        }
    }
    if cli.client.is_some() {
        if let Some(flag) = cli.first_server_only_violation() {
            return Some(format!(
                "some option you are trying to set is server only: \
                 {flag} cannot be used with -c/--client"
            ));
        }
        if cli.end_conditions_conflict() {
            return Some(cli::END_CONDITIONS_MSG.to_string());
        }
    }
    None
}

fn run(cli: Cli) -> std::result::Result<(), Box<dyn std::error::Error>> {
    configure_log4rs(cli.debug.unwrap_or(0));

    // Reject client-only options on the server (#65) before any side effects
    // (pidfile/logfile writes, CPU affinity, runtime build), mirroring iperf3,
    // which raises IECLIENTONLY at parse time — before it applies affinity or
    // does any work. The message embeds iperf3's canonical IECLIENTONLY text as
    // a substring (so anything matching iperf3's string still matches) and adds
    // the offending flag name, which iperf3 omits.

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
    #[cfg(any(unix, windows))]
    let mut sigend = Sigend::install(&rt).map_err(|e| format!("signal handler setup: {e}"))?;

    // Write the PID file LAST before entering the runtime: after daemonizing
    // (it must record the daemon child's pid), after the logfile/affinity
    // fallible setup (their `?` exits can no longer leak a fresh pidfile),
    // and after the signal handlers above.
    if let Some(ref path) = cli.pidfile {
        std::fs::write(path, format!("{}\n", std::process::id()))?;
    }

    // The pidfile must be unlinked on EVERY exit path — normal completion,
    // error, termination signal, or a panic unwinding through run() (#158):
    // an RAII guard replaces the single post-runtime unlink, so panic=unwind
    // can no longer strand the file (iperf3 leaks on crash; strictly better).
    let _pidfile_guard = PidfileGuard(cli.pidfile.clone().map(std::path::PathBuf::from));
    let pidfile = cli.pidfile.clone();
    let is_server = cli.server;
    let (json, json_stream) = (cli.json, cli.json_stream);
    // #210: the first signal no longer cancels the run outright — it fires
    // this watch with iperf3's formatted message, and the lib dumps the
    // accumulated stats + tells the peer (CLIENT_TERMINATE /
    // SERVER_TERMINATE) like iperf_got_sigend, then returns. The bounded
    // wait below keeps a wedged teardown from hanging the exit; the
    // second-signal hard path remains underneath.
    let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel::<Option<String>>(None);
    #[cfg(any(unix, windows))]
    let outcome = rt.block_on(async {
        // Box::pin: select! polls its futures IN PLACE, and async_main's
        // future is the entire client/server state machine — inline it
        // overflows Windows' 1 MiB main-thread stack (unix's 8 MiB masked
        // it). Heap-pin the big one; the signal future is tiny.
        let mut app = Box::pin(async_main(cli, interrupt_rx));
        tokio::select! {
            r = &mut app => r,
            sig = sigend.recv() => {
                // The SECOND-signal hard exit (#158) must cover the dump
                // window below, not just rt-drop — register the raw handler
                // BEFORE awaiting the dump (the libc overwrite wins over
                // tokio's still-registered sigaction). Windows: the
                // best-effort watcher, spawned for the same window.
                #[cfg(unix)]
                second_signal_exits_immediately(pidfile.as_deref());
                #[cfg(windows)]
                {
                    let pf = pidfile.clone();
                    tokio::spawn(async move {
                        let _ = sigend.recv().await;
                        if let Some(ref path) = pf {
                            let _ = std::fs::remove_file(path);
                        }
                        eprintln!("riperf3: interrupt - second signal, exiting immediately");
                        std::process::exit(1);
                    });
                }
                let role = if is_server { "server" } else { "client" };
                let msg =
                    format!("interrupt - the {role} has terminated by signal {sig}");
                let _ = interrupt_tx.send(Some(msg));
                // Give the run loop a bounded window to dump stats and
                // notify the peer (iperf_got_sigend, #210); a run that is
                // wedged — or idle outside any interrupt-aware await —
                // falls back to the plain signal teardown.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut app).await;
                Ok(Exit::Signal(sig))
            }
        }
    });
    #[cfg(not(any(unix, windows)))]
    let outcome = rt.block_on(async_main(cli, interrupt_rx));

    // A SECOND signal during teardown exits immediately (#158): the first
    // one resolved the select, but a still-blasting UDP peer can hold the
    // shared drain (and thus runtime shutdown) for up to its 10 s timeout —
    // pre-#150 a second Ctrl-C always killed the process, and most daemons
    // treat repeat signals as "now means now".
    //
    // Unix: re-register the raw libc handlers — a runtime-spawned watcher
    // would be cancelled at the start of the very rt-drop hang it exists to
    // escape. The handler is async-signal-safe (write + unlink + _exit
    // only); Drop doesn't run under _exit, so it unlinks the pidfile itself.
    // (#158/#210) The hard second-signal handler was registered INSIDE the
    // signal arm above, before the dump window — it stays armed through
    // rt-drop; nothing further to do here on unix.
    // Windows: best-effort runtime watcher. Once rt-drop cancels it, every
    // listener is gone, tokio's console handler returns 0 ("run the next
    // handler"), and the DEFAULT handler terminates the process — so a
    // second Ctrl+C during a drain hang still exits immediately by
    // fall-through, just without the notice; the pidfile was already
    // unlinked by the guard (drop order: guard before rt).
    #[cfg(not(any(unix, windows)))]
    let _ = &pidfile;
    match outcome {
        // iperf3 treats SIGTERM/SIGINT/SIGHUP as a NORMAL exit
        // (iperf_got_sigend → iperf_signormalexit → exit 0), printing an
        // interrupt notice first.
        Ok(Exit::Signal(sig)) => {
            // In the JSON modes the document/event already carried the
            // message — iperf3's signormalexit prints nothing to stderr
            // there (#210 review r1 f3).
            if !json && !json_stream {
                let role = if is_server { "server" } else { "client" };
                eprintln!("riperf3: interrupt - the {role} has terminated by signal {sig}");
            }
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
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    Signal(&'static str),
}

/// Second-signal hard exit (#158): once the orderly shutdown is underway, a
/// repeat SIGTERM/SIGINT/SIGHUP must not be swallowed by tokio's (now
/// shutting-down) signal machinery. Registers plain libc handlers whose body
/// is async-signal-safe only: write(2) a notice, unlink(2) the pidfile,
/// _exit(1).
#[cfg(unix)]
fn second_signal_exits_immediately(pidfile: Option<&str>) {
    use std::sync::atomic::{AtomicPtr, Ordering};
    static PIDFILE: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" fn hard_exit(sig: libc::c_int) {
        // SAFETY: async-signal-safe calls only (write/unlink/signal/raise/
        // pthread_sigmask/_exit; sigemptyset/sigaddset are pure memory ops).
        unsafe {
            let msg = b"riperf3: interrupt - second signal, exiting immediately\n";
            let _ = libc::write(2, msg.as_ptr().cast(), msg.len());
            let p = PIDFILE.load(Ordering::Acquire);
            if !p.is_null() {
                let _ = libc::unlink(p);
            }
            // Die BY the signal (supervisors distinguish signal-death from
            // exit-1; a pre-#150 second Ctrl-C read as 130 in shells). The
            // kernel BLOCKS the handled signal for the handler's duration,
            // so the re-raise goes pending until it is unblocked — without
            // the sigmask step the _exit fallback always won (r2 finding 2).
            libc::signal(sig, libc::SIG_DFL);
            let _ = libc::raise(sig);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::_exit(1); // now genuinely a fallback
        }
    }

    if let Some(path) = pidfile {
        if let Ok(c) = std::ffi::CString::new(path) {
            // Leaked deliberately: the process exits via _exit shortly; the
            // handler needs a stable pointer.
            PIDFILE.store(c.into_raw(), Ordering::Release);
        }
    }
    // SAFETY: replacing the dispositions with our handler; tokio's streams
    // were dropped by the caller.
    let handler = hard_exit as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

/// Unlinks the pidfile on drop — every exit path, panics included (#158).
struct PidfileGuard(Option<std::path::PathBuf>);

impl Drop for PidfileGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
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

/// Windows console-event analog of the unix set (#158): iperf3's CRT
/// signal() only ever yields Ctrl+C there (SIGTERM is never OS-delivered);
/// the extra events are pidfile hygiene on paths where iperf3 dies dirty.
/// Close/logoff/shutdown's grace window exists only while the handler runs —
/// tokio < 1.44 returned from its HandlerRoutine immediately (losing the
/// race to TerminateProcess); the manifest floors >= 1.44, whose handler
/// parks for those events (tokio #7122), so the clean unlink path holds.
#[cfg(windows)]
struct Sigend {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
    ctrl_close: tokio::signal::windows::CtrlClose,
    ctrl_logoff: tokio::signal::windows::CtrlLogoff,
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Sigend {
    fn install(rt: &tokio::runtime::Runtime) -> std::io::Result<Self> {
        use tokio::signal::windows;
        let _guard = rt.enter();
        Ok(Self {
            ctrl_c: windows::ctrl_c()?,
            ctrl_break: windows::ctrl_break()?,
            ctrl_close: windows::ctrl_close()?,
            ctrl_logoff: windows::ctrl_logoff()?,
            ctrl_shutdown: windows::ctrl_shutdown()?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "Interrupt(2)",
            _ = self.ctrl_break.recv() => "Break",
            _ = self.ctrl_close.recv() => "Close",
            _ = self.ctrl_logoff.recv() => "Logoff",
            _ = self.ctrl_shutdown.recv() => "Shutdown",
        }
    }
}

async fn async_main(
    cli: Cli,
    interrupt: tokio::sync::watch::Receiver<Option<String>>,
) -> std::result::Result<Exit, Box<dyn std::error::Error>> {
    if cli.client.is_some() {
        // Client mode. Client-only options on a server and server-only options
        // on a client are both rejected up front in `main` (#65/#100), before
        // any side effects. The arg→builder mapping lives in `Cli::build_client`
        // (cli.rs) so the wiring tests exercise the same code path (#124).
        cli.build_client()?.with_interrupt(interrupt).run().await?;
    } else if cli.server {
        // Server mode. See `Cli::build_server` (cli.rs). `-D`/`--daemon` is
        // handled before the runtime is built (daemonize block in `main`).
        cli.build_server()?.with_interrupt(interrupt).run().await?;
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
