//! Shared CLI-test helpers (`mod common;` per test binary).

#![allow(dead_code)] // each test binary uses a subset

use std::net::TcpListener;
use std::time::{Duration, Instant};

/// Block until a spawned server is actually listening on `port`, by
/// bind-probing: while WE can still bind the port, the server hasn't; once
/// the bind fails (`EADDRINUSE`), the server's listener owns it. Unlike a
/// connect-probe this never consumes a `-s -1` one-off server's single
/// accept, and unlike the old fixed 2 s sleep it can't lose to a loaded CI
/// runner — the UDP `--bidir` startup repeatedly outlived the sleep on
/// GitHub runners, leaving the client to die on ECONNREFUSED with empty
/// stdout (the "not valid JSON (EOF at line 1 column 0)" flake).
pub fn wait_port_listening(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            // We grabbed it — the server hasn't bound yet. Release and retry.
            Ok(probe) => drop(probe),
            // AddrInUse (or anything else): the port is taken — server is up.
            Err(_) => return,
        }
        assert!(
            Instant::now() < deadline,
            "server never bound 127.0.0.1:{port} within 15s"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
