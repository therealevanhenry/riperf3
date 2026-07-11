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

/// #364: with --logfile, the SERVER-ERROR relay line lands in the LOGFILE
/// alongside the errexit line — GT's iperf_err writes to test->outfile when
/// a logfile is open (iperf_error.c:67-71), the same chooser iperf_exit
/// uses. Live-probed (GT 3.21, a --server-bitrate-limit refusal): the
/// client's logfile carries BOTH `iperf3: SERVER ERROR - total required
/// bandwidth is larger than server limit` and the `iperf3: error - <same>`
/// exit line; stderr is EMPTY; exit 1. riperf3 kept the relay half on
/// stderr — the client.rs KNOWN-CORNER record this pin retires.
#[cfg(unix)]
#[test]
fn logfile_receives_the_relay_line() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let log = dir.join(format!("riperf3-relaylog-{pid}.log"));
    let srv_log = dir.join(format!("riperf3-relaylog-srv-{pid}.log"));
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&srv_log);
    let bin = env!("CARGO_BIN_EXE_riperf3");
    let ps = common::free_port().to_string();
    let mut server = common::ChildGuard(
        std::process::Command::new(bin)
            .args([
                "-s",
                "-1",
                "-p",
                &ps,
                "--server-bitrate-limit",
                "1000",
                "--logfile",
                srv_log.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    // Readiness via the server's OWN logfile: the listen banner lands there
    // once the redirect + bind are both up. The refused-retry runner can't
    // gate a --logfile client (it keys off stderr, which --logfile empties).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !std::fs::read_to_string(&srv_log)
        .map(|s| s.contains("Server listening"))
        .unwrap_or(false)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "server banner in its logfile"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = std::process::Command::new(bin)
        .args([
            "-c",
            "127.0.0.1",
            "-p",
            &ps,
            "-b",
            "1M",
            "-t",
            "2",
            "--logfile",
            log.to_str().unwrap(),
        ])
        .output()
        .expect("run client");
    let _ = common::wait_bounded(&mut server.0, std::time::Duration::from_secs(8));
    assert_eq!(out.status.code(), Some(1), "refused-run errexit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "the relay line must not leak to stderr: {stderr:?}"
    );
    let logged = std::fs::read_to_string(&log).expect("logfile written");
    assert!(
        logged.contains(
            "riperf3: SERVER ERROR - total required bandwidth is larger than server limit"
        ),
        "the relay line lands in the logfile: {logged:?}"
    );
    assert!(
        logged.contains("riperf3: error - total required bandwidth is larger than server limit"),
        "the errexit line stays in the logfile too: {logged:?}"
    );
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&srv_log);
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
    // #263 r1 n1: dropping the ValueEnum must not cost -f's accepted-charset
    // discoverability — GT's help names the set: `[kmgtKMGT] format to
    // report: Kbits, Mbits, Gbits, Tbits`.
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("[kmgtKMGT] format to report"),
        "-f help names the accepted charset like GT: {help}"
    );
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
        stderr.contains("riperf3: parameter error - ") && stderr.contains("client only"),
        "the #65 rejection uses GT's parameter-error wording (#270): {stderr:?}"
    );
    assert!(
        stderr.contains("Usage:") && stderr.contains("--help"),
        "the usage trailer rides the parameter-error class (#270): {stderr:?}"
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
    // #303 item 3: NEGATIVE args take GT's parameter-error class — its
    // atoi wraps them into the same range checks (live-probed wordings) —
    // and -O has GT's own bounds check (MAX_OMIT_TIME 600, in-loop).
    let neg_cases: &[(&[&str], &str)] = &[
        (
            &["-c", "127.0.0.1", "-t", "-1"],
            "parameter error - test duration valid values are 0 to 86400 seconds",
        ),
        (
            &["-s", "--idle-timeout", "-5"],
            "parameter error - idle timeout parameter is not positive or larger than allowed limit",
        ),
        (
            &["-c", "127.0.0.1", "-O", "-3"],
            "parameter error - bogus value for --omit (maximum = 600 seconds)",
        ),
        (
            &["-c", "127.0.0.1", "-O", "700"],
            "parameter error - bogus value for --omit (maximum = 600 seconds)",
        ),
        (
            &["-s", "--server-max-duration", "-2"],
            "parameter error - test duration valid values are 0 to 86400 seconds",
        ),
    ];
    for (args, want) in neg_cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!("riperf3: {want}")),
            "{args:?}: GT wording expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
    // -O 600 is the legal boundary (fails later on connect, not at parse).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "-O", "600"])
        .output()
        .expect("spawn riperf3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("parameter error"),
        "-O 600 is legal (0..=600): {stderr}"
    );

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

/// #317: GT parses duration-like flags with atoi — suffixed garbage takes
/// the leading digits, non-numerics are 0, and overflow wraps through int
/// truncation (strtol-saturate then (int) cast: 2^32 → 0, 2^31 → INT_MIN →
/// the range error). riperf3 rejected all of these with clap shapes.
#[test]
fn duration_like_flags_parse_like_atoi() {
    // Parse-and-proceed cases: the value lands, the run fails on CONNECT
    // (port 9), never on a parse error.
    for args in [
        &["-c", "127.0.0.1", "-p", "9", "-t", "5x"][..],
        &["-c", "127.0.0.1", "-p", "9", "-t", "abc"][..],
        &["-c", "127.0.0.1", "-p", "9", "-t", "-abc"][..],
        &["-c", "127.0.0.1", "-p", "9", "-t", "4294967296"][..],
        &["-c", "127.0.0.1", "-p", "9", "-t", " 7"][..],
        &["-c", "127.0.0.1", "-p", "9", "-O", "5x"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect") && !stderr.contains("parameter error"),
            "{args:?} parses via atoi semantics and proceeds: {stderr}"
        );
    }
    // The int-truncation edge: 2^31 wraps negative -> the range sentence.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-t", "2147483648"])
        .output()
        .expect("spawn riperf3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with(
            "riperf3: parameter error - test duration valid values are 0 to 86400 seconds"
        ),
        "2^31 truncates to INT_MIN like GT's atoi: {stderr}"
    );
}

/// #263: GT's -f parse (iperf_api.c:1236-1256) takes `*optarg` — the FIRST
/// character only — and rejects anything outside [kmgtKMGT] with
/// IEBADFORMAT's exact sentence. 'b'/'B' are CLI-unreachable in GT too
/// (lib-only unit_snprintf arms), so riperf3 rejects them identically.
#[test]
fn format_specifier_rejections_match_gt() {
    const WANT: &str =
        "parameter error - bad format specifier (valid formats are in the set [kmgtKMGT])";
    for args in [
        &["-c", "127.0.0.1", "-f", "x"][..],
        &["-c", "127.0.0.1", "-f", "b"][..],
        &["-c", "127.0.0.1", "-f", "B"][..],
        &["-s", "-f", "x"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!("riperf3: {WANT}")),
            "{args:?}: GT's IEBADFORMAT sentence expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
    // First-char parse: `-f kilobits` is `-f k` in GT (*optarg), NOT an
    // invalid-value rejection. It sails past parsing and fails on connect.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "-f", "kilobits"])
        .output()
        .expect("spawn riperf3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("parameter error") && stderr.contains("unable to connect"),
        "-f kilobits parses as -f k (optarg[0]): {stderr}"
    );
}

/// #309: GT rejects `-R --bidir` in the getopt loop with IEREVERSEBIDIR —
/// `cannot be both reverse and bidirectional` (iperf_api.c:1423/:1431),
/// both flag orders, parameter-error class (trailer + exit 1). riperf3
/// used to accept the pair and run a reverse-flagged bidir test.
#[test]
fn reverse_plus_bidir_rejects_like_gt() {
    for args in [
        &["-c", "127.0.0.1", "-R", "--bidir"][..],
        &["-c", "127.0.0.1", "--bidir", "-R"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .starts_with("riperf3: parameter error - cannot be both reverse and bidirectional"),
            "{args:?}: IEREVERSEBIDIR sentence expected: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// #328: the rest of GT's atoi option surface (-p/--cport/-P/-M/--rcv-timeout/
// --snd-timeout/--time-skew-threshold). Every expectation below was
// live-probed against iperf 3.21 (/tmp gt build, 2026-07-03).
// ---------------------------------------------------------------------------

/// #328: the atoi-set flags parse with C atoi semantics — suffixed garbage
/// takes the leading digits and the run proceeds to CONNECT (port 9), never
/// a parse error. Live-probed: GT accepts `-P 5x`, `-p 17299x`, `-M 1400x`,
/// `--cport 12345x`, `--snd-timeout 5000x`, `-R --rcv-timeout 5000x`,
/// `-P 0`, `-P -1`, `-M -100` (all "unable to connect" on a dead port).
#[test]
fn atoi_family_flags_parse_like_atoi() {
    for args in [
        &["-c", "127.0.0.1", "-p", "9", "-P", "5x"][..],
        &["-c", "127.0.0.1", "-p", "9", "-P", "0"][..],
        &["-c", "127.0.0.1", "-p", "9", "-P", "-1"][..],
        &["-c", "127.0.0.1", "-p", "17299x"][..],
        &["-c", "127.0.0.1", "-p", "9", "-M", "1400x"][..],
        &["-c", "127.0.0.1", "-p", "9", "-M", "-100"][..],
        &["-c", "127.0.0.1", "-p", "9", "--cport", "12345x"][..],
        &["-c", "127.0.0.1", "-p", "9", "-R", "--rcv-timeout", "5000x"][..],
        &[
            "-c",
            "127.0.0.1",
            "-p",
            "9",
            "-R",
            "--rcv-timeout",
            "86400000",
        ][..],
        &[
            "-c",
            "127.0.0.1",
            "-p",
            "9",
            "--bidir",
            "--rcv-timeout",
            "5000",
        ][..],
        &["-c", "127.0.0.1", "-p", "9", "--snd-timeout", "5000x"][..],
        &["-c", "127.0.0.1", "-p", "9", "--snd-timeout", "0"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect")
                && !stderr.contains("parameter error")
                && !stderr.contains("error: invalid value"),
            "{args:?} parses via atoi semantics and proceeds: {stderr}"
        );
    }
}

/// #328: GT's in-loop range checks for the atoi set, exact sentences +
/// usage trailer + exit 1 (live-probed, iperf_api.c:1229/:1479 IEBADPORT,
/// :1415 IENUMSTREAMS, :1487 IEMSS, :1603 IERCVTIMEOUT, :1614 IESNDTIMEOUT,
/// :1761 IESKEWTHRESHOLD). IERCVTIMEOUT/IESNDTIMEOUT/IESKEWTHRESHOLD notes:
/// the first two carry perr=1, so iperf_strerror appends ": " (errno is 0 at
/// parse time, so nothing follows — the trailing colon-space is part of the
/// live-probed line). `abc` rows pin atoi's garbage->0 falling into the same
/// range checks.
#[test]
fn atoi_family_range_validations_match_gt() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-c", "127.0.0.1", "-p", "0"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "-p", "65536"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "-p", "-1"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "-p", "abc"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-s", "-p", "0"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "--cport", "0"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "--cport", "65536"],
            "parameter error - port number must be between 1 and 65535 inclusive",
        ),
        (
            &["-c", "127.0.0.1", "-P", "129"],
            "parameter error - number of parallel streams too large (maximum = 128)",
        ),
        (
            &["-c", "127.0.0.1", "-M", "32768"],
            "parameter error - TCP MSS too large (maximum = 32767 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "--rcv-timeout", "10"],
            "parameter error - receive timeout value is incorrect or not in range: ",
        ),
        (
            &["-c", "127.0.0.1", "--rcv-timeout", "99"],
            "parameter error - receive timeout value is incorrect or not in range: ",
        ),
        (
            &["-c", "127.0.0.1", "--rcv-timeout", "86400001"],
            "parameter error - receive timeout value is incorrect or not in range: ",
        ),
        (
            &["-c", "127.0.0.1", "--snd-timeout", "-1"],
            "parameter error - send timeout value is incorrect or not in range: ",
        ),
        (
            &["-c", "127.0.0.1", "--snd-timeout", "86400001"],
            "parameter error - send timeout value is incorrect or not in range: ",
        ),
        (
            &["-s", "--time-skew-threshold", "0"],
            "parameter error - skew threshold must be a positive number",
        ),
        (
            &["-s", "--time-skew-threshold", "-3"],
            "parameter error - skew threshold must be a positive number",
        ),
        (
            &["-s", "--time-skew-threshold", "abc"],
            "parameter error - skew threshold must be a positive number",
        ),
        // GT's in-loop <=0 check beats the post-loop IESERVERONLY role check
        // (live-probed: `-c ... --time-skew-threshold 0` gives the skew
        // sentence, `--time-skew-threshold 5x` the server-only one).
        (
            &["-c", "127.0.0.1", "--time-skew-threshold", "0"],
            "parameter error - skew threshold must be a positive number",
        ),
        (
            &["-c", "127.0.0.1", "--time-skew-threshold", "5x"],
            "parameter error - some option you are trying to set is server only",
        ),
    ];
    for (args, want) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
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
}

/// #328: GT's IERVRSONLYRCVTIMEOUT (iperf_api.c:1880-1882) — a plain
/// sending-mode client rejects --rcv-timeout post-loop with the perr-shaped
/// sentence (live-probed: `-c ... --rcv-timeout 5000` errors; with -R or
/// --bidir it proceeds — those accept legs ride
/// atoi_family_flags_parse_like_atoi).
#[test]
fn client_rcv_timeout_requires_receiving_mode() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "--rcv-timeout", "5000"])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1), "exits 1 like GT");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with(
            "riperf3: parameter error - client receive timeout is valid only in receiving mode: "
        ),
        "IERVRSONLYRCVTIMEOUT sentence (with perr's trailing colon-space) expected: {stderr}"
    );
    assert!(
        stderr.contains("Usage:") && stderr.contains("--help"),
        "the usage trailer rides parameter errors: {stderr}"
    );
}

/// #328 (issue comment): raw invalid-UTF-8 argv bytes. GT's atoi on a lone
/// 0xA0 byte yields 0 and the run PROCEEDS (live-probed: `-P $'\xa0'` runs
/// a 0-stream test; `-t $'\xa0'` runs with duration 0). riperf3 died at
/// clap's OsStr->str conversion ("invalid UTF-8 was detected"). The value
/// parsers for the atoi set now work on raw bytes. Unix-only: the invalid
/// byte sequence is an OS-string concept (Windows argv is WTF-16).
#[cfg(unix)]
#[test]
fn raw_invalid_utf8_argv_parses_like_gt_atoi() {
    use std::os::unix::ffi::OsStringExt as _;
    for flag in ["-P", "-t", "-M"] {
        let raw = std::ffi::OsString::from_vec(vec![0xA0]);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9", flag])
            .arg(&raw)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect") && !stderr.contains("invalid UTF-8"),
            "{flag} <0xA0> parses as 0 like GT's atoi and proceeds: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// #328: the unit_atoi family (-n/-k/-l/--pacing-timer/--connect-timeout).
// GT parses these with units.c's unit_atoi (units.c:190-227):
// `sscanf(s, "%lf%c", ...)` — the longest C-double prefix (exponents, hex,
// leading dot included), then AT MOST ONE suffix char in [tTgGmMkK]
// (1024-based); any other suffix char or an unparseable number is IEUNITVAL,
// and junk AFTER a valid suffix is ignored (sscanf never reads past %c).
// All expectations live-probed against iperf 3.21.
// ---------------------------------------------------------------------------

/// #328: IEUNITVAL's exact surface — `iperf3: parameter error - invalid
/// unit value or suffix: '<arg>'` + usage trailer + exit 1 (live-probed;
/// iperf_error.c:399-401, routed through main.c:117-122's parameter-error
/// shape).
#[test]
fn unit_atoi_flags_reject_bad_units_with_ieunitval() {
    let cases: &[(&[&str], &str)] = &[
        (&["-n", "10x"], "10x"),
        (&["-n", "abc"], "abc"),
        (&["-n", "1e"], "1e"),   // scanf can't back up: %lf fails outright
        (&["-n", "1ex"], "1ex"), // prefix "1", suffix 'e' -> not in the set
        (&["-n", ""], ""),
        (&["-n", "."], "."),
        (&["-n", "0x"], "0x"),
        (&["-n", "10 K"], "10 K"), // %c reads the SPACE, not the K
        (&["-k", "10x"], "10x"),
        (&["-l", "10x"], "10x"),
        (&["--pacing-timer", "10x"], "10x"),
        (&["--connect-timeout", "10x"], "10x"),
    ];
    for (args, errarg) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!(
                "riperf3: parameter error - invalid unit value or suffix: '{errarg}'"
            )),
            "{args:?}: IEUNITVAL's exact line expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors (GT shape): {stderr}"
        );
    }
}

/// #328: the unit_atoi accept surface — every GT-accepted form parses and
/// the run proceeds to CONNECT (live-probed: `-n 10Kx` is 10240 with the
/// junk after the suffix ignored, `-n 1.5K`, `-n .5m`, `-n 1e3`, `-n 0x10`
/// (strtod hex), `-n -5` ((uint64) wrap -> a huge byte target, GT runs),
/// `-l 0` (0 = protocol default), `--pacing-timer 3G` ((int) wrap ->
/// negative, GT proceeds), `--connect-timeout -100` (poll(<0) = no
/// timeout)).
#[test]
fn unit_atoi_flags_accept_gt_forms_and_proceed() {
    for args in [
        &["-n", "10Kx"][..],
        &["-n", "1.5K"][..],
        &["-n", ".5m"][..],
        &["-n", "1e3"][..],
        &["-n", "0x10"][..],
        &["-n", "-5"][..],
        &["-n", " 10K"][..],
        &["-k", "10Kx"][..],
        &["-l", "10Kx"][..],
        &["-l", "0"][..],
        &["--pacing-timer", "1K"][..],
        &["--pacing-timer", "3G"][..],
        &["--connect-timeout", "1K"][..],
        &["--connect-timeout", "-100"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect")
                && !stderr.contains("parameter error")
                && !stderr.contains("error: invalid value"),
            "{args:?} parses via unit_atoi semantics and proceeds: {stderr}"
        );
    }
}

/// #328: GT's -l range checks fire in iperf_parse_arguments' post-loop
/// (iperf_api.c:1926-1944) with the parameter-error shape (live-probed):
/// IEBLOCKSIZE for TCP `<= 0` or anything > MAX_BLOCKSIZE (1 MiB,
/// iperf.h:465), IEUDPBLOCKSIZE for UDP outside 16..=65507
/// (iperf.h:467/:469). riperf3 rejected these post-sink in lib build()
/// with the plain-error shape.
#[test]
fn blocksize_range_validations_match_gt() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-c", "127.0.0.1", "-l", "2M"],
            "parameter error - block size too large (maximum = 1048576 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "-l", "-5"],
            "parameter error - block size too large (maximum = 1048576 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "-u", "-l", "2M"],
            "parameter error - block size too large (maximum = 1048576 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "-u", "-l", "70000"],
            "parameter error - block size invalid (minimum = 16 bytes, maximum = 65507 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "-u", "-l", "8"],
            "parameter error - block size invalid (minimum = 16 bytes, maximum = 65507 bytes)",
        ),
        // RECORDED DEVIATION: GT PROCEEDS on `-u -l -5` (its UDP check only
        // fires for blksize > 0, iperf_api.c:1939-1941) into a negative
        // datagram size no stream can honor; riperf3 rejects it with the
        // UDP sentence instead of reproducing the garbage run.
        (
            &["-c", "127.0.0.1", "-u", "-l", "-5"],
            "parameter error - block size invalid (minimum = 16 bytes, maximum = 65507 bytes)",
        ),
        // #328 r1 F1: the blksize checks (iperf_api.c:1926-1944) fire
        // BEFORE the end-conditions check (:1992) in GT's post-loop —
        // live-probed: `-t 5 -n 5 -l -1` reports IEBLOCKSIZE, not
        // IEENDCONDITIONS.
        (
            &["-c", "127.0.0.1", "-t", "5", "-n", "5", "-l", "-1"],
            "parameter error - block size too large (maximum = 1048576 bytes)",
        ),
    ];
    for (args, want) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
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
}

/// #328 (issue comment): a raw invalid-UTF-8 arg to a unit_atoi flag is
/// IEUNITVAL in GT, with the RAW BYTES echoed in the quotes (live-probed:
/// `-n $'\xa0'` prints `invalid unit value or suffix: '<0xA0>'`). riperf3
/// writes the same raw bytes to stderr. Unix-only (Windows argv is WTF-16).
#[cfg(unix)]
#[test]
fn raw_invalid_utf8_unit_arg_is_ieunitval_with_raw_bytes() {
    use std::os::unix::ffi::OsStringExt as _;
    let raw = std::ffi::OsString::from_vec(vec![0xA0]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "-n"])
        .arg(&raw)
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1), "exits 1 like GT");
    let needle = b"invalid unit value or suffix: '\xa0'";
    assert!(
        out.stderr.windows(needle.len()).any(|w| w == &needle[..]),
        "IEUNITVAL echoes the raw byte like GT, got: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// #328: the atof family (-i, --server-bitrate-limit's rate/interval),
// --cntl-ka's pieces + sanity check, and -d/--debug's level. All
// expectations live-probed against iperf 3.21.
// ---------------------------------------------------------------------------

/// #328: -i parses with C atof (iperf_api.c:1260) — strtod's longest
/// prefix, garbage -> 0.0 — so `-i 2x` is 2.0 and `-i x` is 0.0 (both
/// proceed, live-probed); the IEINTERVAL range check
/// `(< MIN_INTERVAL || > MAX_INTERVAL) && != 0` (iperf.h:470-471: 0.1/60)
/// rejects with the exact %g-rendered sentence.
#[test]
fn interval_parses_like_atof_with_ieinterval_range() {
    for args in [
        &["-i", "2x"][..],
        &["-i", "x"][..],
        &["-i", "0"][..],
        &["-i", "0.1"][..],
        &["-i", "60"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect")
                && !stderr.contains("parameter error")
                && !stderr.contains("error: invalid value")
                && !stderr.contains("invalid report interval"),
            "{args:?} parses via atof semantics and proceeds: {stderr}"
        );
    }
    for bad in ["0.01", "-1", "61", "inf"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-i", bad])
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "-i {bad} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(
                "riperf3: parameter error - invalid report interval (min = 0.1, max = 60 seconds)"
            ),
            "-i {bad}: IEINTERVAL's exact sentence expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
}

/// #328: --server-bitrate-limit rate[/interval] (iperf_api.c:1366-1385).
/// The interval piece is C atof + the IETOTALINTERVAL range check (same
/// 0.1..60 bounds as -i), checked BEFORE the rate's unit_atof_rate
/// (live-probed: `10x/0.01` reports the interval, not the unit). The rate
/// piece errors IEUNITVAL in-loop, which beats the post-loop server-only
/// check (live-probed on a client); a VALID spec on a client falls through
/// to IESERVERONLY, proving the parse accepted it without hanging a server.
#[test]
fn server_bitrate_limit_parses_like_gt() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-s", "--server-bitrate-limit", "10x"],
            "parameter error - invalid unit value or suffix: '10x'",
        ),
        (
            &["-s", "--server-bitrate-limit", "abc"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
        (
            &["-s", "--server-bitrate-limit", "10M/0.01"],
            "parameter error - invalid time interval for calculating average data rate",
        ),
        (
            &["-s", "--server-bitrate-limit", "10M/61"],
            "parameter error - invalid time interval for calculating average data rate",
        ),
        // The interval check fires before the rate parse (GT's code order).
        (
            &["-s", "--server-bitrate-limit", "10x/0.01"],
            "parameter error - invalid time interval for calculating average data rate",
        ),
        // In-loop IEUNITVAL beats the post-loop server-only role check.
        (
            &["-c", "127.0.0.1", "--server-bitrate-limit", "10x"],
            "parameter error - invalid unit value or suffix: '10x'",
        ),
        // Valid specs parse clean and only then trip the role check:
        // suffixed-junk rate, atof-garbage interval (0.0 = fine), and a
        // negative rate ((uint64) wrap, like GT) all ACCEPT.
        (
            &["-c", "127.0.0.1", "--server-bitrate-limit", "10M/2x"],
            "parameter error - some option you are trying to set is server only",
        ),
        (
            &["-c", "127.0.0.1", "--server-bitrate-limit", "10M/abc"],
            "parameter error - some option you are trying to set is server only",
        ),
        (
            &["-c", "127.0.0.1", "--server-bitrate-limit", "-5"],
            "parameter error - some option you are trying to set is server only",
        ),
        (
            &["-c", "127.0.0.1", "--server-bitrate-limit", "10Kx"],
            "parameter error - some option you are trying to set is server only",
        ),
    ];
    for (args, want) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(&format!("riperf3: {want}")),
            "{args:?}: GT wording expected, got: {stderr}"
        );
    }
}

/// #328: --cntl-ka[=keepidle[/interval[/count]]] (iperf_api.c:1626-1653):
/// optional arg (bare enables keepalive with defaults), slash-separated
/// pieces each C atoi (empty pieces keep the 0 defaults, :3311-3313), then
/// the sanity check `keepidle != 0 && keepidle <= count*interval` ->
/// IECNTLKA with the perr-shaped sentence (trailing ": ", live-probed).
#[test]
fn cntl_ka_parses_pieces_like_gt() {
    // Accept-and-proceed (live-probed against GT).
    for args in [
        &["--cntl-ka"][..],
        &["--cntl-ka=abc"][..],   // keepidle atoi 0 -> no sanity check
        &["--cntl-ka=10//3"][..], // empty interval keeps default 0
        &["--cntl-ka=10/5/1"][..],
        &["--cntl-ka=20/5/3"][..], // 20 > 5*3: passes the sanity check
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect") && !stderr.contains("parameter error"),
            "{args:?} parses and proceeds: {stderr}"
        );
    }
    // IECNTLKA (live-probed): 10 <= 5*2, count "3x" atoi's to 3 (10 <= 15),
    // 5 <= 5*1, and a negative keepidle is nonzero and <= 0.
    for args in [
        &["--cntl-ka=10/5/2"][..],
        &["--cntl-ka=10/5/3x"][..],
        &["--cntl-ka=5/5/1"][..],
        &["--cntl-ka=-5"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with(
                "riperf3: parameter error - control connection Keepalive period should \
                 be larger than the full retry period (interval * count): "
            ),
            "{args:?}: IECNTLKA's exact perr-shaped sentence expected, got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
}

/// #328 r1 F3: GT's --cntl-ka is optional_argument, so a SEPARATE token is
/// never the spec — `--cntl-ka 5/5/1` leaves optarg NULL (keepalive with
/// defaults) and "5/5/1" is a stray operand GT silently ignores
/// (live-probed: it proceeds). With require_equals, riperf3 matches the
/// =-only attachment exactly; the stray token then falls into the
/// PRE-EXISTING stray-operand divergence class. KNOWN-DIVERGENT: riperf3
/// rejects stray operands (clap's unexpected-argument error) where GT
/// ignores them — the load-bearing part is that the spec is NOT honored
/// (clap must not consume it and fire IECNTLKA on it, which would flip a
/// GT-accept into a spec-driven reject).
#[test]
fn cntl_ka_separate_token_is_a_stray_operand() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-p", "9", "--cntl-ka", "5/5/1"])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("Keepalive period"),
        "the separate token must NOT be parsed as the spec (GT ignores it): {stderr}"
    );
    assert!(
        stderr.contains("unexpected argument"),
        "the stray token takes the pre-existing stray-operand rejection: {stderr}"
    );
}

/// #328: -d/--debug's optional level is C atoi with negative ->
/// DEBUG_LEVEL_MAX (iperf_api.c:1692-1697; DEBUG_LEVEL_MAX 4, iperf.h:300)
/// — GT accepts `--debug=abc` (level 0), `--debug=-1` (4), `--debug=100`
/// (no upper clamp), all live-probed to proceed. riperf3's 1..=4 clap
/// range parser rejected them.
#[test]
fn debug_level_parses_like_atoi() {
    for args in [
        &["-d"][..],
        &["--debug"][..],
        &["--debug=abc"][..],
        &["--debug=-1"][..],
        &["--debug=0"][..],
        &["--debug=100"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect")
                && !stderr.contains("parameter error")
                && !stderr.contains("error: invalid value"),
            "{args:?} parses via atoi semantics and proceeds: {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// #334: -w (unit_atof, 1024-based), -b (rate[/burst], unit_atof_rate +
// atoi), and --fq-rate (unit_atof_rate) wire through GT's own unit parsers
// with GT's error classes (IEUNITVAL / IEBUFSIZE / IEBURST), all in-loop —
// before the post-loop client-only role check. Every expectation below was
// live-probed against iperf 3.21.
// ---------------------------------------------------------------------------

/// #334: the accept surface — every GT-accepted form parses and the run
/// proceeds to CONNECT (dead port 9), never a parse error. Live-probed:
/// `-w 10Kx` (K scales, trailing x ignored, = 10240), `-w 512K`, `-b 1Mx`
/// (rate 1M, x ignored), `-b 100M`, `-b 10M/5` (burst 5), `--fq-rate 1Mx`,
/// `--fq-rate 1M`. riperf3 previously rejected the `x`-suffixed forms with
/// clap's "invalid value for number".
#[test]
fn unit_atof_family_accept_gt_forms_and_proceed() {
    for args in [
        &["-w", "10Kx"][..],
        &["-w", "512K"][..],
        &["-b", "1Mx"][..],
        &["-b", "100M"][..],
        &["-b", "10M/5"][..],
        &["--fq-rate", "1Mx"][..],
        &["--fq-rate", "1M"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-c", "127.0.0.1", "-p", "9"])
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unable to connect")
                && !stderr.contains("parameter error")
                && !stderr.contains("error: invalid value")
                && !stderr.contains("invalid value for number"),
            "{args:?} parses via GT's unit semantics and proceeds: {stderr}"
        );
    }
}

/// #334: the reject surface — GT's exact sentences + usage trailer + exit 1
/// (live-probed). -w: unit_atof (iperf_api.c:1438-1452) → IEUNITVAL on a bad
/// unit, then `> MAX_TCP_BUFFER` (536870912) → IEBUFSIZE (`-w 1G` is
/// 1073741824). -b: slash-split FIRST (iperf_api.c:1347-1365) — burst =
/// atoi(after) with `<= 0 || > MAX_BURST` (1000) → IEBURST, checked BEFORE
/// the rate's unit_atof_rate → IEUNITVAL (so `-b abc/0` is IEBURST, not
/// IEUNITVAL); the IEUNITVAL errarg is the RATE part (before the slash).
/// --fq-rate: unit_atof_rate (iperf_api.c:1726-1737) → IEUNITVAL.
#[test]
fn unit_atof_family_reject_bad_values_with_gt_classes() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["-c", "127.0.0.1", "-w", "abc"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
        (
            &["-c", "127.0.0.1", "-w", "1G"],
            "parameter error - socket buffer size too large (maximum = 536870912 bytes)",
        ),
        (
            &["-c", "127.0.0.1", "-b", "abc"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
        (
            &["-c", "127.0.0.1", "-b", "10M/0"],
            "parameter error - invalid burst count (maximum = 1000)",
        ),
        (
            &["-c", "127.0.0.1", "-b", "10M/1001"],
            "parameter error - invalid burst count (maximum = 1000)",
        ),
        // Burst check precedes the rate parse (GT's code order): a bad rate
        // WITH a bad burst reports IEBURST, and the IEUNITVAL errarg on a
        // sliced spec is the rate part only.
        (
            &["-c", "127.0.0.1", "-b", "abc/0"],
            "parameter error - invalid burst count (maximum = 1000)",
        ),
        (
            &["-c", "127.0.0.1", "-b", "abc/5"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
        (
            &["-c", "127.0.0.1", "--fq-rate", "abc"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
        // In-loop value parse beats the post-loop client-only role check:
        // `-s -b abc` is IEUNITVAL, not IECLIENTONLY (live-probed).
        (
            &["-s", "-b", "abc"],
            "parameter error - invalid unit value or suffix: 'abc'",
        ),
    ];
    for (args, want) in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
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
}

/// #335: GT rejects `-F <file>` with UDP (`-u`) via IEUDPFILETRANSFER
/// (iperf_api.c:1919-1923; iperf_error.c:396-397) — `cannot transfer file
/// using UDP`, a UDP datagram carries its own header so a file can't ride
/// it. Placement is load-bearing: the check sits AFTER the reverse-only
/// rcv-timeout leg and BEFORE the blksize block in GT's post-loop, so it
/// BEATS the -l range rejection. Live-probed: `-u -F x -l 70000` is
/// IEUDPFILETRANSFER, NOT IEUDPBLOCKSIZE (riperf3's -l check used to win).
#[test]
fn udp_file_transfer_rejects_before_blocksize() {
    let cases: &[&[&str]] = &[
        &["-c", "127.0.0.1", "-u", "-F", "x"],
        // The load-bearing ordering cell: a UDP block size that would trip
        // IEUDPBLOCKSIZE must still report IEUDPFILETRANSFER first.
        &["-c", "127.0.0.1", "-u", "-F", "x", "-l", "70000"],
    ];
    for args in cases {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(*args)
            .output()
            .expect("spawn riperf3");
        assert_eq!(out.status.code(), Some(1), "{args:?} exits 1 like GT");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.starts_with("riperf3: parameter error - cannot transfer file using UDP"),
            "{args:?}: IEUDPFILETRANSFER expected (before the blksize block), got: {stderr}"
        );
        assert!(
            stderr.contains("Usage:") && stderr.contains("--help"),
            "the usage trailer rides parameter errors: {stderr}"
        );
    }
    // TCP `-F` is fine (no UDP), and UDP without `-F` is fine — neither trips
    // IEUDPFILETRANSFER (they fail later, on connect).
    for args in [
        &["-c", "127.0.0.1", "-p", "9", "-F", "/dev/null"][..],
        &["-c", "127.0.0.1", "-p", "9", "-u"][..],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(args)
            .output()
            .expect("spawn riperf3");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("cannot transfer file using UDP"),
            "{args:?} must not trip IEUDPFILETRANSFER: {stderr}"
        );
    }
}

/// #365: POST-LOOP parameter errors are stamped unconditionally in GT
/// (the --timestamps format is always parsed by the post-loop checks,
/// iperf_api.c ~:1825+; live: stamped with --timestamps LAST). The stamp
/// rides the error line only — the usage trailer stays bare (GT probed
/// with cat -A). Mid-loop-equivalent errors (the range checks) keep the
/// #301-F4 recorded ordering deviation and stay bare.
#[test]
fn post_loop_parameter_errors_are_stamped() {
    // The parse-class rejection (client-only flag on a server) — stamped.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-s", "--bidir", "--timestamps=XTSX "])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Unix renders the literal format verbatim; Windows uses the #202
    // HH:MM:SS fallback and ignores the format, so the byte-exact half is
    // unix-only (the house stamp-pin convention) — the stamp's PRESENCE
    // asserts everywhere.
    #[cfg(unix)]
    assert!(
        stderr.starts_with("XTSX riperf3: parameter error - "),
        "the post-loop class is stamped with --timestamps LAST: {stderr}"
    );
    assert!(
        !stderr.starts_with("riperf3:") && stderr.contains("riperf3: parameter error - "),
        "a stamp rides the post-loop line on every platform: {stderr}"
    );
    assert!(
        stderr.contains("\nUsage:"),
        "the trailer stays bare (its line has no stamp): {stderr}"
    );
    assert!(
        !stderr.contains("XTSX Usage:"),
        "GT stamps the error line only: {stderr}"
    );

    // IENOROLE (no -s/-c at all) — post-loop in GT, stamped; clap dies
    // pre-parse here so the format rides raw argv.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["--timestamps=XTSX "])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    #[cfg(unix)]
    assert!(
        stderr.starts_with("XTSX riperf3: parameter error - must either be a client"),
        "IENOROLE is stamped off raw argv: {stderr}"
    );
    assert!(
        !stderr.starts_with("riperf3:")
            && stderr.contains("riperf3: parameter error - must either be a client"),
        "a stamp rides IENOROLE on every platform: {stderr}"
    );

    // r1 F4: the bare-flag branch — GT's optional_argument renders the
    // "%c " default; the stamp must be PRESENT (its rendered text is
    // locale/platform-dependent, so presence-only). Last occurrence wins.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["--timestamps"])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.starts_with("riperf3:")
            && stderr.contains("riperf3: parameter error - must either be a client"),
        "the bare --timestamps stamps IENOROLE with the %c default: {stderr}"
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["--timestamps=XTSX ", "--timestamps=YTSY "])
        .output()
        .expect("spawn riperf3");
    let stderr = String::from_utf8_lossy(&out.stderr);
    #[cfg(unix)]
    assert!(
        stderr.starts_with("YTSY riperf3: parameter error - "),
        "the LAST occurrence wins, like getopt: {stderr}"
    );
    let _ = &stderr;

    // The boundary guard: a range check (GT mid-loop) stays BARE — the
    // #301-F4 ordering class is a recorded deviation, not stamped.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args(["-c", "127.0.0.1", "-t", "90000", "--timestamps=XTSX "])
        .output()
        .expect("spawn riperf3");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("riperf3: parameter error - ") && !stderr.starts_with("XTSX"),
        "mid-loop-equivalent range errors stay bare: {stderr}"
    );
}

/// Throwaway 2048-bit RSA test pair for the #395 parse-time cells (PKCS#8 /
/// SPKI, the formats riperf3 parses). Test-only material, not a secret.
const TEST_RSA_PRIV: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7IhH8pfjg1P/o
mz/9oiFCOqDTDb+2W0vr2k5MFxRmNaeNAmYv7otXAC14WBAmHsKdXZ13VJhv9zYR
jyBn+vJYIlT5M3WyAqP5Sn8ejQ9kIYs+CDQl5QlBFaeg6xaJtY6PwBW4q8jJIXtM
QiHN0sE9bClILMyujm8dbnhqwo8VDlIBixfvAeDDfGXETPJf0prO+64roLn44Lmc
uoXiWPtz4cP9OnvExsQ09F2jvY4I0fu0S/EE/rUvt3HYR3dlwfIUEwrkUVLHAXVN
pC8d1oK0b1USQndcDxNLrFsKJrgGMW1QqBg8WhsMRb9T2hkSlejProRPQ8zXx9QU
ZHYNXGHRAgMBAAECggEAHXG3wnjmUeYMhW/WypSLRs2icXhCgocXSTsrwlrdTv8W
knWJWPW1P1FcAN6xB8vOPmQ0+x+N7rbHRKMFwSah9s52xVEyAyTZjRzffJH1v9Zr
/4Iwm7J6hB72pbdnxUTnyLmQvA0XhizPpt4++GY2w/CJXFMO5PusrcUB4HZ60fU4
cSweQ9gS8AcJWSw53oUobUd3P4cs8GppIV9Y7lSf7bVXHW4COi6m1H7wP88EXLdF
bO3TJGoCjCVuc39J7aaddEFnOC1a5pMeDFfRyWI9GddDTbGaO/DMpegBOm1H3PuN
9JRoe0GYDovO0kRedXG0GeSYtWwUhC3TW+9FpoTYQwKBgQD85l7xC152ikbm472N
V1RLdmz7IGj2h1CvAKr9zSHrDHshGH7qu9scsMQJhiaftO4K+j9K/yrLZ6Z0MyZm
qjUhWG5Ko1HNE15Vn/TzJnrbK34AjRgA+WSYu2G1aYonDXFfCqVQTQblBD1JdLFl
yJ2crxGlRLDW6k6zo94vRA3jewKBgQC9bVDItHxS44gOmvm/X1mfb4hCsuB/TXtl
ybUqjitcS+l7LpYlXVK5AsLGOLFalVLpmdTnil+Te6UIfuZRHJbOHwdzbrV9WouJ
QFizVha0hWEiSDIn9rBwW/eiYHx3sUMYrSL9sw3l1hY4rJRye0hNOtDRGvKkGF87
f1WCFoxYIwKBgGHVmPr04/lFqwIdLjIki4aZ7LUFp7VfSZY50uxWU/3DGUsHV+nk
/HHP7DanAAAelUwH3T5dEigYE+u18fAbfmxE8n3LSpTkHkVPmQoLY9GvtJHwA041
LHZtXvI4Puq2p5oV2zgCZF1qU288RnhSsK5xh4kA1sx9Xi7egNCWKi1LAoGAEmri
g+EohVaiD+l5huXabOoHMZT6xuXdrZPjDXQtOxgWZ2esKY3dUSe3kFZKyCfkm9nd
HXYSvOA0t5K/CfvhncDelJabBska8AlVlno0UvD09MRYPBJ8LTCD70G6WFjf0TJk
SI50N3iNSI1fEZJzAS9Kxkn8dX/5ImvLAI74rkkCgYEAsXzOnFN2OIBwKG1gENdT
R7pPrq6q3IrWlWNz9k8ubrLeTeDb1LMsfWfbin4lACLlvUbwU6MYWiahK5fSmf6T
OhSrJ/eZSXSIwnVIKSEMQwy6+D+E6BWF8FtBhL38h8tiZXqpl2pz1G6vfMfIWf3J
saCwXnAhJ1c65yxGrEErPro=
-----END PRIVATE KEY-----"#;
const TEST_RSA_PUB: &str = r#"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuyIR/KX44NT/6Js//aIh
Qjqg0w2/tltL69pOTBcUZjWnjQJmL+6LVwAteFgQJh7CnV2dd1SYb/c2EY8gZ/ry
WCJU+TN1sgKj+Up/Ho0PZCGLPgg0JeUJQRWnoOsWibWOj8AVuKvIySF7TEIhzdLB
PWwpSCzMro5vHW54asKPFQ5SAYsX7wHgw3xlxEzyX9KazvuuK6C5+OC5nLqF4lj7
c+HD/Tp7xMbENPRdo72OCNH7tEvxBP61L7dx2Ed3ZcHyFBMK5FFSxwF1TaQvHdaC
tG9VEkJ3XA8TS6xbCia4BjFtUKgYPFobDEW/U9oZEpXoz66ET0PM18fUFGR2DVxh
0QIDAQAB
-----END PUBLIC KEY-----"#;

/// #395: GT's post-loop auth parameter checks (iperf_api.c:1843-1913) — a
/// half-configured auth flag set is a STAMPED parameter error at parse time,
/// never a silent unauthenticated run. All cells live-probed on 3.21 (banked
/// on the issue): the two IESETCLIENTAUTH/IESETSERVERAUTH combination checks,
/// the users-file access probe (IESERVERAUTHUSERS — checked BEFORE the
/// privkey load), the key-load failure arms (an error line precedes the
/// parameter error), and the headless-password cell (no IPERF3_PASSWORD, EOF
/// stdin → getpass fails → IESETCLIENTAUTH before any connect attempt).
/// Pre-fix riperf3 raised NONE of these: the client connected and a
/// half-auth server LISTENED and served unauthenticated (the security shape).
#[test]
fn auth_param_validations_match_gt() {
    const CLIENT_MSG: &str = "parameter error - you must specify a username, \
                              password, and path to a valid RSA public key";
    const SERVER_MSG: &str = "parameter error - you must specify a path to a \
                              valid RSA private key and a user credential file";
    const USERS_MSG: &str = "parameter error - cannot access authorized users file";

    let dir = std::env::temp_dir().join(format!("riperf3-auth-395-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pub_pem = dir.join("pub.pem");
    let priv_pem = dir.join("priv.pem");
    let users = dir.join("users.csv");
    let bogus = dir.join("bogus.pem");
    let missing = dir.join("nosuch.csv");
    std::fs::write(&pub_pem, TEST_RSA_PUB).unwrap();
    std::fs::write(&priv_pem, TEST_RSA_PRIV).unwrap();
    std::fs::write(&users, "user1,0123456789abcdef\n").unwrap();
    std::fs::write(&bogus, "not a key\n").unwrap();
    let p = |b: &std::path::Path| b.to_str().unwrap().to_string();

    // (args, IPERF3_PASSWORD value, wanted parameter-error sentence)
    let cases: Vec<(Vec<String>, Option<&str>, &str)> = vec![
        // The two halves of the client pair, each alone.
        (
            vec!["-c".into(), "127.0.0.1".into(), "--username".into(), "u".into()],
            None,
            CLIENT_MSG,
        ),
        (
            vec!["-c".into(), "127.0.0.1".into(), "--rsa-public-key-path".into(), p(&pub_pem)],
            None,
            CLIENT_MSG,
        ),
        // The two halves of the server pair, each alone.
        (
            vec!["-s".into(), "--rsa-private-key-path".into(), p(&priv_pem)],
            None,
            SERVER_MSG,
        ),
        (
            vec!["-s".into(), "--authorized-users-path".into(), p(&users)],
            None,
            SERVER_MSG,
        ),
        // Users-file access probe fires BEFORE the privkey load (GT order:
        // fopen at :1890, load_privkey at :1899) — the bogus key must not win.
        (
            vec![
                "-s".into(),
                "--rsa-private-key-path".into(),
                p(&bogus),
                "--authorized-users-path".into(),
                p(&missing),
            ],
            None,
            USERS_MSG,
        ),
        // Key-load failure arms (both halves present, file unparseable).
        (
            vec![
                "-c".into(),
                "127.0.0.1".into(),
                "--username".into(),
                "u".into(),
                "--rsa-public-key-path".into(),
                p(&bogus),
            ],
            Some("pw"),
            CLIENT_MSG,
        ),
        (
            vec![
                "-s".into(),
                "--rsa-private-key-path".into(),
                p(&bogus),
                "--authorized-users-path".into(),
                p(&users),
            ],
            None,
            SERVER_MSG,
        ),
        // Headless password cell: both halves valid, no env, stdin EOF —
        // GT's getpass fails and the PARSE errors; no connect is attempted.
        (
            vec![
                "-c".into(),
                "127.0.0.1".into(),
                "--username".into(),
                "u".into(),
                "--rsa-public-key-path".into(),
                p(&pub_pem),
            ],
            None,
            CLIENT_MSG,
        ),
    ];
    // Bounded runner: a REGRESSED check lets the `-s` cells pass the parse
    // and listen forever — `output()` would hang the suite (and eat a CI
    // wedge-guard timeout) instead of going red. Poll-wait 10 s, then kill.
    fn run_bounded(cmd: &mut std::process::Command) -> std::process::Output {
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = child.kill();
        let mut out = child.wait_with_output().unwrap();
        out.stderr
            .extend_from_slice(b"[test] KILLED: still running at the 10s bound");
        out
    }

    for (args, pw_env, want) in &cases {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"));
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .env_remove("IPERF3_PASSWORD")
            .env_remove("RIPERF3_PASSWORD");
        if let Some(pw) = pw_env {
            cmd.env("IPERF3_PASSWORD", pw);
        }
        let out = run_bounded(&mut cmd);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: exit 1: {stderr:?}");
        assert!(stderr.contains(want), "{args:?}: wanted {want:?}: {stderr:?}");
        assert!(
            stderr.contains("Usage: riperf3 [-s|-c host] [options]"),
            "{args:?}: the usage trailer rides the parameter error: {stderr:?}"
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?}: no banner, no doc — the check fires before any run \
             (the security half: a half-auth server must never listen): {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            !stderr.contains("unable to connect"),
            "{args:?}: parse-time, before any connect attempt: {stderr:?}"
        );
    }
}

/// #395 control: VALID auth combinations still pass the parse — the client
/// proceeds to its (dead-port) connect error, the server reaches its
/// listening banner. Guards the checks against over-firing.
#[test]
fn auth_param_valid_combos_pass_parse() {
    let dir = std::env::temp_dir().join(format!("riperf3-auth-395-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pub_pem = dir.join("pub.pem");
    let priv_pem = dir.join("priv.pem");
    let users = dir.join("users.csv");
    std::fs::write(&pub_pem, TEST_RSA_PUB).unwrap();
    std::fs::write(&priv_pem, TEST_RSA_PRIV).unwrap();
    std::fs::write(&users, "user1,0123456789abcdef\n").unwrap();

    // Client: parse passes, the run fails LATER at connect (dead port 1).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args([
            "-c",
            "127.0.0.1",
            "-p",
            "1",
            "--username",
            "u",
            "--rsa-public-key-path",
            pub_pem.to_str().unwrap(),
        ])
        .env("IPERF3_PASSWORD", "pw")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("parameter error"),
        "valid client auth trio passes the parse: {stderr:?}"
    );
    assert!(
        stderr.contains("unable to connect"),
        "the run proceeded to the connect attempt: {stderr:?}"
    );

    // Server: parse passes, the banner prints. Bounded spawn + kill; the
    // banner is well under FreeBSD's 8KiB single-write pipe bound, so
    // wait-then-read is safe here (#305 rule).
    let port = common::free_port().to_string();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_riperf3"))
        .args([
            "-s",
            "-p",
            &port,
            "--rsa-private-key-path",
            priv_pem.to_str().unwrap(),
            "--authorized-users-path",
            users.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("parameter error"),
        "valid server auth pair passes the parse: {stderr:?}"
    );
    assert!(
        stdout.contains("Server listening"),
        "the fully-configured auth server reaches its banner: {stdout:?}"
    );
}
