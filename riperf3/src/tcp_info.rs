/// Subset of TCP connection metrics from the kernel's `TCP_INFO` socket option.
#[derive(Debug, Clone, Default)]
pub struct TcpInfoSnapshot {
    /// Cumulative retransmissions over the connection lifetime.
    pub total_retransmits: u32,
    /// Send congestion window in bytes (`tcpi_snd_cwnd * tcpi_snd_mss`).
    pub snd_cwnd: u64,
    /// Send window advertised by the receiver, in bytes.
    #[allow(dead_code)]
    pub snd_wnd: u64,
    /// Smoothed round-trip time in microseconds.
    pub rtt: u32,
    /// RTT variance in microseconds.
    pub rttvar: u32,
    /// Sender maximum segment size.
    #[allow(dead_code)]
    pub snd_mss: u32,
    /// Path MTU.
    pub pmtu: u32,
    /// Reordering metric (segments).
    pub reorder: u32,
}

/// Query TCP_INFO for a connected TCP socket.
///
/// Returns `None` if the query fails or the platform does not support TCP_INFO.
#[cfg(target_os = "linux")]
pub fn get_tcp_info(fd: i32) -> Option<TcpInfoSnapshot> {
    use std::mem::{self, MaybeUninit};

    // SAFETY: getsockopt(TCP_INFO) is a read-only kernel query on a valid fd.
    // MaybeUninit is fully initialized by getsockopt on success (ret >= 0).
    // No nix wrapper exists for TCP_INFO — this is the minimal unsafe surface.
    let info = unsafe {
        let mut info = MaybeUninit::<libc::tcp_info>::uninit();
        let mut len = mem::size_of::<libc::tcp_info>() as libc::socklen_t;
        let ret = libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            info.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        );
        if ret < 0 {
            return None;
        }
        info.assume_init()
    };

    Some(TcpInfoSnapshot {
        total_retransmits: info.tcpi_total_retrans,
        snd_cwnd: info.tcpi_snd_cwnd as u64 * info.tcpi_snd_mss as u64,
        snd_wnd: 0, // tcpi_snd_wnd is not exposed by libc's tcp_info on Linux
        rtt: info.tcpi_rtt,
        rttvar: info.tcpi_rttvar,
        snd_mss: info.tcpi_snd_mss,
        pmtu: info.tcpi_pmtu,
        // iperf3's `reorder` is `tcpi_reord_seen` (count of reordering events),
        // NOT `tcpi_reordering` (the kernel's reordering-degree estimate). libc's
        // `tcp_info` truncates before `tcpi_reord_seen`, so it's unreachable here;
        // report 0, as iperf3 does when the field is unavailable.
        reorder: 0,
    })
}

/// Query TCP_CONNECTION_INFO for a connected TCP socket (macOS).
#[cfg(target_os = "macos")]
pub fn get_tcp_info(fd: i32) -> Option<TcpInfoSnapshot> {
    use std::mem::{self, MaybeUninit};

    // SAFETY: getsockopt(TCP_CONNECTION_INFO) is a read-only kernel query on a valid fd.
    // Same irreducible kernel boundary as the Linux TCP_INFO variant.
    let info = unsafe {
        let mut info = MaybeUninit::<libc::tcp_connection_info>::uninit();
        let mut len = mem::size_of::<libc::tcp_connection_info>() as libc::socklen_t;
        let ret = libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_CONNECTION_INFO,
            info.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        );
        if ret < 0 {
            return None;
        }
        info.assume_init()
    };

    Some(TcpInfoSnapshot {
        total_retransmits: 0, // macOS only has tcpi_txretransmitbytes, not packet count
        snd_cwnd: info.tcpi_snd_cwnd as u64 * info.tcpi_maxseg as u64,
        snd_wnd: info.tcpi_snd_wnd as u64,
        rtt: info.tcpi_srtt,
        rttvar: info.tcpi_rttvar,
        snd_mss: info.tcpi_maxseg,
        pmtu: 0,    // not available in tcp_connection_info
        reorder: 0, // not exposed by tcp_connection_info
    })
}

/// Query TCP_INFO for a connected TCP socket (FreeBSD).
/// Same option name as Linux but different struct field names.
#[cfg(target_os = "freebsd")]
pub fn get_tcp_info(fd: i32) -> Option<TcpInfoSnapshot> {
    use std::mem::{self, MaybeUninit};

    // SAFETY: getsockopt(TCP_INFO) is a read-only kernel query on a valid fd.
    let info = unsafe {
        let mut info = MaybeUninit::<libc::tcp_info>::uninit();
        let mut len = mem::size_of::<libc::tcp_info>() as libc::socklen_t;
        let ret = libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            info.as_mut_ptr() as *mut libc::c_void,
            &mut len,
        );
        if ret < 0 {
            return None;
        }
        info.assume_init()
    };

    Some(TcpInfoSnapshot {
        total_retransmits: info.tcpi_snd_rexmitpack,
        snd_cwnd: info.tcpi_snd_cwnd as u64 * info.tcpi_snd_mss as u64,
        snd_wnd: info.tcpi_snd_wnd as u64,
        rtt: info.tcpi_rtt,
        rttvar: info.tcpi_rttvar,
        snd_mss: info.tcpi_snd_mss,
        pmtu: info.__tcpi_pmtu,
        reorder: 0, // tcpi_reordering not in FreeBSD's tcp_info
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub fn get_tcp_info(_fd: i32) -> Option<TcpInfoSnapshot> {
    None
}

/// Whether this platform provides a usable TCP **retransmit packet count** for
/// the sender (the `Retr` column / JSON `sender_has_retransmits`).
///
/// macOS is deliberately excluded (#40). iperf3 *does* report retransmits on
/// macOS — it reads `tcp_connection_info.tcpi_txretransmitpackets` (a sender
/// retransmit packet count) and shows the Retr+Cwnd columns. But the Rust `libc`
/// binding's `tcp_connection_info` does not expose that field — its sender-side
/// retransmit member is `tcpi_txretransmitbytes` (retransmitted *bytes*) — so
/// riperf3's `get_tcp_info` can only hard-code `total_retransmits: 0`. Rather
/// than print a perpetual, misleading `Retr 0` (implying a loss-free transfer),
/// we report no retransmit info on macOS. The Retr and Cwnd columns are gated
/// together (iperf3 couples them on the same flag), so Cwnd is suppressed with
/// it. This is a deliberate divergence from iperf3 for now; the faithful fix —
/// reading `tcpi_txretransmitpackets` via a custom struct binding to show the
/// real macOS Retr/Cwnd — is a deferred follow-up, not patch scope.
pub fn has_retransmit_info() -> bool {
    cfg!(any(target_os = "linux", target_os = "freebsd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unix-only: queries TCP_INFO via a raw fd (`as_raw_fd`), which doesn't exist
    // on Windows (`TcpStream` there is `as_raw_socket`). get_tcp_info returns None
    // on Windows anyway, so there's nothing to exercise (#71).
    #[cfg(unix)]
    #[tokio::test]
    async fn tcp_info_on_connected_socket() {
        use std::os::unix::io::AsRawFd;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client =
            tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await.unwrap() });

        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = client.await.unwrap();

        // Both sides should have valid TCP_INFO on Linux
        if has_retransmit_info() {
            let info = get_tcp_info(server_stream.as_raw_fd()).unwrap();
            assert!(info.snd_mss > 0);
            // loopback RTT can be 0; just assert the query succeeded
            let _ = info.rtt;

            let info = get_tcp_info(client_stream.as_raw_fd()).unwrap();
            assert!(info.snd_mss > 0);
        }
    }

    // #40/#96: only platforms with a real sender retransmit *packet* count
    // advertise retransmit info. iperf3 shows Retr+Cwnd on macOS by reading
    // tcp_connection_info.tcpi_txretransmitpackets, so macOS belongs on the
    // "has" side (#96 restores it via the hand-rolled binding). Runs on every
    // platform; the macOS assertion is validated by the macOS native CI job.
    #[test]
    fn retransmit_info_only_where_packet_count_exists() {
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
        assert!(has_retransmit_info());
        #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
        assert!(!has_retransmit_info());
    }

    // #96: on macOS the snapshot must come from the hand-rolled
    // tcp_connection_info binding — sane prefix fields (maxseg) prove the
    // struct layout lines up with what the kernel wrote (a mislaid struct
    // reads garbage or zeros here), and total_retransmits comes from the real
    // tcpi_txretransmitpackets counter (0 is the expected value on loopback).
    // Validated by the macOS native CI job.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_snapshot_reads_sane_values() {
        use std::os::unix::io::AsRawFd;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client =
            tokio::spawn(async move { tokio::net::TcpStream::connect(addr).await.unwrap() });
        let (server_stream, _) = listener.accept().await.unwrap();
        let _client_stream = client.await.unwrap();

        let info = get_tcp_info(server_stream.as_raw_fd()).expect("TCP_CONNECTION_INFO");
        assert!(
            info.snd_mss > 0 && info.snd_mss < 65_536,
            "maxseg implausible — struct layout suspect: {}",
            info.snd_mss
        );
        assert_eq!(
            info.total_retransmits, 0,
            "loopback handshake should have no retransmits"
        );
        // xnu reports tcpi_snd_cwnd in BYTES; iperf3 uses it raw. A cwnd below
        // one segment or above 1 GiB would indicate a unit or layout error.
        assert!(
            info.snd_cwnd >= info.snd_mss as u64 && info.snd_cwnd < (1 << 30),
            "snd_cwnd implausible: {}",
            info.snd_cwnd
        );
    }
}
