// The iperf_api module mimicks the iperf3_api.h header file from the iperf3 project. It is unused
// at this time, but will (likely) be used in the future to interact with the iperf3 C API via FFI.
pub mod iperf_api;

// The macros module contains custom macros used in the riperf3 project.
#[macro_use]
mod macros;

// The error module contains the error handling types used in the riperf3 project.
pub mod error;
pub use error::ConfigError;

// The utils module contains utility functions and types used in the riperf3 project.
pub mod utils;

// The client module contains the client-specific types and functions for the riperf3 project.
pub mod client;
pub use client::{Client, ClientBuilder};

// The server module contains the server-specific types and functions for the riperf3 project.
pub mod server;
pub use server::{Server, ServerBuilder};
