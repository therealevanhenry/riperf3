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

#[doc(hidden)]
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        {
            log::info!($($arg)*);
            println!(
                "{}{}",
                $crate::macros::output_title_prefix(),
                format_args!($($arg)*)
            );
        }
    };
}
