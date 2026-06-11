//! Shared lib-test helpers (`mod common;` per test binary).
//!
//! Implementations live in the dev-only `riperf3-test-support` crate (#192).

#![allow(dead_code)] // each test binary uses a subset

pub use riperf3_test_support::free_port;
