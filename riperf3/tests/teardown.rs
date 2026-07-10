//! #375: an abnormal-path early return from `Client::run` must still tear
//! down the spawned stream tasks. A detached task parked in `read().await`
//! against a holding peer survives `done` (the flag cannot wake a parked
//! read) and leaks with its fd — visible only to LIBRARY consumers, so
//! this is an in-process lib test: a CLI process exit closes the fds and
//! masks the class (the #356 bound_server.rs in-process precedent).

use std::io::{Read, Write};
use std::time::Duration;

use riperf3::ClientBuilder;

fn read_exact(s: &mut std::net::TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).expect("read_exact");
    buf
}

/// Mock server: full setup through TestRunning, ~0.3 s of reverse traffic,
/// then SILENCE — the client's receivers drain the tail and PARK in
/// `read().await` (the silence-before-the-kill timing is load-bearing, the
/// #354 r1 F1 lesson: a still-flowing peer parks nothing) — then a plain
/// FIN of the ctrl. The client's data-phase control watch folds the dead
/// ctrl to its closed class and `run()` propagates the error out of the
/// TestRunning arm. The mock then reads each still-held data socket with a
/// bounded timeout: a teardown-less client HOLDS them (the parked tasks
/// own the fds, and the test's runtime is still alive), so the reads time
/// out; a run() that reaps its tasks before returning closes them
/// immediately.
fn mock_fin_mid_running(
    listener: std::net::TcpListener,
    parallel: usize,
) -> Vec<Result<usize, std::io::ErrorKind>> {
    let (mut ctrl, _) = listener.accept().expect("ctrl accept");
    read_exact(&mut ctrl, 37); // cookie
    ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
    let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
    read_exact(&mut ctrl, len); // the client's params blob
    ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
    let mut datas = Vec::new();
    for _ in 0..parallel {
        let (mut data, _) = listener.accept().expect("data accept");
        read_exact(&mut data, 37); // data-stream cookie
        datas.push(data);
    }
    ctrl.write_all(&[1u8]).unwrap(); // TestStart
    ctrl.write_all(&[2u8]).unwrap(); // TestRunning
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_millis(300) {
        for data in &mut datas {
            data.write_all(&[0u8; 8192]).expect("reverse burst");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Silence: let the receivers drain and park before the ctrl dies.
    std::thread::sleep(Duration::from_millis(200));
    drop(ctrl); // FIN → the client's closed class → run() errors
    let mut reads = Vec::new();
    for data in &mut datas {
        data.set_read_timeout(Some(Duration::from_secs(4)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        reads.push(data.read(&mut buf).map_err(|e| e.kind()));
    }
    reads
}

/// #380: like [`mock_fin_mid_running`] but the mock NEVER kills the round —
/// it holds ctrl and data open, silent, while the caller CANCELS `run()`
/// (a dropped future skips every teardown gate). The bounded reads then
/// tell leak from abort.
fn mock_hold_mid_running(
    listener: std::net::TcpListener,
    parallel: usize,
    hold: std::sync::mpsc::Receiver<()>,
) -> Vec<Result<usize, std::io::ErrorKind>> {
    let (mut ctrl, _) = listener.accept().expect("ctrl accept");
    read_exact(&mut ctrl, 37); // cookie
    ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
    let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
    read_exact(&mut ctrl, len); // the client's params blob
    ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
    let mut datas = Vec::new();
    for _ in 0..parallel {
        let (mut data, _) = listener.accept().expect("data accept");
        read_exact(&mut data, 37); // data-stream cookie
        datas.push(data);
    }
    ctrl.write_all(&[1u8]).unwrap(); // TestStart
    ctrl.write_all(&[2u8]).unwrap(); // TestRunning
    let t0 = std::time::Instant::now();
    while t0.elapsed() < Duration::from_millis(300) {
        for data in &mut datas {
            data.write_all(&[0u8; 8192]).expect("reverse burst");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Silence: the receivers drain and PARK (the #354 timing lesson) —
    // then wait for the caller to have dropped the run() future.
    let _ = hold.recv_timeout(Duration::from_secs(10));
    let mut reads = Vec::new();
    for data in &mut datas {
        data.set_read_timeout(Some(Duration::from_secs(4)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        reads.push(data.read(&mut buf).map_err(|e| e.kind()));
    }
    // ctrl stayed open the whole time: the round was never killed by the
    // peer — only the cancellation ended it.
    drop(ctrl);
    reads
}

/// #380 (r1 F1): the mock completes setup through TestStart (the grace
/// is owed only from TestStart on — #427 r1 F2), then FINs ctrl WITHOUT
/// TestRunning — the client errors at the next recv_state and reaches
/// the gate as the FIRST to stop the streams, so it owes the 100 ms
/// grace sleep. `setup_done` fires right before the FIN so the caller
/// can time its cancel INTO that sleep: a disarm placed before the
/// grace await leaves a window where the guard is dead but the gate's
/// own abort hasn't run — the drop leaks exactly like an unguarded
/// cancel.
fn mock_fin_after_test_start(
    listener: std::net::TcpListener,
    parallel: usize,
    setup_done: tokio::sync::oneshot::Sender<()>,
    hold: std::sync::mpsc::Receiver<()>,
) -> Vec<Result<usize, std::io::ErrorKind>> {
    let (mut ctrl, _) = listener.accept().expect("ctrl accept");
    read_exact(&mut ctrl, 37); // cookie
    ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
    let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
    read_exact(&mut ctrl, len); // the client's params blob
    ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
    let mut datas = Vec::new();
    for _ in 0..parallel {
        let (mut data, _) = listener.accept().expect("data accept");
        read_exact(&mut data, 37); // data-stream cookie
        datas.push(data);
    }
    // TestStart first: the grace is owed only once the test started
    // (#427 r1 F2) — a pre-TestStart FIN would exit without the sleep
    // and the cancel window under pin would not exist.
    ctrl.write_all(&[1u8]).unwrap();
    // setup_done BEFORE the FIN (#426 r2 F1): the grace clock starts at
    // the client's FIN observation, the cancel clock at this send — a
    // mock stall between the two must delay the GATE (safe: an early
    // cancel lands pre-gate, still guarded), never the cancel (a late
    // cancel could miss a finished gate and trip the res assert).
    let _ = setup_done.send(());
    drop(ctrl); // FIN pre-TestRunning — the gate will owe the grace
    let _ = hold.recv_timeout(Duration::from_secs(10));
    let mut reads = Vec::new();
    for data in &mut datas {
        data.set_read_timeout(Some(Duration::from_secs(4)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        reads.push(data.read(&mut buf).map_err(|e| e.kind()));
    }
    reads
}

/// #380 r1 F1: a cancel landing INSIDE the gate's grace sleep (the
/// 100 ms owed when the gate is first to stop the streams) must still
/// abort — the guard has to stay armed through every await between the
/// gate's entry and its abort loop. The cancel is timed off the mock's
/// setup_done signal (+60 ms of purely local client work), not an
/// absolute deadline, so a slow runner degrades to an earlier —
/// still-guarded — cancel point instead of a mistimed miss. run() can't
/// win the select: this path owes ≥100 ms of grace after a FIN that
/// never precedes the setup_done send.
#[test]
fn client_cancelled_during_grace_window_aborts_stream_tasks() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let (setup_tx, setup_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let mock =
        std::thread::spawn(move || mock_fin_after_test_start(listener, 2, setup_tx, dropped_rx));

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .num_streams(2)
        .duration(30)
        .json_output(true)
        .build()
        .expect("build client");
    let res = rt.block_on(async {
        tokio::select! {
            r = client.run() => Some(r),
            _ = async {
                let _ = setup_rx.await;
                tokio::time::sleep(Duration::from_millis(60)).await;
            } => None, // the run() future is dropped as select! returns
        }
    });
    assert!(res.is_none(), "the cancel wins — run() owes the grace");
    dropped_tx.send(()).expect("mock alive");

    let reads = mock.join().expect("mock");
    for (i, r) in reads.iter().enumerate() {
        assert!(
            matches!(r, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
            "stream {i}: a cancel inside the grace window must still abort \
             the stream tasks (#380 r1 F1): {r:?}"
        );
    }
    drop(rt);
}

/// #380: a run() future dropped mid-TEST_RUNNING (tokio::time::timeout —
/// the pattern every library consumer reaches for) must not leak the
/// parked stream tasks: the drop skips every teardown gate, `done` can't
/// wake a parked read, and Drop can't await joins — the abort guard is
/// the only thing standing between the cancel and a leaked fd. The
/// runtime stays alive (dropping it would reap the tasks and mask the
/// leak).
#[test]
fn client_cancelled_run_aborts_stream_tasks() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
    let mock = std::thread::spawn(move || mock_hold_mid_running(listener, 2, dropped_rx));

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .num_streams(2)
        .duration(30) // the CANCEL at ~1 s ends the run, not the timer
        .json_output(true)
        .build()
        .expect("build client");
    let res = rt.block_on(async {
        // ~3 s: past the mock's 300 ms burst + parked silence, with margin
        // for a starved 2-core runner (a cancel BEFORE setup completes
        // would pass vacuously; the mock holds for 10 s either way).
        tokio::time::timeout(Duration::from_secs(3), client.run()).await
    });
    assert!(res.is_err(), "the timeout cancels the run — no result");
    // The future is dropped; tell the mock to start its bounded reads.
    dropped_tx.send(()).expect("mock alive");

    let reads = mock.join().expect("mock");
    for (i, r) in reads.iter().enumerate() {
        assert!(
            matches!(r, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
            "stream {i}: a cancelled run() must abort its stream tasks — \
             the held data socket sees a bounded close (#380): {r:?}"
        );
    }
    drop(rt);
}

/// #380, server half: a `BoundServer::run_once` future dropped
/// mid-TEST_RUNNING must abort ITS stream tasks (+ the UDP demux) the same
/// way. A mock client completes setup (forward TCP — the server's receiver
/// parks in read() once the burst stops), holds everything open, and the
/// bounded read on the held data socket tells leak from abort.
#[test]
fn server_cancelled_run_once_aborts_stream_tasks() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let (bound, port) = rt.block_on(async {
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.expect("bind");
        let port = bound.local_addr().unwrap().port();
        (bound, port)
    });
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel::<()>();
    let mock = std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        let params = br#"{"tcp":true,"time":30,"parallel":1,"len":4096}"#;
        ctrl.write_all(&(params.len() as u32).to_be_bytes())
            .unwrap();
        ctrl.write_all(params).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        let mut data = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data");
        data.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 1, "TestStart");
        assert_eq!(read_exact(&mut ctrl, 1)[0], 2, "TestRunning");
        // Forward burst, then SILENCE — the server's receiver parks.
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_millis(300) {
            data.write_all(&[0u8; 8192]).expect("forward burst");
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = dropped_rx.recv_timeout(Duration::from_secs(10));
        data.set_read_timeout(Some(Duration::from_secs(4)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        let r = data.read(&mut buf).map_err(|e| e.kind());
        drop(ctrl);
        r
    });

    let res = rt.block_on(async {
        // ~3 s: past the burst + parked silence (starved-runner margin,
        // like the client cell); the future drops here.
        tokio::time::timeout(Duration::from_secs(3), bound.run_once()).await
    });
    assert!(res.is_err(), "the timeout cancels run_once — no result");
    dropped_tx.send(()).expect("mock alive");

    let read = mock.join().expect("mock");
    assert!(
        matches!(read, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
        "a cancelled run_once must abort its stream tasks — the held data \
         socket sees a bounded close (#380): {read:?}"
    );
    drop(rt);
}

/// #381 (client half): the mock completes setup through CreateStreams,
/// accepts data conn 1, and drops the listener so the client's SECOND
/// data connect is refused. `create_streams` errors mid-loop with stream
/// 1's task already spawned: a local-vec build drops that partial
/// progress on the floor (the gate joins an empty `ctx.streams`) and the
/// parked task leaks with its fd. The held conn-1 read tells leak from
/// reap after `run()` returns.
///
/// NOT fully deterministic (#427 r1 F1): conn 2's SYN completes via the
/// accept BACKLOG, independent of accept() — if it beats the drop, conn 2
/// succeeds and the round parks instead of erring. The listener drops
/// IMMEDIATELY after conn 1's accept (before the cookie read, which would
/// serialize on the client's write and hand conn 2 the whole window);
/// the residual race is caller-handled by the bounded-retry loop.
fn mock_refuse_second_data_conn(
    listener: std::net::TcpListener,
    hold: std::sync::mpsc::Receiver<()>,
) -> Result<usize, std::io::ErrorKind> {
    let (mut ctrl, _) = listener.accept().expect("ctrl accept");
    read_exact(&mut ctrl, 37); // cookie
    ctrl.write_all(&[9u8]).unwrap(); // ParamExchange
    let len = u32::from_be_bytes(read_exact(&mut ctrl, 4).try_into().unwrap()) as usize;
    read_exact(&mut ctrl, len); // the client's params blob
    ctrl.write_all(&[10u8]).unwrap(); // CreateStreams
    let (mut data1, _) = listener.accept().expect("data 1 accept");
    drop(listener); // conn 2 → ECONNREFUSED (unless it already sneaked in)
    read_exact(&mut data1, 37); // data-stream cookie — stream 1 spawns
    let _ = hold.recv_timeout(Duration::from_secs(10));
    data1
        .set_read_timeout(Some(Duration::from_secs(4)))
        .expect("set_read_timeout");
    let mut buf = [0u8; 16];
    let r = data1.read(&mut buf).map_err(|e| e.kind());
    drop(ctrl);
    r
}

/// #381: a mid-`create_streams` error (the second data connect refused)
/// must still tear down the FIRST stream's already-spawned task — partial
/// setup progress has to be visible to the teardown gate, not dropped in
/// a local vec.
///
/// #427 r1 F1: each attempt detects the backlog-sneak mode (conn 2 got in
/// before the drop → run() parks on the silent ctrl) via a 3 s timeout
/// and retries with a fresh listener — the dropped attempt's tasks are
/// reaped by the #380 abort guard (pinned by the cancel cells above), so
/// a retried attempt leaks nothing. The leak assert runs only on a
/// refused-mode round. P(sneak) is a few percent under 2-core load with
/// the old ordering and near zero with the drop-early mock; five
/// attempts bound the flake without weakening the assert.
#[test]
fn client_error_mid_create_streams_tears_down_earlier_streams() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut refused_mode_read = None;
    for _ in 0..5 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let port = listener.local_addr().unwrap().port();
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        let mock = std::thread::spawn(move || mock_refuse_second_data_conn(listener, returned_rx));

        let client = ClientBuilder::new("127.0.0.1")
            .port(Some(port))
            .reverse(true)
            .num_streams(2)
            .duration(30)
            .json_output(true)
            .build()
            .expect("build client");
        let res = rt.block_on(async {
            // Refused mode errors out in well under a second; only the
            // sneak mode parks (the mock holds ctrl silently).
            tokio::time::timeout(Duration::from_secs(3), client.run()).await
        });
        match res {
            Ok(r) => {
                assert!(r.is_err(), "the refused data connect surfaces an error");
                returned_tx.send(()).expect("mock alive");
                refused_mode_read = Some(mock.join().expect("mock"));
                break;
            }
            Err(_elapsed) => {
                // Sneak mode: the round parked. The drop above already
                // cancelled run(); the abort guard reaped its tasks —
                // release the mock and go again.
                let _ = returned_tx.send(());
                let _ = mock.join();
            }
        }
    }
    let read = refused_mode_read.expect("no attempt reached the refused mode in 5 tries");
    assert!(
        matches!(read, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
        "a mid-create_streams error must tear down stream 1's spawned \
         task — the held data socket sees a bounded close (#381): {read:?}"
    );
    drop(rt);
}

/// #381 (server half, cancel flavor — the #426 r1 F2 window): a
/// `run_once` future dropped while the server waits for the SECOND data
/// connection must abort the FIRST stream's already-spawned task. The
/// wait state is stable (the no-progress clock is 120 s), so the 3 s
/// cancel lands deterministically mid-setup; pre-fix the abort guard is
/// not yet armed there and the parked task leaks.
#[test]
fn server_cancelled_mid_setup_aborts_earlier_streams() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let (bound, port) = rt.block_on(async {
        let server = riperf3::ServerBuilder::new()
            .port(Some(0))
            .emit_output(false)
            .build()
            .unwrap();
        let bound = server.bind().await.expect("bind");
        let port = bound.local_addr().unwrap().port();
        (bound, port)
    });
    let (dropped_tx, dropped_rx) = std::sync::mpsc::channel::<()>();
    let mock = std::thread::spawn(move || {
        let cookie = [b'x'; 37];
        let mut ctrl = std::net::TcpStream::connect(("127.0.0.1", port)).expect("ctrl");
        ctrl.write_all(&cookie).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 9, "ParamExchange");
        let params = br#"{"tcp":true,"time":30,"parallel":2,"len":4096}"#;
        ctrl.write_all(&(params.len() as u32).to_be_bytes())
            .unwrap();
        ctrl.write_all(params).unwrap();
        assert_eq!(read_exact(&mut ctrl, 1)[0], 10, "CreateStreams");
        // Data conn 1 only — the server spawns its receiver (parked: no
        // traffic follows) and keeps waiting for conn 2. NEVER connect it.
        let mut data1 = std::net::TcpStream::connect(("127.0.0.1", port)).expect("data 1");
        data1.write_all(&cookie).unwrap();
        let _ = dropped_rx.recv_timeout(Duration::from_secs(10));
        data1
            .set_read_timeout(Some(Duration::from_secs(4)))
            .expect("set_read_timeout");
        let mut buf = [0u8; 16];
        let r = data1.read(&mut buf).map_err(|e| e.kind());
        drop(ctrl);
        r
    });

    let res = rt.block_on(async {
        // 3 s: the mock's setup takes ms; the server then sits in the
        // stable conn-2 accept wait, where the cancel lands.
        tokio::time::timeout(Duration::from_secs(3), bound.run_once()).await
    });
    assert!(res.is_err(), "the timeout cancels run_once mid-setup");
    dropped_tx.send(()).expect("mock alive");

    let read = mock.join().expect("mock");
    assert!(
        matches!(read, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
        "a run_once cancelled mid-setup must abort stream 1's spawned \
         task (#381): {read:?}"
    );
    drop(rt);
}

/// #375: reverse -P 2 (a partial teardown — stream 0 only — still reds),
/// ctrl FIN mid-running.
#[test]
fn client_error_return_tears_down_stream_tasks() {
    // Plain #[test] + explicit runtime: the runtime must stay ALIVE while
    // the mock's bounded reads run — dropping it reaps leaked tasks and
    // would mask exactly the leak under test.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let mock = std::thread::spawn(move || mock_fin_mid_running(listener, 2));

    let client = ClientBuilder::new("127.0.0.1")
        .port(Some(port))
        .reverse(true)
        .num_streams(2)
        .duration(30) // the FIN at ~0.5 s ends the run, not the timer
        .json_output(true)
        .build()
        .expect("build client");
    let res = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(8), client.run()).await })
        .expect("run() exits bounded after the ctrl FIN");
    assert!(
        res.is_err(),
        "the dead ctrl surfaces an error, not a report"
    );

    let reads = mock.join().expect("mock");
    for (i, r) in reads.iter().enumerate() {
        assert!(
            matches!(r, Ok(0) | Err(std::io::ErrorKind::ConnectionReset)),
            "stream {i}: the held data socket sees a bounded close \
             (run() reaped its stream tasks before returning): {r:?}"
        );
    }
    drop(rt);
}
