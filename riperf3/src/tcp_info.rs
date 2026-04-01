/// Subset of TCP connection metrics from the kernel's `TCP_INFO` socket option.
#[derive(Debug, Clone, Default)]
pub struct TcpInfoSnapshot {
    /// Cumulative retransmissions over the connection lifetime.
    pub total_retransmits: u32,
    /// Send congestion window in bytes (`tcpi_snd_cwnd * tcpi_snd_mss`).
    pub snd_cwnd: u64,
    /// Send window advertised by the receiver, in bytes.
    pub snd_wnd: u64,
    /// Smoothed round-trip time in microseconds.
    pub rtt: u32,
    /// RTT variance in microseconds.
    pub rttvar: u32,
    /// Sender maximum segment size.
    pub snd_mss: u32,
    /// Path MTU.
    pub pmtu: u32,
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
        snd_wnd: 0, // tcpi_snd_wnd not available in all libc versions
        rtt: info.tcpi_rtt,
        rttvar: info.tcpi_rttvar,
        snd_mss: info.tcpi_snd_mss,
        pmtu: info.tcpi_pmtu,
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
        pmtu: 0, // not available in tcp_connection_info
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
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
pub fn get_tcp_info(_fd: i32) -> Option<TcpInfoSnapshot> {
    None
}

/// Whether this platform provides TCP retransmit information.
pub fn has_retransmit_info() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
