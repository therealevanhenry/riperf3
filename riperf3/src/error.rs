use std::fmt;

use thiserror::Error;

/// Configuration errors — validation failures when building Client/Server.
/// Kept separate so existing tests that match on these variants continue to work.
#[derive(Error, Debug, PartialEq)]
pub enum ConfigError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error("invalid value for {0}: {1}")]
    InvalidValue(&'static str, String),
}

/// Runtime errors covering I/O, protocol, and JSON failures.
#[derive(Error, Debug)]
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

    #[error("server is busy")]
    ServerBusy,

    #[error("connection timed out")]
    ConnectionTimeout,

    #[error("protocol violation: {0}")]
    Protocol(String),

    #[error("test aborted: {0}")]
    Aborted(String),

    #[error("peer disconnected")]
    PeerDisconnected,
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
