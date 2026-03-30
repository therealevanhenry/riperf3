// The macros module must come first so other modules can use vprintln!.
#[macro_use]
mod macros;

pub mod error;
pub use error::{ConfigError, RiperfError, Result};

pub mod utils;
pub mod units;

pub mod protocol;
pub mod net;
pub mod stream;
pub mod tcp_info;
pub mod cpu;
pub mod reporter;

// The iperf_api module contains reference type definitions mirroring iperf3's C API.
pub mod iperf_api;

pub mod client;
pub use client::{Client, ClientBuilder};

pub mod server;
pub use server::{Server, ServerBuilder, TestConfig};
