//! Authenticated owner IPC. Loopback by itself is never treated as authority.
use super::{PrivateFile, VERSION, invalid};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use usb_most_connection::{config::Endpoint, pairing::Store};
use zeroize::{Zeroize, Zeroizing};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub version: String,
    pub stack: bool,
    pub connected: bool,
    pub active: usize,
    pub peer: Option<String>,
    pub config_hash: String,
}
pub struct State {
    pub snapshot: Status,
    paused_until: Option<std::time::Instant>,
}
pub type Shared = Arc<Mutex<State>>;
impl State {
    pub fn reserve_import(&mut self) -> bool {
        if self
            .paused_until
            .is_some_and(|until| until > std::time::Instant::now())
        {
            return false;
        }
        self.paused_until = None;
        self.snapshot.active += 1;
        true
    }
    fn pause(&mut self, owner: Option<&str>, allow_idle: bool) -> bool {
        let same = owner.is_some() && owner == self.snapshot.peer.as_deref();
        if !same && (self.snapshot.active > 0 || self.snapshot.connected && !allow_idle) {
            return false;
        }
        self.paused_until = Some(std::time::Instant::now() + Duration::from_secs(120));
        true
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Health,
    Peers,
    Revoke {
        id: [u8; 16],
    },
    Pause {
        owner: Option<String>,
        allow_idle: bool,
    },
    Pair {
        endpoint: Endpoint,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Health { status: Status },
    Pair { code: String },
    Paused,
    Peers { peers: Vec<([u8; 16], String)> },
    Revoked,
    Denied,
}
impl Drop for Response {
    fn drop(&mut self) {
        if let Self::Pair { code } = self {
            code.zeroize();
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Address {
    port: u16,
    proof: String,
}
impl Drop for Address {
    fn drop(&mut self) {
        self.proof.zeroize();
    }
}
pub struct Pairing {
    pub store: Arc<Store>,
    pub pin: String,
    pub port: u16,
}
pub async fn start(
    root: &Path,
    status: Shared,
    pairing: Option<Pairing>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> io::Result<tokio::task::JoinHandle<io::Result<()>>> {
    let listener = super::network::loopback_listener()?;
    let proof = usb_most_connection::security::random::<32>().map_err(|_| invalid())?;
    let hint = Address {
        port: listener.local_addr()?.port(),
        proof: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(proof),
    };
    PrivateFile::new(root.join("admin.json"), 4096)?.replace(&Zeroizing::new(
        serde_json::to_vec(&hint).map_err(|_| invalid())?,
    ))?;
    Ok(tokio::spawn(serve(listener, proof, status, pairing, stop)))
}
async fn serve(
    listener: tokio::net::TcpListener,
    proof: [u8; 32],
    status: Shared,
    pairing: Option<Pairing>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> io::Result<()> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::timeout,
    };
    let slots = Arc::new(tokio::sync::Semaphore::new(4));
    let mut jobs = tokio::task::JoinSet::new();
    let proof = Arc::new(Zeroizing::new(proof));
    let pairing = Arc::new(pairing);
    loop {
        tokio::select! {
            result=listener.accept()=>{
                let(mut socket,_)=result?;let Ok(permit)=slots.clone().try_acquire_owned()else{continue;};
                let(proof,status,pairing)=(proof.clone(),status.clone(),pairing.clone());
                jobs.spawn(async move {
                    let _permit=permit;
                    let _=timeout(Duration::from_secs(10),async {
                        usb_most_connection::gateway::server(&mut socket,&proof).await?;
                        let len=socket.read_u16().await?;if len>1024{return Err(invalid());}
                        let mut bytes=vec![0;usize::from(len)];socket.read_exact(&mut bytes).await?;
                        let request:Request=serde_json::from_slice(&bytes).map_err(|_|invalid())?;
                        let response=match request {
                            Request::Health=>Response::Health{status:status.lock().map_err(|_|invalid())?.snapshot.clone()},
                            Request::Peers => match pairing.as_ref().as_ref() {
                                Some(context) => Response::Peers { peers: context.store.peers().map_err(|_|invalid())? },
                                None => Response::Denied,
                            },
                            Request::Revoke{id} => match pairing.as_ref().as_ref() {
                                Some(context) if context.store.revoke(id).is_ok() => Response::Revoked,
                                _ => Response::Denied,
                            },
                            Request::Pause{owner,allow_idle}=>{
                            let accepted=status.lock().map_err(|_|invalid())?.pause(owner.as_deref(),allow_idle);
                            if accepted {Response::Paused}else{Response::Denied}
                        },
                        Request::Pair{endpoint}=>{
                                let context=pairing.as_ref().as_ref().ok_or_else(invalid)?;
                                if endpoint.port!=context.port{return Err(invalid());}
                                let code=context.store.challenge(endpoint,context.pin.clone(),Duration::from_secs(300)).map_err(|_|invalid())?.encode().map_err(|_|invalid())?;
                                Response::Pair{code:code.to_string()}
                            }
                        };
                        let bytes=Zeroizing::new(serde_json::to_vec(&response).map_err(|_|invalid())?);
                        if bytes.len()>4096{return Err(invalid());}
                        socket.write_u16(bytes.len() as u16).await?;socket.write_all(&bytes).await
                    }).await;
                });
            },
            _=jobs.join_next(),if !jobs.is_empty()=>{},
            _=stop.changed()=>return Ok(()),
        }
    }
}
pub fn request(root: &Path, request: &Request) -> io::Result<Response> {
    let bytes = PrivateFile::new(root.join("admin.json"), 4096)?
        .read()?
        .ok_or_else(invalid)?;
    let address: Address = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if address.port == 0 {
        return Err(invalid());
    }
    let proof = usb_most_connection::pairing::token(&address.proof).map_err(|_| invalid())?;
    let mut socket = std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], address.port).into(),
        Duration::from_secs(2),
    )?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    usb_most_connection::gateway::client_sync(&mut socket, &proof)?;
    let bytes = serde_json::to_vec(request).map_err(|_| invalid())?;
    if bytes.len() > 1024 {
        return Err(invalid());
    }
    socket.write_all(&(bytes.len() as u16).to_be_bytes())?;
    socket.write_all(&bytes)?;
    let mut len = [0; 2];
    socket.read_exact(&mut len)?;
    let len = usize::from(u16::from_be_bytes(len));
    if len > 4096 {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(vec![0; len]);
    socket.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}
pub fn initial(peer: Option<String>, config_hash: String) -> Shared {
    Arc::new(Mutex::new(State {
        paused_until: None,
        snapshot: Status {
            version: VERSION.into(),
            stack: false,
            connected: false,
            active: 0,
            peer,
            config_hash,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn install_pause_and_import_reservation_have_no_foreign_takeover_gap() {
        let shared = initial(Some("owner".into()), "hash".into());
        let mut state = shared.lock().unwrap();
        assert!(state.reserve_import());
        assert!(!state.pause(Some("other"), false));
        state.snapshot.active = 0;
        state.snapshot.connected = false;
        assert!(state.pause(Some("other"), false));
        assert!(!state.reserve_import());
        state.paused_until = Some(std::time::Instant::now() - Duration::from_secs(1));
        assert!(state.reserve_import());
        state.snapshot.connected = true;
        state.snapshot.active = 0;
        assert!(!state.pause(Some("other"), false));
        assert!(state.pause(Some("other"), true));
        state.snapshot.active = 1;
        assert!(!state.pause(Some("other"), true));
        assert!(state.pause(Some("owner"), false));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_owner_ipc_authenticates_before_metadata_and_foreign_pause() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let shared = initial(Some("owner".into()), "hash".into());
        shared.lock().unwrap().snapshot.connected = true;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = start(dir.path(), shared.clone(), None, rx).await.unwrap();
        let root = dir.path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            let response = request(&root, &Request::Health).unwrap();
            assert!(
                matches!(&response,Response::Health{status}if status.peer.as_deref()==Some("owner"))
            );
            assert!(matches!(
                request(
                    &root,
                    &Request::Pause {
                        owner: Some("other".into()),
                        allow_idle: false
                    }
                )
                .unwrap(),
                Response::Denied
            ));
            assert!(matches!(
                request(
                    &root,
                    &Request::Pause {
                        owner: Some("owner".into()),
                        allow_idle: false
                    }
                )
                .unwrap(),
                Response::Paused
            ));
            let file = PrivateFile::new(root.join("admin.json"), 4096).unwrap();
            let mut hint: Address = serde_json::from_slice(&file.read().unwrap().unwrap()).unwrap();
            hint.proof = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([99; 32]);
            file.replace(&serde_json::to_vec(&hint).unwrap()).unwrap();
            assert!(request(&root, &Request::Health).is_err());
        })
        .await
        .unwrap();
        assert!(!shared.lock().unwrap().reserve_import());
        stop.send_replace(true);
        task.await.unwrap().unwrap();
    }
}
