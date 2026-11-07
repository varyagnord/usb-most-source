//! Dual-stack public listener and advisory LAN discovery. Discovery never carries
//! a pairing claim, capability, SSH credential or authority to trust a peer.
use mdns_sd::{ServiceDaemon, ServiceInfo};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    time::Duration,
};
pub const SERVICE: &str = "_usbmost._tcp.local.";
fn bind(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = Socket::new(
        if address.is_ipv6() {
            Domain::IPV6
        } else {
            Domain::IPV4
        },
        Type::STREAM,
        Some(Protocol::TCP),
    )?;
    if address.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsSocket;
        usb_most_platform::windows::exclusive_socket(socket.as_socket())?;
    }
    // Linux retains actively closed connections in TIME_WAIT after shutdown.
    // Set this on every generation so a receiver can reopen its fixed port;
    // an occupied listening socket is still exclusive (no SO_REUSEPORT).
    // Windows keeps SO_EXCLUSIVEADDRUSE above and must not enable this flag.
    #[cfg(target_os = "linux")]
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(128)?;
    tokio::net::TcpListener::from_std(socket.into())
}
pub fn loopback_listener() -> io::Result<tokio::net::TcpListener> {
    bind(([127, 0, 0, 1], 0).into())
}
pub fn listener(port: u16) -> io::Result<(tokio::net::TcpListener, bool)> {
    match bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)) {
        Ok(listener) => Ok((listener, true)),
        // IPv6 disabled/unavailable, not a port collision or permission failure.
        Err(error) if matches!(error.raw_os_error(), Some(97 | 99 | 10047 | 10049)) => {
            bind(([0, 0, 0, 0], port).into()).map(|listener| (listener, false))
        }
        Err(error) => Err(error),
    }
}
pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}
impl Announcement {
    pub fn start(port: u16, public_id: &str, ipv6: bool) -> io::Result<Self> {
        if port < 1024 || public_id.len() != 16 || !public_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let daemon = ServiceDaemon::new().map_err(|_| io::ErrorKind::AddrNotAvailable)?;
        let result = (|| {
            if !ipv6 {
                daemon
                    .disable_interface(mdns_sd::IfKind::IPv6)
                    .map_err(|_| io::ErrorKind::Other)?;
            }
            daemon
                .set_ip_check_interval(5)
                .map_err(|_| io::ErrorKind::Other)?;
            let record = ServiceInfo::new(
                SERVICE,
                &format!("USB Most {}", &public_id[..6]),
                &format!("usb-most-{public_id}.local."),
                "",
                port,
                &[("v", "1")][..],
            )
            .map_err(|_| io::ErrorKind::InvalidData)?
            .enable_addr_auto();
            let fullname = record.get_fullname().to_owned();
            daemon.register(record).map_err(|_| io::ErrorKind::Other)?;
            Ok(fullname)
        })();
        match result {
            Ok(fullname) => Ok(Self { daemon, fullname }),
            Err(error) => {
                let _ = daemon.shutdown();
                Err(error)
            }
        }
    }
}
impl Drop for Announcement {
    fn drop(&mut self) {
        // Bounded shutdown; never keep the receiver alive for multicast delivery.
        if let Ok(reply) = self.daemon.unregister(&self.fullname) {
            let _ = reply.recv_timeout(Duration::from_millis(500));
        }
        if let Ok(reply) = self.daemon.shutdown() {
            let _ = reply.recv_timeout(Duration::from_millis(500));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn public_listener_rebinds_after_accepted_connection_active_close() {
        for ipv6 in [false, true] {
            let (listener, dual_stack) = listener(0).unwrap();
            if ipv6 && !dual_stack {
                continue;
            }
            let port = listener.local_addr().unwrap().port();
            let address = if ipv6 {
                format!("[::1]:{port}")
            } else {
                format!("127.0.0.1:{port}")
            };
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut accepted, _) = listener.accept().await.unwrap();
            accepted.write_all(b"synced").await.unwrap();
            // The server actively closes, as when stopping a receiver. Read
            // the FIN before closing the client to leave a server TIME_WAIT.
            accepted.shutdown().await.unwrap();
            let mut received = Vec::new();
            client.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, b"synced");
            drop(client);
            drop(accepted);
            drop(listener);
            let (_rebound, _) = super::listener(port)
                .expect("receiver must reopen its configured port after an active close");
        }
    }
    #[tokio::test]
    async fn public_listener_accepts_ipv4_and_ipv6_without_a_second_insecure_port() {
        let (listener, v6) = listener(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        for address in [format!("127.0.0.1:{port}"), format!("[::1]:{port}")]
            .iter()
            .take(if v6 { 2 } else { 1 })
        {
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            client.write_all(b"same-listener").await.unwrap();
            let mut data = [0; 13];
            server.read_exact(&mut data).await.unwrap();
            assert_eq!(&data, b"same-listener");
        }
        assert!(
            super::listener(port).is_err(),
            "occupied port must fail, not silently select a different binding"
        );
    }
}
