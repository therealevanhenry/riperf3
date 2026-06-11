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

    /// iperf3's IECTRLCLOSE: the control connection died mid-test (#170).
    #[error("control socket has closed unexpectedly")]
    ControlSocketClosed,

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

    #[error("peer disconnected")]
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

/// iperf3's iperf_strerror, for the SERVER_ERROR relay codes a client can
/// receive (#224). The bool is iperf3's `perr`: those codes append
/// ", errno: <strerror>" when the relayed os errno is non-zero — rendered via
/// io::Error, whose "(os error N)" suffix is the convention riperf3's raw os
/// errors already carry (#151). Unknown codes mirror iperf3's literal
/// "int_errno=%d" fallback (iperf_error.c default case).
pub(crate) fn iperf3_strerror(i_errno: u32, os_errno: u32) -> String {
    let (base, perr) = match i_errno {
        // IETOTALRATE — the --server-bitrate-limit breach
        27 => (
            "total required bandwidth is larger than server limit".to_string(),
            false,
        ),
        // IESERVERTERM — a server relaying its own terminate as an error
        120 => ("the server has terminated".to_string(), false),
        // IESERVERTESTDURATIONEXPIRED — the --server-max-duration timer
        160 => ("server test duration expired".to_string(), true),
        other => (format!("int_errno={other}"), true),
    };
    if perr && os_errno > 0 {
        format!(
            "{base}, errno: {}",
            std::io::Error::from_raw_os_error(os_errno as i32)
        )
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
        assert_eq!(iperf3_strerror(160, 0), "server test duration expired");
    }

    /// perr-class codes append the os strerror only when errno > 0; the
    /// unknown-code fallback is iperf3's literal int_errno=%d.
    #[test]
    fn perr_append_and_fallback() {
        assert_eq!(iperf3_strerror(9999, 0), "int_errno=9999");
        let with_errno = iperf3_strerror(160, 104);
        assert!(
            with_errno.starts_with("server test duration expired, errno: "),
            "{with_errno}"
        );
        // Non-perr codes never append, whatever the errno.
        assert_eq!(
            iperf3_strerror(27, 104),
            "total required bandwidth is larger than server limit"
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
