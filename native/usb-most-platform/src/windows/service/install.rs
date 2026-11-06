//! Fixed-name SCM operations. Refuse foreign services, never use a shell.
use super::NAME;
use crate::windows::{binary_root, denied, invalid, wide};
use std::{
    ffi::OsStr,
    io,
    path::PathBuf,
    ptr, thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST,
        ERROR_SERVICE_NOT_ACTIVE,
    },
    System::Services::*,
};
struct Handle(SC_HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: uniquely owned successful SCM handle, closed exactly once.
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
fn handle(raw: SC_HANDLE) -> io::Result<Handle> {
    if raw.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(raw))
    }
}
pub fn executable() -> io::Result<PathBuf> {
    Ok(binary_root()?.join("usb-most-desktop.exe"))
}
fn command() -> io::Result<Vec<u16>> {
    let path = executable()?;
    let text = path.to_str().ok_or_else(invalid)?;
    if text.contains('"') {
        return Err(invalid());
    }
    wide(OsStr::new(&format!("\"{text}\" --service")))
}
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub active: bool,
    pub start: u32,
    /// None only when restoring a legacy journal that did not record this field.
    pub sid_type: Option<u32>,
}
pub struct Manager {
    manager: Handle,
}
const ACCESS: u32 = SERVICE_QUERY_CONFIG
    | SERVICE_QUERY_STATUS
    | SERVICE_CHANGE_CONFIG
    | SERVICE_START
    | SERVICE_STOP
    | 0x10000; // DELETE
impl Manager {
    pub fn open() -> io::Result<Self> {
        // SAFETY: local active SCM database; fixed administrative rights.
        Ok(Self {
            manager: handle(unsafe {
                OpenSCManagerW(
                    ptr::null(),
                    ptr::null(),
                    SC_MANAGER_CONNECT | SC_MANAGER_CREATE_SERVICE,
                )
            })?,
        })
    }
    fn open_service(&self) -> io::Result<Option<Handle>> {
        let name = wide(OsStr::new(NAME))?;
        // SAFETY: borrowed live SCM handle and terminated fixed service name.
        let raw = unsafe { OpenServiceW(self.manager.0, name.as_ptr(), ACCESS) };
        match handle(raw) {
            Ok(value) => {
                validate(&value)?;
                sid_type(&value)?;
                Ok(Some(value))
            }
            Err(e) if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn snapshot(&self) -> io::Result<Option<Snapshot>> {
        let Some(service) = self.open_service()? else {
            return Ok(None);
        };
        let state = status(&service)?;
        if !matches!(state, SERVICE_STOPPED | SERVICE_RUNNING) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(Some(Snapshot {
            active: state == SERVICE_RUNNING,
            start: validate(&service)?,
            sid_type: Some(sid_type(&service)?),
        }))
    }
    pub fn activate(&self) -> io::Result<()> {
        let service = match self.open_service()? {
            Some(value) => value,
            None => {
                let name = wide(OsStr::new(NAME))?;
                let display = wide(OsStr::new("USB Мост"))?;
                let binary = command()?;
                // SAFETY: owned strings and local manager; null account means
                // LocalSystem, no credentials/dependencies/interactive desktop.
                handle(unsafe {
                    CreateServiceW(
                        self.manager.0,
                        name.as_ptr(),
                        display.as_ptr(),
                        ACCESS,
                        SERVICE_WIN32_OWN_PROCESS,
                        SERVICE_AUTO_START,
                        SERVICE_ERROR_NORMAL,
                        binary.as_ptr(),
                        ptr::null(),
                        ptr::null_mut(),
                        ptr::null(),
                        ptr::null(),
                        ptr::null(),
                    )
                })?
            }
        };
        // Firewall rules bind to this service identity. With SID_TYPE_NONE,
        // loopback works but real LAN traffic cannot match the service rule.
        // This adds the service SID without restricting or broadening its ACLs.
        set_sid_type(&service, SERVICE_SID_TYPE_UNRESTRICTED)?;
        set_start(&service, SERVICE_AUTO_START)?;
        start(&service)
    }
    pub fn stop(&self) -> io::Result<()> {
        if let Some(service) = self.open_service()? {
            stop(&service)?;
        }
        Ok(())
    }
    pub fn restore(&self, previous: Option<Snapshot>) -> io::Result<()> {
        let Some(service) = self.open_service()? else {
            return if previous.is_none() {
                Ok(())
            } else {
                Err(io::ErrorKind::NotFound.into())
            };
        };
        match previous {
            None => {
                stop(&service)?;
                // SAFETY: exact validated owned service, stopped before deletion.
                if unsafe { DeleteService(service.0) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            }
            Some(old) => {
                if old.sid_type.is_some_and(|v| {
                    !matches!(v, SERVICE_SID_TYPE_NONE | SERVICE_SID_TYPE_UNRESTRICTED)
                }) {
                    return Err(invalid());
                }
                let identity_changed = match old.sid_type {
                    Some(value) => sid_type(&service)? != value,
                    None => false,
                };
                if !old.active || identity_changed {
                    stop(&service)?;
                }
                // Old journals never changed the SID. Preserve the observed
                // setting instead of inventing a previous value for them.
                if let Some(value) = old.sid_type {
                    set_sid_type(&service, value)?;
                }
                set_start(&service, old.start)?;
                if old.active {
                    start(&service)?;
                }
                Ok(())
            }
        }
    }
}
fn sid_type(service: &Handle) -> io::Result<u32> {
    let mut info = SERVICE_SID_INFO::default();
    let mut needed = 0;
    // SAFETY: live service handle and aligned exact-size writable SID structure.
    if unsafe {
        QueryServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            (&mut info as *mut SERVICE_SID_INFO).cast(),
            size_of::<SERVICE_SID_INFO>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // Never replace an administrator's restricted token with a less restrictive
    // one. Only the known legacy/default and current product states are managed.
    if !matches!(
        info.dwServiceSidType,
        SERVICE_SID_TYPE_NONE | SERVICE_SID_TYPE_UNRESTRICTED
    ) {
        return Err(denied());
    }
    Ok(info.dwServiceSidType)
}
fn set_sid_type(service: &Handle, desired: u32) -> io::Result<()> {
    if !matches!(
        desired,
        SERVICE_SID_TYPE_NONE | SERVICE_SID_TYPE_UNRESTRICTED
    ) {
        return Err(invalid());
    }
    if sid_type(service)? == desired {
        return Ok(());
    }
    // A token change takes effect on the next start. Quiescence belongs to the
    // deployment transaction; do not interrupt a running session implicitly.
    if status(service)? != SERVICE_STOPPED {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let info = SERVICE_SID_INFO {
        dwServiceSidType: desired,
    };
    // SAFETY: initialized borrowed structure, synchronous API, validated handle.
    if unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_SERVICE_SID_INFO,
            (&info as *const SERVICE_SID_INFO).cast(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if sid_type(service)? != desired {
        return Err(invalid());
    }
    Ok(())
}
fn process_status(service: &Handle) -> io::Result<SERVICE_STATUS_PROCESS> {
    let mut data = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0;
    // SAFETY: initialized exact-size writable status structure and live handle.
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut data as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(data)
}
fn status(service: &Handle) -> io::Result<u32> {
    Ok(process_status(service)?.dwCurrentState)
}
fn wait(service: &Handle, expected: u32, deadline: Duration) -> io::Result<()> {
    let end = Instant::now() + deadline;
    loop {
        let current = status(service)?;
        if current == expected {
            return Ok(());
        }
        if expected == SERVICE_RUNNING && current == SERVICE_STOPPED {
            return Err(io::Error::other("service_start_failed"));
        }
        if Instant::now() >= end {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}
fn start(service: &Handle) -> io::Result<()> {
    // SAFETY: no startup argv (fixed protected configuration), owned handle.
    if unsafe { StartServiceW(service.0, 0, ptr::null()) } == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(e);
        }
    }
    wait(service, SERVICE_RUNNING, Duration::from_secs(35))
}
fn stop(service: &Handle) -> io::Result<()> {
    let observed = process_status(service)?;
    let current = observed.dwCurrentState;
    if current == SERVICE_STOPPED {
        return Ok(());
    }
    // Pin the exact process before stopping. STOPPED can be reported a few
    // milliseconds before the executable image is released by process exit.
    let process = if observed.dwProcessId != 0 {
        use std::os::windows::io::FromRawHandle;
        // SAFETY: SCM supplied PID; synchronization only, no kill/write rights.
        let raw = unsafe {
            windows_sys::Win32::System::Threading::OpenProcess(0x00100000, 0, observed.dwProcessId)
        };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(87) {
                return Err(error);
            }
            None // Process already exited before OpenProcess.
        } else {
            // SAFETY: successful OpenProcess transfers one unique owned handle.
            Some(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) })
        }
    } else {
        None
    };
    if current != SERVICE_STOP_PENDING {
        let mut output = SERVICE_STATUS::default();
        // SAFETY: initialized writable structure, exact owned service only.
        if unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut output) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
                return Err(e);
            }
        }
    }
    wait(service, SERVICE_STOPPED, Duration::from_secs(120))?;
    if let Some(process) = process {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: pinned live synchronization handle, bounded wait, no mutation.
        match unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                process.as_raw_handle(),
                5000,
            )
        } {
            0 => {}
            258 => return Err(io::ErrorKind::TimedOut.into()),
            _ => return Err(io::Error::last_os_error()),
        }
    }
    Ok(())
}
fn set_start(service: &Handle, start: u32) -> io::Result<()> {
    if !matches!(
        start,
        SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED
    ) {
        return Err(invalid());
    }
    // SAFETY: change only start type; protected path/account stay intact.
    if unsafe {
        ChangeServiceConfigW(
            service.0,
            SERVICE_NO_CHANGE,
            start,
            SERVICE_NO_CHANGE,
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn validate(service: &Handle) -> io::Result<u32> {
    let mut needed = 0;
    // SAFETY: documented size query, no writable data buffer yet.
    let result = unsafe { QueryServiceConfigW(service.0, ptr::null_mut(), 0, &mut needed) };
    if result != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || !(size_of::<QUERY_SERVICE_CONFIGW>()..=8192).contains(&(needed as usize))
    {
        return Err(invalid());
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: usize storage provides pointer alignment and at least needed bytes.
    let config = buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>();
    if unsafe { QueryServiceConfigW(service.0, config, needed, &mut needed) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful API filled this aligned structure; storage stays live.
    let config = unsafe { &*config };
    let text = |value: *const u16| -> io::Result<Vec<u16>> {
        let base = buffer.as_ptr() as usize;
        let offset = (value as usize).checked_sub(base).ok_or_else(invalid)?;
        let bytes = buffer.len() * size_of::<usize>();
        if !offset.is_multiple_of(2)
            || offset < size_of::<QUERY_SERVICE_CONFIGW>()
            || offset >= bytes
        {
            return Err(invalid());
        }
        // SAFETY: bounds/alignment checked; Windows filled this zeroed allocation.
        let units = unsafe { std::slice::from_raw_parts(value, (bytes - offset) / 2) };
        let end = units.iter().position(|v| *v == 0).ok_or_else(invalid)?;
        Ok(units[..end].to_vec())
    };
    let expected = command()?;
    if config.dwServiceType != SERVICE_WIN32_OWN_PROCESS
        || !matches!(
            config.dwStartType,
            SERVICE_AUTO_START | SERVICE_DEMAND_START | SERVICE_DISABLED
        )
        || config.dwErrorControl != SERVICE_ERROR_NORMAL
        || text(config.lpBinaryPathName)? != expected[..expected.len() - 1]
        || text(config.lpServiceStartName)? != "LocalSystem".encode_utf16().collect::<Vec<_>>()
        || (!config.lpLoadOrderGroup.is_null() && !text(config.lpLoadOrderGroup)?.is_empty())
        || (!config.lpDependencies.is_null() && !text(config.lpDependencies)?.is_empty())
    {
        return Err(denied());
    }
    Ok(config.dwStartType)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an owned Windows VM after product bootstrap"]
    fn installed_service_has_firewall_identity() {
        assert!(
            std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
                || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
        );
        let service = Manager::open().unwrap().snapshot().unwrap().unwrap();
        assert!(service.active);
        assert_eq!(service.sid_type, Some(SERVICE_SID_TYPE_UNRESTRICTED));
    }
}
