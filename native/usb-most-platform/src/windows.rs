//! Win32 ownership/security primitives. No arbitrary commands or shell strings.
pub mod driver;
#[cfg(feature = "windows-eject")]
pub mod eject;
#[cfg(feature = "firewall")]
pub mod firewall;
pub mod service;
#[cfg(feature = "shell-ui")]
pub mod shell_ui;
use std::{
    ffi::{OsStr, c_void},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_ALREADY_EXISTS, LocalFree},
    Security::{Authorization::*, *},
    Storage::FileSystem::{
        CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        MoveFileExW,
    },
    System::Com::CoTaskMemFree,
    UI::Shell::*,
};
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
pub(crate) fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut s: Vec<_> = text.encode_wide().collect();
    if s.is_empty() || s.len() > 32766 || s.contains(&0) {
        return Err(invalid());
    };
    s.push(0);
    Ok(s)
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Win32 LocalAlloc-family ownership, transferred once into this guard.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}
struct Com(*mut u16);
impl Drop for Com {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: SHGetKnownFolderPath transfers CoTaskMem ownership to the caller.
            unsafe {
                CoTaskMemFree(self.0.cast());
            }
        }
    }
}
fn folder(id: &windows_sys::core::GUID) -> io::Result<PathBuf> {
    let mut raw = ptr::null_mut();
    // SAFETY: valid GUID/output pointers; no token, no caller-controlled allocation.
    let result = unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut raw) };
    let guard = Com(raw);
    if result < 0 {
        return Err(io::Error::other("known_folder_unavailable"));
    };
    if guard.0.is_null() {
        return Err(invalid());
    }
    let mut n = 0;
    while n < 32767 {
        // SAFETY: successful KnownFolder API returns a terminated Windows path.
        if unsafe { *guard.0.add(n) } == 0 {
            break;
        }
        n += 1;
    }
    if n == 0 || n == 32767 {
        return Err(invalid());
    }
    // SAFETY: validated terminated path length; copy before freeing its allocation.
    let text = std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(guard.0, n) });
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(path)
}
pub fn state_root() -> io::Result<PathBuf> {
    Ok(folder(&FOLDERID_ProgramData)?.join("USBMost"))
}
pub fn system_directory() -> io::Result<PathBuf> {
    folder(&FOLDERID_System)
}
pub fn binary_root() -> io::Result<PathBuf> {
    Ok(folder(&FOLDERID_ProgramFiles)?.join("USBMost"))
}
/// Convert only an OS-validated descriptor; the guard keeps all internal pointers live.
fn descriptor_text(
    sd: PSECURITY_DESCRIPTOR,
    information: OBJECT_SECURITY_INFORMATION,
) -> io::Result<String> {
    let mut text = ptr::null_mut();
    let mut n = 0;
    // SAFETY: sd belongs to an active Win32 guard; output pointers are writable.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            information,
            &mut text,
            &mut n,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let guard = Local(text.cast());
    if text.is_null() || n == 0 || n > 4096 {
        return Err(invalid());
    }
    // SAFETY: API reports allocated length including its NUL; bounded before copy.
    let units = unsafe { std::slice::from_raw_parts(text, n as usize) };
    let _keep = &guard;
    let end = units.iter().position(|v| *v == 0).ok_or_else(invalid)?;
    String::from_utf16(&units[..end]).map_err(|_| invalid())
}
fn administrator_descriptor(sd: PSECURITY_DESCRIPTOR, inheritance: bool) -> io::Result<()> {
    let text = descriptor_text(sd, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION)?;
    let Some((owner, dacl)) = text.split_once("D:") else {
        return Err(denied());
    };
    if !matches!(owner, "O:SY" | "O:BA") {
        return Err(denied());
    }
    let protected = dacl.starts_with('P');
    let dacl = dacl.strip_prefix('P').unwrap_or(dacl);
    let inherited = dacl.starts_with("AI");
    let dacl = dacl.strip_prefix("AI").unwrap_or(dacl);
    if !protected && (inheritance || !inherited) {
        return Err(denied());
    }
    let (system, admins) = if inheritance {
        ("(A;OICI;FA;;;SY)", "(A;OICI;FA;;;BA)")
    } else {
        ("(A;;FA;;;SY)", "(A;;FA;;;BA)")
    };
    // Exact canonical ACL: no Everyone, Users, owner-rights, callback/unknown ACEs.
    let explicit =
        protected && (dacl == format!("{system}{admins}") || dacl == format!("{admins}{system}"));
    let inherited_child = !inheritance
        && matches!(
            dacl,
            "(A;ID;FA;;;SY)(A;ID;FA;;;BA)" | "(A;ID;FA;;;BA)(A;ID;FA;;;SY)"
        );
    if !explicit && !inherited_child {
        return Err(denied());
    }
    Ok(())
}
/// Verify ownership and the complete protected DACL, not merely write access.
/// File/reparse validation is performed by the caller using its owned file handle.
fn validate_private_handle(
    handle: &impl std::os::windows::io::AsRawHandle,
    directory: bool,
) -> io::Result<()> {
    let mut sd = ptr::null_mut();
    // SAFETY: borrow of a live owned file handle; returned descriptor is LocalFree-owned.
    let error = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut sd,
        )
    };
    let guard = Local(sd);
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    };
    if guard.0.is_null() {
        return Err(invalid());
    }
    administrator_descriptor(guard.0, directory)
}
/// Open and pin a protected directory without following a final reparse point.
/// Omitting FILE_SHARE_DELETE prevents its renaming while the caller owns it.
pub fn open_private_directory(path: &std::path::Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied());
    }
    validate_private_handle(&file, true)?;
    Ok(file)
}
/// A child may inherit the already protected parent's exact SYSTEM/Admin ACL.
pub fn validate_private_file(file: &std::fs::File, parent: &std::fs::File) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    validate_private_handle(parent, true)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied());
    }
    validate_private_handle(file, false)
}
/// Creation is protected from its first filesystem-visible instant. Never
/// "repair" an existing untrusted/pre-created directory by taking ownership.
pub fn create_private_directory(path: &std::path::Path) -> io::Result<std::fs::File> {
    let text = wide(OsStr::new("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"))?;
    let path_wide = wide(path.as_os_str())?;
    let mut raw = ptr::null_mut();
    // SAFETY: terminated literal; writable output pointer, immediately RAII-owned.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sd = Local(raw);
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    // SAFETY: live descriptor and terminated path during synchronous CreateDirectory.
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), &sa) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    open_private_directory(path)
}
/// Create state with an explicit protected owner/DACL. An elevated interactive
/// administrator's default file owner may be their account SID, not BA.
pub fn create_private_file(
    path: &std::path::Path,
    parent: &std::fs::File,
) -> io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, READ_CONTROL},
    };
    validate_private_handle(parent, true)?;
    let text = wide(OsStr::new("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)"))?;
    let path_wide = wide(path.as_os_str())?;
    let mut raw = ptr::null_mut();
    // SAFETY: terminated literal and valid output, immediately put in RAII guard.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sd = Local(raw);
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    // SAFETY: synchronous create-new only, live descriptor/path, no overlapped I/O.
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_WRITE | READ_CONTROL,
            1,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreateFile transfers one unique file-handle ownership.
    let file = unsafe { std::fs::File::from_raw_handle(handle) };
    validate_private_file(&file, parent)?;
    Ok(file)
}
/// Same-volume replacement only; never fall back to a non-atomic copy/delete.
pub fn replace_private_file(pending: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
    if pending.parent() != target.parent() {
        return Err(invalid());
    }
    let from = wide(pending.as_os_str())?;
    let to = wide(target.as_os_str())?;
    // SAFETY: both terminated caller-owned paths remain live through the call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Windows SO_REUSEADDR can let another process capture an existing binding.
/// Public listeners explicitly require exclusive address ownership before bind.
pub fn exclusive_socket(socket: std::os::windows::io::BorrowedSocket<'_>) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        SO_EXCLUSIVEADDRUSE, SOL_SOCKET, WSAGetLastError, setsockopt,
    };
    let enabled = 1i32;
    // SAFETY: borrowed live socket, valid fixed-size BOOL value for this call.
    if unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&enabled as *const i32).cast(),
            size_of::<i32>() as i32,
        )
    } != 0
    {
        // SAFETY: thread-local Winsock error query has no pointer/ownership input.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(text: &str) -> Local {
        let mut raw = ptr::null_mut();
        let text = wide(OsStr::new(text)).unwrap();
        // SAFETY: test-owned terminated input and valid output; result immediately owned.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut raw,
                    ptr::null_mut(),
                )
            },
            0
        );
        Local(raw)
    }
    #[test]
    fn private_acl_refuses_owner_inheritance_and_other_local_principals() {
        for (s, ok) in [
            ("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)", true),
            ("O:BAD:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)", true),
            ("O:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)", true),
            ("O:SYD:P(A;;FA;;;SY)(A;;FA;;;BA)", true),
            ("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;WD)", false),
            ("O:BAD:(A;;FA;;;SY)(A;;FA;;;BA)", false),
            ("O:BUD:P(A;;FA;;;SY)(A;;FA;;;BA)", false),
            ("O:BAD:P(A;;FR;;;SY)(A;;FA;;;BA)", false),
        ] {
            let sd = descriptor(s);
            assert_eq!(administrator_descriptor(sd.0, false).is_ok(), ok, "{s}")
        }
    }
    #[test]
    fn known_folders_are_absolute_and_do_not_depend_on_environment() {
        assert!(state_root().unwrap().is_absolute());
        assert!(binary_root().unwrap().is_absolute());
        assert!(wide(OsStr::new("x\0y")).is_err())
    }
    #[test]
    fn real_private_directory_file_and_atomic_replace() {
        use std::io::Write;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("usb-most-acl-{}-{nonce}", std::process::id()));
        let parent = create_private_directory(&path).unwrap();
        let pending = path.join("fixture.pending");
        let target = path.join("fixture.state");
        let mut file = create_private_file(&pending, &parent).unwrap();
        validate_private_file(&file, &parent).unwrap();
        file.write_all(b"synthetic-private-data").unwrap();
        file.sync_all().unwrap();
        drop(file);
        replace_private_file(&pending, &target).unwrap();
        let file = std::fs::File::open(&target).unwrap();
        validate_private_file(&file, &parent).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&target).unwrap(), b"synthetic-private-data");
        drop(parent);
        std::fs::remove_file(target).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}
