//! # riperf3
//!
//! ## Unsafe audit
//!
//! All unsafe blocks are raw kernel syscalls with no safe Rust wrapper.
//! Each is platform-gated, documented with a SAFETY comment, and behind
//! a safe public API. No unsafe exists in cross-platform or application code.
//!
//! | Flag | Syscall | Platform | File |
//! |------|---------|----------|------|
//! | `--fq-rate` | `setsockopt(SO_MAX_PACING_RATE)` | Linux | net.rs |
//! | `--dont-fragment` | `setsockopt(IP_MTU_DISCOVER)` | Linux | net.rs |
//! | `--dont-fragment` | `setsockopt(IP_DONTFRAG)` | macOS/FreeBSD | net.rs |
//! | `--dont-fragment` | `setsockopt(IP_DONTFRAGMENT)` | Windows | net.rs |
//! | `--flowlabel` | `setsockopt(IPV6_FLOWINFO_SEND)` | Linux | net.rs |
//! | `-M` (MSS) | `setsockopt(TCP_MAXSEG)` | Windows | net.rs |
//! | `--bind-dev` | `setsockopt(IP_BOUND_IF)` | macOS | net.rs |
//! | `-A` (affinity) | `SetThreadAffinityMask` | Windows | net.rs |
//! | *(internal)* | `getsockopt(IP_MTU_DISCOVER)` | Linux | net.rs |
//! | *(internal)* | `getsockopt(TCP_INFO)` | Linux | tcp_info.rs |
//! | *(internal)* | `getsockopt(TCP_CONNECTION_INFO)` | macOS | tcp_info.rs |
//! | *(internal)* | `getsockopt(TCP_INFO)` | FreeBSD | tcp_info.rs |

// The macros module must come first so other modules can use vprintln!.
#[macro_use]
mod macros;

pub mod error;
pub use error::{ConfigError, Result, RiperfError};

pub mod units;
pub mod utils;

pub mod auth;
pub mod cpu;
pub mod net;
pub mod protocol;
pub mod reporter;
pub mod stream;
pub mod tcp_info;

pub mod client;
pub use client::{Client, ClientBuilder};

pub mod server;
pub use server::{Server, ServerBuilder, TestConfig};
