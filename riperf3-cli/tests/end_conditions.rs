//! CLI process-level test: conflicting end conditions (#140) must reject like
//! iperf3's IEENDCONDITIONS — exit 1, the message on stderr, and BEFORE any
//! side effects (iperf3 raises it in parse_arguments; no pidfile is created).
#![cfg(unix)]

use std::process::Command;

#[test]
fn conflicting_end_conditions_exit_before_side_effects() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let pidfile = std::env::temp_dir().join(format!("riperf3-endcond-{}.pid", std::process::id()));
    let _ = std::fs::remove_file(&pidfile);

    let out = Command::new(bin)
        .args([
            "-c",
            "127.0.0.1",
            "-t",
            "5",
            "-n",
            "1G",
            "-I",
            pidfile.to_str().unwrap(),
        ])
        .output()
        .expect("spawn riperf3");

    assert_eq!(
        out.status.code(),
        Some(1),
        "iperf3 exits 1 on IEENDCONDITIONS"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("only one test end condition (-t, -n, -k) may be specified"),
        "stderr must carry iperf3's IEENDCONDITIONS text, got: {stderr}"
    );
    assert!(
        !pidfile.exists(),
        "the rejection must fire BEFORE side effects (iperf3 creates no pidfile); found {pidfile:?}"
    );
    let _ = std::fs::remove_file(&pidfile);
}
