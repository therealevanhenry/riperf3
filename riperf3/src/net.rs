use std::net::SocketAddr;
use std::time::Duration;

use socket2::{Domain, Socket, Type};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::error::{Result, RiperfError};

// ---------------------------------------------------------------------------
// TCP
// ---------------------------------------------------------------------------

/// Connect to a TCP endpoint, optionally with a timeout.
pub async fn tcp_connect(
    host: &str,
    port: u16,
    timeout: Option<Duration>,
) -> Result<TcpStream> {
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

/// Create a TCP listener with SO_REUSEADDR.
/// If `bind_addr` is `None`, binds to `0.0.0.0`.
pub async fn tcp_listen(bind_addr: Option<&str>, port: u16) -> Result<TcpListener> {
    let addr: SocketAddr = format!("{}:{}", bind_addr.unwrap_or("0.0.0.0"), port)
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

/// Create a TCP listener pre-configured with optional socket options (MSS, buffer sizes, etc.).
/// Used by the server when client requests specific socket options for data streams.
pub async fn tcp_listen_with_opts(
    bind_addr: Option<&str>,
    port: u16,
    mss: Option<i32>,
    recv_buf: Option<i32>,
    send_buf: Option<i32>,
    no_delay: bool,
) -> Result<TcpListener> {
    let addr: SocketAddr = format!("{}:{}", bind_addr.unwrap_or("0.0.0.0"), port)
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

    if let Some(mss_val) = mss {
        // TCP_MAXSEG must be set before listen
        #[cfg(target_os = "linux")]
        unsafe {
            let val = mss_val as libc::c_int;
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(&socket) as libc::c_int,
                libc::IPPROTO_TCP,
                libc::TCP_MAXSEG,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    if let Some(size) = recv_buf {
        socket.set_recv_buffer_size(size as usize)?;
    }
    if let Some(size) = send_buf {
        socket.set_send_buffer_size(size as usize)?;
    }
    socket.set_nodelay(no_delay)?;

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
        let listener = tcp_listen(Some("127.0.0.1"), 0).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let client_task = tokio::spawn(async move {
            tcp_connect("127.0.0.1", port, None).await.unwrap()
        });

        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = client_task.await.unwrap();

        assert!(server_stream.peer_addr().is_ok());
        assert!(client_stream.peer_addr().is_ok());
    }

    #[tokio::test]
    async fn tcp_connect_timeout() {
        // Connect to a non-routable address with a short timeout
        let result = tcp_connect("192.0.2.1", 12345, Some(Duration::from_millis(50))).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn udp_bind_ephemeral() {
        let socket = udp_bind(Some("127.0.0.1"), 0).await.unwrap();
        assert!(socket.local_addr().is_ok());
    }
}
