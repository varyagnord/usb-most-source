//! Windows filesystem adapter for the shared journal. Every directory is pinned
//! without share-delete; binaries, backups and secrets have explicit BA/SYSTEM ACL.
use super::invalid;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::Path,
};
use usb_most_platform::windows as os;
use zeroize::Zeroizing;
const NO_REPARSE: u32 = 0x00200000;
pub(super) fn read(path: &Path, max: usize) -> io::Result<Option<(Zeroizing<Vec<u8>>, u32)>> {
    let parent = os::open_private_directory(path.parent().ok_or_else(invalid)?)?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(NO_REPARSE)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    os::validate_private_file(&file, &parent)?;
    if file.metadata()?.len() > max as u64 {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(invalid());
    }
    Ok(Some((bytes, 0o600)))
}
pub(super) fn atomic(path: &Path, data: &[u8], mode: u32) -> io::Result<()> {
    if mode != 0o600 {
        return Err(invalid());
    }
    let directory = path.parent().ok_or_else(invalid)?;
    let parent = os::open_private_directory(directory)?;
    // Validate any old entry before replacement (reparse points are refused).
    read(path, 20 * 1024 * 1024)?;
    let random = usb_most_connection::security::random::<16>().map_err(|_| invalid())?;
    let name = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let pending = directory.join(format!(".usb-most-{name}.pending"));
    let mut file = os::create_private_file(&pending, &parent)?;
    let result = (|| {
        file.write_all(data)?;
        file.sync_all()
    })();
    // Windows denies replacement of our own open write handle without SHARE_DELETE.
    drop(file);
    let result = result.and_then(|()| os::replace_private_file(&pending, path));
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}
pub(super) fn remove(path: &Path) -> io::Result<()> {
    let _parent = os::open_private_directory(path.parent().ok_or_else(invalid)?)?;
    if read(path, 20 * 1024 * 1024)?.is_none() {
        return Ok(());
    }
    // Exact owned files only, never directories or recursive cleanup.
    fs::remove_file(path)
}
pub(super) fn installation_lock(root: &Path) -> io::Result<File> {
    let parent = os::open_private_directory(root)?;
    let path = root.join("install.lock");
    match os::create_private_file(&path, &parent) {
        Ok(file) => {
            file.sync_all()?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(3)
        .custom_flags(NO_REPARSE)
        .open(path)?;
    os::validate_private_file(&file, &parent)?;
    Ok(file)
}

#[cfg(test)]
mod tests;
