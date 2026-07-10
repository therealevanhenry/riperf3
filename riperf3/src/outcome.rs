//! The result of a test run (#293).
//!
//! Before 0.9.0, [`Client::run`](crate::Client::run) and
//! [`Server::run_once`](crate::Server::run_once) returned `Result<Report>` and
//! signaled an abnormal end inconsistently: a local signal came back as
//! `Ok(Report)` with the partial data, but a server-terminated or
//! server-relayed-error run came back as `Err` — with the partial report
//! built, printed, then discarded, unreachable to a library caller.
//!
//! [`RunOutcome`] unifies that: a report-producing run — clean or abnormal —
//! returns `Ok(RunOutcome)` carrying both the [`Report`] and a [`Termination`]
//! saying how it ended. The client's endings are a clean run, a local signal, a
//! `SERVER_TERMINATE`, and a relayed `SERVER_ERROR`; the server's are a clean
//! run, a local signal, and each peer/self abnormal end ([`Termination`]
//! enumerates them). `Err` is reserved for a round that produced no report at
//! all — a failed connect or control handshake, or a server round interrupted
//! before any test started. A caller that only wants the data reads
//! `outcome.report`; one that needs to branch on the ending matches
//! `outcome.termination`.
//!
//! Two rarer CLIENT-side abnormal endings are not yet folded in and still
//! return `Err` even though they emit a populated document in the JSON modes:
//! `RiperfError::ControlSocketClosed` (#267) and `RiperfError::RecvResultsFailed`
//! (#374). Folding those client paths into a [`Termination`] is a follow-up;
//! the CLI already renders them faithfully. (The server side is complete — its
//! `ControlClosed`/`RecvResultsFailed` endings are [`Termination`] variants.)
//!
//! This changes only the Rust return shape: the wire bytes, the text/JSON the
//! CLI prints, and the process exit codes are unchanged (the CLI maps
//! [`Termination`] to the exit code it already produced).

use crate::json_report::Report;

/// How a test run ended (#293).
///
/// Shared by [`Client::run`](crate::Client::run) and
/// [`Server::run_once`](crate::Server::run_once). Some variants are role-
/// specific: `ServerTerminated`/`ServerError` only occur on the client side
/// (the server told the client), and `ClientTerminated`/`ControlClosed`/
/// `UnknownMessage`/`RecvResultsFailed`/`SendFailed`/`SelfTerminated` only on
/// the server side (what the server saw). `Completed` and `Interrupted` occur
/// on both.
///
/// `non_exhaustive`: future end states are additive, not breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Termination {
    /// The test ran to completion (iperf3's `IPERF_DONE`). The report is full.
    /// (Both roles.)
    Completed,

    /// A local termination signal (SIGTERM/SIGINT/SIGHUP) ended the run.
    /// iperf3 treats this as a normal exit (process exit 0); the report carries
    /// the stats accumulated so far (empty if the signal landed before data).
    /// (Both roles.)
    Interrupted,

    // --- Client-side endings (the server told the client) ---
    /// The server terminated the test mid-run (`SERVER_TERMINATE`, iperf3's
    /// IESERVERTERM). The report carries the partial LOCAL stats — no peer half.
    ServerTerminated,

    /// The server relayed an error (`SERVER_ERROR`) — e.g. an upfront refusal
    /// (`--server-bitrate-limit`, `--server-max-duration`) or a mid-test
    /// breach. The `String` is iperf3's mapped `iperf_strerror` message.
    /// The report's `end` is BARE (sums/CPU `None`) at any stage — iperf3
    /// never end-processes this kill; the partial stats ride `intervals`
    /// (#404). Contrast [`Self::ServerTerminated`], which iperf3
    /// end-processes into a full `end`.
    ServerError(String),

    // --- Server-side endings (what the server observed) ---
    /// The client sent `CLIENT_TERMINATE` mid-test (iperf3's IECLIENTTERM —
    /// the symmetric counterpart of [`Self::ServerTerminated`]). The report
    /// carries the server's partial stats.
    ClientTerminated,

    /// The client's control connection closed abruptly mid/post-test (iperf3's
    /// IECTRLCLOSE). The report carries the accumulated stats.
    ControlClosed,

    /// The client sent an unrecognized control message (iperf3's IEMESSAGE).
    /// The report carries the accumulated stats.
    UnknownMessage,

    /// The post-test results exchange failed (iperf3's IERECVRESULTS). The
    /// report carries the accumulated stats.
    RecvResultsFailed,

    /// An exchange-phase send to the client failed (iperf3's IESENDMESSAGE /
    /// IESENDRESULTS, #371). The `String` is iperf3's mapped message. The
    /// report carries the accumulated stats.
    SendFailed(String),

    /// The server ended the test on its OWN limit — `--server-bitrate-limit`,
    /// the `--server-max-duration` watchdog, or the idle watchdog (the symmetric
    /// counterpart, server-side, of the client's [`Self::ServerError`]). The
    /// `String` is the reason. The report carries the partial stats.
    SelfTerminated(String),
}

impl Termination {
    /// The iperf3 errexit-line message for a CLIENT abnormal ending that exits
    /// non-zero, or `None` otherwise. The CLI renders `riperf3: error - <msg>`
    /// from this on the client's non-zero-exit paths (the library already
    /// emitted its doc/receipt line during the run). Matches iperf3's
    /// `iperf_errexit` text.
    ///
    /// The server-side endings return `None`: iperf3's server keeps serving
    /// (or a one-off exits 0) even on a failed round — `main` errexits only
    /// on setup failures (rc < -1), the #224 wart — so they do not drive a
    /// non-zero exit through this path.
    #[must_use]
    pub fn errexit_message(&self) -> Option<&str> {
        match self {
            Termination::ServerTerminated => Some("the server has terminated"),
            Termination::ServerError(msg) => Some(msg),
            // Clean exits + all server-side endings (no client errexit).
            Termination::Completed
            | Termination::Interrupted
            | Termination::ClientTerminated
            | Termination::ControlClosed
            | Termination::UnknownMessage
            | Termination::RecvResultsFailed
            | Termination::SendFailed(_)
            | Termination::SelfTerminated(_) => None,
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

#[cfg(test)]
mod tests {
    use super::Termination;

    /// The errexit contract: only the two client-side "the server ended it"
    /// classes drive a non-zero exit through this path — iperf3's server
    /// keeps serving (one-off exits 0) on a failed round, the #224 wart, so
    /// every server-side ending is `None`. The CLI relies on the split; a
    /// variant drifting across it flips a process exit code.
    #[test]
    fn errexit_message_is_some_only_for_the_client_server_ended_classes() {
        assert_eq!(
            Termination::ServerTerminated.errexit_message(),
            Some("the server has terminated"),
        );
        assert_eq!(
            Termination::ServerError("busy".into()).errexit_message(),
            Some("busy"),
        );
        for none in [
            Termination::Completed,
            Termination::Interrupted,
            Termination::ClientTerminated,
            Termination::ControlClosed,
            Termination::UnknownMessage,
            Termination::RecvResultsFailed,
            Termination::SendFailed("send failed".into()),
            Termination::SelfTerminated("limit".into()),
        ] {
            assert_eq!(none.errexit_message(), None, "{none:?} must not errexit");
        }
    }
}
