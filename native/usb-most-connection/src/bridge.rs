//! Per-service proof for the app's dedicated loopback USB endpoint.
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use zeroize::Zeroizing;
#[derive(Clone)]
pub struct LocalBridge {
    address: SocketAddr,
    proof: Arc<Zeroizing<[u8; 32]>>,
}
impl LocalBridge {
    pub fn new(address: SocketAddr, proof: [u8; 32]) -> Result<Self, &'static str> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err("invalid_bridge");
        }
        Ok(Self {
            address,
            proof: Arc::new(Zeroizing::new(proof)),
        })
    }
    pub async fn connect(&self) -> Result<TcpStream, &'static str> {
        timeout(Duration::from_secs(5), async {
            let mut local = TcpStream::connect(self.address).await?;
            local.set_nodelay(true)?;
            local.write_all(b"UMLOCAL1").await?;
            local.write_all(self.proof.as_slice()).await?;
            let mut ready = [0];
            local.read_exact(&mut ready).await?;
            if ready != [1] {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            Ok::<_, std::io::Error>(local)
        })
        .await
        .map_err(|_| "bridge_timeout")?
        .map_err(|_| "bridge_unavailable")
    }
}
