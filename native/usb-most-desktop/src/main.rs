//! Product receiver. This binary never opens/mounts/formats USB filesystems.
#![forbid(unsafe_code)]
mod admin;
mod deployment;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(feature = "local-ui")]
mod local_ui;
mod managed_runtime;
mod network;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "windows")]
use windows as backend;
mod setup;
#[cfg(windows)]
mod setup_windows;
#[cfg(all(windows, feature = "bundled-driver"))]
mod windows_stack;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use usb_most_connection::{
    config::Endpoint,
    direct::{self, GatewayConnector},
    pairing::{PairCode, Store},
    security::{self, Identity},
    vault::PrivateFile,
};
use usb_most_protocol::agent::{self, Connector};
use zeroize::{Zeroize, Zeroizing};
use {std::sync::Mutex, usb_most_protocol::agent::Policy};
const VERSION: &str = env!("CARGO_PKG_VERSION");
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
#[derive(Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
enum Config {
    Ssh {
        port: u16,
        proof: String,
        peer: String,
    },
    Direct {
        port: u16,
    },
}
impl Drop for Config {
    fn drop(&mut self) {
        if let Self::Ssh { proof, .. } = self {
            proof.zeroize();
        }
    }
}
impl Config {
    fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        let config: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let port = match &config {
            Self::Ssh { port, proof, peer } => {
                if peer.len() != 32 || !peer.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(invalid());
                }
                let bytes = usb_most_connection::pairing::token(proof).map_err(|_| invalid())?;
                if bytes.as_slice() == [0; 32] {
                    return Err(invalid());
                }
                *port
            }
            Self::Direct { port } => *port,
        };
        if port < 1024 {
            return Err(invalid());
        }
        Ok(config)
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn state_root() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Ok(PathBuf::from("/var/lib/usb-most"))
    }
    #[cfg(target_os = "windows")]
    {
        usb_most_platform::windows::state_root()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        compile_error!("USB Most supports Linux and Windows receivers only");
    }
}
fn stdin_bounded(max: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut b = Zeroizing::new(Vec::new());
    io::stdin().take(max as u64 + 1).read_to_end(&mut b)?;
    if b.len() > max {
        return Err(invalid());
    }
    Ok(b)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SerializedIdentity {
    certificate: String,
    private: String,
}
impl Drop for SerializedIdentity {
    fn drop(&mut self) {
        self.private.zeroize();
    }
}
fn identity(root: &Path) -> io::Result<Identity> {
    let file = PrivateFile::new(root.join("identity.json"), 32768)?;
    if let Some(bytes) = file.read()? {
        let stored: SerializedIdentity = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        return Identity::from_parts(
            B64.decode(&stored.certificate).map_err(|_| invalid())?,
            B64.decode(&stored.private).map_err(|_| invalid())?,
        )
        .map_err(|_| invalid());
    }
    let identity = Identity::generate().map_err(|_| invalid())?;
    let stored = SerializedIdentity {
        certificate: B64.encode(identity.certificate()),
        private: B64.encode(identity.private_key().as_slice()),
    };
    file.replace(Zeroizing::new(serde_json::to_vec(&stored).map_err(|_| invalid())?).as_slice())?;
    Ok(identity)
}
async fn request_pair(root: &Path, host: String) -> io::Result<Zeroizing<String>> {
    let config = PrivateFile::new(root.join("receiver.json"), 4096)?
        .read()?
        .ok_or_else(invalid)?;
    let Config::Direct { port } = Config::parse(&config)? else {
        return Err(invalid());
    };
    let endpoint = Endpoint { host, port };
    endpoint.validate().map_err(|_| invalid())?;
    let root = root.to_path_buf();
    let reply = tokio::task::spawn_blocking(move || {
        admin::request(&root, &admin::Request::Pair { endpoint })
    })
    .await
    .map_err(|_| invalid())??;
    if let admin::Response::Pair { code } = &reply {
        PairCode::decode(code).map_err(|_| invalid())?;
        Ok(Zeroizing::new(code.clone()))
    } else {
        Err(invalid())
    }
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap()
}
fn digest_hex(bytes: &[u8]) -> String {
    digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
fn import_loop(
    remote: Arc<dyn Connector>,
    halt: Arc<AtomicBool>,
    root: PathBuf,
    expected: [u8; 32],
    status: admin::Shared,
) {
    let policy = Arc::new(Mutex::new(Policy::default()));
    let mut active: BTreeMap<String, managed_runtime::Import<backend::Attached>> = BTreeMap::new();
    let mut retry: BTreeMap<String, (u32, std::time::Instant)> = BTreeMap::new();
    let mut management_seen = false;
    while !halt.load(Ordering::Acquire) {
        active.retain(|_, a| !a.finished());
        let devices = agent::devices_with(remote.as_ref());
        if let Ok(mut current) = status.lock() {
            current.snapshot.connected = devices.is_ok();
            {
                current.snapshot.stack = backend::available();
                current.snapshot.active = managed_runtime::Permit::active();
            }
        }
        let activated = PrivateFile::new(root.join("active"), 32)
            .and_then(|file| file.read())
            .is_ok_and(|value| value.is_some_and(|bytes| bytes.as_slice() == expected));
        if !activated {
            active.clear();
        }
        if let Ok(devices) = devices {
            if !activated {
                for _ in 0..10 {
                    if halt.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                continue;
            }
            // This optional read-only negotiation precedes every new claim.
            // A legacy peer can reject it on the SAME authenticated transport.
            // Once management was observed, transient errors never downgrade
            // this receiver lifetime to untracked imports.
            let capabilities = agent::management_capabilities(remote.as_ref())
                .ok()
                .flatten();
            let managed =
                capabilities.filter(|caps| caps.features & usb_most_protocol::managed::EJECT != 0);
            management_seen |= managed.is_some();
            let mut selected = Vec::new();
            for (d, _) in devices {
                let name = &d[256..288];
                let end = name.iter().position(|b| *b == 0).unwrap_or(0);
                let Ok(id) = std::str::from_utf8(&name[..end]) else {
                    continue;
                };
                if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
                    selected.push(id.to_owned());
                }
            }
            // Stop only our own imported sessions, never other VHCI devices.
            active.retain(|id, import| {
                import.retain(selected.contains(id), capabilities.map(|c| c.session))
            });
            retry.retain(|id, _| selected.contains(id));
            for id in selected {
                if halt.load(Ordering::Acquire) {
                    break;
                }
                if active.contains_key(&id) {
                    continue;
                }
                if management_seen && managed.is_none() {
                    continue;
                }
                if managed.is_some_and(|caps| active.len() >= caps.max_imports as usize) {
                    continue;
                }
                if retry
                    .get(&id)
                    .is_some_and(|(_, at)| *at > std::time::Instant::now())
                {
                    continue;
                }
                // The process-wide permit includes workers left running after
                // a profile withdrawal. Reserve under the administrative gate
                // so installation/pairing cannot observe a false idle interval.
                let permit = status.lock().ok().and_then(|mut state| {
                    let permit = managed_runtime::Permit::acquire().ok()?;
                    if !state.reserve_import() {
                        return None;
                    }
                    state.snapshot.active = managed_runtime::Permit::active();
                    Some(permit)
                });
                let Some(permit) = permit else {
                    continue;
                };
                let result = (|| {
                    let attachment = match managed {
                        Some(caps) => backend::attach_with_management(
                            &id,
                            remote.clone(),
                            policy.clone(),
                            Some(caps),
                        )?,
                        None => backend::attach(&id, remote.clone(), policy.clone())?,
                    };
                    let key = attachment.import_key();
                    let import =
                        managed_runtime::Import::new(attachment, permit, key, remote.clone())?;
                    active.insert(id.clone(), import);
                    Ok::<_, io::Error>(())
                })();
                if let Ok(mut current) = status.lock() {
                    current.snapshot.active = managed_runtime::Permit::active();
                }
                match result {
                    Ok(()) => {
                        retry.remove(&id);
                    }
                    Err(_) => {
                        let tries = retry.get(&id).map_or(0, |(n, _)| *n);
                        let seconds = [5, 15, 30, 60, 120, 300][(tries as usize).min(5)];
                        retry.insert(
                            id,
                            (
                                tries.saturating_add(1),
                                std::time::Instant::now() + Duration::from_secs(seconds),
                            ),
                        );
                    }
                }
            }
        }
        for _ in 0..20 {
            if halt.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}
async fn serve(
    root: PathBuf,
    #[cfg(windows)] service: Option<usb_most_platform::windows::service::Control>,
) -> io::Result<()> {
    let bytes = PrivateFile::new(root.join("receiver.json"), 4096)?
        .read()?
        .ok_or_else(invalid)?;
    let config = Config::parse(&bytes)?;
    let expected = digest(&bytes);
    let peer = match &config {
        Config::Ssh { peer, .. } => Some(peer.clone()),
        _ => None,
    };
    let status = admin::initial(peer, digest_hex(&bytes));
    let mut pairing = None;
    let mut _announcement = None;
    let halt = Arc::new(AtomicBool::new(false));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    let remote: Arc<dyn Connector> = match &config {
        Config::Ssh { port, proof, .. } => Arc::new(
            GatewayConnector::new(
                SocketAddr::from(([127, 0, 0, 1], *port)),
                *usb_most_connection::pairing::token(proof).map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?,
        ),
        Config::Direct { port } => {
            let identity = identity(&root)?;
            let store = Store::open(PrivateFile::new(root.join("pairs.json"), 16384)?)
                .map_err(|_| invalid())?;
            let (listener, ipv6) = network::listener(*port)?;
            let public_id = &digest_hex(identity.certificate())[..16];
            // LAN discovery is advisory. An occupied multicast port/firewall
            // cannot disable an otherwise usable pinned direct connection.
            _announcement = network::Announcement::start(*port, public_id, ipv6).ok();
            let gateway = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = gateway.local_addr()?;
            let proof = security::random::<32>().map_err(|_| invalid())?;
            pairing = Some(admin::Pairing {
                store: store.clone(),
                pin: identity.pin(),
                port: *port,
            });
            let config = identity.server_config().map_err(|_| invalid())?;
            let server_rx = rx.clone();
            tasks.spawn(async move {
                direct::run_server(listener, gateway, config, store, proof, server_rx)
                    .await
                    .map_err(|_| invalid())
            });
            Arc::new(GatewayConnector::new(address, proof).map_err(|_| invalid())?)
        }
    };
    let admin_job = admin::start(&root, status.clone(), pairing, rx.clone()).await?;
    tasks.spawn(async move { admin_job.await.map_err(|_| invalid())? });
    let worker_halt = halt.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        import_loop(remote, worker_halt, root, expected, status)
    });
    #[cfg(target_os = "linux")]
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(windows)]
    let ready_failed = service
        .as_ref()
        .is_some_and(|control| control.ready().is_err());
    #[cfg(not(windows))]
    let ready_failed = false;
    if ready_failed {
        halt.store(true, Ordering::Release);
    }
    let shutdown = async {
        #[cfg(target_os = "linux")]
        tokio::select! {result=tokio::signal::ctrl_c()=>result,_=termination.recv()=>Ok(())}
        #[cfg(target_os = "windows")]
        {
            if let Some(control) = &service {
                while !control.stopping() && !halt.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(())
            } else {
                tokio::signal::ctrl_c().await
            }
        }
    };
    let failure = tokio::select! {
        result=shutdown=>result.is_err(),
        _=tasks.join_next(),if !tasks.is_empty()=>true,
        _=&mut worker=>true,
    };
    halt.store(true, Ordering::Release);
    stop.send_replace(true);
    if !worker.is_finished() {
        let _ = worker.await;
    }
    managed_runtime::drain().await;
    while let Some(result) = tasks.join_next().await {
        if !matches!(result, Ok(Ok(()))) {
            return Err(invalid());
        }
    }
    if failure || ready_failed {
        return Err(invalid());
    }
    Ok(())
}
fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
}
fn main() {
    #[cfg(windows)]
    if std::env::args().skip(1).collect::<Vec<_>>() == ["--service"] {
        if let Err(error) = usb_most_platform::windows::service::dispatch(|control| {
            runtime()?.block_on(serve(state_root()?, Some(control)))
        }) {
            eprintln!(
                "USB Most: {:?} os={}",
                error.kind(),
                error.raw_os_error().unwrap_or(0)
            );
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = runtime().and_then(|rt| rt.block_on(run())) {
        eprintln!(
            "USB Most: {:?} os={}",
            error.kind(),
            error.raw_os_error().unwrap_or(0)
        );
        std::process::exit(if error.raw_os_error() == Some(3010) {
            76
        } else {
            1
        });
    }
}
async fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        #[cfg(feature = "local-ui")]
        [] => local_ui::launch(false).await,
        #[cfg(feature = "local-ui")]
        [arg] if arg == "--setup" => local_ui::launch(false).await,
        #[cfg(feature = "local-ui")]
        [arg] if arg == "--setup-elevated" => local_ui::launch(true).await,
        [arg] if arg == "--version" => {
            println!("{VERSION}");
            Ok(())
        }
        [arg] if arg == "--health" => {
            let reply = admin::request(&state_root()?, &admin::Request::Health)?;
            if !matches!(reply, admin::Response::Health { .. }) {
                return Err(invalid());
            }
            println!("{}", serde_json::to_string(&reply).map_err(|_| invalid())?);
            Ok(())
        }
        [arg] if arg == "--install-stdin" => {
            let bytes = stdin_bounded(4096)?;
            let config = Config::parse(&bytes)
                .inspect_err(|_| eprintln!("USB Most setup: configuration"))?;
            setup::install(&config)
        }
        [arg] if arg == "--serve" => {
            #[cfg(windows)]
            {
                serve(state_root()?, None).await
            }
            #[cfg(not(windows))]
            {
                serve(state_root()?).await
            }
        }
        [arg, host] if arg == "--pair" => {
            let code = request_pair(&state_root()?, host.clone()).await?;
            println!("{}", code.as_str());
            Ok(())
        }
        _ => {
            eprintln!("USB Most {VERSION}: --install-stdin | --serve | --pair HOST | --version");
            Err(io::ErrorKind::InvalidInput.into())
        }
    }
}
