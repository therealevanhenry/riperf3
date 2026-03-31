use std::net::SocketAddr;
use std::time::Duration;

use socket2::{Domain, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::error::{Result, RiperfError};

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
        let remote: SocketAddr = format!("{host}:{port}")
            .parse()
            .map_err(|e| RiperfError::Protocol(format!("bad address: {e}")))?;
        let domain = if remote.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
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
        let addr = format!("{host}:{port}");
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
    let addr: SocketAddr = format!("{}:{}", bind_addr.unwrap_or(default_bind_addr(ip_version)), port)
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

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();

        if let Some(mss_val) = mss {
            unsafe {
                let val = mss_val as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_MAXSEG,
                    &val as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        if let Some(size) = window {
            let size = size as libc::c_int;
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        if let Some(algo) = congestion {
            let algo_bytes = algo.as_bytes();
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_CONGESTION,
                    algo_bytes.as_ptr() as *const libc::c_void,
                    algo_bytes.len() as libc::socklen_t,
                );
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// UDP
// ---------------------------------------------------------------------------

/// Bind a UDP socket. If `bind_addr` is `None`, binds to `0.0.0.0`.
pub async fn udp_bind(bind_addr: Option<&str>, port: u16) -> Result<UdpSocket> {
    let addr = format!("{}:{}", bind_addr.unwrap_or("0.0.0.0"), port);
    Ok(UdpSocket::bind(&addr).await?)
}

/// Set SO_RCVTIMEO on a socket (receive timeout in milliseconds).
#[cfg(target_os = "linux")]
pub fn set_rcv_timeout(fd: i32, ms: u64) -> Result<()> {
    let tv = libc::timeval {
        tv_sec: (ms / 1000) as libc::time_t,
        tv_usec: ((ms % 1000) * 1000) as libc::suseconds_t,
    };
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_rcv_timeout(_fd: i32, _ms: u64) -> Result<()> {
    Ok(())
}

/// Set TCP_USER_TIMEOUT on a socket (send timeout in milliseconds).
#[cfg(target_os = "linux")]
pub fn set_snd_timeout(fd: i32, ms: u64) -> Result<()> {
    let val = ms as libc::c_uint;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_USER_TIMEOUT,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_snd_timeout(_fd: i32, _ms: u64) -> Result<()> {
    Ok(())
}

/// Set the IPv4 Don't Fragment bit on a socket.
#[cfg(target_os = "linux")]
pub fn set_dont_fragment(fd: i32) -> Result<()> {
    let val: libc::c_int = libc::IP_PMTUDISC_DO;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MTU_DISCOVER,
            &val as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_dont_fragment(_fd: i32) -> Result<()> {
    Ok(()) // Not supported on this platform
}

/// Set SO_MAX_PACING_RATE for FQ-based socket pacing (Linux only).
#[cfg(target_os = "linux")]
pub fn set_fq_rate(fd: i32, rate_bits_per_sec: u64) -> Result<()> {
    let rate_bytes = (rate_bits_per_sec / 8) as libc::c_uint;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_MAX_PACING_RATE,
            &rate_bytes as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_fq_rate(_fd: i32, _rate: u64) -> Result<()> {
    Ok(())
}

/// Bind socket to a specific network device (SO_BINDTODEVICE, Linux only, needs CAP_NET_RAW).
#[cfg(target_os = "linux")]
pub fn set_bind_dev(fd: i32, dev: &str) -> Result<()> {
    let dev_bytes = dev.as_bytes();
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            dev_bytes.as_ptr() as *const libc::c_void,
            dev_bytes.len() as libc::socklen_t,
        )
    };
    if ret < 0 {
        return Err(RiperfError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_bind_dev(_fd: i32, _dev: &str) -> Result<()> {
    Ok(())
}

/// Set TCP keepalive options on a socket.
#[cfg(target_os = "linux")]
pub fn set_tcp_keepalive(
    fd: i32,
    idle: Option<u32>,
    interval: Option<u32>,
    count: Option<u32>,
) -> Result<()> {
    let enable: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &enable as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if let Some(val) = idle {
            let val = val as libc::c_int;
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPIDLE,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        if let Some(val) = interval {
            let val = val as libc::c_int;
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPINTVL,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        if let Some(val) = count {
            let val = val as libc::c_int;
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_KEEPCNT,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_tcp_keepalive(
    _fd: i32,
    _idle: Option<u32>,
    _interval: Option<u32>,
    _count: Option<u32>,
) -> Result<()> {
    Ok(())
}

/// Set CPU affinity for the current thread (Linux only).
#[cfg(target_os = "linux")]
pub fn set_cpu_affinity(core: usize) -> Result<()> {
    unsafe {
        let mut cpuset = std::mem::MaybeUninit::<libc::cpu_set_t>::zeroed().assume_init();
        libc::CPU_ZERO(&mut cpuset);
        libc::CPU_SET(core, &mut cpuset);
        let ret = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &cpuset);
        if ret < 0 {
            return Err(RiperfError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_cpu_affinity(_core: usize) -> Result<()> {
    Ok(())
}

/// Bind a UDP socket with SO_REUSEADDR, allowing multiple sockets on the same port.
/// Used by the server to recycle the UDP listener after each stream connect.
pub async fn udp_bind_reusable(bind_addr: Option<&str>, port: u16) -> Result<UdpSocket> {
    let addr: SocketAddr = format!("{}:{}", bind_addr.unwrap_or("0.0.0.0"), port)
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
            tcp_connect("127.0.0.1", port, None, None, false).await.unwrap()
        });

        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = client_task.await.unwrap();

        assert!(server_stream.peer_addr().is_ok());
        assert!(client_stream.peer_addr().is_ok());
    }

    #[tokio::test]
    async fn tcp_connect_timeout() {
        // Connect to a non-routable address with a short timeout
        let result = tcp_connect("192.0.2.1", 12345, Some(Duration::from_millis(50)), None, false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn udp_bind_ephemeral() {
        let socket = udp_bind(Some("127.0.0.1"), 0).await.unwrap();
        assert!(socket.local_addr().is_ok());
    }
}
