// riperf3/riperf3/src/macros.rs

use std::sync::RwLock;

/// The active `-T/--title` prefix, set for the duration of a client run (#34).
///
/// iperf3 prepends `<title>:  ` (colon + two spaces) to every client output line
/// via `iperf_printf` (iperf_api.c). riperf3 has two output paths — the
/// `vprintln!` macro and the reporter's line printers — so rather than thread the
/// title through every call site (which would change the public reporter
/// signatures and break SemVer on a patch), both paths read this run-scoped
/// global. Process-global is acceptable: human text output already shares stdout,
/// so concurrent client runs in one process can't interleave coherently anyway.
static OUTPUT_TITLE: RwLock<Option<String>> = RwLock::new(None);

/// `--get-server-output` capture sink (#33): when active, the server's report
/// lines (vprintln! + the reporter's `titled` printers) are TEE'd here in
/// addition to stdout — iperf3's `iperf_printf` dual-writes (fprintf + the
/// server_output_list linebuffer; it has never diverted via tmpfile), so the
/// server console stays live while the output also rides the exchange.
/// Shares the OUTPUT_TITLE process-global caveat (one server run at a time).
static OUTPUT_CAPTURE: RwLock<Option<String>> = RwLock::new(None);

/// Tee `line` into the active capture (no-op when none is active); the caller
/// always prints to stdout as well, like iperf3's dual-write.
pub(crate) fn capture_line(line: &str) {
    if let Ok(mut g) = OUTPUT_CAPTURE.write() {
        if let Some(buf) = g.as_mut() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
}

/// RAII capture for the server's `--get-server-output` diversion (#33). Drop
/// clears the sink even on early `?` returns; `take()` finishes the capture
/// and returns the buffered text.
pub(crate) struct OutputCaptureGuard;

impl OutputCaptureGuard {
    pub(crate) fn start() -> Self {
        if let Ok(mut g) = OUTPUT_CAPTURE.write() {
            *g = Some(String::new());
        }
        OutputCaptureGuard
    }

    pub(crate) fn take(self) -> String {
        OUTPUT_CAPTURE
            .write()
            .ok()
            .and_then(|mut g| g.take())
            .unwrap_or_default()
        // self drops here; Drop sees the sink already cleared.
    }
}

impl Drop for OutputCaptureGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = OUTPUT_CAPTURE.write() {
            *g = None;
        }
    }
}

/// `--timestamps` prefix state (#168): set run-scoped like OUTPUT_TITLE; when
/// active, `titled` prepends the rendered prefix to EVERY report line, so the
/// console and the --get-server-output capture both carry it — iperf3 buffers
/// the PREFIXED linebuffer. (Same one-run-at-a-time process-global caveat.)
/// Holds the strftime FORMAT string (#202); iperf3's default is "%c ".
static OUTPUT_TIMESTAMPS: RwLock<Option<String>> = RwLock::new(None);

pub(crate) struct OutputTimestampGuard;

impl OutputTimestampGuard {
    /// Construct ONLY when timestamps are active (callers gate then
    /// `.map(OutputTimestampGuard::set)`): an unconditional clear from a
    /// concurrent in-process run would clobber another run's stored format —
    /// the exact server-clobbers-client topology of the lib's
    /// `timestamps_runs` test (#168 review r1 n3). Mirrors OutputTitleGuard,
    /// whose construct-only-when-titled shape has no such mode.
    pub(crate) fn set(format: &str) -> Self {
        if let Ok(mut g) = OUTPUT_TIMESTAMPS.write() {
            *g = Some(format.to_string());
        }
        OutputTimestampGuard
    }
}

impl Drop for OutputTimestampGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = OUTPUT_TIMESTAMPS.write() {
            *g = None;
        }
    }
}

/// The rendered `--timestamps` prefix for the current line, or "" when off.
/// Rendered from the run's stored strftime FORMAT (#202; unix), with a
/// documented HH:MM:SS fallback on Windows — see `render_timestamp`.
pub(crate) fn output_timestamp_prefix() -> String {
    let fmt = match OUTPUT_TIMESTAMPS.read() {
        Ok(g) => match g.as_deref() {
            Some(f) => f.to_string(),
            None => return String::new(),
        },
        Err(_) => return String::new(),
    };
    render_timestamp(&fmt)
}

/// Unix: localtime + strftime with the GIVEN format, exactly iperf3's
/// iperf_printf (the CLI's bare `--timestamps` default is "%c " — the
/// trailing space lives in the format; user formats are used verbatim)
/// (#202; public via the crate-root re-export, #348).
#[cfg(unix)]
pub fn render_timestamp(fmt: &str) -> String {
    let Ok(cfmt) = std::ffi::CString::new(fmt) else {
        return String::new();
    };
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r fills `tm` from a valid time_t; strftime writes at
    // most buf.len()-1 bytes plus NUL and returns the byte count (0 = didn't
    // fit or empty result — both render as no prefix).
    unsafe {
        if libc::localtime_r(&now, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0u8; 128];
        let n = libc::strftime(buf.as_mut_ptr().cast(), buf.len(), cfmt.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

/// Windows fallback: libc's msvc surface has no strftime/localtime_r, and
/// native Windows has no iperf3 ground truth to match — keep the simple
/// HH:MM:SS UTC shape (#202).
#[cfg(not(unix))]
pub fn render_timestamp(_fmt: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!(
        "{:02}:{:02}:{:02} ",
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Console-quiet flag for library callers (#290): when set (run-scoped, via
/// [`OutputQuietGuard`]), every console `println!`/`eprintln!` the crate makes
/// is skipped — while `log::` records and the `--get-server-output` capture
/// tee keep working, so a quiet server can still relay its report on the
/// wire. Process-global like OUTPUT_TITLE (one run at a time); visible across
/// the reporter task, which a thread-local could not be.
static OUTPUT_QUIET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// True while any active run asked for no console output (#290).
pub(crate) fn output_quiet() -> bool {
    OUTPUT_QUIET.load(std::sync::atomic::Ordering::Relaxed) > 0
}

/// RAII guard arming the run-scoped console-quiet flag (#290). Construct ONLY
/// when quiet is requested (callers gate then `.then(OutputQuietGuard::set)`).
/// A COUNTER, not a bool: an in-process client + bound-server pair each hold
/// a guard, and the first one to finish must not un-silence the other
/// (a bool's drop did exactly that — the closing "iperf Done." leaked).
// The private unit field makes `set()` the ONLY constructor (r1 finding 3):
// a literal `OutputQuietGuard` elsewhere in the crate would skip the
// increment and its Drop would underflow the counter into permanent silence.
pub(crate) struct OutputQuietGuard(());

impl OutputQuietGuard {
    pub(crate) fn set() -> Self {
        OUTPUT_QUIET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        OutputQuietGuard(())
    }
}

impl Drop for OutputQuietGuard {
    fn drop(&mut self) {
        OUTPUT_QUIET.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The `--logfile` error-line sink (#364): when armed, the crate's
/// iperf_err-shaped stderr lines (e.g. the client's SERVER-ERROR relay
/// receipt) append to this file instead — iperf3's iperf_err writes to
/// test->outfile whenever a logfile is open (iperf_error.c:67-71), the same
/// chooser its exit paths use. The binary arms this alongside its --logfile
/// stdout redirect; the lib itself has no logfile flag (#198).
/// Process-global like OUTPUT_TITLE (one run at a time).
static ERR_SINK: RwLock<Option<String>> = RwLock::new(None);

/// RAII guard routing this crate's error lines to a logfile instead of
/// stderr, matching iperf3's `--logfile` behavior (its iperf_err writes to
/// the logfile whenever one is open). Currently routed: the client's
/// `SERVER ERROR - …` relay receipt; the crate's remaining stderr
/// diagnostics are tracked in #398 and still print to stderr. A quiet run
/// (#290) suppresses these lines entirely — quiet wins over the sink.
/// Lines are appended; stderr remains the fallback when the file cannot be
/// opened. Construct ONLY when a logfile is active (callers gate then
/// `.map(ErrorSinkGuard::set)`); Drop restores stderr routing.
/// Process-global: one run at a time.
// The private unit field makes `set()` the only constructor, like
// OutputQuietGuard — a literal construction elsewhere would pair a no-op
// arm with a sink-clearing Drop.
#[must_use = "the sink disarms when the guard drops — bind it for the run's duration"]
pub struct ErrorSinkGuard(());

impl ErrorSinkGuard {
    /// Arm the sink: error lines append to the file at `path` until drop.
    pub fn set(path: &str) -> Self {
        if let Ok(mut g) = ERR_SINK.write() {
            *g = Some(path.to_string());
        }
        ErrorSinkGuard(())
    }
}

impl Drop for ErrorSinkGuard {
    fn drop(&mut self) {
        if let Ok(mut g) = ERR_SINK.write() {
            *g = None;
        }
    }
}

/// Print one fully-formatted error line to the active sink (#364): append
/// to the armed logfile, else stderr — stderr is also the fallback when the
/// file cannot be opened, like the CLI's errexit sink (#198).
pub(crate) fn err_println(line: &str) {
    if let Ok(g) = ERR_SINK.read() {
        if let Some(path) = g.as_deref() {
            use std::io::Write;
            let logged = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if logged.is_ok() {
                return;
            }
        }
    }
    eprintln!("{line}");
}

pub(crate) fn set_output_title(title: Option<String>) {
    if let Ok(mut g) = OUTPUT_TITLE.write() {
        *g = title;
    }
}

/// The `"<title>:  "` prefix for client output lines, or `""` when no title is
/// set. Colon followed by two spaces, matching iperf3.
pub(crate) fn output_title_prefix() -> String {
    match OUTPUT_TITLE.read() {
        Ok(g) => g.as_deref().map(|t| format!("{t}:  ")).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// RAII guard that clears the run-scoped title on drop, so a title can't leak
/// into a later run (e.g. a server run) in the same process — even on an early
/// `?` return or panic.
pub(crate) struct OutputTitleGuard;

impl OutputTitleGuard {
    pub(crate) fn set(title: Option<String>) -> Self {
        set_output_title(title);
        OutputTitleGuard
    }
}

impl Drop for OutputTitleGuard {
    fn drop(&mut self) {
        set_output_title(None);
    }
}

#[doc(hidden)]
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        {
            log::info!($($arg)*);
            let line = format!(
                "{}{}{}",
                $crate::macros::output_timestamp_prefix(),
                $crate::macros::output_title_prefix(),
                format_args!($($arg)*)
            );
            $crate::macros::capture_line(&line);
            // #290: a quiet run logs + captures but never touches stdout.
            if !$crate::macros::output_quiet() {
                println!("{line}");
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #202: the prefix renders the STORED strftime format (was hardcoded
    /// HH:MM:SS UTC ignoring the argument).
    #[cfg(unix)]
    #[test]
    fn timestamp_prefix_honors_the_format() {
        let _g = OutputTimestampGuard::set("%Y ");
        let p = output_timestamp_prefix();
        assert!(
            p.len() == 5 && p[..4].bytes().all(|b| b.is_ascii_digit()) && p.ends_with(' '),
            "%Y must render the 4-digit year: {p:?}"
        );
        drop(_g);
        assert_eq!(output_timestamp_prefix(), "", "cleared on drop");
        let _g = OutputTimestampGuard::set("%c ");
        assert!(
            !output_timestamp_prefix().is_empty(),
            "the %c default renders non-empty"
        );
    }

    // #34: iperf3 prefixes client lines with "<title>:  " (colon + two spaces),
    // and the prefix must be cleared when the run ends so it can't leak.
    #[test]
    fn title_prefix_matches_iperf3_and_clears() {
        {
            let _g = OutputTitleGuard::set(Some("my test".to_string()));
            assert_eq!(output_title_prefix(), "my test:  ");
        }
        // Guard dropped → prefix cleared.
        assert_eq!(output_title_prefix(), "");
    }
}
