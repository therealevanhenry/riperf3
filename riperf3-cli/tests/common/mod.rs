//! Shared CLI-test helpers (`mod common;` per test binary).
//!
//! The implementations live in the dev-only `riperf3-test-support` crate
//! (#192) — single source for the #176 port allocator and refused-retry
//! runner, the #191 UDP serialization lock, and the child reaper guard. The
//! `run_client*` wrappers exist because `CARGO_BIN_EXE_riperf3` is only set
//! while compiling THIS crate's tests; the support crate takes the path as an
//! argument.

#![allow(dead_code)] // each test binary uses a subset

use std::time::Duration;

#[allow(unused_imports)] // each test binary uses a subset
pub use riperf3_test_support::{
    free_port, refused, udp_serial, wait_bounded, ChildGuard, ClientRun,
};

/// See `riperf3_test_support::run_client_with`.
pub fn run_client(args: &[&str], timeout: Duration, who: &str) -> ClientRun {
    riperf3_test_support::run_client_with(env!("CARGO_BIN_EXE_riperf3"), args, timeout, who)
}

/// See `riperf3_test_support::run_client_ok_with`.
pub fn run_client_ok(args: &[&str], timeout: Duration, who: &str) -> ClientRun {
    riperf3_test_support::run_client_ok_with(env!("CARGO_BIN_EXE_riperf3"), args, timeout, who)
}
