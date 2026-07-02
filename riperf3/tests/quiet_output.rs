//! #290: `emit_output(false)` — a library caller can run a test without the
//! crate writing to the host process's stdout/stderr. Verified by re-exec:
//! the parent spawns THIS test binary with a marker env var; the child arm
//! runs a real loopback test in-process and the parent asserts on the child's
//! actual stdout/stderr bytes (an in-process capture can't observe the
//! `println!` layer these gates suppress).

use std::process::Command;

/// Child arm: run one loopback text-mode test with the given emit setting and
/// exit. Everything the crate prints (or correctly doesn't) lands on the
/// child's real stdout/stderr for the parent to inspect.
fn child_run(quiet: bool) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(!quiet)
            .build()
            .unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .emit_output(!quiet)
            .build()
            .unwrap();
        let report = client.run().await.expect("client run");
        assert!(
            report.end.sum_sent.as_ref().unwrap().bytes > 0,
            "the quiet run still returns a real report"
        );
        let _ = server_task.await.expect("server task").expect("run_once");
    });
    // The child prints this marker itself so the parent can prove the child
    // arm actually ran (as opposed to the harness filtering everything).
    eprintln!("QUIET_CHILD_DONE");
}

fn spawn_child(quiet: bool, test_name: &str) -> std::process::Output {
    Command::new(std::env::current_exe().unwrap())
        .args([test_name, "--exact", "--nocapture"])
        .env("RIPERF3_QUIET_CHILD", if quiet { "1" } else { "0" })
        .output()
        .expect("re-exec child")
}

/// Lines the libtest harness itself prints in the child; everything else on
/// stdout must have come from the riperf3 crate.
fn non_harness_stdout(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("running ")
                && !t.starts_with("test ")
                && !t.starts_with("test result:")
        })
        .map(str::to_owned)
        .collect()
}

#[test]
fn quiet_run_writes_nothing_to_stdout() {
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("1") {
        child_run(true);
        return;
    }
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("0") {
        // This test fn doubles as the child entry for both settings so the
        // two parents below can't race each other's env.
        child_run(false);
        return;
    }

    let out = spawn_child(true, "quiet_run_writes_nothing_to_stdout");
    assert!(out.status.success(), "child failed: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("QUIET_CHILD_DONE"),
        "the child arm must have run: {err}"
    );
    let leaked = non_harness_stdout(&out.stdout);
    assert!(
        leaked.is_empty(),
        "emit_output(false) must write NOTHING to stdout, got: {leaked:#?}"
    );
}

/// Positive control for the harness filter above: the SAME child with output
/// enabled must visibly print the text report — proving the quiet assertion
/// isn't vacuously green because the filter ate everything.
#[test]
fn loud_run_control_prints_the_report() {
    if std::env::var("RIPERF3_QUIET_CHILD").is_ok() {
        // Child arms are handled by the quiet test's entry; this fn is
        // parent-only (spawns THAT test's name with quiet off).
        return;
    }
    let out = spawn_child(false, "quiet_run_writes_nothing_to_stdout");
    assert!(out.status.success(), "child failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("iperf Done."),
        "the loud control run must print the closing banner: {stdout}"
    );
    assert!(
        stdout.contains("Connecting to host"),
        "the loud control run must print the connect banner: {stdout}"
    );
}
