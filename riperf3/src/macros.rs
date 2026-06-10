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
                "{}{}",
                $crate::macros::output_title_prefix(),
                format_args!($($arg)*)
            );
            $crate::macros::capture_line(&line);
            println!("{line}");
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

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
