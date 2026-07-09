//! The result of a test run (#293).
//!
//! Before 0.9.0, [`Client::run`](crate::Client::run) and
//! [`Server::run_once`](crate::Server::run_once) returned `Result<Report>` and
//! signaled an abnormal end inconsistently: a local signal came back as
//! `Ok(Report)` with the partial data, but a server-terminated or
//! server-relayed-error run came back as `Err` — with the partial report
//! built, printed, then discarded, unreachable to a library caller.
//!
//! [`RunOutcome`] unifies that: the four common endings — a clean run, a local
//! signal, a server-terminate, and a relayed `SERVER_ERROR` — all return
//! `Ok(RunOutcome)` carrying both the [`Report`] and a [`Termination`] saying
//! how it ended. `Err` covers runs that produced no report (a failed connect
//! or control handshake). A caller that only wants the data reads
//! `outcome.report`; one that needs to branch on the ending matches
//! `outcome.termination`.
//!
//! Two rarer abnormal endings are NOT yet folded in and still return `Err`
//! even though they emit a populated document in the JSON modes:
//! `RiperfError::ControlSocketClosed` (#267) and `RiperfError::RecvResultsFailed`
//! (#374). Folding those (and the `Server::run_once` side) into `Termination`
//! is a follow-up; the CLI already renders them faithfully.
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
