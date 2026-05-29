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
/// `None` → `::` (dual-stack via IPV6_V6ONLY=0); `Some(4)` → IPv4 only;
/// `Some(6)` → IPv6 only.
fn default_bind_addr(ip_version: Option<u8>) -> &'static str {
    match ip_version {
        Some(4) => "0.0.0.0",
        _ => "::",
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

/// Resolve `host:port` to a single `SocketAddr`, honoring an IP-version
/// preference. For an IP literal it validates the family matches `ip_version`
/// (rejecting e.g. `-6` against an IPv4 literal). For a hostname it resolves
/// and returns the first address of the requested family — this is how `-4`/
/// `-6` constrain the connection (issue #10).
pub async fn resolve_host(host: &str, port: u16, ip_version: Option<u8>) -> Result<SocketAddr> {
    let family_ok = |a: &SocketAddr| match ip_version {
        Some(4) => a.is_ipv4(),
        Some(6) => a.is_ipv6(),
        _ => true,
    };
    let want = || match ip_version {
        Some(4) => "IPv4",
        Some(6) => "IPv6",
        _ => "any",
    };

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let addr = SocketAddr::new(ip, port);
        match ip_version {
            Some(4) if !addr.is_ipv4() => {
                return Err(RiperfError::Protocol(format!(
                    "address {host} is not IPv4 (conflicts with -4)"
                )))
            }
            Some(6) if !addr.is_ipv6() => {
                return Err(RiperfError::Protocol(format!(
                    "address {host} is not IPv6 (conflicts with -6)"
                )))
            }
            _ => {}
        }
        return Ok(addr);
    }

    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(RiperfError::Io)?;
    addrs
        .find(family_ok)
        .ok_or_else(|| RiperfError::Protocol(format!("no {} address found for {host}", want())))
}

/// Resolve a client `-B` bind address to a local source IP in the target's
/// address family. The bind family must match the connection, and the target
/// was already resolved honoring `-4`/`-6`, so `target_is_ipv6` is authoritative
/// — resolving the bind host in that family makes a dual-stack bind *hostname*
/// pick the matching address (rather than the resolver's first result) and
/// rejects a wrong-family bind *literal* with a clear message. `host%dev` keeps
/// only the address part (device binding is `--bind-dev`); note an IPv6
/// link-local zone id like `fe80::1%eth0` is therefore not supported here.
pub async fn resolve_bind_ip(
    bind_address: &str,
    target_is_ipv6: bool,
    target_host: &str,
) -> Result<std::net::IpAddr> {
    let addr = bind_address.split('%').next().unwrap_or(bind_address);
    let family = if target_is_ipv6 { 6 } else { 4 };
    let resolved = resolve_host(addr, 0, Some(family)).await.map_err(|_| {
        RiperfError::Protocol(format!(
            "bind address {addr} has no {} address to match target {target_host}",
            if target_is_ipv6 { "IPv6" } else { "IPv4" }
        ))
    })?;
    Ok(resolved.ip())
}

/// Connect to a TCP (or MPTCP) endpoint.
/// Uses socket2 when a local port (`--cport`), a bind address (`-B`), or mptcp
/// is set; tokio's built-in connect otherwise. `ip_version` constrains
/// address-family selection for hostnames (`-4`/`-6`); when `None`, the OS
/// resolver's full address list is tried. A `bind_address` is resolved honoring
/// `ip_version` and must share the target's address family (it's the client
/// source address; `host%dev` device binding is `--bind-dev`'s job).
#[allow(clippy::too_many_arguments)] // connect tuning knobs map 1:1 to CLI flags
pub async fn tcp_connect(
    host: &str,
    port: u16,
    timeout: Option<Duration>,
    local_port: Option<u16>,
    bind_address: Option<&str>,
    mptcp: bool,
    ip_version: Option<u8>,
) -> Result<TcpStream> {
    if local_port.is_some() || bind_address.is_some() || mptcp {
        let remote = resolve_host(host, port, ip_version).await?;
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
        if bind_address.is_some() || local_port.is_some() {
            let lport = local_port.unwrap_or(0);
            let local_ip = match bind_address {
                Some(b) => resolve_bind_ip(b, remote.is_ipv6(), host).await?,
                None if remote.is_ipv6() => std::net::Ipv6Addr::UNSPECIFIED.into(),
                None => std::net::Ipv4Addr::UNSPECIFIED.into(),
            };
            let local = SocketAddr::new(local_ip, lport);
            socket.bind(&local.into()).map_err(|e| {
                RiperfError::Protocol(format!("failed to bind local address {local}: {e}"))
            })?;
        }
        socket.set_nonblocking(true)?;
        match socket.connect(&remote.into()) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
            Err(e) => return Err(RiperfError::Io(e)),
        }
        let std_stream: std::net::TcpStream = socket.into();
        let stream = TcpStream::from_std(std_stream)?;
        // Honor connect_timeout on the writability wait (the EINPROGRESS connect
        // completes asynchronously); previously this path ignored the timeout.
        match timeout {
            Some(dur) => tokio::time::timeout(dur, stream.writable())
                .await
                .map_err(|_| RiperfError::ConnectionTimeout)?
                .map_err(RiperfError::Io)?,
            None => stream.writable().await.map_err(RiperfError::Io)?,
        }
        if let Some(e) = stream.take_error()? {
            return Err(RiperfError::Io(e));
        }
        Ok(stream)
    } else if ip_version.is_some() {
        // Honor -4/-6: connect to a single resolved address of the chosen
        // family rather than letting the resolver try every family.
        let remote = resolve_host(host, port, ip_version).await?;
        match timeout {
            Some(dur) => Ok(tokio::time::timeout(dur, TcpStream::connect(remote))
                .await
                .map_err(|_| RiperfError::ConnectionTimeout)?
                .map_err(RiperfError::Io)?),
            None => Ok(TcpStream::connect(remote).await?),
        }
    } else {
        // No version preference: let the OS resolver try all addresses.
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
/// If `bind_addr` is `None`, binds dual-stack (`::` with IPV6_V6ONLY=0) by
/// default, matching iperf3's `getaddrinfo`+`AI_PASSIVE` behavior.
/// `ip_version=Some(4)` restricts to IPv4 (`0.0.0.0`); `Some(6)` restricts to
/// IPv6 only (sets IPV6_V6ONLY=1).
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
    if addr.is_ipv6() {
        // Set V6ONLY explicitly on every IPv6 bind, including when an explicit
        // `-B` address is given: `-6 -B ::` must be IPv6-only and `-B ::` alone
        // must be dual-stack. BSDs default V6ONLY=1 and Linux defaults 0, so we
        // can't rely on the platform default. For a non-wildcard IPv6 address
        // (e.g. `::1`) the flag is moot but harmless.
        socket.set_only_v6(ip_version == Some(6))?;
    }
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

/// Read the TCP maximum segment size (`TCP_MAXSEG`) of a connected stream.
///
/// iperf3 uses this to size UDP datagrams when `-l` is not given (issue #6): on
/// a jumbo-frame path the control connection negotiates a large MSS, so UDP
/// datagrams should be sized to match rather than pinned at 1460. Returns
/// `None` when the option can't be read (non-Unix, or a getsockopt error).
#[cfg(unix)]
pub fn tcp_maxseg(stream: &TcpStream) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut mss: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `fd` is a valid connected TCP socket for the lifetime of `stream`;
    // TCP_MAXSEG yields a single c_int and `len` matches the buffer size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_MAXSEG,
            &mut mss as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0 && mss > 0).then_some(mss as u32)
}

#[cfg(not(unix))]
pub fn tcp_maxseg(_stream: &TcpStream) -> Option<u32> {
    None
}

// ---------------------------------------------------------------------------
// UDP
// ---------------------------------------------------------------------------

/// Wall-clock bound a blocking UDP `sendmmsg`/`send` so a wedged link can't park
/// the sender thread forever (the per-batch `done`/deadline checks only run
/// between blocking calls). On expiry the syscall returns `EAGAIN`, which the
/// sender treats as a zero-progress batch and loops to re-check those flags.
#[cfg(unix)]
const UDP_SEND_TIMEOUT_MS: u64 = 1000;

/// Prepare a UDP socket for the blocking batched sender (issue #6).
///
/// tokio sockets are non-blocking, and `into_std()` preserves that flag. A
/// non-blocking socket makes `sendmmsg` busy-spin on `EAGAIN` once the (small)
/// send buffer fills, redundantly re-staging the whole batch and starving the
/// async runtime. Switching to blocking lets the kernel backpressure the
/// sender thread instead. The `SO_SNDBUF` bump is best-effort (clamped by
/// `net.core.wmem_max`); `SO_SNDTIMEO` bounds a wedged link (see
/// [`UDP_SEND_TIMEOUT_MS`]). Note: this is `SO_SNDTIMEO`, *not* the
/// `TCP_USER_TIMEOUT` of [`set_snd_timeout`] (which is a no-op on UDP).
#[cfg(unix)]
pub fn configure_udp_sender(socket: &std::net::UdpSocket, sndbuf_target: usize) -> Result<()> {
    use nix::sys::socket::{self, sockopt};
    use nix::sys::time::TimeVal;
    socket.set_nonblocking(false)?;
    let sock = socket2::SockRef::from(socket);
    let _ = sock.set_send_buffer_size(sndbuf_target);
    let tv = TimeVal::new(
        (UDP_SEND_TIMEOUT_MS / 1000) as libc::time_t,
        ((UDP_SEND_TIMEOUT_MS % 1000) * 1000) as libc::suseconds_t,
    );
    let _ = socket::setsockopt(socket, sockopt::SendTimeout, &tv);
    Ok(())
}

// Non-Unix: switch to blocking only. There's no portable SO_SNDTIMEO here, so a
// wedged send can block until the link recovers; the per-batch deadline can't
// fire mid-block. Acceptable given the sendmmsg fast path is Unix-only anyway.
#[cfg(not(unix))]
pub fn configure_udp_sender(socket: &std::net::UdpSocket, _sndbuf_target: usize) -> Result<()> {
    socket.set_nonblocking(false)?;
    Ok(())
}

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
/// `ip_version=None` binds dual-stack (`::` with IPV6_V6ONLY=0); `Some(4)`
/// restricts to IPv4; `Some(6)` restricts to IPv6 only.
pub async fn udp_bind_reusable(
    bind_addr: Option<&str>,
    port: u16,
    ip_version: Option<u8>,
) -> Result<UdpSocket> {
    let host = bind_addr.unwrap_or(default_bind_addr(ip_version));
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
    if addr.is_ipv6() {
        // Set V6ONLY on every IPv6 bind, explicit `-B` included — see tcp_listen.
        socket.set_only_v6(ip_version == Some(6))?;
    }
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
            tcp_connect("127.0.0.1", port, None, None, None, false, None)
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
            None,
            false,
            None,
        )
        .await;
        assert!(result.is_err());
    }

    // ---- client -B local bind address (issue #15) ------------------------

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn tcp_connect_binds_local_address() {
        // Bind the client source to 127.0.0.2 (loopback /8 on Linux). The OS
        // would otherwise pick 127.0.0.1, so observing 127.0.0.2 proves -B
        // actually took effect rather than being silently ignored (#15).
        let listener = tcp_listen(Some("127.0.0.1"), 0, None).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client_task = tokio::spawn(async move {
            tcp_connect(
                "127.0.0.1",
                port,
                None,
                None,
                Some("127.0.0.2"),
                false,
                None,
            )
            .await
            .unwrap()
        });
        let (_server, _) = listener.accept().await.unwrap();
        let client = client_task.await.unwrap();
        assert_eq!(
            client.local_addr().unwrap().ip(),
            "127.0.0.2".parse::<std::net::IpAddr>().unwrap(),
            "client should have bound its source to -B 127.0.0.2"
        );
    }

    #[tokio::test]
    async fn tcp_connect_rejects_bind_family_mismatch() {
        // A -B with a v6 literal while connecting to a v4 target must error,
        // not silently ignore it (#15 family validation, mirroring #12).
        let listener = tcp_listen(Some("127.0.0.1"), 0, None).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let result = tcp_connect("127.0.0.1", port, None, None, Some("::1"), false, None).await;
        assert!(
            result.is_err(),
            "v6 bind address against a v4 target must be rejected"
        );
    }

    #[tokio::test]
    async fn tcp_connect_socket2_path_honors_timeout() {
        // A bind address forces the socket2 path; a connect to a non-routable
        // target with a short timeout must fail fast, not hang — this path
        // previously ignored connect_timeout (#15 review).
        let start = std::time::Instant::now();
        let result = tcp_connect(
            "192.0.2.1", // TEST-NET-1, non-routable
            12345,
            Some(Duration::from_millis(150)),
            None,
            Some("0.0.0.0"),
            false,
            None,
        )
        .await;
        assert!(result.is_err(), "non-routable connect must fail");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "socket2 path must honor the timeout, not hang (took {:?})",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn udp_bind_ephemeral() {
        let socket = udp_bind(Some("127.0.0.1"), 0, false).await.unwrap();
        assert!(socket.local_addr().is_ok());
    }

    // ---- resolve_host: honoring -4/-6 (issue #10) -------------------------

    #[tokio::test]
    async fn resolve_host_literal_no_preference() {
        let a = resolve_host("127.0.0.1", 5201, None).await.unwrap();
        assert!(a.is_ipv4() && a.port() == 5201);
        let a = resolve_host("::1", 5201, None).await.unwrap();
        assert!(a.is_ipv6());
    }

    #[tokio::test]
    async fn resolve_host_literal_matching_family_ok() {
        assert!(resolve_host("127.0.0.1", 0, Some(4))
            .await
            .unwrap()
            .is_ipv4());
        assert!(resolve_host("::1", 0, Some(6)).await.unwrap().is_ipv6());
    }

    #[tokio::test]
    async fn resolve_host_literal_family_mismatch_errors() {
        // -6 against an IPv4 literal (and vice versa) must be rejected, not
        // silently connected to the wrong family.
        assert!(resolve_host("127.0.0.1", 0, Some(6)).await.is_err());
        assert!(resolve_host("::1", 0, Some(4)).await.is_err());
    }

    #[tokio::test]
    async fn resolve_host_hostname_filters_by_family() {
        // localhost typically resolves to 127.0.0.1 and/or ::1; assert the
        // requested family is honored when available, soft-skip if not.
        match resolve_host("localhost", 5201, Some(4)).await {
            Ok(a) => assert!(a.is_ipv4(), "Some(4) must yield IPv4, got {a}"),
            Err(_) => eprintln!("SKIP: localhost has no IPv4 address on this host"),
        }
        match resolve_host("localhost", 5201, Some(6)).await {
            Ok(a) => assert!(a.is_ipv6(), "Some(6) must yield IPv6, got {a}"),
            Err(_) => eprintln!("SKIP: localhost has no IPv6 address on this host"),
        }
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

    // ---- Regression tests for issue #1: server binds IPv4 only ------------
    //
    // `tcp_listen_dual_stack_default` and `tcp_listen_ipv6_only` are genuine
    // regressions: they fail on `main`'s default-IPv4 behavior and pass after
    // the dual-stack fix. `tcp_listen_ipv4_only` is a guard for behavior that
    // was already correct on `main` (`Some(4)` always meant `0.0.0.0`); it's
    // kept to lock that in.

    /// Default listener (`ip_version=None`) must accept both IPv4 and IPv6
    /// clients on the same port — matches iperf3's dual-stack default.
    #[tokio::test]
    async fn tcp_listen_dual_stack_default() {
        let listener = tcp_listen(None, 0, None).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // IPv4 connect must succeed.
        let v4 = tcp_connect("127.0.0.1", port, None, None, None, false, None).await;
        assert!(v4.is_ok(), "IPv4 connect failed: {v4:?}");
        let _ = listener.accept().await.unwrap();

        // IPv6 connect must also succeed against the same listener.
        let v6 = tcp_connect("::1", port, None, None, None, false, None).await;
        match v6 {
            Ok(_) => {
                let _ = listener.accept().await.unwrap();
            }
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                // No IPv6 loopback on this host — the IPv6 half can't be
                // exercised. Make the skip visible so it isn't a silent vacuous
                // pass (run with `--nocapture` to see it).
                eprintln!(
                    "SKIP tcp_listen_dual_stack_default: ::1 unavailable, IPv6 path not exercised"
                );
            }
            Err(e) => panic!("IPv6 connect to default listener failed: {e:?}"),
        }
    }

    /// `ip_version=Some(4)` restricts the listener to IPv4 only.
    #[tokio::test]
    async fn tcp_listen_ipv4_only() {
        let listener = tcp_listen(None, 0, Some(4)).await.unwrap();
        let local = listener.local_addr().unwrap();
        assert!(local.is_ipv4(), "expected IPv4 local_addr, got {local}");
        let port = local.port();

        // IPv6 connect must be refused.
        let v6 = tcp_connect("::1", port, None, None, None, false, None).await;
        match v6 {
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                // ::1 unavailable: this confirms "no IPv6 reach" but not
                // specifically that the listener rejected it. Flag the weaker
                // assertion rather than pass silently.
                eprintln!(
                    "SKIP tcp_listen_ipv4_only: ::1 unavailable, refusal not specifically verified"
                );
            }
            Ok(_) => panic!("IPv6 connect should fail against IPv4-only listener"),
            Err(e) => panic!("unexpected error from IPv6 connect: {e:?}"),
        }
    }

    /// `ip_version=Some(6)` restricts the listener to IPv6 only (sets
    /// IPV6_V6ONLY). On Linux without this, `::` accepts IPv4 via v4-mapped
    /// addresses; the test guards against that regression.
    #[tokio::test]
    async fn tcp_listen_ipv6_only() {
        let listener = tcp_listen(None, 0, Some(6)).await.unwrap();
        let local = listener.local_addr().unwrap();
        assert!(local.is_ipv6(), "expected IPv6 local_addr, got {local}");
        let port = local.port();

        // IPv4 connect must be refused.
        let v4 = tcp_connect("127.0.0.1", port, None, None, None, false, None).await;
        match v4 {
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                // No IPv4 loopback on this host — can't exercise the refusal.
                eprintln!("SKIP tcp_listen_ipv6_only: 127.0.0.1 unavailable");
            }
            Ok(_) => panic!("IPv4 connect should fail against IPv6-only listener"),
            Err(e) => panic!("unexpected error from IPv4 connect: {e:?}"),
        }
    }

    /// Regression for the cold-review should-fix: `-6`/`-4` must be honored
    /// even when an explicit `-B` bind address is given. `-B :: -6` is
    /// IPv6-only; `-B ::` alone is dual-stack. Previously V6ONLY was only set
    /// on the implicit-default path, so `-B :: -6` silently stayed dual-stack.
    #[tokio::test]
    async fn tcp_listen_explicit_bind_respects_ip_version() {
        // `-B :: -6`  → IPv6-only: an IPv4 client must be refused.
        let v6only = tcp_listen(Some("::"), 0, Some(6)).await.unwrap();
        let port = v6only.local_addr().unwrap().port();
        let v4 = tcp_connect("127.0.0.1", port, None, None, None, false, None).await;
        assert!(
            matches!(&v4, Err(RiperfError::Io(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused),
            "`-B :: -6` must refuse IPv4, got {v4:?}"
        );
        drop(v6only);

        // `-B ::` alone → dual-stack: an IPv4 client connects via v4-mapped.
        let dual = tcp_listen(Some("::"), 0, None).await.unwrap();
        let port = dual.local_addr().unwrap().port();
        let v4 = tcp_connect("127.0.0.1", port, None, None, None, false, None).await;
        match v4 {
            Ok(_) => {
                let _ = dual.accept().await.unwrap();
            }
            Err(RiperfError::Io(ref e)) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                eprintln!("SKIP tcp_listen_explicit_bind: v4-mapped unavailable on this host");
            }
            Err(e) => panic!("`-B ::` (dual-stack) must accept IPv4, got {e:?}"),
        }
    }

    // ---- tcp_maxseg + UDP sender socket tuning (issue #6) -----------------

    #[cfg(unix)]
    #[tokio::test]
    async fn tcp_maxseg_reports_mss_for_connected_stream() {
        // A connected TCP stream exposes a positive MSS via TCP_MAXSEG — this
        // is what the UDP datagram-size default is derived from (iperf3 parity).
        let listener = tcp_listen(Some("127.0.0.1"), 0, None).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client_task = tokio::spawn(async move {
            tcp_connect("127.0.0.1", port, None, None, None, false, None)
                .await
                .unwrap()
        });
        let (_server, _) = listener.accept().await.unwrap();
        let client = client_task.await.unwrap();

        let mss = tcp_maxseg(&client);
        assert!(
            mss.is_some(),
            "TCP_MAXSEG should be readable on a connected socket"
        );
        assert!(mss.unwrap() > 0, "MSS should be positive, got {mss:?}");
    }

    #[cfg(unix)]
    #[test]
    fn configure_udp_sender_switches_to_blocking() {
        // tokio sockets are non-blocking and into_std() keeps that flag; the
        // blocking sender thread needs a blocking socket so sendmmsg
        // backpressures in-kernel instead of busy-spinning on EAGAIN (#6).
        use std::os::unix::io::AsRawFd;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        assert!(
            is_nonblocking(socket.as_raw_fd()),
            "precondition: socket starts non-blocking"
        );

        configure_udp_sender(&socket, 128 * 1460).unwrap();
        assert!(
            !is_nonblocking(socket.as_raw_fd()),
            "sender socket must be switched to blocking"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configure_udp_sender_sets_real_send_timeout() {
        // Regression: a blocking sender needs a real SO_SNDTIMEO so a wedged
        // link can't park the thread forever. TCP_USER_TIMEOUT (set_snd_timeout)
        // is a no-op on UDP and would leave the timeout unset (#6 review).
        use nix::sys::socket::{getsockopt, sockopt};
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        configure_udp_sender(&socket, 128 * 1460).unwrap();
        let tv = getsockopt(&socket, sockopt::SendTimeout).unwrap();
        assert!(
            tv.tv_sec() > 0 || tv.tv_usec() > 0,
            "SO_SNDTIMEO must be non-zero, got {}.{:06}",
            tv.tv_sec(),
            tv.tv_usec()
        );
    }

    #[cfg(unix)]
    fn is_nonblocking(fd: std::os::unix::io::RawFd) -> bool {
        // SAFETY: F_GETFL on a valid fd returns the file status flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        flags >= 0 && (flags & libc::O_NONBLOCK) != 0
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
