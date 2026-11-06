//! Direct reverse TLS streams. Entirely peer-to-peer; no relay, rendezvous or account service.
use crate::{
    bridge::LocalBridge,
    config::Endpoint,
    pairing::{PeerId, Store},
    security,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, copy_bidirectional},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
    time::{interval, timeout},
};
use zeroize::Zeroizing;
type Stream = tokio_rustls::server::TlsStream<TcpStream>;
const MAGIC: &[u8; 8] = b"UMTLS1\0\0";
const OPEN: u8 = 1;
const PING: u8 = 2;
const PONG: u8 = 3;
const OPEN_FAILED: u8 = 4;
const PAIR: u8 = 1;
const CONTROL: u8 = 2;
const DATA: u8 = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Network,
    Timeout,
    IdentityRejected,
    Denied,
    Busy,
    Closed,
    Storage,
}
fn io_error(_: impl Sized) -> Error {
    Error::Network
}
fn tls_error(error: security::Error) -> Error {
    match error {
        security::Error::IdentityRejected => Error::IdentityRejected,
        security::Error::Network => Error::Network,
        security::Error::Timeout => Error::Timeout,
        security::Error::InvalidIdentity
        | security::Error::Random
        | security::Error::Peer
        | security::Error::Protocol => Error::Invalid,
    }
}
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// The protocol adapter gets a socket only after the remote stream is ready.
/// Its RTT measurement therefore excludes fresh TLS setup and discovery.
pub struct GatewayConnector {
    address: SocketAddr,
    proof: Arc<Zeroizing<[u8; 32]>>,
}
impl GatewayConnector {
    pub fn new(address: SocketAddr, proof: [u8; 32]) -> Result<Self, Error> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            address,
            proof: Arc::new(Zeroizing::new(proof)),
        })
    }
}
impl usb_most_protocol::agent::Connector for GatewayConnector {
    fn authenticated(&self) -> bool {
        true
    }
    fn connect(&self) -> io::Result<std::net::TcpStream> {
        let mut socket =
            std::net::TcpStream::connect_timeout(&self.address, Duration::from_secs(5))?;
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(Duration::from_secs(45)))?;
        socket.set_write_timeout(Some(Duration::from_secs(15)))?;
        crate::gateway::client_sync(&mut socket, &self.proof)?;
        let mut reply = [0];
        socket.read_exact(&mut reply)?;
        if reply != [1] {
            return Err(denied());
        }
        socket.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(socket)
    }
}

struct Header {
    op: u8,
    token: Zeroizing<[u8; 32]>,
    nonce: [u8; 16],
    name: String,
}
async fn header(stream: &mut (impl AsyncRead + Unpin)) -> Result<Header, Error> {
    timeout(Duration::from_secs(10), async {
        let mut bytes = Zeroizing::new([0u8; 58]);
        stream.read_exact(bytes.as_mut()).await.map_err(io_error)?;
        if &bytes[..8] != MAGIC || bytes[57] > 64 {
            return Err(Error::Invalid);
        }
        let mut name = vec![0; usize::from(bytes[57])];
        stream.read_exact(&mut name).await.map_err(io_error)?;
        Ok(Header {
            op: bytes[8],
            token: Zeroizing::new(bytes[9..41].try_into().unwrap()),
            nonce: bytes[41..57].try_into().unwrap(),
            name: String::from_utf8(name).map_err(|_| Error::Invalid)?,
        })
    })
    .await
    .map_err(|_| Error::Timeout)?
}
async fn send_header(
    stream: &mut (impl AsyncWrite + Unpin),
    op: u8,
    token: &[u8; 32],
    nonce: [u8; 16],
    name: &str,
) -> Result<(), Error> {
    if name.len() > 64 {
        return Err(Error::Invalid);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(58 + name.len()));
    bytes.extend(MAGIC);
    bytes.push(op);
    bytes.extend(token);
    bytes.extend(nonce);
    bytes.push(name.len() as u8);
    bytes.extend(name.as_bytes());
    timeout(Duration::from_secs(10), stream.write_all(&bytes))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)
}
async fn accepted(stream: &mut (impl AsyncRead + Unpin)) -> Result<(), Error> {
    let mut reply = [0];
    timeout(Duration::from_secs(15), stream.read_exact(&mut reply))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)?;
    match reply[0] {
        1 => Ok(()),
        2 => Err(Error::Busy),
        3 => Err(Error::Storage),
        _ => Err(Error::Denied),
    }
}
async fn acknowledge(stream: &mut (impl AsyncWrite + Unpin)) -> Result<(), Error> {
    timeout(Duration::from_secs(10), stream.write_all(&[1]))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)
}

pub async fn pair(
    code: &crate::pairing::PairCode,
    nonce: [u8; 16],
    name: &str,
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let token = crate::pairing::token(&code.claim).map_err(|_| Error::Invalid)?;
    let mut stream = security::connect(&code.endpoint, &code.server_pin)
        .await
        .map_err(tls_error)?;
    send_header(&mut stream, PAIR, &token, nonce, name).await?;
    accepted(&mut stream).await?;
    let mut grant = Zeroizing::new([0; 32]);
    timeout(Duration::from_secs(10), stream.read_exact(grant.as_mut()))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)?;
    Ok(grant)
}

struct Hub {
    peer: PeerId,
    open: mpsc::Sender<[u8; 16]>,
    pending: Mutex<BTreeMap<[u8; 16], oneshot::Sender<Stream>>>,
    stop: watch::Sender<bool>,
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
type HubSlot = Arc<Mutex<Option<Arc<Hub>>>>;
struct Pending {
    hub: Arc<Hub>,
    id: [u8; 16],
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut p) = self.hub.pending.lock() {
            p.remove(&self.id);
        }
    }
}
struct ActiveHub {
    slot: HubSlot,
    hub: Arc<Hub>,
}
impl Drop for ActiveHub {
    fn drop(&mut self) {
        self.hub.stop.send_replace(true);
        if let Ok(mut pending) = self.hub.pending.lock() {
            pending.clear();
        }
        if let Ok(mut slot) = self.slot.lock()
            && slot.as_ref().is_some_and(|h| Arc::ptr_eq(h, &self.hub))
        {
            *slot = None;
        }
    }
}
struct Abort(tokio::task::AbortHandle);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn control_reader(
    mut reader: impl AsyncRead + Unpin + Send + 'static,
) -> (mpsc::Receiver<Result<[u8; 17], Error>>, Abort) {
    let (tx, rx) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        loop {
            let mut message = [0; 17];
            let result =
                match timeout(Duration::from_secs(45), reader.read_exact(&mut message)).await {
                    Ok(Ok(_)) => Ok(message),
                    Ok(Err(_)) => Err(Error::Closed),
                    Err(_) => Err(Error::Timeout),
                };
            let end = result.is_err();
            if tx.send(result).await.is_err() || end {
                break;
            }
        }
    });
    (rx, Abort(task.abort_handle()))
}
async fn send_control(
    writer: &mut (impl AsyncWrite + Unpin),
    op: u8,
    id: [u8; 16],
) -> Result<(), Error> {
    let mut message = [0; 17];
    message[0] = op;
    message[1..].copy_from_slice(&id);
    timeout(Duration::from_secs(10), writer.write_all(&message))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)
}

async fn serve_control(
    mut stream: Stream,
    store: Arc<Store>,
    token: Zeroizing<[u8; 32]>,
    slot: HubSlot,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let mut lease = match store.authenticate(&token) {
        Ok(lease) => lease,
        Err(_) => {
            let _ = timeout(Duration::from_secs(2), stream.write_all(&[0])).await;
            return Err(Error::Denied);
        }
    };
    let (tx, mut opens) = mpsc::channel(8);
    let (halt, _) = watch::channel(false);
    let hub = Arc::new(Hub {
        peer: lease.id,
        open: tx,
        pending: Mutex::new(BTreeMap::new()),
        stop: halt,
    });
    let busy = {
        let mut target = slot.lock().map_err(|_| Error::Closed)?;
        if target.is_some() {
            true
        } else {
            *target = Some(hub.clone());
            false
        }
    };
    if busy {
        let _ = timeout(Duration::from_secs(2), stream.write_all(&[2])).await;
        return Err(Error::Busy);
    }
    let _guard = ActiveHub { slot, hub };
    acknowledge(&mut stream).await?;
    let (reader, mut writer) = tokio::io::split(stream);
    let (mut messages, _reader_guard) = control_reader(reader);
    loop {
        tokio::select! {
            message=messages.recv()=>{
                let message=message.ok_or(Error::Closed)??;
                if message[0]==PING && message[1..]==[0;16] {
                    send_control(&mut writer,PONG,[0;16]).await?;
                } else if message[0]==OPEN_FAILED && message[1..]!=[0;16] {
                    let id:[u8;16]=message[1..].try_into().unwrap();
                    _guard.hub.pending.lock().map_err(|_|Error::Closed)?.remove(&id);
                } else {return Err(Error::Invalid);}
            }
            open=opens.recv()=>send_control(&mut writer,OPEN,open.ok_or(Error::Closed)?).await?,
            _=stop.changed()=>return Ok(()),
            _=lease.revoked.changed()=>return Err(Error::Denied),
        }
    }
}
async fn network_peer(
    socket: TcpStream,
    config: Arc<rustls::ServerConfig>,
    store: Arc<Store>,
    slot: HubSlot,
    stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    let mut stream = security::accept(socket, config)
        .await
        .map_err(|_| Error::Network)?;
    let request = header(&mut stream).await?;
    match request.op {
        PAIR => {
            let granted = tokio::task::spawn_blocking(move || {
                store.redeem(&request.token, request.nonce, &request.name)
            })
            .await
            .map_err(|_| Error::Storage)?;
            let grant = match granted {
                Ok(grant) => grant,
                Err(e) => {
                    let status = match e {
                        crate::pairing::Error::Storage => 3,
                        crate::pairing::Error::Capacity => 2,
                        _ => 0,
                    };
                    let _ = timeout(Duration::from_secs(2), stream.write_all(&[status])).await;
                    return Err(if status == 3 {
                        Error::Storage
                    } else {
                        Error::Denied
                    });
                }
            };
            acknowledge(&mut stream).await?;
            timeout(Duration::from_secs(10), stream.write_all(grant.as_slice()))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(io_error)?;
            let _ = timeout(Duration::from_secs(2), stream.shutdown()).await;
            Ok(())
        }
        CONTROL if request.nonce == [0; 16] && request.name.is_empty() => {
            serve_control(stream, store, request.token, slot, stop).await
        }
        DATA if request.nonce != [0; 16] && request.name.is_empty() => {
            let lease = store
                .authenticate(&request.token)
                .map_err(|_| Error::Denied)?;
            let hub = slot
                .lock()
                .map_err(|_| Error::Closed)?
                .clone()
                .ok_or(Error::Closed)?;
            if hub.peer != lease.id {
                return Err(Error::Denied);
            }
            let pending = hub
                .pending
                .lock()
                .map_err(|_| Error::Closed)?
                .remove(&request.nonce)
                .ok_or(Error::Denied)?;
            if pending.is_closed() {
                return Err(Error::Closed);
            }
            acknowledge(&mut stream).await?;
            // Do not expose a "ready" gateway until Android has also opened
            // its authenticated local endpoint. A TLS socket alone is not USB-ready.
            accepted(&mut stream).await?;
            pending.send(stream).map_err(|_| Error::Closed)?;
            Ok(())
        }
        _ => Err(Error::Invalid),
    }
}
async fn gateway_peer(
    mut local: TcpStream,
    proof: Arc<Zeroizing<[u8; 32]>>,
    slot: HubSlot,
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    timeout(
        Duration::from_secs(5),
        crate::gateway::server(&mut local, &proof),
    )
    .await
    .map_err(|_| Error::Timeout)?
    .map_err(|_| Error::Denied)?;
    let hub = slot
        .lock()
        .map_err(|_| Error::Closed)?
        .clone()
        .ok_or(Error::Closed)?;
    let mut halted = hub.stop.subscribe();
    if *halted.borrow() {
        return Err(Error::Closed);
    }
    let id = security::random::<16>().map_err(|_| Error::Network)?;
    if id == [0; 16] {
        return Err(Error::Invalid);
    }
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = hub.pending.lock().map_err(|_| Error::Closed)?;
        if pending.len() >= 8 || pending.contains_key(&id) {
            return Err(Error::Busy);
        }
        pending.insert(id, tx);
    }
    let _guard = Pending {
        hub: hub.clone(),
        id,
    };
    hub.open.try_send(id).map_err(|_| Error::Busy)?;
    let mut stream = tokio::select! {
        result=timeout(Duration::from_secs(35),rx)=>result.map_err(|_|Error::Timeout)?.map_err(|_|Error::Closed)?,
        _=halted.changed()=>return Err(Error::Closed),
        _=stop.changed()=>return Err(Error::Closed),
    };
    acknowledge(&mut local).await?;
    tokio::select! {
        _=copy_bidirectional(&mut local,&mut stream)=>{},
        _=halted.changed()=>{},
        _=stop.changed()=>{},
    }
    Ok(())
}

/// Gateway is a private authenticated hop for our protocol adapter, not a raw
/// listener for arbitrary local users. The platform driver API is separate.
pub async fn run_server(
    network: TcpListener,
    gateway: TcpListener,
    config: Arc<rustls::ServerConfig>,
    store: Arc<Store>,
    proof: [u8; 32],
    mut stop: watch::Receiver<bool>,
) -> Result<(), Error> {
    if *stop.borrow() {
        return Ok(());
    }
    if !gateway.local_addr().map_err(io_error)?.ip().is_loopback() {
        return Err(Error::Invalid);
    }
    let slot: HubSlot = Arc::new(Mutex::new(None));
    let proof = Arc::new(Zeroizing::new(proof));
    let peers = Arc::new(Semaphore::new(16));
    let streams = Arc::new(Semaphore::new(8));
    let mut jobs = JoinSet::new();
    loop {
        tokio::select! {
            result=network.accept()=>{
                let(socket,_)=result.map_err(io_error)?;let Ok(permit)=peers.clone().try_acquire_owned() else{continue;};
                let(config,store,slot,stop)=(config.clone(),store.clone(),slot.clone(),stop.clone());
                jobs.spawn(async move{let _permit=permit;let _=network_peer(socket,config,store,slot,stop).await;});
            }
            result=gateway.accept()=>{
                let(socket,_)=result.map_err(io_error)?;let Ok(permit)=streams.clone().try_acquire_owned() else{continue;};
                let(proof,slot,stop)=(proof.clone(),slot.clone(),stop.clone());
                jobs.spawn(async move{let _permit=permit;let _=gateway_peer(socket,proof,slot,stop).await;});
            }
            _=jobs.join_next(),if !jobs.is_empty()=>{},
            _=stop.changed()=>return Ok(()),
        }
    }
}

pub async fn run_client(
    endpoint: Endpoint,
    pin: String,
    token: Zeroizing<[u8; 32]>,
    bridge: LocalBridge,
    mut stop: watch::Receiver<bool>,
    ready: oneshot::Sender<()>,
) -> Result<(), Error> {
    if *stop.borrow() {
        return Ok(());
    }
    let mut stream = security::connect(&endpoint, &pin)
        .await
        .map_err(tls_error)?;
    send_header(&mut stream, CONTROL, &token, [0; 16], "").await?;
    accepted(&mut stream).await?;
    let _ = ready.send(());
    let (reader, mut writer) = tokio::io::split(stream);
    let (mut messages, _reader_guard) = control_reader(reader);
    let mut heartbeat = interval(Duration::from_secs(15));
    let token = Arc::new(token);
    let mut jobs = JoinSet::new();
    let mut active = BTreeSet::new();
    loop {
        tokio::select! {
            message=messages.recv()=>{
                let message=message.ok_or(Error::Closed)??;let id:[u8;16]=message[1..].try_into().unwrap();
                if message[0]==PONG && id==[0;16] {continue;}
                if message[0]!=OPEN || id==[0;16] || active.len()>=8 || !active.insert(id) {return Err(Error::Invalid);}
                let(endpoint,pin,token,bridge,mut stop)=(endpoint.clone(),pin.clone(),token.clone(),bridge.clone(),stop.clone());
                jobs.spawn(async move {
                    let task=async {
                        let mut remote=security::connect(&endpoint,&pin).await.map_err(tls_error)?;
                        send_header(&mut remote,DATA,&token,id,"").await?;accepted(&mut remote).await?;
                        let mut local=bridge.connect().await.map_err(|_|Error::Network)?;
                        acknowledge(&mut remote).await?;
                        let _=copy_bidirectional(&mut remote,&mut local).await;Ok::<_,Error>(())
                    };
                    let result=tokio::select!{result=task=>result,_=stop.changed()=>Ok(())};(id,result)
                });
            }
            _=heartbeat.tick()=>send_control(&mut writer,PING,[0;16]).await?,
            result = jobs.join_next(), if !jobs.is_empty() => {
                let (id, result) = result.ok_or(Error::Closed)?.map_err(|_| Error::Closed)?;
                active.remove(&id);
                if matches!(result, Err(Error::IdentityRejected | Error::Invalid)) {
                    return result;
                }
                if result.is_err() {
                    send_control(&mut writer, OPEN_FAILED, id).await?;
                }
            },
            _=stop.changed()=>return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
