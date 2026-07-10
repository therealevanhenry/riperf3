//! # riperf3
//!
//! A wire- and API-faithful replacement for iperf3 (3.21) in idiomatic Rust:
//! the same protocol, flags, output, and JSON schema, usable as a CLI or as
//! this library crate.
//!
//! Run a test and read its result — quiet by default (#294), with the
//! measured [`Report`] and a [`Termination`] saying how the run ended
//! (#293, see the [`outcome`] module):
//!
//! ```no_run
//! # async fn demo() -> riperf3::Result<()> {
//! let client = riperf3::ClientBuilder::new("198.51.100.7")
//!     .duration(5)
//!     .build()?;
//! let outcome = client.run().await?;
//! if outcome.termination != riperf3::Termination::Completed {
//!     eprintln!("test ended early: {:?}", outcome.termination);
//! }
//! if let Some(sent) = &outcome.report.end.sum_sent {
//!     println!("{} bytes sent", sent.bytes);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The server side mirrors it: [`ServerBuilder`] builds a [`Server`], and
//! [`Server::run_once`] (or [`Server::bind`] + [`BoundServer::run_once`])
//! returns the same [`RunOutcome`] per served test, while [`Server::run`]
//! is the persistent daemon loop the CLI drives.
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

// The run-result model (#293): `Client::run` and `Server::run_once` both return
// `RunOutcome` (the measured `Report` + how the run ended) rather than signaling
// an abnormal end through `Err`. Public as a MODULE (review sweep): its
// module-level narrative is the #293 contract documentation, which a private
// `mod` would keep off docs.rs entirely (the `json_report` precedent).
pub mod outcome;
pub use outcome::{RunOutcome, Termination};

mod client;
pub use client::{Client, ClientBuilder};

mod server;
// TestConfig is server-internal (built from received wire params, not used in
// any public signature); it stays crate-private rather than a public type (#67).
pub use server::{BoundServer, Server, ServerBuilder};

// The transport enum used across the public builder API. `TestResultsJson` /
// `StreamResultJson` are the internal control-channel exchange model and are no
// longer re-exported (0.8.0 breaking, #137).
pub use protocol::TransportProtocol;

// The rich iperf3-schema result model — the same object `-J` / `--json`
// serializes (#137). Since 0.9.0 both `Client::run` and `Server::run_once`
// return a `RunOutcome` carrying this `Report` plus a `Termination` (#293).
pub use json_report::Report;

pub use net::set_cpu_affinity;

/// Render a `--timestamps` strftime FORMAT into the line prefix
/// (#202/#348) — the same renderer the lib's own iperf_err-class sites
/// use, for CLI-layer stderr lines that print outside a run's scope
/// (GT's stamp is process-global; the lib's is run-scoped).
pub use macros::render_timestamp as render_timestamp_prefix;

/// `--logfile` routing for the lib's error lines (#364) — armed by the
/// binary next to its stdout redirect, so the SERVER-ERROR relay receipt
/// follows iperf_err's logfile-or-stderr chooser.
pub use macros::ErrorSinkGuard;

// --- Internal modules: implementation detail, NOT part of the public API. ---
// Crate-private (`pub(crate)`), so nothing here is a semver commitment; the
// few genuinely-public types are re-exported at the crate root above (#67).
// `json_report` is the exception — it is the iperf3-schema result model that
// `Client::run` and `Server::run_once` both return inside a `RunOutcome`, so it
// is a documented public module (#137); its top-level `Report` is re-exported
// at the crate root above.
// The module's PUBLIC surface is `Report` + its serialized sub-structs only;
// the builder INPUT types (`ReportInput`/`StreamReport`/`TcpEndExtras`/
// `UdpStreamStats`) are `pub(crate)` (incidentally exposed by #137, hidden in
// 0.8.0 — #283), since they are assembled solely by the crate-private
// `build_report_input`.

pub(crate) mod auth;
pub(crate) mod cpu;
pub mod json_report;
pub(crate) mod net;
pub(crate) mod protocol;
pub(crate) mod reporter;
pub(crate) mod stream;
pub(crate) mod tcp_info;
pub(crate) mod units;
pub(crate) mod utils;
