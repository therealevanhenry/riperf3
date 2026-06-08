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
//! | *(internal)* | `getsockopt(TCP_MAXSEG)` | Unix | net.rs |
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

// The wire protocol enum and the result model returned by `Client::run()`.
pub use protocol::{StreamResultJson, TestResultsJson, TransportProtocol};

pub use net::set_cpu_affinity;

// --- Internal modules: implementation detail, NOT part of the public API. ---
// Crate-private (`pub(crate)`), so nothing here is a semver commitment; the
// few genuinely-public types are re-exported at the crate root above (#67).
// `json_report` is the exception — it is the iperf3-schema result model and
// stays publicly accessible (kept `#[doc(hidden)]` to keep its ~25 structs out
// of the rendered API surface).

pub(crate) mod auth;
pub(crate) mod cpu;
#[doc(hidden)]
pub mod json_report;
pub(crate) mod net;
pub(crate) mod protocol;
pub(crate) mod reporter;
pub(crate) mod stream;
pub(crate) mod tcp_info;
pub(crate) mod units;
pub(crate) mod utils;
