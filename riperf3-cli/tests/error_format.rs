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

/// #198 item 1: with -J, a failed run puts the message in the JSON document's
/// "error" key on STDOUT and prints NOTHING to stderr — iperf3's iperf_errexit
/// json path (live-captured shape: start{connected:[],version,system_info},
/// intervals:[], end:{}, error).
#[test]
fn json_mode_errors_emit_the_document_not_stderr() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let out = std::process::Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", "1", "-J"])
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr.trim().is_empty(),
        "iperf3 prints nothing to stderr under -J: {stderr:?}"
    );
    let doc: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout is the JSON document");
    assert!(doc["error"]
        .as_str()
        .is_some_and(|e| e.contains("unable to connect")));
    assert!(doc["start"]["connected"]
        .as_array()
        .is_some_and(Vec::is_empty));
    assert!(doc["intervals"].as_array().is_some_and(Vec::is_empty));
    assert!(doc["end"].is_object());
}

/// #198 item 1 (json-stream): the pre-test failure emits an `error` event then
/// an empty `end` event — live iperf3 shape — and nothing on stderr.
#[test]
fn json_stream_errors_emit_error_then_empty_end_events() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let out = std::process::Command::new(bin)
        .args(["-c", "127.0.0.1", "-p", "1", "--json-stream"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(stderr.trim().is_empty(), "stderr: {stderr:?}");
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert!(
        lines[0].starts_with("{\"event\":\"error\",\"data\":\"unable to connect"),
        "first event: {:?}",
        lines.first()
    );
    assert_eq!(
        lines.last().copied(),
        Some("{\"event\":\"end\",\"data\":{}}"),
        "the stream still closes with an empty end event"
    );
}

/// #198 item 2: with --logfile, the error line lands in the LOGFILE (iperf3
/// writes to outfile when it isn't stdout), not stderr.
#[cfg(unix)]
#[test]
fn logfile_receives_the_error_line() {
    let dir = std::env::temp_dir();
    let log = dir.join(format!("riperf3-errlog-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&log);
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let out = std::process::Command::new(bin)
        .args([
            "-c",
            "127.0.0.1",
            "-p",
            "1",
            "--logfile",
            log.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.trim().is_empty(), "stderr: {stderr:?}");
    let logged = std::fs::read_to_string(&log).expect("logfile written");
    assert!(
        logged.contains("riperf3: error - unable to connect"),
        "the error line goes to the logfile: {logged:?}"
    );
    let _ = std::fs::remove_file(&log);
}

/// #198 items 3+4: usage errors exit 1 like iperf3's getopt path (clap's
/// default is 2), and the bare invocation prints iperf3's exact parameter
/// error.
#[test]
fn usage_errors_exit_one() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let out = std::process::Command::new(bin)
        .arg("--bogus")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "unknown flag exits 1");
    let out = std::process::Command::new(bin).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "bare invocation exits 1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("parameter error - must either be a client (-c) or server (-s)"),
        "iperf3's exact no-mode sentence: {stderr:?}"
    );
    // -s -c h: iperf3's IESERVCLIENT sentence (review r1 n4).
    let out = std::process::Command::new(bin)
        .args(["-s", "-c", "h"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("parameter error - cannot be both server and client"),
        "IESERVCLIENT sentence: {stderr:?}"
    );
    assert!(
        stderr.contains("Usage: riperf3 [-s|-c host] [options]"),
        "the usage trailer rides parameter errors: {stderr:?}"
    );
    // --help / --version still exit 0.
    let out = std::process::Command::new(bin)
        .arg("--help")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

/// #198 review r1 f1: parse-class rejections (#65/#100/#140) print to STDERR
/// in every mode — iperf3's iperf_exit only mode-sinks POST-parse errors
/// (json_top exists / outfile open). Live: `iperf3 -s -t 5 -J` errors in
/// plain text on stderr with empty stdout.
#[test]
fn parse_class_errors_stay_on_stderr_even_with_json() {
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let out = std::process::Command::new(bin)
        .args(["-s", "-t", "5", "-J"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stdout.is_empty(),
        "no JSON document for a parse-class error: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("riperf3: error - ") && stderr.contains("client only"),
        "the #65 rejection stays plain text on stderr: {stderr:?}"
    );
}

/// #259: GT's post-parse range validations (iperf_api.c:1386/1588/1596,
/// MAX_TIME = 86400 in iperf.h:472), with GT's exact wordings + the usage
/// trailer + exit 1 (all live-captured on the issue).
#[test]
fn duration_range_validations_match_gt() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-c", "127.0.0.1", "-t", "86401"],
            "parameter error - test duration valid values are 0 to 86400 seconds",
        ),
        (
            &["-s", "--idle-timeout", "0"],
            "parameter error - idle timeout parameter is not positive or larger than allowed limit",
        ),
        (
            &["-s", "--idle-timeout", "86401"],
            "parameter error - idle timeout parameter is not positive or larger than allowed limit",
        ),
        (
            &["-s", "--server-max-duration", "86401"],
            "parameter error - test duration valid values are 0 to 86400 seconds",
        ),
        // r1 F5: GT's range checks fire during the getopt loop, BEFORE its
        // client-flag-on-server check — `-s -t 86401` reports the duration
        // range, not the #65 client-only-flag error (live-verified).
        (
            &["-s", "-t", "86401"],
            "parameter error - test duration valid values are 0 to 86400 seconds",
        ),
    ];
    for (args, want) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(
            out.status.code(),
            Some(1),
            "range violations exit 1 like GT: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!("riperf3: {want}")),
            "{args:?}: GT wording expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors (GT shape): {stderr}"
        );
    }
    // The boundary VALUES are legal: -t 86400 must not be rejected at parse
    // time (it fails later on connect, not with a parameter error).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "-t", "86400"])
        .output()
        .expect("spawn riperf3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("parameter error"),
        "-t 86400 is legal (0..=86400): {stderr}"
    );
}
