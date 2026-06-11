//! #151: top-level CLI errors print in iperf3's `error - ...` shape, not
//! Rust's Debug rendering. Scripts written against iperf3 grep stderr for
//! `error - ` (iperf3 prints `iperf3: error - <text>`; ours prefixes the
//! actual binary name).

mod common;

#[test]
fn connect_failure_prints_iperf3_error_shape_and_exits_1() {
    // A TcpListener bound then dropped gives a port that refuses connections.
    let port = common::free_port();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", &port.to_string(), "-t", "1"])
        .output()
        .expect("spawn riperf3");

    assert_eq!(out.status.code(), Some(1), "iperf3 exits 1 on errors");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("riperf3: error - "),
        "stderr must start with the iperf3 error shape, got: {stderr}"
    );
    assert!(
        stderr.contains(
            "unable to connect to server - server may have stopped running \
             or use a different port, firewall issue, etc."
        ),
        "connect failures carry iperf3's FULL canonical IECONNECT sentence \
         (review r1 found a line-join artifact the prefix check missed), got: {stderr}"
    );
    assert!(
        !stderr.contains("Error:"),
        "Rust Debug rendering must be gone, got: {stderr}"
    );
}
