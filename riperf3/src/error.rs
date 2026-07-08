use std::fmt;

use thiserror::Error;

/// Configuration errors — validation failures when building Client/Server.
/// Kept separate so existing tests that match on these variants continue to work.
#[derive(Error, Debug, PartialEq)]
#[non_exhaustive] // future validation variants must be additive, not breaking (#100, #45)
pub enum ConfigError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error("invalid value for {0}: {1}")]
    InvalidValue(&'static str, String),

    #[error("{0}")]
    Unsupported(String),
}

/// Runtime errors covering I/O, protocol, and JSON failures.
#[derive(Error, Debug)]
#[non_exhaustive] // future error variants must be additive, not breaking
pub enum RiperfError {
    #[error("{0}")]
    Config(#[from] ConfigError),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("cookie mismatch")]
    CookieMismatch,

    #[error("access denied by server")]
    AccessDenied,

    #[error("connection timed out")]
    ConnectionTimeout,

    #[error("protocol violation: {0}")]
    Protocol(String),

    #[error("test aborted: {0}")]
    Aborted(String),

    /// iperf3's IECTRLCLOSE: the control connection died mid-test — the
    /// client's class (#170), and since #330 the SERVER's doc'd mid/end-loop
    /// EOF return from `run_once` (the doc/stderr carry GT's read-site
    /// sentence "the client has unexpectedly closed the connection"; the
    /// round is clean in GT, exit 0).
    #[error("control socket has closed unexpectedly")]
    ControlSocketClosed,

    /// iperf3's IERECVRESULTS: the results exchange failed — riperf3 raises
    /// it for a malformed exchange (#271: exactly one omitted_* key present,
    /// GT iperf_api.c:2888-2892). GT's sentence, no "protocol violation"
    /// wrapper (#151 convention).
    #[error("unable to receive results")]
    RecvResultsFailed,

    /// iperf3's IERECVCOOKIE(106): the server's initial cookie read failed —
    /// a truncated or absent cookie, a port scan, a peer that closed before
    /// the 37-byte cookie arrived, or a timed-out read (GT bounds every
    /// Nread, net.c:75-76; its IERECVCOOKIE comment names the timeout case,
    /// iperf_server_api.c:194-200). GT's iperf_accept sets it, cleanup_server
    /// relays SERVER_ERROR(-2) + the code, and the surface is exit-0
    /// keep-serving with the message in the -J skeleton doc / one text line
    /// (#330). GT's sentence, no "protocol violation" wrapper (#151), perr=1
    /// so the emit sites carry the #248 dangling ": ".
    #[error("unable to receive cookie at server")]
    RecvCookieFailed,

    /// iperf3's IERECVCOOKIE(106) from the SETUP phase's data-stream cookie
    /// gate (#359): a HARD read error mid-cookie (e.g. ECONNRESET) takes
    /// GT's cleanup_server round-kill (iperf_tcp.c:155-159) — unlike the
    /// pre-test ctrl-cookie class above (skeleton doc), this one carries
    /// the POPULATED setup doc, so the serve loop must tell them apart.
    /// Same wire sentence and perr dangling as [`Self::RecvCookieFailed`].
    #[error("unable to receive cookie at server")]
    RecvDataCookieFailed,

    /// iperf3's IERECVPARAMS(114): the server could not read or parse the
    /// client's ParamExchange blob — malformed JSON, a short/absent
    /// length-prefixed body, or a timed-out read (bounded like every GT
    /// Nread). GT's get_parameters sets it whenever JSON_read
    /// returns NULL (a read failure OR a cJSON parse failure alike); the
    /// surface mirrors IERECVCOOKIE (#330). GT's sentence (#151), perr=1.
    #[error("unable to receive parameters from client")]
    RecvParamsFailed,

    /// iperf3's IESENDMESSAGE(111): a control-channel send failed — the
    /// post-cookie ParamExchange state write (GT's iperf_set_send_state
    /// failure path inside iperf_accept, #345). Unlike the errno-0 dangling
    /// `: ` siblings above, GT's suffix here is a LIVE deterministic
    /// strerror, so the wrapped io::Error rides the Display. RECORDED
    /// DEVIATION (r1 F2): the errno CLASS itself diverges, not just the
    /// "(os error N)" tail — GT's select loop sees the RST before its write
    /// (ENOTCONN); riperf3's write_all is the first post-cookie op and eats
    /// it (ECONNRESET). Both are honest live errnos for the same peer RST.
    /// Relay + exit-0 keep-serving like the sibling pre-test classes (#330).
    #[error("unable to send control message - port may not be available, the other side may have stopped running, etc.: {0}")]
    SendControlFailed(std::io::Error),

    /// iperf3's IESENDMESSAGE(111) on the POST-TEST_END exchange phase — the
    /// `send_state(EXCHANGE_RESULTS)` / `send_state(DISPLAY_RESULTS)` writes
    /// failing against a peer that RST the control socket after TEST_END
    /// (#371). Distinct from the pre-test [`Self::SendControlFailed`] only in
    /// PHASE: the reporter already ran at TEST_END, so GT's json_finish emits
    /// the POPULATED doc (start/intervals/end) + this error key, where the
    /// pre-test sibling emits the skeleton. Same GT sentence (IESENDMESSAGE,
    /// perr). Live errno rides per the #345 honest-errno convention (GT's
    /// stale-global "Transport endpoint is not connected" is not mirrored).
    #[error("unable to send control message - port may not be available, the other side may have stopped running, etc.: {0}")]
    ExchangeSendMessageFailed(std::io::Error),

    /// iperf3's IESENDRESULTS(116, iperf_api.h:465): the `send_results` write
    /// in the post-TEST_END exchange failed (#371). GT's sentence, perr; the
    /// populated-doc surface of [`Self::ExchangeSendMessageFailed`]. (In
    /// practice the buffered results write often succeeds and the failure
    /// lands on the following state send → IESENDMESSAGE; this class is the
    /// faithful mapping for the write that does fail.)
    #[error("unable to send results: {0}")]
    ExchangeSendResultsFailed(std::io::Error),

    /// iperf3's IEACCEPT(104): the control accept() failed
    /// (iperf_server_api.c:163; herr+perr). The site-captured errno rides
    /// — and MATCHES GT's text surface in this pre-test cell (#387 r2 F1
    /// live probes, all four sinks: GT prints the LIVE strerror here;
    /// nothing gets closed before the print, so no clobber — the clobber
    /// is real only in the mid-setup double-close cells, see
    /// [`Self::StreamConnectFailed`]). BSD-class reachable (ECONNABORTED);
    /// Linux EMFILE (#362 — previously the raw io line on the generic
    /// arm).
    #[error("unable to accept connection from client: {0}")]
    AcceptFailed(std::io::Error),

    /// iperf3's IESTREAMCONNECT(203): the SETUP data-stream accept()
    /// failed (iperf_tcp.c:134-135) — GT's cleanup_server round-kill with
    /// the fe+203+LIVE-errno wire-back and the populated setup doc (#362,
    /// the PR #384 r2 F4 cell). The site-captured errno rides text and
    /// wire (GT's TEXT surface prints the clobbered post-cleanup errno
    /// but WIRES the live one — #387 r1 F2/F6).
    #[error("unable to connect stream: {0}")]
    StreamConnectFailed(std::io::Error),

    /// iperf3's IESETNODELAY(122, iperf_api.h:471): TCP_NODELAY on the
    /// just-accepted control socket failed (iperf_server_api.c:170-173;
    /// perr) — the #362 macOS kind-only-InvalidInput cell's likeliest
    /// site. The fe+122+errno relay is best-effort on the failing ctrl.
    #[error("unable to set TCP/SCTP NODELAY: {0}")]
    SetNoDelayFailed(std::io::Error),

    /// iperf3's IENOMSG(144): the server's no-progress watchdog fired — no
    /// messages/data received within `--rcv-timeout` (default 120000 ms, GT's
    /// DEFAULT_NO_MSG_RCVD_TIMEOUT, iperf_api.h:70). First wired at the
    /// CREATE_STREAMS wait (#338; the TEST_RUNNING half is #351). GT relays
    /// SERVER_ERROR + 144 via cleanup_server and keeps serving: text
    /// `iperf3: error - <sentence>`, -J `error - `-prefixed doc key, exit 0.
    /// GT's sentence (#151 convention).
    #[error("idle timeout for receiving data")]
    DataIdleTimeout,

    /// iperf3's IEMESSAGE (#325): an unhandled control byte. GT's end-loop
    /// switch has arms for only TEST_START / TEST_END / IPERF_DONE /
    /// CLIENT_TERMINATE — every other value, known state or not, hits
    /// `default: i_errno = IEMESSAGE` (iperf_server_api.c:309-311). The
    /// CLIENT's message handler defaults to the same code
    /// (iperf_client_api.c:409-411), so recv_state surfaces this on both
    /// roles — the client's stderr line, -J error key (BARE sentence, no
    /// `error - ` prefix), and exit 1 match GT (r2/r3-verified); the
    /// client's -J doc BODY is the skeleton error_document where GT emits
    /// its accumulated doc — the #330 item-2 gap, tracked there. GT's
    /// sentence, no "protocol violation" wrapper (#151 convention).
    #[error("received an unknown control message (ensure other side is iperf3 and not iperf)")]
    UnknownControlMessage,

    /// iperf3's IESERVERTERM: the server sent SERVER_TERMINATE mid-test; a
    /// partial summary is rendered from local data before this surfaces (#170).
    #[error("the server has terminated")]
    ServerTerminated,

    /// iperf3's IECLIENTTERM: the client sent CLIENT_TERMINATE mid-test; the
    /// server dumps its partial results before this surfaces (#210). iperf3
    /// prints it WITHOUT the "error - " prefix ("iperf3: the client has
    /// terminated").
    #[error("the client has terminated")]
    ClientTerminated,

    /// #267: GT's IECTRLCLOSE wording — the class covers any abrupt loss of
    /// the control connection (iperf_error.c).
    #[error("control socket has closed unexpectedly")]
    PeerDisconnected,

    /// iperf3's SERVER_ERROR relay (#224): the server failed mid-test and
    /// sent its (i_errno, errno) pair on the control connection; the client
    /// ADOPTS the mapped iperf_strerror text as its own error, exactly like
    /// iperf_handle_message_client (iperf_client_api.c:392). The Display is
    /// the mapped message alone — the CLI prefixes it ("riperf3: error - …"),
    /// matching iperf3's errexit line.
    #[error("{0}")]
    ServerErrorRelayed(String),
}

/// iperf3's SERVER_ERROR relay rendering, client side (#224). The errno
/// append follows iperf_handle_message_client (iperf_client_api.c:403-404),
/// which appends ", errno: <strerror>" UNCONDITIONALLY whenever the relayed
/// os errno is non-zero — for any code (r1 review, live-verified; real
/// iperf3 servers relay stale/live errno from the bitrate and
/// cleanup_server sites). Rendered via io::Error, whose "(os error N)"
/// suffix is the convention riperf3's raw os errors already carry (#151).
/// PERR SUFFIX (#248): iperf_strerror's perr-class codes emit a dangling ": "
/// even at errno==0 (live: "server test duration expired: "). Mirrored per-code
/// from the d39cf41 iperf_error.c switch — 160 and the int_errno default are
/// perr=1 (they get the suffix); 27/37/120 are perr=0 (snprintf-only, stay
/// bare). Unknown codes use iperf3's literal "int_errno=%d" fallback (the
/// default case, which is perr=1).
pub(crate) fn iperf3_strerror(i_errno: u32, os_errno: u32) -> String {
    // `perr` is iperf_error.c's per-code flag: when set, GT dangles a trailing
    // ": " even at errno==0.
    let (base, perr) = match i_errno {
        // IETOTALRATE — the --server-bitrate-limit breach (perr=0)
        27 => (
            "total required bandwidth is larger than server limit".to_string(),
            false,
        ),
        // IEMAXSERVERTESTDURATIONEXCEEDED — the upfront param-exchange
        // reject modern iperf3 servers send for over-limit or unbounded
        // requests. riperf3's own upfront check is #230, but a real iperf3
        // server can relay this TODAY, so the client must render it. (perr=0)
        37 => (
            "client's requested duration exceeds the server's maximum permitted limit".to_string(),
            false,
        ),
        // IESERVERTERM — a server relaying its own terminate as an error (perr=0)
        120 => ("the server has terminated".to_string(), false),
        // IESERVERTESTDURATIONEXPIRED — the --server-max-duration timer (perr=1)
        160 => ("server test duration expired".to_string(), true),
        // iperf3's default case is perr=1.
        other => (format!("int_errno={other}"), true),
    };
    if os_errno > 0 {
        // Unchanged r1 decision (#248 out of scope): append ", errno: <strerror>"
        // for any code at errno>0. Unreachable from a riperf3 server, which
        // always relays errno 0.
        format!(
            "{base}, errno: {}",
            std::io::Error::from_raw_os_error(os_errno as i32)
        )
    } else if perr {
        // GT's dangling ": " for the perr-class codes at errno==0 (#248).
        format!("{base}: ")
    } else {
        base
    }
}

#[cfg(test)]
mod strerror_tests {
    use super::iperf3_strerror;

    /// The #224 relay codes, pinned to iperf 3.21's iperf_error.c strings.
    #[test]
    fn maps_the_relay_codes() {
        assert_eq!(
            iperf3_strerror(27, 0),
            "total required bandwidth is larger than server limit"
        );
        assert_eq!(iperf3_strerror(120, 0), "the server has terminated");
        // #248: 160 is perr=1, so GT dangles a trailing ": " even at errno==0;
        // 27/120 above are perr=0 and stay bare.
        assert_eq!(iperf3_strerror(160, 0), "server test duration expired: ");
    }

    /// The errno append is UNCONDITIONAL on errno > 0 for every code
    /// (iperf_client_api.c:403-404 — the r1 review corrected the earlier
    /// perr-gated model with a live probe). At errno == 0, perr-class codes
    /// (160 and the int_errno default) get GT's dangling ": " (#248); perr=0
    /// codes (27/37/120) stay bare. Unknown codes fall back to int_errno=%d.
    #[test]
    fn errno_append_and_fallback() {
        // #248: the int_errno default is perr=1 → trailing ": " at errno==0.
        assert_eq!(iperf3_strerror(9999, 0), "int_errno=9999: ");
        for (code, base) in [
            (160u32, "server test duration expired"),
            (
                27u32,
                "total required bandwidth is larger than server limit",
            ),
        ] {
            let with_errno = iperf3_strerror(code, 104);
            assert!(
                with_errno.starts_with(&format!("{base}, errno: ")),
                "{with_errno}"
            );
        }
        assert_eq!(
            iperf3_strerror(37, 0),
            "client's requested duration exceeds the server's maximum permitted limit"
        );
    }
}

/// Result alias used throughout the library.
pub type Result<T> = std::result::Result<T, RiperfError>;

/// The wire protocol transmits test state as a single signed byte.
/// Unknown values are captured here for forward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownState(pub i8);

impl fmt::Display for UnknownState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown state byte: {}", self.0)
    }
}
