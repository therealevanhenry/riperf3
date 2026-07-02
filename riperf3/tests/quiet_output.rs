//! #290: `emit_output(false)` — a library caller can run a test without the
//! crate writing to the host process's stdout/stderr. Verified by re-exec:
//! the parent spawns THIS test binary with a marker env var; the child arm
//! runs a real loopback test in-process and the parent asserts on the child's
//! actual stdout/stderr bytes (an in-process capture can't observe the
//! `println!` layer these gates suppress).

use std::process::Command;

/// Child arm (r1 finding 1 / mutations a+c): a QUIET one-off `Server::run`
/// serving a LOUD in-process client, then a fully LOUD pair in the SAME
/// process. The parent asserts (1) no server-side line — the listening banner
/// included — leaked while the quiet server ran, (2) the loud client's output
/// is present during the overlap (the client's own loudness must survive the
/// server's guard is a NON-goal: the guard is process-global by design, so
/// the overlap window silences both — what MUST hold is that the SECOND,
/// fully-loud pair prints, proving the guard count returned to zero).
fn child_quiet_server_then_loud_pair() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Phase 1: quiet daemon-loop server (one_off via run()) + client.
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .emit_output(false)
            .build()
            .unwrap();
        client.run().await.expect("phase-1 client");
        server_task.await.unwrap().expect("phase-1 server");

        // Phase 1b: the DAEMON entry point itself — a quiet Server::run()
        // (one_off) with no client, killed by idle timeout. The listening
        // banner is the r1-finding-1 leak site.
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .idle_timeout(1)
            .one_off(true)
            .emit_output(false)
            .build()
            .unwrap();
        let _ = server.run().await; // idle timeout -> Err(Aborted), fine

        // Phase 2: a fully LOUD pair in the same process — proves every quiet
        // guard dropped back to zero (a leaked increment would silence this).
        let server = riperf3::ServerBuilder::new().port(Some(0)).build().unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .build()
            .unwrap();
        client.run().await.expect("phase-2 client");
        server_task.await.unwrap().expect("phase-2 server");
    });
    eprintln!("QUIET_CHILD_DONE");
}

/// Child arm (r1 mutation b): a quiet `--json-stream` pair — no `{"event"...`
/// lines may reach stdout.
fn child_quiet_json_stream() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .json_stream(true)
            .emit_output(false)
            .build()
            .unwrap();
        let report = client.run().await.expect("client run");
        assert!(
            report.end.sum_sent.as_ref().unwrap().bytes > 0,
            "quiet json-stream still returns the report"
        );
        server_task.await.unwrap().expect("server");
    });
    eprintln!("QUIET_CHILD_DONE");
}

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
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("server-then-loud") {
        child_quiet_server_then_loud_pair();
        return;
    }
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("json-stream") {
        child_quiet_json_stream();
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

/// r1 finding 1 + mutations a/c: the quiet daemon path may not leak the
/// listening banner, and a loud run AFTER quiet runs in one process must
/// still print (the guard count must return to zero).
#[test]
fn quiet_server_daemon_is_silent_and_loudness_recovers() {
    if std::env::var("RIPERF3_QUIET_CHILD").is_ok() {
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "quiet_run_writes_nothing_to_stdout",
            "--exact",
            "--nocapture",
        ])
        .env("RIPERF3_QUIET_CHILD", "server-then-loud")
        .output()
        .expect("re-exec child");
    assert!(out.status.success(), "child failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("Server listening"),
        "the quiet daemon's listening banner leaked (r1 finding 1): {stdout}"
    );
    // Phase 2 (fully loud) must have printed: guard count returned to zero.
    assert!(
        stdout.contains("iperf Done."),
        "the loud phase after quiet runs printed nothing — a quiet guard \
         leaked its increment (r1 mutation a): {stdout}"
    );
}

/// r1 mutation b: a quiet --json-stream run emits no event lines.
#[test]
fn quiet_json_stream_emits_no_events() {
    if std::env::var("RIPERF3_QUIET_CHILD").is_ok() {
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "quiet_run_writes_nothing_to_stdout",
            "--exact",
            "--nocapture",
        ])
        .env("RIPERF3_QUIET_CHILD", "json-stream")
        .output()
        .expect("re-exec child");
    assert!(out.status.success(), "child failed: {out:?}");
    let leaked = non_harness_stdout(&out.stdout);
    assert!(
        leaked.is_empty(),
        "quiet json-stream leaked stdout lines (r1 mutation b): {leaked:#?}"
    );
}
