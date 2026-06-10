//! Shared lib-test helpers (`mod common;` per test binary).

#![allow(dead_code)] // each test binary uses a subset

use std::sync::atomic::{AtomicU16, Ordering};

/// Sub-ephemeral, PID-windowed port allocation — same scheme as the CLI test
/// harness (`riperf3-cli/tests/common`, #176): ephemeral-range picks collide
/// with concurrent test binaries' connect() source ports under the parallel
/// harness. Windows are PID-offset so concurrently-running test binaries don't
/// share one; an atomic counter serializes callers within a binary; a
/// bind-check skips anything still occupied.
pub fn free_port() -> u16 {
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};

    static NEXT: AtomicU16 = AtomicU16::new(0);
    let window = 7000 + (std::process::id() % 250) as u16 * 100;
    for _ in 0..100 {
        let port = window + NEXT.fetch_add(1, Ordering::Relaxed) % 100;
        // Sequential probes — a held `::` listener (v6only=0 on Linux) claims
        // the v4 side too; `.is_ok()` drops each listener before the next bind.
        if TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok()
            && TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
        {
            return port;
        }
    }
    panic!("no free port in test window {window}-{}", window + 99);
}
