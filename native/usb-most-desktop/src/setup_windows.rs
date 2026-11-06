//! Recoverable Windows receiver deployment using the shared transaction engine.
//! Driver staging is a separate prerequisite; no unsigned-driver fallback.
use super::{Config, PrivateFile, admin, deployment, digest_hex, state_root};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use usb_most_platform::windows::{
    self as os, firewall,
    service::install::{Manager, Snapshot, executable},
};
use zeroize::Zeroizing;
fn phase<T>(name: &'static str, result: io::Result<T>) -> io::Result<T> {
    result.inspect_err(|_| eprintln!("USB Most setup: {name}"))
}
pub fn install(config: &Config) -> io::Result<()> {
    let manager = phase("rights", Manager::open())?; // Require actual OS rights before mutation.
    let root = state_root()?;
    let _state_guard = phase("state_directory", os::create_private_directory(&root))?;
    #[cfg(feature = "bundled-driver")]
    phase("driver", super::windows_stack::prepare(&root))?;
    if !super::windows::available() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let _binary_guard = phase(
        "binary_directory",
        os::create_private_directory(&os::binary_root()?),
    )?;
    PrivateFile::new(root.join("receiver.json"), 4096)?;
    let mut binary = Zeroizing::new(Vec::new());
    File::open(std::env::current_exe()?)?
        .take(20 * 1024 * 1024 + 1)
        .read_to_end(&mut binary)?;
    if binary.len() > 20 * 1024 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let configuration =
        Zeroizing::new(serde_json::to_vec(config).map_err(|_| io::ErrorKind::InvalidData)?);
    let config_hash = digest_hex(&configuration);
    let targets = vec![
        deployment::Target {
            path: executable()?,
            data: binary,
            mode: 0o600,
            maximum: 20 * 1024 * 1024,
        },
        deployment::Target {
            path: root.join("receiver.json"),
            data: configuration,
            mode: 0o600,
            maximum: 4096,
        },
    ];
    let peer = match config {
        Config::Ssh { peer, .. } => Some(peer.clone()),
        _ => None,
    };
    let desired_firewall = match config {
        Config::Direct { port } => firewall::State::direct(*port),
        _ => firewall::State::default(),
    };
    let mut services = Scm {
        desired_firewall,
        manager,
        root: root.clone(),
        config_hash: config_hash.clone(),
        peer,
        ssh: matches!(config, Config::Ssh { .. }),
        changed: !deployment::current(&targets)?,
    };
    phase(
        "deployment",
        deployment::install(
            &root,
            &targets,
            &root.join("active"),
            &config_hash,
            &mut services,
            &mut || Ok(()),
        ),
    )
}
struct Scm {
    manager: Manager,
    root: PathBuf,
    config_hash: String,
    peer: Option<String>,
    ssh: bool,
    changed: bool,
    desired_firewall: firewall::State,
}
impl Scm {
    fn pause_and_stop(&self, allow_idle: bool) -> io::Result<()> {
        if self.manager.snapshot()?.is_some_and(|s| s.active) {
            let reply = admin::request(
                &self.root,
                &admin::Request::Pause {
                    owner: self.peer.clone(),
                    allow_idle,
                },
            )?;
            if !matches!(reply, admin::Response::Paused) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        self.manager.stop()
    }
}
impl deployment::Services for Scm {
    fn state(&mut self) -> io::Result<deployment::ServiceState> {
        let firewall = firewall::state()?;
        let snapshot = self.manager.snapshot()?;
        let active = snapshot.is_some_and(|s| s.active);
        if active {
            let admin::Response::Health { ref status } =
                admin::request(&self.root, &admin::Request::Health)?
            else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            let same_peer = self
                .peer
                .as_ref()
                .is_some_and(|p| status.peer.as_ref() == Some(p));
            if !same_peer
                && (status.active > 0 || status.connected)
                && (self.changed
                    || firewall != self.desired_firewall
                    || snapshot.is_some_and(|s| s.sid_type != Some(1)))
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        Ok(deployment::ServiceState {
            active,
            enabled: snapshot.is_some_and(|s| s.start == 2),
            windows_start: snapshot.map(|s| s.start),
            windows_sid_type: snapshot.and_then(|s| s.sid_type),
            windows_firewall_port: firewall.tls,
            windows_discovery: firewall.discovery,
        })
    }
    fn configuration_current(&mut self) -> io::Result<bool> {
        Ok(firewall::state()? == self.desired_firewall
            && self
                .manager
                .snapshot()?
                .is_some_and(|s| s.sid_type == Some(1)))
    }
    fn quiesce(&mut self, _: deployment::ServiceState) -> io::Result<()> {
        self.pause_and_stop(false)
    }
    fn recover_quiesce(&mut self, _: deployment::ServiceState) -> io::Result<()> {
        self.pause_and_stop(true)
    }
    fn activate(&mut self) -> io::Result<()> {
        firewall::apply(self.desired_firewall)?;
        self.manager.activate()
    }
    fn health(&mut self) -> io::Result<()> {
        if !self.configuration_current()? {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let end = Instant::now() + Duration::from_secs(45);
        while Instant::now() < end {
            if let Ok(admin::Response::Health { ref status }) =
                admin::request(&self.root, &admin::Request::Health)
                && status.version == super::VERSION
                && status.config_hash == self.config_hash
                && status.stack
                && (!self.ssh || status.connected)
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        Err(io::ErrorKind::TimedOut.into())
    }
    fn restore(&mut self, previous: deployment::ServiceState) -> io::Result<()> {
        firewall::apply(firewall::State {
            tls: previous.windows_firewall_port,
            discovery: previous.windows_discovery,
        })?;
        self.manager
            .restore(previous.windows_start.map(|start| Snapshot {
                start,
                active: previous.active,
                sid_type: previous.windows_sid_type,
            }))
    }
}
