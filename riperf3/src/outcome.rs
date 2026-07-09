//! The result of a test run (#293).
//!
//! Before 0.9.0, [`Client::run`](crate::Client::run) and
//! [`Server::run_once`](crate::Server::run_once) returned `Result<Report>` and
//! signaled an abnormal end inconsistently: a local signal came back as
//! `Ok(Report)` with the partial data, but a server-terminated or
//! server-relayed-error run came back as `Err` — with the partial report
//! built, printed, then discarded, unreachable to a library caller.
//!
//! [`RunOutcome`] unifies that: every run that produced a report returns
//! `Ok(RunOutcome)` carrying both the [`Report`] and a [`Termination`] saying
//! how it ended. `Err` is reserved for runs that produced NO report at all — a
//! failed connect or a control-handshake failure. A caller that only wants the
//! data reads `outcome.report`; one that needs to branch on the ending matches
//! `outcome.termination`.
//!
//! This changes only the Rust return shape: the wire bytes, the text/JSON the
//! CLI prints, and the process exit codes are unchanged (the CLI maps
//! [`Termination`] to the exit code it already produced).

use crate::json_report::Report;

/// How a test run ended (#293).
///
/// `non_exhaustive`: future end states are additive, not breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Termination {
    /// The test ran to completion (iperf3's `IPERF_DONE`). The report is full.
    Completed,

    /// A local termination signal (SIGTERM/SIGINT/SIGHUP) ended the run.
    /// iperf3 treats this as a normal exit (process exit 0); the report carries
    /// the stats accumulated so far (empty if the signal landed before data).
    Interrupted,

    /// The server terminated the test mid-run (`SERVER_TERMINATE`, iperf3's
    /// IESERVERTERM). The report carries the partial LOCAL stats — no peer half.
    ServerTerminated,

    /// The server relayed an error (`SERVER_ERROR`) — e.g. an upfront refusal
    /// (`--server-bitrate-limit`, `--server-max-duration`) or a mid-test
    /// breach. The `String` is iperf3's mapped `iperf_strerror` message.
    ServerError(String),
}

impl Termination {
    /// True for the endings iperf3 treats as a clean process exit (exit 0):
    /// a completed run or a local signal. `ServerTerminated`/`ServerError` are
    /// non-zero-exit endings. The CLI uses this for its exit-code mapping.
    #[must_use]
    pub fn is_clean_exit(&self) -> bool {
        matches!(self, Termination::Completed | Termination::Interrupted)
    }

    /// The iperf3 errexit-line message for an abnormal ending, or `None` for a
    /// clean exit. The CLI renders `riperf3: error - <msg>` from this on the
    /// non-zero-exit paths (the library already emitted its doc/receipt line
    /// during the run). Matches iperf3's `iperf_errexit` text.
    #[must_use]
    pub fn errexit_message(&self) -> Option<String> {
        match self {
            Termination::Completed | Termination::Interrupted => None,
            Termination::ServerTerminated => Some("the server has terminated".to_string()),
            Termination::ServerError(msg) => Some(msg.clone()),
        }
    }
}

/// The outcome of [`Client::run`](crate::Client::run) /
/// [`Server::run_once`](crate::Server::run_once) (#293): the measured
/// [`Report`] plus how the run ended.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOutcome {
    /// The measured report — full on a clean run, partial on an abnormal end.
    /// The same object `-J` / `--json` serializes.
    pub report: Report,

    /// How the run ended.
    pub termination: Termination,
}

impl RunOutcome {
    pub(crate) fn new(report: Report, termination: Termination) -> Self {
        Self {
            report,
            termination,
        }
    }
}
