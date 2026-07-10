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

/// #380 (r1 F1): the mock completes setup through CreateStreams, then
/// FINs ctrl WITHOUT TestStart — the client errors at the next
/// recv_state and reaches the gate as the FIRST to stop the streams, so
/// it owes the 100 ms grace sleep. `setup_done` fires right after the
/// FIN so the caller can time its cancel INTO that sleep: a disarm
/// placed before the grace await leaves a window where the guard is
/// dead but the gate's own abort hasn't run — the drop leaks exactly
/// like an unguarded cancel.
fn mock_fin_after_create_streams(
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
    // setup_done BEFORE the FIN (#426 r2 F1): the grace clock starts at
    // the client's FIN observation, the cancel clock at this send — a
    // mock stall between the two must delay the GATE (safe: an early
    // cancel lands pre-gate, still guarded), never the cancel (a late
    // cancel could miss a finished gate and trip the res assert).
    let _ = setup_done.send(());
    drop(ctrl); // FIN mid-setup — the gate will owe the grace
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
    let mock = std::thread::spawn(move || {
        mock_fin_after_create_streams(listener, 2, setup_tx, dropped_rx)
    });

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
