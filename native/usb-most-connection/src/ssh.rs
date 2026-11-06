//! Embedded SSH client. Credentials never enter configuration, command lines or logs.
pub use crate::bridge::LocalBridge;
use crate::config::{Profile, decode_pin, encode_pin};
use russh::{
    Channel, ChannelMsg, ChannelOpenFailure, client,
    client::{ChannelOpenHandle, Msg},
};
use std::{
    net::Shutdown,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, copy_bidirectional},
    net::TcpStream,
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
    time::timeout,
};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidConfig,
    Network,
    Timeout,
    HostKey,
    Authentication,
    InvalidKey,
    Forwarding,
    RemoteCommand,
    OutputLimit,
    Closed,
}
pub enum Credential {
    Password(Zeroizing<String>),
    Key {
        private: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
}
pub struct AppKey {
    pub private: Zeroizing<String>,
    pub public: String,
}
/// A separate client-side key for the product; its private part is never sent
/// to the target during enrollment and must be encrypted by the Android vault.
pub fn generate_app_key() -> Result<AppKey, Error> {
    let seed = Zeroizing::new(crate::security::random::<32>().map_err(|_| Error::InvalidKey)?);
    let pair = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let key = russh::keys::PrivateKey::from(pair);
    let private = key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .map_err(|_| Error::InvalidKey)?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|_| Error::InvalidKey)?;
    Ok(AppKey { private, public })
}
pub struct TrustRequest {
    pub pin: String,
    pub reply: oneshot::Sender<bool>,
}
struct Handler {
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
    refused: Arc<AtomicU8>,
    approval: mpsc::Sender<TrustRequest>,
    bridge: LocalBridge,
    proof: Arc<Zeroizing<[u8; 32]>>,
    port: Arc<AtomicU32>,
    slots: Arc<Semaphore>,
    stop: watch::Receiver<bool>,
    jobs: JoinSet<()>,
}
impl client::Handler for Handler {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // Certificates require a separately defined CA/principal policy. Never
        // treat an arbitrary presented certificate as a trusted bare host key.
        if key.certificate().is_some() {
            self.refused.store(1, Ordering::Release);
            return Ok(false);
        }
        let pin = match key.public_key().to_bytes() {
            Ok(bytes) => encode_pin(&bytes),
            Err(_) => return Ok(false),
        };
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(pin.clone());
        }
        if let Some(expected) = &self.expected {
            use subtle::ConstantTimeEq;
            let matches = match (decode_pin(expected), decode_pin(&pin)) {
                (Ok(a), Ok(b)) => bool::from(a.ct_eq(&b)),
                _ => false,
            };
            if !matches {
                self.refused.store(1, Ordering::Release);
            }
            return Ok(matches);
        }
        let (tx, rx) = oneshot::channel();
        if self
            .approval
            .try_send(TrustRequest { pin, reply: tx })
            .is_err()
        {
            self.refused.store(1, Ordering::Release);
            return Ok(false);
        }
        let accepted = matches!(timeout(Duration::from_secs(90), rx).await, Ok(Ok(true)));
        if !accepted {
            self.refused.store(1, Ordering::Release);
        }
        Ok(accepted)
    }
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        address: &str,
        port: u32,
        _origin: &str,
        _origin_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        while self.jobs.try_join_next().is_some() {}
        let permit = self.slots.clone().try_acquire_owned();
        if address != "127.0.0.1"
            || port == 0
            || port != self.port.load(Ordering::Acquire)
            || permit.is_err()
            || *self.stop.borrow()
        {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        let permit = permit.unwrap();
        reply.accept().await;
        let mut stop = self.stop.clone();
        let bridge = self.bridge.clone();
        let proof = self.proof.clone();
        self.jobs.spawn(async move {
            let _permit = permit;
            let mut stream = channel.into_stream();
            let establish = async {
                crate::gateway::server(&mut stream, &proof).await.ok()?;
                let local = bridge.connect().await.ok()?;
                stream.write_all(&[1]).await.ok()?;
                Some(local)
            };
            let mut local = tokio::select! {
                result=timeout(Duration::from_secs(10),establish)=>{
                    match result { Ok(Some(local))=>local, _=>return }
                },
                _=stop.changed()=>return,
            };
            tokio::select! {
                _=copy_bidirectional(&mut stream,&mut local)=>{},
                _=stop.changed()=>{},
            }
        });
        Ok(())
    }
}

struct ShutdownSocket(std::net::TcpStream);
impl Drop for ShutdownSocket {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}
pub struct SshClient {
    session: client::Handle<Handler>,
    socket: ShutdownSocket,
    stop: watch::Sender<bool>,
    port: Arc<AtomicU32>,
    proof: Arc<Zeroizing<[u8; 32]>>,
    pub host_pin: String,
}
impl Drop for SshClient {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        let _ = self.socket.0.shutdown(Shutdown::Both);
    }
}
pub struct Output {
    pub exit: u32,
    pub stdout: Zeroizing<Vec<u8>>,
    pub stderr: Zeroizing<Vec<u8>>,
}
impl SshClient {
    pub async fn connect(
        profile: &Profile,
        credential: Credential,
        bridge: LocalBridge,
        approval: mpsc::Sender<TrustRequest>,
    ) -> Result<Self, Error> {
        profile.validate().map_err(|_| Error::InvalidConfig)?;
        let Profile::Ssh {
            endpoint,
            user,
            host_key,
            ..
        } = profile
        else {
            return Err(Error::InvalidConfig);
        };
        let socket = timeout(
            Duration::from_secs(15),
            TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| Error::Network)?;
        socket.set_nodelay(true).map_err(|_| Error::Network)?;
        let stdsocket = socket.into_std().map_err(|_| Error::Network)?;
        let guard = ShutdownSocket(stdsocket.try_clone().map_err(|_| Error::Network)?);
        let socket = TcpStream::from_std(stdsocket).map_err(|_| Error::Network)?;
        let observed = Arc::new(Mutex::new(None));
        let refused = Arc::new(AtomicU8::new(0));
        let port = Arc::new(AtomicU32::new(0));
        let (stop, rx) = watch::channel(false);
        let proof = Arc::new(Zeroizing::new(
            crate::security::random::<32>().map_err(|_| Error::Network)?,
        ));
        let handler = Handler {
            expected: host_key.clone(),
            observed: observed.clone(),
            refused: refused.clone(),
            approval,
            bridge,
            proof: proof.clone(),
            port: port.clone(),
            slots: Arc::new(Semaphore::new(8)),
            stop: rx,
            jobs: JoinSet::new(),
        };
        let mut config = client::Config {
            window_size: 128 * 1024,
            maximum_packet_size: 32768,
            channel_buffer_size: 8,
            keepalive_interval: Some(Duration::from_secs(15)),
            keepalive_max: 3,
            nodelay: true,
            ..Default::default()
        };
        // The upstream compatibility list includes legacy ssh-rsa/SHA1. Do not
        // negotiate it even though RSA/SHA2 keys remain supported.
        config
            .preferred
            .key
            .to_mut()
            .retain(|a| !matches!(a, russh::keys::Algorithm::Rsa { hash: None }));
        let mut session = timeout(
            Duration::from_secs(if host_key.is_some() { 20 } else { 120 }),
            client::connect_stream(Arc::new(config), socket, handler),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|_| {
            if refused.load(Ordering::Acquire) != 0 {
                Error::HostKey
            } else {
                Error::Network
            }
        })?;
        let auth = timeout(Duration::from_secs(45), async {
            match credential {
                Credential::Password(password) => {
                    if password.len() > 4096 {
                        return Err(Error::InvalidConfig);
                    }
                    session
                        .authenticate_password(user, password.to_string())
                        .await
                        .map_err(|_| Error::Authentication)
                }
                Credential::Key {
                    private,
                    passphrase,
                } => {
                    if private.len() > 64 * 1024
                        || passphrase.as_ref().is_some_and(|p| p.len() > 4096)
                    {
                        return Err(Error::InvalidKey);
                    }
                    crate::key_guard::validate(&private).map_err(|_| Error::InvalidKey)?;
                    let key = russh::keys::decode_secret_key(
                        private.trim_start_matches('\u{feff}').trim(),
                        passphrase.as_deref().map(|v| v.as_str()),
                    )
                    .map_err(|_| Error::InvalidKey)?;
                    let hash = session
                        .best_supported_rsa_hash()
                        .await
                        .map_err(|_| Error::Authentication)?
                        .flatten();
                    if matches!(key.algorithm(), russh::keys::Algorithm::Rsa { .. })
                        && hash.is_none()
                    {
                        return Err(Error::Authentication);
                    }
                    session
                        .authenticate_publickey(
                            user,
                            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                        )
                        .await
                        .map_err(|_| Error::Authentication)
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)??;
        if !auth.success() {
            return Err(Error::Authentication);
        }
        let host_pin = observed
            .lock()
            .ok()
            .and_then(|x| x.clone())
            .ok_or(Error::HostKey)?;
        Ok(Self {
            session,
            socket: guard,
            stop,
            port,
            proof,
            host_pin,
        })
    }
    pub fn is_closed(&self) -> bool {
        self.session.is_closed()
    }
    /// Transferred to our receiver only over the authenticated SSH channel.
    /// Other local processes cannot use the loopback reverse port without it.
    pub fn gateway_proof(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(**self.proof)
    }
    pub async fn forward(&self, requested: u16) -> Result<u16, Error> {
        if requested != 0 && requested < 1024 {
            return Err(Error::InvalidConfig);
        }
        if self
            .port
            .compare_exchange(0, u32::MAX, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::Forwarding);
        }
        let actual = match timeout(
            Duration::from_secs(15),
            self.session
                .tcpip_forward("127.0.0.1", u32::from(requested)),
        )
        .await
        {
            Ok(Ok(p)) if requested == 0 || p == 0 || p == u32::from(requested) => p,
            _ => {
                let _ = self.socket.0.shutdown(Shutdown::Both);
                return Err(Error::Forwarding);
            }
        };
        let actual = if requested == 0 {
            u16::try_from(actual).map_err(|_| Error::Forwarding)?
        } else {
            requested
        };
        if actual < 1024 {
            return Err(Error::Forwarding);
        }
        self.port.store(u32::from(actual), Ordering::Release);
        Ok(actual)
    }
    /// Commands are fixed application literals. All variable input uses stdin.
    pub async fn exec(
        &self,
        command: &'static str,
        input: Zeroizing<Vec<u8>>,
        deadline: Duration,
    ) -> Result<Output, Error> {
        if input.len() > 32 * 1024 * 1024
            || deadline > Duration::from_secs(1800)
            || deadline.is_zero()
        {
            return Err(Error::InvalidConfig);
        }
        let channel = timeout(Duration::from_secs(10), self.session.channel_open_session())
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::RemoteCommand)?;
        timeout(Duration::from_secs(10), channel.exec(true, command))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::RemoteCommand)?;
        let (mut reader, writer) = channel.split();
        let writer = Arc::new(writer);
        let sender = writer.clone();
        let task = tokio::spawn(async move {
            sender.data(input.as_slice()).await?;
            sender.eof().await
        });
        struct Abort(tokio::task::AbortHandle);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _abort = Abort(task.abort_handle());
        let received = timeout(deadline, async {
            let mut stdout = Zeroizing::new(Vec::new());
            let mut stderr = Zeroizing::new(Vec::new());
            let mut exit = None;
            while let Some(message) = reader.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        if stdout.len() + data.len() > 16384 {
                            return Err(Error::OutputLimit);
                        }
                        stdout.extend_from_slice(&data);
                    }
                    ChannelMsg::ExtendedData { data, .. } => {
                        if stderr.len() + data.len() > 16384 {
                            return Err(Error::OutputLimit);
                        }
                        stderr.extend_from_slice(&data);
                    }
                    ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                    ChannelMsg::Close => break,
                    ChannelMsg::Failure => return Err(Error::RemoteCommand),
                    _ => {}
                }
            }
            Ok(Output {
                exit: exit.ok_or(Error::RemoteCommand)?,
                stdout,
                stderr,
            })
        })
        .await
        .map_err(|_| Error::Timeout)
        .and_then(|v| v);
        if received.is_err() {
            task.abort();
            let _ = self.socket.0.shutdown(Shutdown::Both);
        }
        let sent = timeout(Duration::from_secs(2), task).await;
        let _ = timeout(Duration::from_secs(2), writer.close()).await;
        if received.is_ok() && !matches!(sent, Ok(Ok(Ok(())))) {
            return Err(Error::RemoteCommand);
        }
        received
    }
}

#[cfg(test)]
mod tests;
