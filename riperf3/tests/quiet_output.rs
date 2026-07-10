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
        let _ = server.run().await; // one-off idle timeout ends the loop Ok(())

        // Phase 2: a fully LOUD pair in the same process — proves every quiet
        // guard dropped back to zero (a leaked increment would silence this).
        // #294: the library default is now quiet, so the loud pair opts in
        // explicitly with emit_output(true).
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(true)
            .build()
            .unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .emit_output(true)
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
        let report = client.run().await.expect("client run").report;
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
        let report = client.run().await.expect("client run").report;
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

/// Child arm (#294 pin, CLIENT half): a BARE client — no `emit_output` call,
/// so the flipped default itself is what's exercised — against a mock server
/// that accepts, reads the cookie, and goes silent; the interrupt watch ends
/// the run. Crucially NO riperf3 server runs in this process: the quiet guard
/// is process-global, so a same-process server's correct default would mask a
/// reverted client default (which is exactly why a bare PAIR pins nothing —
/// the review's revert probe survived the whole suite). A `true` client
/// default leaks the interrupt dump (the `- - - - -` separator + interval
/// header) — r1: the connect banner does NOT leak here, it waits on a
/// PARAM_EXCHANGE the mock never sends; the dump is synchronous with the
/// interrupt in any phase, so the pin doesn't ride the 500 ms timing.
fn child_bare_default_client() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mock = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::AsyncReadExt;
                let mut cookie = [0u8; 37];
                let _ = sock.read_exact(&mut cookie).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        });
        let (tx, rx) = tokio::sync::watch::channel::<Option<String>>(None);
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(5)
            .interrupt(rx)
            .build()
            .unwrap();
        let run = tokio::spawn(async move { client.run().await });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        tx.send(Some(
            "interrupt - the client has terminated by signal Terminated(15)".into(),
        ))
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("interrupt honored")
            .expect("join")
            .expect("interrupted run is Ok");
        mock.abort();
    });
    eprintln!("QUIET_CHILD_DONE");
}

/// Child arm (#294 pin, SERVER half): a BARE `-J` server — no `emit_output`
/// call — serving one test from an explicitly LOUD text client. The loud
/// client arms no quiet guard, so the server's own default alone decides
/// whether its `-J` document reaches stdout: a reverted `true` default
/// prints it (a line starting with `{`); the correct quiet default prints
/// nothing server-side (its guard silences the loud client too — the
/// process-global overlap — so the parent just asserts no `{` line).
fn child_bare_default_server() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .json_output(true)
            .build()
            .unwrap();
        let bound = server.bind().await.unwrap();
        let port = bound.local_addr().unwrap().port();
        let server_task = tokio::spawn(async move { bound.run_once().await });
        let client = riperf3::ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .duration(1)
            .emit_output(true)
            .build()
            .unwrap();
        client.run().await.expect("client run");
        let _ = server_task.await.expect("server task").expect("run_once");
    });
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
/// r2 nit: quiet children may emit ONLY the completion marker on stderr —
/// any other line means a stderr gate regressed (the error sinks, the auth
/// prompt, the daemon loop's eprintln paths).
fn assert_stderr_only_marker(out: &std::process::Output) {
    let err = String::from_utf8_lossy(&out.stderr);
    let stray: Vec<&str> = err
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && t != "QUIET_CHILD_DONE"
        })
        .collect();
    assert!(
        stray.is_empty(),
        "a quiet child wrote to stderr beyond the marker: {stray:#?}"
    );
}

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
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("bare-client") {
        child_bare_default_client();
        return;
    }
    if std::env::var("RIPERF3_QUIET_CHILD").as_deref() == Ok("bare-server") {
        child_bare_default_server();
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
    assert_stderr_only_marker(&out);
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

/// #294 (review sweep): a client built with NO `emit_output` call is quiet —
/// the flipped DEFAULT itself is under test, in a process with no riperf3
/// server whose own guard could mask a revert. Reverting the ClientBuilder
/// default to `true` fails this (the interrupt dump lines leak).
#[test]
fn bare_default_client_is_quiet() {
    if std::env::var("RIPERF3_QUIET_CHILD").is_ok() {
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "quiet_run_writes_nothing_to_stdout",
            "--exact",
            "--nocapture",
        ])
        .env("RIPERF3_QUIET_CHILD", "bare-client")
        .output()
        .expect("re-exec child");
    assert!(out.status.success(), "child failed: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("QUIET_CHILD_DONE"),
        "the child arm must have run: {err}"
    );
    let leaked = non_harness_stdout(&out.stdout);
    assert!(
        leaked.is_empty(),
        "a bare (no emit_output call) client must write NOTHING to stdout \
         — the #294 client default regressed: {leaked:#?}"
    );
    assert_stderr_only_marker(&out);
}

/// #294 (review sweep), server half: a `-J` server built with NO
/// `emit_output` call emits no JSON document. Reverting the ServerBuilder
/// default to `true` fails this (the doc's `{` line leaks — the driving
/// client is explicitly loud, so no other guard is in play).
#[test]
fn bare_default_server_is_quiet() {
    if std::env::var("RIPERF3_QUIET_CHILD").is_ok() {
        return;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "quiet_run_writes_nothing_to_stdout",
            "--exact",
            "--nocapture",
        ])
        .env("RIPERF3_QUIET_CHILD", "bare-server")
        .output()
        .expect("re-exec child");
    assert!(out.status.success(), "child failed: {out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("QUIET_CHILD_DONE"),
        "the child arm must have run: {err}"
    );
    let json_leak: Vec<String> = non_harness_stdout(&out.stdout)
        .into_iter()
        .filter(|l| l.trim_start().starts_with('{'))
        .collect();
    assert!(
        json_leak.is_empty(),
        "a bare (no emit_output call) -J server must not print its document \
         — the #294 server default regressed: {json_leak:#?}"
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
    assert_stderr_only_marker(&out);
}
