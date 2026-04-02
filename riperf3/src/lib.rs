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

// --- Public API: what library consumers should use ---

mod error;
pub use error::{ConfigError, Result, RiperfError};

mod client;
pub use client::{Client, ClientBuilder};

mod server;
pub use server::{Server, ServerBuilder, TestConfig};

pub use protocol::TransportProtocol;

pub use net::set_cpu_affinity;

// --- Internal modules: exposed for the CLI crate and integration tests ---
// These are implementation details, not part of the stable library API.
// Use `#[doc(hidden)]` to keep them out of rustdoc while remaining accessible.

#[doc(hidden)]
pub mod auth;
#[doc(hidden)]
pub mod cpu;
#[doc(hidden)]
pub mod net;
#[doc(hidden)]
pub mod protocol;
#[doc(hidden)]
pub mod reporter;
#[doc(hidden)]
pub mod stream;
#[doc(hidden)]
pub mod tcp_info;
#[doc(hidden)]
pub mod units;
#[doc(hidden)]
pub mod utils;
