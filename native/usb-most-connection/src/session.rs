//! Application connection lifecycle. JNI/UI poll bounded events; no Java callback
//! runs on a network thread and cancellation drops every owned connection.
use crate::{
    bridge::LocalBridge,
    config::Profile,
    ssh::{Credential, SshClient, TrustRequest},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::Read,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::{Zeroize, Zeroizing};
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("usb-most-connect")
        .enable_all()
        .build()
        .expect("connection runtime")
});
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Secret {
    pub password: Option<String>,
    pub sudo_password: Option<String>,
    pub private_key: Option<String>,
    pub passphrase: Option<String>,
    pub token: Option<String>,
    pub pair_code: Option<String>,
    pub pair_nonce: Option<String>,
    pub client_id: Option<String>,
}
impl Drop for Secret {
    fn drop(&mut self) {
        for value in [
            &mut self.password,
            &mut self.sudo_password,
            &mut self.private_key,
            &mut self.passphrase,
            &mut self.token,
            &mut self.pair_code,
            &mut self.pair_nonce,
            &mut self.client_id,
        ]
        .into_iter()
        .flatten()
        {
            value.zeroize();
        }
    }
}
impl Secret {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 131072 {
            return Err("invalid_credentials");
        }
        let s: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_credentials")?;
        s.validate()?;
        Ok(s)
    }
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self
            .sudo_password
            .as_ref()
            .is_some_and(|v| v.len() > 4096 || v.contains(['\n', '\r', '\0']))
            || self.password.as_ref().is_some_and(|v| v.len() > 4096)
            || self.private_key.as_ref().is_some_and(|v| v.len() > 65536)
            || self.passphrase.as_ref().is_some_and(|v| v.len() > 4096)
            || self.token.as_ref().is_some_and(|v| v.len() > 64)
            || self.pair_code.as_ref().is_some_and(|v| v.len() > 2048)
            || self.pair_nonce.as_ref().is_some_and(|v| v.len() > 64)
            || self.client_id.as_ref().is_some_and(|v| v.len() > 32)
        {
            return Err("invalid_credentials");
        }
        Ok(())
    }
    fn credential(&self) -> Result<Credential, &'static str> {
        if let Some(private) = &self.private_key {
            Ok(Credential::Key {
                private: Zeroizing::new(private.clone()),
                passphrase: self.passphrase.clone().map(Zeroizing::new),
            })
        } else if let Some(password) = &self.password {
            Ok(Credential::Password(Zeroizing::new(password.clone())))
        } else {
            Err("credentials_required")
        }
    }
}
struct Pending {
    pin: String,
    reply: oneshot::Sender<bool>,
}
struct Save {
    id: u64,
    reply: oneshot::Sender<bool>,
}
struct State {
    events: VecDeque<Zeroizing<String>>,
    pending: Option<Pending>,
    save: Option<Save>,
    next_save: u64,
    ended: bool,
    exposed: bool,
}
pub struct Session {
    state: Mutex<State>,
    stop: watch::Sender<bool>,
}
impl Session {
    pub fn start(
        profile: Profile,
        secret: Secret,
        bridge: LocalBridge,
        artifacts: PathBuf,
    ) -> Arc<Self> {
        let (stop, stopped) = watch::channel(false);
        let session = Arc::new(Self {
            state: Mutex::new(State {
                events: VecDeque::new(),
                pending: None,
                save: None,
                next_save: 1,
                ended: false,
                exposed: false,
            }),
            stop,
        });
        let runner = session.clone();
        let task =
            RUNTIME.spawn(async move { run(&runner, profile, secret, bridge, artifacts).await });
        session.monitor(stopped, task);
        session
    }
    fn monitor(
        self: &Arc<Self>,
        mut stopped: watch::Receiver<bool>,
        mut task: tokio::task::JoinHandle<Result<(), &'static str>>,
    ) {
        let owned = self.clone();
        RUNTIME.spawn(async move {
            let result = tokio::select! {
                result=&mut task=>result.unwrap_or(Err("internal_error")),
                _=stopped.changed()=>{
                    task.abort();
                    let _=task.await;
                    Ok(())
                },
            };
            if let Ok(mut state) = owned.state.lock() {
                state.pending = None;
                state.save = None;
                state.ended = true;
            }
            match result {
                Ok(()) => owned.event(serde_json::json!({"event":"closed"})),
                Err(code) => owned.event(serde_json::json!({"event":"error","code":code})),
            }
        });
    }
    fn event(&self, event: serde_json::Value) {
        if let Ok(mut s) = self.state.lock() {
            if s.events.len() == 32 {
                s.events.pop_front();
            }
            if let Ok(json) = serde_json::to_string(&event) {
                s.events.push_back(Zeroizing::new(json));
            }
        }
    }
    pub fn poll(&self) -> Option<Zeroizing<String>> {
        self.state.lock().ok()?.events.pop_front()
    }
    pub fn approve(&self, pin: &str, accept: bool) -> bool {
        let Ok(mut s) = self.state.lock() else {
            return false;
        };
        if s.pending.as_ref().is_none_or(|p| p.pin != pin) {
            return false;
        }
        s.pending
            .take()
            .is_some_and(|p| p.reply.send(accept).is_ok())
    }
    async fn persist(&self, mut event: serde_json::Value) -> Result<(), &'static str> {
        if *self.stop.borrow() {
            return Err("connection_closed");
        }
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut state = self.state.lock().map_err(|_| "storage_failed")?;
            let id = state.next_save;
            state.next_save = id
                .checked_add(1)
                .filter(|next| *next <= i64::MAX as u64)
                .ok_or("storage_failed")?;
            state.save = Some(Save { id, reply: tx });
            id
        };
        event["save_id"] = serde_json::json!(id);
        self.event(event);
        if matches!(
            tokio::time::timeout(Duration::from_secs(60), rx).await,
            Ok(Ok(true))
        ) {
            Ok(())
        } else {
            Err("storage_failed")
        }
    }
    pub fn saved(&self, id: u64, success: bool) -> bool {
        if *self.stop.borrow() {
            return false;
        }
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.save.as_ref().is_none_or(|save| save.id != id) {
            return false;
        }
        state
            .save
            .take()
            .is_some_and(|save| save.reply.send(success).is_ok())
    }
    pub fn stop(&self) {
        self.stop.send_replace(true);
    }
    pub fn ended(&self) -> bool {
        self.state.lock().map_or(true, |s| s.ended)
    }
}
async fn run(
    owner: &Arc<Session>,
    mut profile: Profile,
    mut secret: Secret,
    bridge: LocalBridge,
    artifacts: PathBuf,
) -> Result<(), &'static str> {
    let mut attempts = 0u32;
    let mut busy_until = None;
    loop {
        let started = std::time::Instant::now();
        let mut established = false;
        let result = run_once(
            owner,
            &mut profile,
            &mut secret,
            bridge.clone(),
            &artifacts,
            &mut established,
        )
        .await;
        let Err(code) = result else {
            return Ok(());
        };
        let busy_recovery = code == "computer_busy"
            && busy_until.is_some_and(|until| std::time::Instant::now() < until);
        if code == "computer_busy" {
            // TLS CONTROL was refused before any DATA stream could be opened.
            // The preceding exposed connection already crossed its storage
            // withdrawal barrier. Do not invent another exposed USB epoch.
            owner.state.lock().map_err(|_| "connection_closed")?.exposed = false;
        }
        // Trust/authentication/protocol/install failures need correction, never
        // an automatic retry of a password, pairing code or a USB OUT operation.
        if !busy_recovery && !matches!(code, "connection_lost" | "network_unavailable" | "timeout")
        {
            return Err(code);
        }
        // Android withdraws storage grants before acknowledging this barrier.
        // A stalled UI/storage adapter cannot reconnect with stale approvals.
        let exposed = owner.state.lock().map_err(|_| "connection_closed")?.exposed;
        if exposed {
            owner
                .persist(serde_json::json!({"event":"disconnected","code":code}))
                .await?;
            owner.state.lock().map_err(|_| "connection_closed")?.exposed = false;
        }
        if established {
            // A route loss may strand the receiver's previous CONTROL until
            // its 45-second read deadline. Permit a bounded grace period only
            // after our own established session and completed withdrawal.
            // Busy on first use remains terminal; no receiver takeover occurs.
            busy_until = Some(std::time::Instant::now() + Duration::from_secs(60));
        }
        if started.elapsed() >= Duration::from_secs(60) {
            attempts = 0;
        }
        let seconds = [3, 5, 10, 20, 40, 60][(attempts as usize).min(5)];
        attempts = attempts.saturating_add(1);
        let random = crate::security::random::<2>().map_err(|_| "network_unavailable")?;
        let mut delay = Duration::from_secs(seconds)
            + Duration::from_millis(u64::from(u16::from_le_bytes(random) % 1000));
        if busy_recovery {
            delay = delay.min(
                busy_until
                    .unwrap()
                    .saturating_duration_since(std::time::Instant::now()),
            );
            if delay.is_zero() {
                return Err("computer_busy");
            }
        }
        owner.event(
            serde_json::json!({"event":"state","code":"reconnecting","reason":code,"delay_ms":delay.as_millis()}),
        );
        tokio::time::sleep(delay).await;
    }
}
async fn run_once(
    owner: &Arc<Session>,
    profile: &mut Profile,
    secret: &mut Secret,
    bridge: LocalBridge,
    artifacts: &std::path::Path,
    established: &mut bool,
) -> Result<(), &'static str> {
    owner.event(serde_json::json!({"event":"state","code":"connecting"}));
    match profile {
        Profile::Ssh { .. } => {
            let (approval, mut questions) = mpsc::channel::<TrustRequest>(1);
            let weak = Arc::downgrade(owner);
            let task = tokio::spawn(async move {
                while let Some(request) = questions.recv().await {
                    let Some(owner) = weak.upgrade() else {
                        break;
                    };
                    if let Ok(mut state) = owner.state.lock() {
                        state.pending = Some(Pending {
                            pin: request.pin.clone(),
                            reply: request.reply,
                        });
                    }
                    owner.event(serde_json::json!({"event":"trust","pin":request.pin}));
                }
            });
            struct Abort(tokio::task::AbortHandle);
            impl Drop for Abort {
                fn drop(&mut self) {
                    self.0.abort();
                }
            }
            let _guard = Abort(task.abort_handle());
            let client = SshClient::connect(profile, secret.credential()?, bridge, approval)
                .await
                .map_err(ssh_error)?;
            owner
                .persist(serde_json::json!({"event":"trusted","pin":client.host_pin}))
                .await?;
            if let Profile::Ssh { host_key, .. } = profile {
                *host_key = Some(client.host_pin.clone());
            }
            let requested = if let Profile::Ssh { reverse_port, .. } = profile {
                *reverse_port
            } else {
                0
            };
            let port = client.forward(requested).await.map_err(ssh_error)?;
            owner.state.lock().map_err(|_| "connection_closed")?.exposed = true;
            let prepare = matches!(profile, Profile::Ssh { prepare: true, .. });
            let peer = secret.client_id.as_deref().unwrap_or("");
            if prepare && (peer.len() != 32 || !peer.bytes().all(|b| b.is_ascii_hexdigit())) {
                return Err("invalid_client_id");
            }
            if prepare {
                owner.event(serde_json::json!({"event":"state","code":"checking"}));
                let probe = crate::bootstrap::probe(&client)
                    .await
                    .map_err(|_| "probe_failed")?;
                probe.check_prerequisites()?;
                let file = match (probe.os, probe.arch) {
                    (crate::provision::Os::Linux, crate::provision::Arch::X86_64) => {
                        "usb-most-linux-x86_64"
                    }
                    (crate::provision::Os::Linux, crate::provision::Arch::Aarch64) => {
                        "usb-most-linux-aarch64"
                    }
                    (crate::provision::Os::Windows, crate::provision::Arch::X86_64) => {
                        "usb-most-windows-x86_64.exe"
                    }
                    (crate::provision::Os::Windows, crate::provision::Arch::Aarch64) => {
                        "usb-most-windows-aarch64.exe"
                    }
                };
                if probe.os == crate::provision::Os::Windows && !probe.elevated {
                    return Err("windows_administrator_required");
                }
                let (bytes, hash) = artifact(artifacts.to_path_buf(), file)?;
                let config=Zeroizing::new(serde_json::to_vec(&serde_json::json!({"transport":"ssh","port":port,"proof":B64.encode(client.gateway_proof().as_slice()),"peer":peer})).map_err(|_|"configuration_failed")?);
                owner.event(serde_json::json!({"event":"state","code":"preparing"}));
                match probe.os {
                    crate::provision::Os::Linux => {
                        crate::bootstrap::install_linux(
                            &client,
                            &probe,
                            &bytes,
                            &hash,
                            &config,
                            secret
                                .sudo_password
                                .as_deref()
                                .or(secret.password.as_deref()),
                        )
                        .await
                    }
                    crate::provision::Os::Windows => {
                        crate::bootstrap::install_windows(&client, &probe, &bytes, &hash, &config)
                            .await
                    }
                }
                .map_err(|error| match error {
                    crate::bootstrap::Error::RestartRequired => "restart_required",
                    _ => "preparation_failed",
                })?;
                let after = crate::bootstrap::probe(&client)
                    .await
                    .map_err(|_| "health_failed")?;
                if after.agent_version.as_deref() != Some(env!("CARGO_PKG_VERSION"))
                    || after.agent_sha256.as_deref() != Some(hash.as_str())
                    || !after.usb_stack
                    || !after.service_running
                {
                    return Err("health_failed");
                }
            }
            *established = true;
            owner.event(serde_json::json!({"event":"state","code":"connected","port":port,"receiver_verified":prepare}));
            while !client.is_closed() {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err("connection_lost")
        }
        Profile::Direct {
            endpoint,
            server_pin,
        } => {
            let token = if let Some(token) = &secret.token {
                crate::pairing::token(token).map_err(|_| "pairing_required")?
            } else {
                let code = crate::pairing::PairCode::decode(
                    secret.pair_code.as_deref().ok_or("pairing_required")?,
                )
                .map_err(|_| "invalid_pairing_code")?;
                if code.endpoint != *endpoint || code.server_pin != *server_pin {
                    return Err("invalid_pairing_code");
                }
                let bytes = B64
                    .decode(
                        secret
                            .pair_nonce
                            .as_deref()
                            .ok_or("invalid_pairing_nonce")?,
                    )
                    .map_err(|_| "invalid_pairing_nonce")?;
                let nonce: [u8; 16] = bytes.try_into().map_err(|_| "invalid_pairing_nonce")?;
                if nonce == [0; 16] {
                    return Err("invalid_pairing_nonce");
                }
                let token = crate::direct::pair(&code, nonce, "Android").await.map_err(
                    |error| match error {
                        crate::direct::Error::IdentityRejected => "host_key_rejected",
                        crate::direct::Error::Invalid => "protocol_error",
                        _ => "pairing_failed",
                    },
                )?;
                owner
                    .persist(
                        serde_json::json!({"event":"paired","token":B64.encode(token.as_slice())}),
                    )
                    .await?;
                secret.token = Some(B64.encode(token.as_slice()));
                for old in [&mut secret.pair_code, &mut secret.pair_nonce] {
                    if let Some(mut value) = old.take() {
                        value.zeroize();
                    }
                }
                token
            };
            owner.state.lock().map_err(|_| "connection_closed")?.exposed = true;
            let (tx, mut ready) = oneshot::channel();
            let (_stop, rx) = watch::channel(false);
            let client = crate::direct::run_client(
                endpoint.clone(),
                server_pin.clone(),
                token,
                bridge,
                rx,
                tx,
            );
            tokio::pin!(client);
            // Observe acceptance in this task: no detached notification can
            // outlive cancellation or be attributed to a later connection.
            let result = tokio::select! {
                biased;
                accepted = &mut ready => {
                    if accepted.is_ok() {
                        *established = true;
                        owner.event(serde_json::json!({"event":"state","code":"connected"}));
                    }
                    client.await
                },
                result = &mut client => {
                    if ready.try_recv().is_ok() {
                        *established = true;
                        owner.event(serde_json::json!({"event":"state","code":"connected"}));
                    }
                    result
                },
            };
            result.map_err(|error| match error {
                crate::direct::Error::IdentityRejected => "host_key_rejected",
                crate::direct::Error::Network => "network_unavailable",
                crate::direct::Error::Timeout => "timeout",
                crate::direct::Error::Closed => "connection_lost",
                crate::direct::Error::Denied => "pairing_required",
                crate::direct::Error::Busy => "computer_busy",
                crate::direct::Error::Storage => "storage_failed",
                crate::direct::Error::Invalid => "protocol_error",
            })
        }
    }
}
fn ssh_error(error: crate::ssh::Error) -> &'static str {
    use crate::ssh::Error;
    match error {
        Error::HostKey => "host_key_rejected",
        Error::Authentication => "authentication_failed",
        Error::InvalidKey => "invalid_key",
        Error::Forwarding => "forwarding_denied",
        Error::Timeout => "timeout",
        Error::Network | Error::Closed => "network_unavailable",
        _ => "ssh_failed",
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: String,
    artifacts: Vec<Artifact>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    name: String,
    sha256: String,
    bytes: usize,
}
fn artifact(root: PathBuf, name: &str) -> Result<(Zeroizing<Vec<u8>>, String), &'static str> {
    fn read(path: PathBuf, max: usize) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        let mut file = std::fs::File::open(path).map_err(|_| "missing_receiver")?;
        let mut bytes = Zeroizing::new(Vec::new());
        Read::by_ref(&mut file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "missing_receiver")?;
        if bytes.len() > max {
            return Err("invalid_receiver");
        }
        Ok(bytes)
    }
    let manifest: Manifest = serde_json::from_slice(&read(root.join("manifest.json"), 8192)?)
        .map_err(|_| "invalid_receiver")?;
    if manifest.version != env!("CARGO_PKG_VERSION") || manifest.artifacts.len() > 4 {
        return Err("invalid_receiver");
    }
    let a = manifest
        .artifacts
        .iter()
        .find(|a| a.name == name)
        .ok_or("missing_receiver")?;
    if a.bytes > 20 * 1024 * 1024 {
        return Err("invalid_receiver");
    }
    let bytes = read(root.join(name), a.bytes)?;
    let actual = ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if bytes.len() != a.bytes || actual != a.sha256 {
        return Err("invalid_receiver");
    }
    Ok((bytes, a.sha256.clone()))
}

#[cfg(test)]
mod tests;
