//! Fixed platform operations around the recoverable deployment engine.
use super::Config;
use std::io;
#[cfg(target_os = "linux")]
use {
    super::{PrivateFile, admin, deployment, state_root},
    std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    },
    zeroize::Zeroizing,
};
pub fn install(config: &Config) -> io::Result<()> {
    // Kept gated until isolated OS installer/recovery E2E passes.
    if !cfg!(feature = "installation") {
        return Err(io::ErrorKind::Unsupported.into());
    }
    #[cfg(target_os = "linux")]
    {
        linux(config)
    }
    #[cfg(target_os = "windows")]
    {
        super::setup_windows::install(config)
    }
}
#[cfg(target_os = "linux")]
fn protected_directory(path: &Path, mode: u32, owned: bool) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let created = !path.exists();
    if created {
        fs::DirBuilder::new().mode(mode).create(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // sudo/service umask may be 077. Set final permissions explicitly after
    // validation, including an interrupted earlier creation of our directories.
    // Existing shared OS directories retain administrator-selected permissions.
    if created || owned {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn linux(config: &Config) -> io::Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if !Path::new("/run/systemd/system").is_dir() {
        return Err(io::ErrorKind::Unsupported.into());
    }
    // Shared OS prerequisites are not removed on rollback: another product may
    // already rely on them. No unsigned/out-of-tree kernel module is installed.
    if !super::linux::available() {
        run("/usr/sbin/modprobe", &["vhci_hcd"], Duration::from_secs(20))?;
    }
    if !super::linux::available() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let root = state_root()?;
    protected_directory(&root, 0o700, true)?;
    PrivateFile::new(root.join("receiver.json"), 4096)?;
    protected_directory(Path::new("/usr/local/libexec"), 0o755, false)?;
    let bin = Path::new("/usr/local/libexec/usb-most");
    protected_directory(bin, 0o755, true)?;
    protected_directory(Path::new("/etc/systemd/system"), 0o755, false)?;
    protected_directory(Path::new("/etc/modules-load.d"), 0o755, false)?;
    // Pin the image actually executing, even if its download path is renamed
    // while a desktop user approves PolicyKit. Never reopen that mutable path.
    use std::io::Read;
    let executable = fs::File::open("/proc/self/exe")?;
    if executable.metadata()?.len() > 20 * 1024 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut binary = Zeroizing::new(Vec::new());
    executable
        .take(20 * 1024 * 1024 + 1)
        .read_to_end(&mut binary)?;
    if binary.len() > 20 * 1024 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let configuration =
        Zeroizing::new(serde_json::to_vec(config).map_err(|_| io::ErrorKind::InvalidData)?);
    let config_hash = super::digest_hex(&configuration);
    let targets = vec![
        deployment::Target {
            path: bin.join("usb-most-desktop"),
            data: binary,
            mode: 0o755,
            maximum: 20 * 1024 * 1024,
        },
        deployment::Target {
            path: root.join("receiver.json"),
            data: configuration,
            mode: 0o600,
            maximum: 4096,
        },
        deployment::Target {
            path: PathBuf::from("/etc/systemd/system/usb-most-desktop.service"),
            data: Zeroizing::new(
                include_bytes!("../../../server/usb-most-desktop.service").to_vec(),
            ),
            mode: 0o644,
            maximum: 32768,
        },
        deployment::Target {
            path: PathBuf::from("/etc/modules-load.d/usb-most.conf"),
            data: Zeroizing::new(b"vhci_hcd\n".to_vec()),
            mode: 0o644,
            maximum: 4096,
        },
    ];
    let desired_peer = match config {
        Config::Ssh { peer, .. } => Some(peer.clone()),
        _ => None,
    };
    let mut systemd = Systemd {
        root: root.clone(),
        config_hash,
        ssh: matches!(config, Config::Ssh { .. }),
        peer: desired_peer,
        changed: !deployment::current(&targets)?,
    };
    deployment::install(
        &root,
        &targets,
        &root.join("active"),
        &super::digest_hex(&targets[1].data),
        &mut systemd,
        &mut || Ok(()),
    )
}
#[cfg(target_os = "linux")]
struct Systemd {
    root: PathBuf,
    config_hash: String,
    ssh: bool,
    peer: Option<String>,
    changed: bool,
}
#[cfg(target_os = "linux")]
impl deployment::Services for Systemd {
    fn state(&mut self) -> io::Result<deployment::ServiceState> {
        let active = state(&["is-active", "--quiet", "usb-most-desktop.service"])?;
        let enabled = state(&["is-enabled", "--quiet", "usb-most-desktop.service"])?;
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
            if !same_peer && (status.active > 0 || status.connected) && self.changed {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        Ok(deployment::ServiceState {
            active,
            enabled,
            windows_start: None,
            windows_sid_type: None,
            windows_firewall_port: None,
            windows_discovery: false,
        })
    }
    fn quiesce(&mut self, previous: deployment::ServiceState) -> io::Result<()> {
        self.pause_and_stop(previous, false)
    }
    fn recover_quiesce(&mut self, previous: deployment::ServiceState) -> io::Result<()> {
        self.pause_and_stop(previous, true)
    }
    fn activate(&mut self) -> io::Result<()> {
        run(
            "/usr/bin/systemctl",
            &["daemon-reload"],
            Duration::from_secs(15),
        )?;
        run(
            "/usr/bin/systemctl",
            &["enable", "usb-most-desktop.service"],
            Duration::from_secs(15),
        )?;
        run(
            "/usr/bin/systemctl",
            &["start", "usb-most-desktop.service"],
            Duration::from_secs(30),
        )
    }
    fn health(&mut self) -> io::Result<()> {
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
            thread::sleep(Duration::from_millis(500));
        }
        Err(io::ErrorKind::TimedOut.into())
    }
    fn restore(&mut self, previous: deployment::ServiceState) -> io::Result<()> {
        run(
            "/usr/bin/systemctl",
            &["daemon-reload"],
            Duration::from_secs(15),
        )?;
        if previous.enabled {
            run(
                "/usr/bin/systemctl",
                &["enable", "usb-most-desktop.service"],
                Duration::from_secs(15),
            )?;
        }
        if previous.active {
            run(
                "/usr/bin/systemctl",
                &["start", "usb-most-desktop.service"],
                Duration::from_secs(30),
            )?;
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
impl Systemd {
    fn pause_and_stop(
        &mut self,
        previous: deployment::ServiceState,
        allow_idle: bool,
    ) -> io::Result<()> {
        // Disable the temporary new unit before it might be removed on rollback.
        if Path::new("/etc/systemd/system/usb-most-desktop.service").exists() {
            if state(&["is-active", "--quiet", "usb-most-desktop.service"])? {
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
            run(
                "/usr/bin/systemctl",
                &["stop", "usb-most-desktop.service"],
                Duration::from_secs(100),
            )?;
            if !previous.enabled {
                run(
                    "/usr/bin/systemctl",
                    &["disable", "usb-most-desktop.service"],
                    Duration::from_secs(15),
                )?;
            }
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn state(args: &[&str]) -> io::Result<bool> {
    match exit("/usr/bin/systemctl", args, Duration::from_secs(15))? {
        0 => Ok(true),
        1 | 3 | 4 => Ok(false),
        _ => Err(io::Error::other("service_probe_failed")),
    }
}
#[cfg(target_os = "linux")]
fn run(program: &str, args: &[&str], deadline: Duration) -> io::Result<()> {
    if exit(program, args, deadline)? == 0 {
        Ok(())
    } else {
        Err(io::Error::other("platform_operation_failed"))
    }
}
#[cfg(target_os = "linux")]
fn exit(program: &str, args: &[&str], deadline: Duration) -> io::Result<i32> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let end = Instant::now() + deadline;
    loop {
        if let Some(status) = child.try_wait()? {
            return status
                .code()
                .ok_or_else(|| io::Error::other("platform_operation_interrupted"));
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
