use std::net::SocketAddr;
use std::time::Duration;

use socket2::{Domain, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::error::{Result, RiperfError};

// ---------------------------------------------------------------------------
// Custom socket options not wrapped by nix
// ---------------------------------------------------------------------------

/// Custom socket option types for Linux-specific options that nix doesn't expose.
/// Each impl contains a single unsafe block for the libc::setsockopt call;
/// the public API via `nix::sys::socket::setsockopt()` remains safe.
#[cfg(target_os = "linux")]
mod custom_sockopt {
    use nix::sys::socket::{GetSockOpt, SetSockOpt};
    use std::os::fd::AsFd;

    /// SO_MAX_PACING_RATE — FQ-based socket pacing.
    #[derive(Clone, Copy, Debug)]
    pub struct MaxPacingRate;

    impl SetSockOpt for MaxPacingRate {
        type Val = u32;
        fn set<F: AsFd>(&self, fd: &F, val: &u32) -> nix::Result<()> {
            // SAFETY: setsockopt on a valid fd with correct level/optname/size.
            unsafe {
                let res = libc::setsockopt(
                    fd.as_fd().as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_MAX_PACING_RATE,
                    val as *const _ as *const libc::c_void,
                    std::mem::size_of::<u32>() as libc::socklen_t,
                );
                nix::errno::Errno::result(res).map(drop)
            }
        }
    }

    /// IP_MTU_DISCOVER — path MTU discovery mode.
    #[derive(Clone, Copy, Debug)]
    pub struct IpMtuDiscover;

    impl SetSockOpt for IpMtuDiscover {
        type Val = libc::c_int;
        fn set<F: AsFd>(&self, fd: &F, val: &libc::c_int) -> nix::Result<()> {
            // SAFETY: setsockopt on a valid fd with correct level/optname/size.
            unsafe {
                let res = libc::setsockopt(
                    fd.as_fd().as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_MTU_DISCOVER,
                    val as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
                nix::errno::Errno::result(res).map(drop)
            }
        }
    }

    impl GetSockOpt for IpMtuDiscover {
        type Val = libc::c_int;
        fn get<F: AsFd>(&self, fd: &F) -> nix::Result<libc::c_int> {
            // SAFETY: getsockopt on a valid fd with correct level/optname/size.
            unsafe {
                let mut val: libc::c_int = 0;
                let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
                let res = libc::getsockopt(
                    fd.as_fd().as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_MTU_DISCOVER,
                    &mut val as *mut _ as *mut libc::c_void,
                    &mut len,
                );
                nix::errno::Errno::result(res).map(|_| val)
            }
        }
    }

    /// IPV6_FLOWINFO_SEND — enable sending IPv6 flow label.
    #[derive(Clone, Copy, Debug)]
    pub struct Ipv6FlowInfoSend;

    impl SetSockOpt for Ipv6FlowInfoSend {
        type Val = libc::c_int;
        fn set<F: AsFd>(&self, fd: &F, val: &libc::c_int) -> nix::Result<()> {
            // SAFETY: setsockopt on a valid fd with correct level/optname/size.
            unsafe {
                let res = libc::setsockopt(
                    fd.as_fd().as_raw_fd(),
                    libc::IPPROTO_IPV6,
                    libc::IPV6_FLOWINFO_SEND,
                    val as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
                nix::errno::Errno::result(res).map(drop)
            }
        }
    }

    use std::os::fd::AsRawFd;
}

// ---------------------------------------------------------------------------
// TCP
// ---------------------------------------------------------------------------

/// Resolve the default bind address for the given IP version preference.
fn default_bind_addr(ip_version: Option<u8>) -> &'static str {
    match ip_version {
        Some(6) => "::",
        _ => "0.0.0.0",
    }
}

/// Format host:port for SocketAddr parsing (brackets IPv6 addresses).
pub fn format_addr(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// MPTCP protocol number (not in libc/socket2 yet).
const IPPROTO_MPTCP: i32 = 262;

/// Connect to a TCP (or MPTCP) endpoint.
/// Uses socket2 when local_port or mptcp is set; tokio's built-in connect otherwise.
pub async fn tcp_connect(
    host: &str,
    port: u16,
    timeout: Option<Duration>,
    local_port: Option<u16>,
    mptcp: bool,
) -> Result<TcpStream> {
    if local_port.is_some() || mptcp {
        let remote: SocketAddr = format_addr(host, port)
            .parse()
            .map_err(|e| RiperfError::Protocol(format!("bad address: {e}")))?;
        let domain = if remote.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        };
        let protocol = if mptcp {
            Some(socket2::Protocol::from(IPPROTO_MPTCP))
        } else {
            None
        };
        let socket = Socket::new(domain, Type::STREAM, protocol)?;
        socket.set_reuse_address(true)?;
        if let Some(lport) = local_port {
            let local_addr: SocketAddr = if remote.is_ipv6() {
                format!("[::]:{lport}").parse().unwrap()
            } else {
                format!("0.0.0.0:{lport}").parse().unwrap()
            };
            socket.bind(&local_addr.into())?;
        }
        socket.set_nonblocking(true)?;
        match socket.connect(&remote.into()) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) => return Err(RiperfError::Io(e)),
        }
        let std_stream: std::net::TcpStream = socket.into();
        let stream = TcpStream::from_std(std_stream)?;
        stream.writable().await?;
        if let Some(e) = stream.take_error()? {
            return Err(RiperfError::Io(e));
        }
        Ok(stream)
    } else {
        let addr = format_addr(host, port);
        match timeout {
            Some(dur) => {
                let stream = tokio::time::timeout(dur, TcpStream::connect(&addr))
                    .await
                    .map_err(|_| RiperfError::ConnectionTimeout)?
                    .map_err(RiperfError::Io)?;
                Ok(stream)
            }
            None => Ok(TcpStream::connect(&addr).await?),
        }
    }
}

/// Create a TCP listener with SO_REUSEADDR.
/// If `bind_addr` is `None`, binds to 0.0.0.0 (or :: if `ip_version` is 6).
pub async fn tcp_listen(
    bind_addr: Option<&str>,
    port: u16,
    ip_version: Option<u8>,
) -> Result<TcpListener> {
    let host = bind_addr.unwrap_or(default_bind_addr(ip_version));
    let addr: SocketAddr = format_addr(host, port)
        .parse()
        .map_err(|e| RiperfError::Protocol(format!("bad bind address: {e}")))?;

    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;

    let std_listener: std::net::TcpListener = socket.into();
    Ok(TcpListener::from_std(std_listener)?)
}

/// Configure a connected TCP stream with socket options.
pub fn configure_tcp_stream(stream: &TcpStream, no_delay: bool) -> Result<()> {
    stream.set_nodelay(no_delay)?;
    Ok(())
}

/// Configure a connected TCP stream with all negotiated socket options.
pub fn configure_tcp_stream_full(
    stream: &TcpStream,
    no_delay: bool,
    mss: Option<i32>,
    window: Option<i32>,
    congestion: Option<&str>,
) -> Result<()> {
    stream.set_nodelay(no_delay)?;

    // Window sizes via socket2 (cross-platform). MSS via socket2 (Unix only).
    {
        let sock = socket2::SockRef::from(&stream);

        #[cfg(unix)]
        if let Some(mss_val) = mss {
            let _ = sock.set_mss(mss_val as u32);
        }
        #[cfg(windows)]
        if let Some(mss_val) = mss {
            use std::os::windows::io::{AsRawSocket, AsSocket};
            let raw = stream.as_socket().as_raw_socket();
            // SAFETY: setsockopt on a valid socket with TCP_MAXSEG.
            unsafe {
                windows_sys::Win32::Networking::WinSock::setsockopt(
                    raw as usize,
                    windows_sys::Win32::Networking::WinSock::IPPROTO_TCP as i32,
                    windows_sys::Win32::Networking::WinSock::TCP_MAXSEG as i32,
                    &(mss_val as i32) as *const i32 as *const u8,
                    std::mem::size_of::<i32>() as i32,
                );
            }
        }
        #[cfg(not(any(unix, windows)))]
        let _ = mss;

        if let Some(size) = window {
            let _ = sock.set_recv_buffer_size(size as usize);
            let _ = sock.set_send_buffer_size(size as usize);
        }
    }

    // Congestion control: Linux + FreeBSD (iperf3 uses HAVE_TCP_CONGESTION)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(algo) = congestion {
        use nix::sys::socket::{self, sockopt};
        use std::ffi::OsString;
        let _ = socket::setsockopt(stream, sockopt::TcpCongestion, &OsString::from(algo));
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = congestion;

    Ok(())
}

// ---------------------------------------------------------------------------
// UDP
// ---------------------------------------------------------------------------

/// Bind a UDP socket. If `bind_addr` is `None`, uses `0.0.0.0` (or `[::]` for IPv6).
pub async fn udp_bind(bind_addr: Option<&str>, port: u16, ipv6: bool) -> Result<UdpSocket> {
    let default = if ipv6 { "::" } else { "0.0.0.0" };
    let host = bind_addr.unwrap_or(default);
    let addr = format_addr(host, port);
    Ok(UdpSocket::bind(&addr).await?)
}

/// Set SO_RCVTIMEO on a socket (receive timeout in milliseconds).
#[cfg(unix)]
pub fn set_rcv_timeout(fd: &impl std::os::unix::io::AsFd, ms: u64) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    use nix::sys::time::TimeVal;
    let tv = TimeVal::new(
        (ms / 1000) as libc::time_t,
        ((ms % 1000) * 1000) as libc::suseconds_t,
    );
    socket::setsockopt(fd, sockopt::ReceiveTimeout, &tv)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(not(unix))]
pub fn set_rcv_timeout<F>(_fd: &F, _ms: u64) -> Result<()> {
    Ok(())
}

/// Set TCP_USER_TIMEOUT on a socket (send timeout in milliseconds).
#[cfg(target_os = "linux")]
pub fn set_snd_timeout(fd: &impl std::os::unix::io::AsFd, ms: u64) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    socket::setsockopt(fd, sockopt::TcpUserTimeout, &(ms as u32))
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(not(target_os = "linux"))]
pub fn set_snd_timeout<F>(_fd: &F, _ms: u64) -> Result<()> {
    Ok(())
}

/// Set the IPv4 Don't Fragment bit on a socket.
/// Uses IP_MTU_DISCOVER on Linux, IP_DONTFRAG on macOS/FreeBSD.
#[cfg(target_os = "linux")]
pub fn set_dont_fragment(fd: &impl std::os::unix::io::AsFd) -> Result<()> {
    use nix::sys::socket;
    socket::setsockopt(fd, custom_sockopt::IpMtuDiscover, &libc::IP_PMTUDISC_DO)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
pub fn set_dont_fragment(fd: &impl std::os::unix::io::AsFd) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let val: libc::c_int = 1;
    // SAFETY: setsockopt on a valid fd with IP_DONTFRAG.
    let ret = unsafe {
        libc::setsockopt(
            fd.as_fd().as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_DONTFRAG,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
pub fn set_dont_fragment(fd: &impl std::os::windows::io::AsSocket) -> Result<()> {
    use std::os::windows::io::AsRawSocket;
    let raw = fd.as_socket().as_raw_socket();
    let val: i32 = 1;
    // SAFETY: setsockopt on a valid socket with IP_DONTFRAGMENT.
    let ret = unsafe {
        windows_sys::Win32::Networking::WinSock::setsockopt(
            raw as usize,
            windows_sys::Win32::Networking::WinSock::IPPROTO_IP as i32,
            windows_sys::Win32::Networking::WinSock::IP_DONTFRAGMENT as i32,
            &val as *const i32 as *const u8,
            std::mem::size_of::<i32>() as i32,
        )
    };
    if ret != 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn set_dont_fragment<F>(_fd: &F) -> Result<()> {
    Ok(())
}

/// Set SO_MAX_PACING_RATE for FQ-based socket pacing (Linux only).
#[cfg(target_os = "linux")]
pub fn set_fq_rate(fd: &impl std::os::unix::io::AsFd, rate_bits_per_sec: u64) -> Result<()> {
    use nix::sys::socket;
    let rate_bytes = (rate_bits_per_sec / 8) as u32;
    socket::setsockopt(fd, custom_sockopt::MaxPacingRate, &rate_bytes)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(not(target_os = "linux"))]
pub fn set_fq_rate<F>(_fd: &F, _rate: u64) -> Result<()> {
    Ok(())
}

/// Bind socket to a specific network device.
/// Linux: SO_BINDTODEVICE (by name). macOS: IP_BOUND_IF (by index).
#[cfg(target_os = "linux")]
pub fn set_bind_dev(fd: &impl std::os::unix::io::AsFd, dev: &str) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    use std::ffi::OsString;
    socket::setsockopt(fd, sockopt::BindToDevice, &OsString::from(dev))
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(target_os = "macos")]
pub fn set_bind_dev(fd: &impl std::os::unix::io::AsFd, dev: &str) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    // Resolve device name to interface index (safe via nix)
    let idx =
        nix::net::if_::if_nametoindex(dev).map_err(|e| RiperfError::Io(std::io::Error::from(e)))?;
    // SAFETY: setsockopt on a valid fd with IP_BOUND_IF. No nix wrapper exists.
    let ret = unsafe {
        libc::setsockopt(
            fd.as_fd().as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_BOUND_IF,
            &idx as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn set_bind_dev<F>(_fd: &F, _dev: &str) -> Result<()> {
    Ok(())
}

/// Set TCP keepalive options on a socket.
/// SO_KEEPALIVE works everywhere; idle/interval/count need platform support.
#[cfg(unix)]
pub fn set_tcp_keepalive(
    fd: &impl std::os::unix::io::AsFd,
    idle: Option<u32>,
    interval: Option<u32>,
    count: Option<u32>,
) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    let _ = socket::setsockopt(fd, sockopt::KeepAlive, &true);
    // TcpKeepIdle: Linux, FreeBSD (not macOS — uses TCP_KEEPALIVE instead)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(val) = idle {
        let _ = socket::setsockopt(fd, sockopt::TcpKeepIdle, &val);
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = idle;
    if let Some(val) = interval {
        let _ = socket::setsockopt(fd, sockopt::TcpKeepInterval, &val);
    }
    if let Some(val) = count {
        let _ = socket::setsockopt(fd, sockopt::TcpKeepCount, &val);
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn set_tcp_keepalive<F>(
    _fd: &F,
    _idle: Option<u32>,
    _interval: Option<u32>,
    _count: Option<u32>,
) -> Result<()> {
    Ok(())
}

/// Set CPU affinity for the current thread (Linux + FreeBSD).
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn set_cpu_affinity(core: usize) -> Result<()> {
    use nix::sched::{sched_setaffinity, CpuSet};
    use nix::unistd::Pid;
    let mut cpuset = CpuSet::new();
    cpuset
        .set(core)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))?;
    sched_setaffinity(Pid::from_raw(0), &cpuset)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(windows)]
pub fn set_cpu_affinity(core: usize) -> Result<()> {
    if core >= usize::BITS as usize {
        return Err(RiperfError::Protocol("core index too large".into()));
    }
    let mask: usize = 1 << core;
    // SAFETY: GetCurrentThread returns a pseudo-handle (always valid).
    // SetThreadAffinityMask is safe with a valid thread handle and mask.
    let prev = unsafe {
        windows_sys::Win32::System::Threading::SetThreadAffinityMask(
            windows_sys::Win32::System::Threading::GetCurrentThread(),
            mask,
        )
    };
    if prev == 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd", windows)))]
pub fn set_cpu_affinity(_core: usize) -> Result<()> {
    Ok(())
}

/// Set IPv6 flow label on a socket (Linux only).
#[cfg(target_os = "linux")]
pub fn set_ipv6_flowlabel(fd: &impl std::os::unix::io::AsFd, label: i32) -> Result<()> {
    use nix::sys::socket;
    if let Err(e) = socket::setsockopt(fd, custom_sockopt::Ipv6FlowInfoSend, &label) {
        log::debug!("IPV6_FLOWINFO_SEND failed: {e}");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_ipv6_flowlabel<F>(_fd: &F, _label: i32) -> Result<()> {
    Ok(())
}

/// Enable UDP GSO (Generic Segmentation Offload) on a UDP socket.
/// Sets UDP_SEGMENT to the datagram size so the kernel can batch sends.
#[cfg(target_os = "linux")]
pub fn set_udp_gso(fd: &impl std::os::unix::io::AsFd, segment_size: u16) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    socket::setsockopt(fd, sockopt::UdpGsoSegment, &(segment_size as i32))
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(not(target_os = "linux"))]
pub fn set_udp_gso<F>(_fd: &F, _segment_size: u16) -> Result<()> {
    Ok(())
}

/// Enable UDP GRO (Generic Receive Offload) on a UDP socket.
#[cfg(target_os = "linux")]
pub fn set_udp_gro(fd: &impl std::os::unix::io::AsFd) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    socket::setsockopt(fd, sockopt::UdpGroSegment, &true)
        .map_err(|e| RiperfError::Io(std::io::Error::from(e)))
}

#[cfg(not(target_os = "linux"))]
pub fn set_udp_gro<F>(_fd: &F) -> Result<()> {
    Ok(())
}

/// Set IP_TOS on a socket. Cross-platform via socket2.
#[cfg(unix)]
pub fn set_tos(fd: &impl std::os::unix::io::AsFd, tos: u32) -> Result<()> {
    let sock = socket2::SockRef::from(fd);
    sock.set_tos(tos)?;
    Ok(())
}

#[cfg(windows)]
pub fn set_tos(fd: &impl std::os::windows::io::AsSocket, tos: u32) -> Result<()> {
    let sock = socket2::SockRef::from(fd);
    sock.set_tos(tos)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn set_tos<F>(_fd: &F, _tos: u32) -> Result<()> {
    Ok(())
}

/// Bind a UDP socket with SO_REUSEADDR, allowing multiple sockets on the same port.
/// Used by the server to recycle the UDP listener after each stream connect.
pub async fn udp_bind_reusable(
    bind_addr: Option<&str>,
    port: u16,
    ipv6: bool,
) -> Result<UdpSocket> {
    let default = if ipv6 { "::" } else { "0.0.0.0" };
    let host = bind_addr.unwrap_or(default);
    let addr: SocketAddr = format_addr(host, port)
        .parse()
        .map_err(|e| RiperfError::Protocol(format!("bad bind address: {e}")))?;

    let domain = if addr.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, None)?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;

    let std_socket: std::net::UdpSocket = socket.into();
    Ok(UdpSocket::from_std(std_socket)?)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tcp_listen_and_connect() {
        let listener = tcp_listen(Some("127.0.0.1"), 0, None).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let client_task = tokio::spawn(async move {
            tcp_connect("127.0.0.1", port, None, None, false)
                .await
                .unwrap()
        });

        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = client_task.await.unwrap();

        assert!(server_stream.peer_addr().is_ok());
        assert!(client_stream.peer_addr().is_ok());
    }

    #[tokio::test]
    async fn tcp_connect_timeout() {
        // Connect to a non-routable address with a short timeout
        let result = tcp_connect(
            "192.0.2.1",
            12345,
            Some(Duration::from_millis(50)),
            None,
            false,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn udp_bind_ephemeral() {
        let socket = udp_bind(Some("127.0.0.1"), 0, false).await.unwrap();
        assert!(socket.local_addr().is_ok());
    }

    #[test]
    fn format_addr_ipv4() {
        assert_eq!(format_addr("127.0.0.1", 5201), "127.0.0.1:5201");
        assert_eq!(format_addr("0.0.0.0", 80), "0.0.0.0:80");
    }

    #[test]
    fn format_addr_ipv6_brackets() {
        assert_eq!(format_addr("::1", 5201), "[::1]:5201");
        assert_eq!(format_addr("::", 0), "[::]:0");
        assert_eq!(format_addr("fd00:20::20", 8080), "[fd00:20::20]:8080");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tcp_keepalive_readback() {
        use nix::sys::socket::{self, sockopt};
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        set_tcp_keepalive(&socket, Some(10), Some(5), Some(3)).unwrap();
        let enabled = socket::getsockopt(&socket, sockopt::KeepAlive).unwrap();
        assert!(enabled, "SO_KEEPALIVE should be enabled");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rcv_timeout_readback() {
        use nix::sys::socket::{self, sockopt};
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        set_rcv_timeout(&socket, 5000).unwrap(); // 5 seconds
        let tv = socket::getsockopt(&socket, sockopt::ReceiveTimeout).unwrap();
        assert_eq!(tv.tv_sec(), 5, "SO_RCVTIMEO should be 5 seconds");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dont_fragment_readback() {
        use nix::sys::socket;
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        set_dont_fragment(&socket).unwrap();
        let val = socket::getsockopt(&socket, custom_sockopt::IpMtuDiscover).unwrap();
        assert_eq!(val, libc::IP_PMTUDISC_DO, "IP_MTU_DISCOVER should be DO");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn set_bind_dev_loopback() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let result = set_bind_dev(&socket, "lo");
        // Succeeds on Linux 5.7+ (unprivileged SO_BINDTODEVICE)
        // May fail with EPERM on older kernels — acceptable
        if let Err(ref e) = result {
            let msg = format!("{e}");
            if msg.contains("Operation not permitted") {
                return; // old kernel, skip
            }
            panic!("unexpected error from set_bind_dev: {e}");
        }
        assert!(result.is_ok());
    }
}
