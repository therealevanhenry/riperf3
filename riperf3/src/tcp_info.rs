/// Subset of TCP connection metrics from the kernel's `TCP_INFO` socket option.
#[derive(Debug, Clone, Default)]
pub struct TcpInfoSnapshot {
    /// Cumulative retransmissions over the connection lifetime.
    pub total_retransmits: u32,
    /// Send congestion window in bytes. Linux reports segments, so that
    /// reader multiplies by mss; macOS reports bytes and is used raw, like
    /// iperf3. (FreeBSD also reports bytes but its reader still multiplies —
    /// pre-existing unit bug, #155.)
    pub snd_cwnd: u64,
    /// Send window advertised by the receiver, in bytes.
    #[allow(dead_code)]
    pub snd_wnd: u64,
    /// Smoothed round-trip time in microseconds. On macOS the kernel's
    /// `tcpi_srtt` is MILLIseconds and is kept raw — iperf3 has the identical
    /// quirk (its APPLE `get_rtt` feeds tcpi_srtt into a usec-treated field),
    /// so converting here would diverge from iperf3's output.
    pub rtt: u32,
    /// RTT variance in microseconds (macOS: milliseconds, raw — see `rtt`).
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

/// Apple's `struct tcp_connection_info` (xnu `bsd/netinet/tcp.h`), hand-rolled
/// because libc 0.2.x's binding is wrong twice over (#96): it has no
/// `tcpi_txretransmitpackets` — the sender retransmit packet count iperf3
/// reads for the macOS Retr column; its seventh trailing u64 is misnamed
/// `tcpi_rxretransmitpackets`, a field xnu does not have — and it declares the
/// 15 one-bit `tcpi_tfo_*` flags as fifteen separate `u32` fields where the
/// kernel packs them (plus a 17-bit pad) into ONE `u32`, which lands its u64
/// tail at offset 120 where the kernel writes at 56 — every trailing counter
/// read 64 bytes past the data. Layout pinned against the xnu header by the
/// const asserts below; the `u64` block needs no explicit align attribute
/// because offset 56 is already 8-aligned under `repr(C)`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(dead_code)] // kernel ABI mirror — every field is required for layout
struct TcpConnectionInfo {
    tcpi_state: u8,
    tcpi_snd_wscale: u8,
    tcpi_rcv_wscale: u8,
    __pad1: u8,
    tcpi_options: u32,
    tcpi_flags: u32,
    tcpi_rto: u32,
    tcpi_maxseg: u32,
    tcpi_snd_ssthresh: u32,
    tcpi_snd_cwnd: u32,
    tcpi_snd_wnd: u32,
    tcpi_snd_sbbytes: u32,
    tcpi_rcv_wnd: u32,
    tcpi_rttcur: u32,
    tcpi_srtt: u32,
    tcpi_rttvar: u32,
    /// The 15 one-bit `tcpi_tfo_*` flags + `__pad2:17`, packed in one word.
    tcpi_tfo_bits: u32,
    tcpi_txpackets: u64,
    tcpi_txbytes: u64,
    tcpi_txretransmitbytes: u64,
    tcpi_rxpackets: u64,
    tcpi_rxbytes: u64,
    tcpi_rxoutoforderbytes: u64,
    tcpi_txretransmitpackets: u64,
}

#[cfg(target_os = "macos")]
const _: () = {
    // xnu's layout: 4×u8 + 13×u32 = 56, then seven 8-aligned u64s = 112 total.
    assert!(std::mem::size_of::<TcpConnectionInfo>() == 112);
    assert!(std::mem::offset_of!(TcpConnectionInfo, tcpi_txpackets) == 56);
    assert!(std::mem::offset_of!(TcpConnectionInfo, tcpi_txretransmitpackets) == 104);
};

/// Query TCP_CONNECTION_INFO for a connected TCP socket (macOS).
#[cfg(target_os = "macos")]
pub fn get_tcp_info(fd: i32) -> Option<TcpInfoSnapshot> {
    use std::mem::{self, MaybeUninit};

    // SAFETY: getsockopt(TCP_CONNECTION_INFO) is a read-only kernel query on a
    // valid fd — the same irreducible kernel boundary as the Linux TCP_INFO
    // variant, against the hand-rolled struct above (layout const-asserted).
    // The buffer is ZEROED, not uninit: an older kernel that predates the
    // trailing counters returns a shorter length and the unwritten tail must
    // read as 0, not as uninitialized memory.
    let info = unsafe {
        let mut info = MaybeUninit::<TcpConnectionInfo>::zeroed();
        let mut len = mem::size_of::<TcpConnectionInfo>() as libc::socklen_t;
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
        // The real sender retransmit packet count (#96) — the same field
        // iperf3 reads on macOS. Saturate into the cross-platform u32.
        total_retransmits: info.tcpi_txretransmitpackets.min(u32::MAX as u64) as u32,
        // xnu reports tcpi_snd_cwnd in BYTES (unlike Linux's segments), and
        // iperf3 uses it raw — multiplying by maxseg here would inflate the
        // Cwnd column ~1500x (#96).
        snd_cwnd: info.tcpi_snd_cwnd as u64,
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
/// macOS qualifies since #96: `get_tcp_info` reads the kernel's real
/// `tcpi_txretransmitpackets` via the hand-rolled `TcpConnectionInfo` binding
/// (the `libc` crate's `tcp_connection_info` both omits that field and mislays
/// the struct's tail — see the binding's doc comment), restoring the Retr+Cwnd
/// columns iperf3 shows on macOS. The columns are gated together (iperf3
/// couples them on the same flag).
pub fn has_retransmit_info() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos"
    ))
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
