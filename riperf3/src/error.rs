// riperf3/riperf3/src/error.rs
// This file starts with `thiserror` to implement the error types used in riperf3

use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum ConfigError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error("invalid value for {0}: {1}")]
    InvalidValue(&'static str, String),
    //TODO: Add additional error types
}
