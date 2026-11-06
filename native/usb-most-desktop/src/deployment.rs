//! Recoverable installation of fixed product files. A journal contains hashes
//! and service state, never commands/paths received from the network.
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::io::{Read, Write};
use std::{
    io,
    path::{Path, PathBuf},
};
use usb_most_connection::vault::PrivateFile;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceState {
    pub active: bool,
    pub enabled: bool,
    /// Windows startup type; None means absent. Unix journals keep None.
    #[serde(default)]
    pub windows_start: Option<u32>,
    /// Exact pre-install service token identity; absent for Unix/legacy journals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_sid_type: Option<u32>,
    #[serde(default)]
    pub windows_firewall_port: Option<u16>,
    #[serde(default)]
    pub windows_discovery: bool,
}
pub trait Services {
    fn state(&mut self) -> io::Result<ServiceState>;
    fn configuration_current(&mut self) -> io::Result<bool> {
        Ok(true)
    }
    fn quiesce(&mut self, previous: ServiceState) -> io::Result<()>;
    fn recover_quiesce(&mut self, previous: ServiceState) -> io::Result<()> {
        self.quiesce(previous)
    }
    fn activate(&mut self) -> io::Result<()>;
    fn health(&mut self) -> io::Result<()>;
    fn restore(&mut self, previous: ServiceState) -> io::Result<()>;
}
pub struct Target {
    pub path: PathBuf,
    pub data: Zeroizing<Vec<u8>>,
    pub mode: u32,
    pub maximum: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Before {
    hash: Option<String>,
    mode: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u8,
    committed: bool,
    service: ServiceState,
    before: Vec<Before>,
    after: Vec<String>,
    activation: String,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[cfg(unix)]
fn read(path: &Path, max: usize) -> io::Result<Option<(Zeroizing<Vec<u8>>, u32)>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o7022 != 0
        || metadata.len() > max as u64
    {
        return Err(invalid());
    }
    let mut data = Zeroizing::new(Vec::new());
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() > max {
        return Err(invalid());
    }
    Ok(Some((data, metadata.permissions().mode() & 0o777)))
}
#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(path.parent().ok_or_else(invalid)?)?.sync_all()
}
#[cfg(unix)]
fn atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(invalid)?;
    let nonce = usb_most_connection::security::random::<16>()
        .map_err(|_| invalid())?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let pending = parent.join(format!(".usb-most-{nonce}.pending"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&pending)?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&pending, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}
#[cfg(unix)]
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn backup(root: &Path, index: usize) -> PathBuf {
    root.join(format!(".rollback-{index}"))
}
fn persist(file: &PrivateFile, journal: &Journal) -> io::Result<()> {
    file.replace(&Zeroizing::new(
        serde_json::to_vec(journal).map_err(|_| invalid())?,
    ))
}
fn hex32(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(out)
}
fn cleanup(root: &Path, targets: &[Target]) -> io::Result<()> {
    // Only the exact files created by this transaction. No tree/mount cleanup.
    for i in 0..targets.len() {
        remove(&backup(root, i))?;
    }
    remove(&root.join("install-journal.json"))
}
fn recover(
    root: &Path,
    targets: &[Target],
    activation: &Path,
    host: &mut dyn Services,
) -> io::Result<bool> {
    let file = PrivateFile::new(root.join("install-journal.json"), 16384)?;
    let Some(bytes) = file.read()? else {
        return Ok(false);
    };
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if journal.schema != 1
        || journal.before.len() != targets.len() + 1
        || journal.after.len() != targets.len()
    {
        return Err(invalid());
    }
    #[cfg(windows)]
    if journal
        .service
        .windows_start
        .is_some_and(|v| !matches!(v, 2..=4))
        || journal.service.enabled != (journal.service.windows_start == Some(2))
        || journal.service.active && journal.service.windows_start.is_none()
        || journal
            .service
            .windows_sid_type
            .is_some_and(|v| !matches!(v, 0 | 1))
        || journal.service.windows_sid_type.is_some() && journal.service.windows_start.is_none()
        || journal
            .service
            .windows_firewall_port
            .is_some_and(|p| p < 1024)
    {
        return Err(invalid());
    }
    for old in &journal.before {
        if old.mode & !0o777 != 0 || old.mode & 0o022 != 0 {
            return Err(invalid());
        }
    }
    for v in journal
        .after
        .iter()
        .chain(journal.before.iter().filter_map(|v| v.hash.as_ref()))
    {
        hex32(v)?;
    }
    let activation_bytes = hex32(&journal.activation)?;
    if journal.committed {
        for (target, expected) in targets.iter().zip(&journal.after) {
            let data = read(&target.path, target.maximum)?.ok_or_else(invalid)?;
            if hash(&data.0) != *expected {
                return Err(invalid());
            }
        }
        atomic(activation, &activation_bytes, 0o600)?;
        remove(&backup(root, targets.len()))?;
        return cleanup(root, targets).map(|_| true);
    }
    let unchanged = targets
        .iter()
        .map(|t| (t.path.as_path(), t.maximum))
        .chain(std::iter::once((activation, 32)))
        .enumerate()
        .all(|(i, (path, maximum))| {
            read(path, maximum).is_ok_and(|value| {
                value.as_ref().map(|v| hash(&v.0)) == journal.before[i].hash
                    && value
                        .as_ref()
                        .is_none_or(|(_, mode)| *mode == journal.before[i].mode)
            })
        });
    if unchanged {
        host.restore(journal.service)?;
        remove(&backup(root, targets.len()))?;
        return cleanup(root, targets).map(|_| false);
    }
    host.recover_quiesce(journal.service)?;
    for (i, (path, maximum)) in targets
        .iter()
        .map(|t| (t.path.as_path(), t.maximum))
        .chain(std::iter::once((activation, 32)))
        .enumerate()
    {
        let old = &journal.before[i];
        let current = read(path, maximum)?;
        let current_hash = current.as_ref().map(|v| hash(&v.0));
        if current_hash == old.hash {
            if let Some((data, mode)) = current.as_ref()
                && *mode != old.mode
            {
                atomic(path, data, old.mode)?;
            }
            continue;
        }
        // Never overwrite an unrelated administrator's edit during recovery.
        let ours = if i == targets.len() {
            Some(hash(&activation_bytes))
        } else {
            Some(journal.after[i].clone())
        };
        if current_hash.is_some() && current_hash != ours {
            return Err(io::Error::other("recovery_conflict"));
        }
        match &old.hash {
            Some(expected) => {
                let saved = read(&backup(root, i), maximum)?.ok_or_else(invalid)?;
                if hash(&saved.0) != *expected {
                    return Err(invalid());
                }
                atomic(path, &saved.0, old.mode)?;
            }
            None => remove(path)?,
        }
    }
    host.restore(journal.service)?;
    // Include the separate activation snapshot too.
    remove(&backup(root, targets.len()))?;
    cleanup(root, targets).map(|_| false)
}
pub fn current(targets: &[Target]) -> io::Result<bool> {
    for target in targets {
        let value = read(&target.path, target.maximum)?;
        if value.as_ref().is_none_or(|(bytes, mode)| {
            bytes.as_slice() != target.data.as_slice() || *mode != target.mode
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}
/// Fault hook runs before each mutating boundary, allowing deterministic failure
/// injection without changing systemd, modules, real USB or system directories.
pub fn install(
    root: &Path,
    targets: &[Target],
    activation: &Path,
    activation_hash: &str,
    host: &mut dyn Services,
    fault: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<()> {
    if targets.is_empty()
        || targets.len() > 8
        || targets
            .iter()
            .any(|t| t.data.len() > t.maximum || t.maximum > 20 * 1024 * 1024)
    {
        return Err(invalid());
    }
    PrivateFile::new(root.join("install-journal.json"), 16384)?;
    let lock = installation_lock(root)?;
    lock.try_lock()
        .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
    recover(root, targets, activation, host)?;
    let service = host.state()?;
    let mut before = Vec::new();
    let mut snapshots = Vec::new();
    for (path, maximum) in targets
        .iter()
        .map(|t| (t.path.as_path(), t.maximum))
        .chain(std::iter::once((activation, 32)))
    {
        let saved = read(path, maximum)?;
        before.push(Before {
            hash: saved.as_ref().map(|v| hash(&v.0)),
            mode: saved.as_ref().map_or(0o600, |v| v.1),
        });
        snapshots.push(saved);
    }
    let mut journal = Journal {
        schema: 1,
        committed: false,
        service,
        before,
        after: targets.iter().map(|t| hash(&t.data)).collect(),
        activation: activation_hash.into(),
    };
    let activation_bytes = hex32(activation_hash)?;
    if service.active
        && service.enabled
        && current(targets)?
        && host.configuration_current()?
        && snapshots.last().is_some_and(|value| {
            value
                .as_ref()
                .is_some_and(|(bytes, _)| bytes.as_slice() == activation_bytes)
        })
    {
        return host.health();
    }
    let file = PrivateFile::new(root.join("install-journal.json"), 16384)?;
    persist(&file, &journal)?;
    let applied = (|| {
        for (i, snapshot) in snapshots.iter().enumerate() {
            fault()?;
            if let Some((data, _)) = snapshot {
                let path = backup(root, i);
                if path.exists() {
                    return Err(invalid());
                }
                atomic(&path, data, 0o600)?;
            }
        }
        fault()?;
        host.quiesce(service)?;
        fault()?;
        remove(activation)?;
        for (target, snapshot) in targets.iter().zip(&snapshots) {
            fault()?;
            if snapshot.as_ref().is_none_or(|(data, mode)| {
                data.as_slice() != target.data.as_slice() || *mode != target.mode
            }) {
                atomic(&target.path, &target.data, target.mode)?;
            }
        }
        fault()?;
        host.activate()?;
        fault()?;
        host.health()?;
        fault()?;
        journal.committed = true;
        persist(&file, &journal)?;
        fault()?;
        atomic(activation, &activation_bytes, 0o600)?;
        fault()?;
        remove(&backup(root, targets.len()))?;
        cleanup(root, targets)
    })();
    if let Err(error) = applied {
        // A committed deployment is finalized, never rolled back after import
        // might have become visible to the user. Otherwise restore old state.
        let committed = recover(root, targets, activation, host)
            .map_err(|_| io::Error::other("installation_recovery_required"))?;
        if committed
            || journal.committed
                && !root.join("install-journal.json").exists()
                && current(targets)?
                && read(activation, 32)?
                    .is_some_and(|(bytes, _)| bytes.as_slice() == activation_bytes)
        {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}
#[cfg(all(test, unix))]
mod tests;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::{atomic, installation_lock, read, remove};

#[cfg(unix)]
fn installation_lock(root: &Path) -> io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(root.join("install.lock"))?;
    let metadata = lock.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(invalid());
    }
    Ok(lock)
}
