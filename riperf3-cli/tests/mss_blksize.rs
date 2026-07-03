//! #269: the UDP default block size derives from the control socket's
//! TCP_MAXSEG, read AFTER the cookie write like GT (iperf_client_api.c:
//! Nwrite cookie at :467, getsockopt at :476). The kernel's MSS estimate
//! settles once traffic has flowed — on Linux loopback the pre-write read
//! said 32741 (advmss/2 rounding) where GT reports the settled 32768, and
//! the figure is wire-visible in `test_start.blksize`.
//!
//! Unix-gated: the reference measurement needs TCP_MAXSEG, which Windows
//! doesn't expose (riperf3 falls back to the 1460 default there like GT).

#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::Duration;

mod common;

fn maxseg(stream: &TcpStream) -> i64 {
    let mut v: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::IPPROTO_TCP,
            libc::TCP_MAXSEG,
            &mut v as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    assert_eq!(rc, 0, "TCP_MAXSEG must be readable on unix");
    v as i64
}

/// The reference mirrors GT's lifecycle exactly — connect, write the
/// 37-byte cookie, peer consumes it, then read TCP_MAXSEG — and riperf3's
/// advertised UDP blksize must equal it (pre-fix: the pre-cookie read
/// diverged by 27 bytes on loopback and the two tools advertised
/// different defaults).
#[test]
fn udp_default_blksize_reads_mss_after_the_cookie_like_gt() {
    let _serial = common::udp_serial();

    // Reference measurement.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    let reader = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let mut cookie = [0u8; 37];
        s.read_exact(&mut cookie).expect("read cookie");
        s // hold the peer open until joined
    });
    let mut ctrl = TcpStream::connect(addr).expect("connect");
    ctrl.write_all(&[b'x'; 37]).expect("write cookie");
    let _peer = reader.join().expect("reader");
    let want = maxseg(&ctrl);

    // riperf3 -u with no -l advertises the same post-cookie figure.
    let port = common::free_port().to_string();
    let mut server = common::ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_riperf3"))
            .args(["-s", "-1", "-p", &port])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn server"),
    );
    let client = common::run_client_ok(
        &["-c", "127.0.0.1", "-p", &port, "-u", "-t", "1", "-J"],
        Duration::from_secs(30),
        "udp client",
    );
    let doc: serde_json::Value = serde_json::from_str(client.stdout.trim()).expect("client doc");
    assert_eq!(
        doc["start"]["test_start"]["blksize"].as_i64(),
        Some(want),
        "test_start.blksize = the post-cookie TCP_MAXSEG (GT's read point)"
    );
    let _ = server.0.wait();
}
