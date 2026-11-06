//! Atomic private state. OS installer owns/protects the parent directory.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct PrivateFile {
    path: PathBuf,
    maximum: usize,
    #[cfg(windows)]
    parent: std::sync::Arc<std::fs::File>,
}
impl PrivateFile {
    /// On Windows the installer must restrict the parent DACL to the service
    /// identity. On Unix refuse a parent accessible by group/other.
    pub fn new(path: PathBuf, maximum: usize) -> std::io::Result<Self> {
        let parent = path.parent().ok_or_else(invalid)?;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || maximum == 0
            || maximum > 1024 * 1024
        {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
        }
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(invalid());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                if metadata.permissions().mode() & 0o077 != 0
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                {
                    return Err(std::io::ErrorKind::PermissionDenied.into());
                }
            }
        }
        #[cfg(windows)]
        let parent =
            std::sync::Arc::new(usb_most_platform::windows::open_private_directory(parent)?);
        Ok(Self {
            path,
            maximum,
            #[cfg(windows)]
            parent,
        })
    }
    pub fn read(&self) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let mut file = match options.open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > self.maximum as u64 {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
        }
        #[cfg(windows)]
        usb_most_platform::windows::validate_private_file(&file, &self.parent)?;
        let mut bytes = Zeroizing::new(Vec::new());
        Read::by_ref(&mut file)
            .take(self.maximum as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.maximum {
            return Err(invalid());
        }
        Ok(Some(bytes))
    }
    pub fn replace(&self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.len() > self.maximum {
            return Err(invalid());
        }
        let parent = self.path.parent().ok_or_else(invalid)?;
        let random = crate::security::random::<16>().map_err(|_| invalid())?;
        let name = random
            .iter()
            .map(|n| format!("{n:02x}"))
            .collect::<String>();
        let pending = parent.join(format!(".usb-most-{name}.pending"));
        #[cfg(not(windows))]
        let mut file = {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(&pending)?
        };
        #[cfg(windows)]
        let mut file = usb_most_platform::windows::create_private_file(&pending, &self.parent)?;
        let result = (|| {
            #[cfg(windows)]
            usb_most_platform::windows::validate_private_file(&file, &self.parent)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            #[cfg(not(windows))]
            fs::rename(&pending, &self.path)?;
            #[cfg(windows)]
            usb_most_platform::windows::replace_private_file(&pending, &self.path)?;
            #[cfg(unix)]
            {
                std::fs::File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            // Remove only this known newly created file, never a directory tree.
            let _ = fs::remove_file(&pending);
        }
        result
    }
}
fn invalid() -> std::io::Error {
    std::io::ErrorKind::InvalidData.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_atomic_update_retains_previous_value_on_rejected_input() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let file = PrivateFile::new(dir.path().join("state"), 32).unwrap();
        assert!(file.read().unwrap().is_none());
        file.replace(b"old").unwrap();
        assert!(file.replace(&[1; 33]).is_err());
        assert_eq!(file.read().unwrap().unwrap().as_slice(), b"old");
        file.replace(b"new").unwrap();
        assert_eq!(file.read().unwrap().unwrap().as_slice(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("state"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    #[cfg(unix)]
    fn rejects_world_readable_directory_and_state_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrivateFile::new(dir.path().join("state"), 32).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        symlink("/dev/null", dir.path().join("state")).unwrap();
        assert!(PrivateFile::new(dir.path().join("state"), 32).is_err());
    }
}
