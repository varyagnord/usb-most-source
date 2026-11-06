//! Interactive Windows shell boundary. Elevation uses the OS consent dialog;
//! the browser opens through the existing unelevated desktop Explorer.
//! Reference: https://devblogs.microsoft.com/oldnewthing/20131118-00/?p=2643
use std::{io, path::Path};
use windows::{
    Win32::{
        System::{
            Com::{
                CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
                CoCreateInstance, CoInitializeEx, CoUninitialize, IServiceProvider,
            },
            Variant::VARIANT,
        },
        UI::{
            Shell::{
                IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellWindows,
                SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SID_STopLevelBrowser, SVGIO_BACKGROUND,
                SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellExecuteExW, ShellWindows,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{BSTR, Interface, PCWSTR},
};
fn failure(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff0000 == 0x80070000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other("desktop_shell_unavailable")
    }
}
struct Apartment;
impl Apartment {
    fn enter() -> io::Result<Self> {
        // SAFETY: called on an owned short-lived worker thread. Every successful
        // initialization is balanced after all COM references have been dropped.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(failure)?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: paired with this thread's successful CoInitializeEx.
        unsafe { CoUninitialize() }
    }
}
/// Only this executable and a fixed setup operation may be elevated. No URL,
/// password, command, or arbitrary parameters cross the UAC boundary.
pub fn elevate_setup() -> io::Result<()> {
    std::thread::spawn(|| {
        let _apartment = Apartment::enter()?;
        let exe = super::wide(std::env::current_exe()?.as_os_str())?;
        let operation = super::wide(std::ffi::OsStr::new("runas"))?;
        let argument = super::wide(std::ffi::OsStr::new("--setup-elevated"))?;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(operation.as_ptr()),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(argument.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: terminated stable buffers live through synchronous ShellExecute;
        // the standard runas verb requests OS authorization rather than bypassing it.
        unsafe { ShellExecuteExW(&mut info) }.map_err(failure)
    })
    .join()
    .map_err(|_| io::Error::other("shell_worker_failed"))?
}
/// Open an owner-protected local HTML handoff through the user's existing shell.
/// Do not fall back to ShellExecute from the elevated process: file handlers and
/// browsers must not inherit installer privileges.
pub fn open_from_desktop(file: &Path) -> io::Result<()> {
    if !file.is_absolute() || file.extension() != Some(std::ffi::OsStr::new("html")) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let file = file.to_owned();
    std::thread::spawn(move || {
        let _apartment = Apartment::enter()?;
        // SAFETY: COM interface types enforce IIDs and reference lifetimes. The
        // returned background view is the existing desktop's automation object,
        // not a newly created elevated Shell.Application instance.
        unsafe {
            let windows: IShellWindows =
                CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).map_err(failure)?;
            let empty = VARIANT::default();
            let mut hwnd = 0;
            let dispatch = windows
                .FindWindowSW(&empty, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)
                .map_err(failure)?;
            let services: IServiceProvider = dispatch.cast().map_err(failure)?;
            let browser: IShellBrowser = services
                .QueryService(&SID_STopLevelBrowser)
                .map_err(failure)?;
            let view = browser.QueryActiveShellView().map_err(failure)?;
            let dispatch: windows::Win32::System::Com::IDispatch =
                view.GetItemObject(SVGIO_BACKGROUND).map_err(failure)?;
            let folder: IShellFolderViewDual = dispatch.cast().map_err(failure)?;
            let shell: IShellDispatch2 = folder
                .Application()
                .map_err(failure)?
                .cast()
                .map_err(failure)?;
            let name = super::wide(file.as_os_str())?;
            let name = BSTR::from_wide(&name[..name.len() - 1]);
            shell
                .ShellExecute(
                    &name,
                    &empty,
                    &empty,
                    &VARIANT::from("open"),
                    &VARIANT::from(SW_SHOWNORMAL.0),
                )
                .map_err(failure)
        }
    })
    .join()
    .map_err(|_| io::Error::other("shell_worker_failed"))?
}

/// HTML is readable by the actual desktop user even when a different account
/// approved UAC. Other non-administrative users get no access. The capability
/// never appears in browser/launcher arguments or global temporary files.
pub struct Handoff {
    path: std::path::PathBuf,
    directory: std::path::PathBuf,
    pin: Option<std::fs::File>,
    owned: bool,
}
impl Handoff {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for Handoff {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
        self.pin.take();
        let _ = std::fs::remove_dir(&self.directory); // Never recursive.
    }
}
fn desktop_sid() -> io::Result<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
    let mut pid = 0;
    // SAFETY: OS-owned current-session shell window and writable scalar output.
    let window = unsafe { GetShellWindow() };
    if window.is_null() {
        return Err(io::ErrorKind::NotFound.into());
    }
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == 0 {
        return Err(super::invalid());
    }
    process_sid(pid)
}
fn process_sid(pid: u32) -> io::Result<String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    // SAFETY: read-only access to the desktop process, no inheritable handle.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: unique successfully opened handle transferred to RAII.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut raw = std::ptr::null_mut();
    // SAFETY: process handle is live and output is a writable handle pointer.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful OpenProcessToken transfers unique ownership.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut buffer = [0usize; 512]; // Aligned, bounded TOKEN_USER + SID storage.
    let mut length = 0;
    // SAFETY: bounded writable aligned buffer; TOKEN_USER requires TOKEN_QUERY.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            std::mem::size_of_val(&buffer) as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if (length as usize) < std::mem::size_of::<TOKEN_USER>()
        || length as usize > std::mem::size_of_val(&buffer)
    {
        return Err(super::invalid());
    }
    // SAFETY: successful API initialized a correctly aligned TOKEN_USER.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let start = buffer.as_ptr() as usize;
    let at = sid as usize;
    if at < start || at > start + length as usize - 8 {
        return Err(super::invalid());
    }
    let mut text = std::ptr::null_mut();
    // SAFETY: SID returned by GetTokenInformation remains alive in buffer; API
    // allocates a terminated string, immediately owned by a LocalFree guard.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _guard = super::Local(text.cast());
    let mut n = 0;
    // SAFETY: successful SID conversion returns a terminated SID string.
    while n < 184 && unsafe { *text.add(n) } != 0 {
        n += 1;
    }
    if n == 0 || n == 184 {
        return Err(super::invalid());
    }
    // SAFETY: copied validated returned string before LocalFree.
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, n) }).map_err(|_| super::invalid())
}
pub fn create_handoff(nonce: &str, html: &[u8]) -> io::Result<Handoff> {
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) || html.len() > 4096 {
        return Err(super::invalid());
    }
    create_for_sid(nonce, html, &desktop_sid()?)
}
fn create_for_sid(nonce: &str, html: &[u8], sid: &str) -> io::Result<Handoff> {
    use std::{
        ffi::OsStr,
        io::Write,
        os::windows::{fs::OpenOptionsExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::{
            CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        },
        UI::Shell::FOLDERID_ProgramData,
    };
    // sid comes only from OS token conversion (or a fixed unit-test SID).
    if !sid.starts_with("S-1-")
        || !sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
    {
        return Err(super::invalid());
    }
    let directory = super::folder(&FOLDERID_ProgramData)?.join(format!("USBMost-ui-{nonce}"));
    let path = directory.join("open.html");
    fn descriptor(text: &str) -> io::Result<super::Local> {
        let text = super::wide(OsStr::new(text))?;
        let mut raw = ptr::null_mut();
        // SAFETY: terminated owned SDDL and writable output; returned allocation
        // is immediately managed by a LocalFree guard.
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
        Ok(super::Local(raw))
    }
    let sd = descriptor(&format!(
        "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;{sid})"
    ))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let name = super::wide(directory.as_os_str())?;
    // SAFETY: fresh fixed-parent random directory with an atomic private ACL.
    // Any existing path is rejected rather than repaired or reused.
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let pin = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&directory)?;
    {
        use std::os::windows::fs::MetadataExt;
        if pin.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(super::invalid());
        }
    }
    let mut handoff = Handoff {
        path,
        directory,
        pin: Some(pin),
        owned: false,
    };
    let sd = descriptor(&format!(
        "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;{sid})"
    ))?;
    let attributes = SECURITY_ATTRIBUTES {
        lpSecurityDescriptor: sd.0,
        ..attributes
    };
    let name = super::wide(handoff.path.as_os_str())?;
    // SAFETY: create-new only in pinned fresh directory, with private descriptor.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            1,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreateFile transfers one uniquely owned file handle.
    let mut file = unsafe { std::fs::File::from_raw_handle(raw) };
    handoff.owned = true;
    file.write_all(html)?;
    file.sync_all()?;
    Ok(handoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_launch_paths_never_reach_desktop_com_or_file_creation() {
        assert!(open_from_desktop(Path::new("relative.html")).is_err());
        assert!(open_from_desktop(Path::new("C:\\something.exe")).is_err());
        assert!(create_handoff("../elsewhere", b"x").is_err());
        assert!(create_handoff(&"0".repeat(32), &[0; 4097]).is_err());
    }
    #[test]
    #[ignore = "requires explicitly disposable elevated Windows fixture"]
    fn owner_browser_handoff_acl_and_exact_cleanup() {
        use std::{
            os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
            ptr,
        };
        use windows_sys::Win32::Security::{Authorization::*, *};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        assert!(
            std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
                || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
        );
        let sid = process_sid(std::process::id()).unwrap();
        let nonce = format!(
            "{:032x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let page = create_for_sid(&nonce, b"synthetic-owner-page", &sid).unwrap();
        assert_eq!(std::fs::read(page.path()).unwrap(), b"synthetic-owner-page");
        assert!(create_for_sid(&nonce, b"must-not-replace", &sid).is_err());
        assert_eq!(std::fs::read(page.path()).unwrap(), b"synthetic-owner-page");
        let file = std::fs::File::open(page.path()).unwrap();
        let mut descriptor = ptr::null_mut();
        let mut owner = ptr::null_mut();
        let mut acl = ptr::null_mut();
        // SAFETY: owned live file and initialized descriptor output; allocation
        // transferred to the immediately following LocalFree guard.
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut acl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        let descriptor = crate::windows::Local(descriptor);
        assert_eq!(result, 0);
        // Compare OS SID/ACE structures: SDDL can abbreviate a local account
        // (for example the runner's Administrator RID500 as LA).
        fn parsed_sid(value: &str) -> crate::windows::Local {
            let wide = crate::windows::wide(std::ffi::OsStr::new(value)).unwrap();
            let mut raw = ptr::null_mut();
            // SAFETY: terminated valid SID input and immediately owned allocation.
            assert_ne!(
                unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut raw) },
                0
            );
            crate::windows::Local(raw)
        }
        let admin = parsed_sid("S-1-5-32-544");
        let system = parsed_sid("S-1-5-18");
        let user = parsed_sid(&sid);
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: descriptor/owner/ACL are OS-validated pointers owned by the
        // still-live GetSecurityInfo allocation. Each GetAce is checked.
        unsafe {
            assert_ne!(
                GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision),
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            assert_ne!(EqualSid(owner, admin.0), 0);
            assert!(!acl.is_null());
            assert_eq!((*acl).AceCount, 3);
            use windows_sys::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_GENERIC_READ};
            let expected = [
                (system.0, FILE_ALL_ACCESS),
                (admin.0, FILE_ALL_ACCESS),
                (user.0, FILE_GENERIC_READ),
            ];
            let mut seen = [false; 3];
            for index in 0..3 {
                let mut raw = ptr::null_mut();
                assert_ne!(GetAce(acl, index, &mut raw), 0);
                let ace = &*raw.cast::<ACCESS_ALLOWED_ACE>();
                assert_eq!(ace.Header.AceType, 0); // ACCESS_ALLOWED_ACE_TYPE
                assert_eq!(ace.Header.AceFlags, 0);
                let ace_sid = ptr::addr_of!(ace.SidStart).cast_mut().cast();
                let matched = expected
                    .iter()
                    .enumerate()
                    .position(|(i, (sid, mask))| {
                        !seen[i] && EqualSid(ace_sid, *sid) != 0 && ace.Mask == *mask
                    })
                    .unwrap();
                seen[matched] = true;
            }
            assert!(seen.into_iter().all(|value| value));
        }
        drop(file);
        let path = page.path().to_owned();
        std::thread::spawn(move || {
            let mut raw = ptr::null_mut();
            // SAFETY: current process pseudo-handle and writable token output.
            assert_ne!(
                unsafe {
                    OpenProcessToken(
                        GetCurrentProcess(),
                        TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                        &mut raw,
                    )
                },
                0
            );
            // SAFETY: successful API transfers unique owned handle.
            let original = unsafe { OwnedHandle::from_raw_handle(raw) };
            let text = crate::windows::wide(std::ffi::OsStr::new("S-1-5-32-545")).unwrap();
            let mut users = ptr::null_mut();
            // SAFETY: fixed terminated SID and immediately guarded allocation.
            assert_ne!(
                unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut users) },
                0
            );
            let users = crate::windows::Local(users);
            let restricted = SID_AND_ATTRIBUTES {
                Sid: users.0,
                Attributes: 0,
            };
            let mut raw = ptr::null_mut();
            // SAFETY: live token/SID and valid output; no privilege-deletion array.
            assert_ne!(
                unsafe {
                    CreateRestrictedToken(
                        original.as_raw_handle(),
                        DISABLE_MAX_PRIVILEGE,
                        0,
                        ptr::null(),
                        0,
                        ptr::null(),
                        1,
                        &restricted,
                        &mut raw,
                    )
                },
                0
            );
            // SAFETY: new token handle uniquely transferred to RAII.
            let token = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: this API accepts a restricted primary token and applies
            // it only to this owned synchronous test thread. SetThreadToken
            // requires an impersonation token instead.
            assert_ne!(
                unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) },
                0,
                "restricted token impersonation: {}",
                io::Error::last_os_error()
            );
            struct Revert;
            impl Drop for Revert {
                fn drop(&mut self) {
                    unsafe {
                        RevertToSelf();
                    }
                }
            }
            let _revert = Revert;
            assert_eq!(
                std::fs::File::open(path).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        })
        .join()
        .unwrap();
        let directory = page.directory.clone();
        let path = page.path.clone();
        std::fs::write(directory.join("unexpected"), b"retain").unwrap();
        drop(page);
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(directory.join("unexpected")).unwrap(),
            b"retain"
        );
        std::fs::remove_file(directory.join("unexpected")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
