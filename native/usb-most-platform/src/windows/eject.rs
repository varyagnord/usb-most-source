//! OS-confirmed removal of one exact import on the private signed controller.
//! No force removal, global unplug, arbitrary PnP IDs or remote filesystem paths.
use super::driver::{Controller, INSTANCE};
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};
use usb_most_protocol::eject::Outcome;
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::*,
    Foundation::{ERROR_SUCCESS, GetLastError, SetLastError},
    Security::*,
    System::{
        Registry::{REG_DWORD, REG_MULTI_SZ, REG_SZ},
        Threading::{GetCurrentThread, OpenThreadToken},
    },
};
use zeroize::Zeroizing;

static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Slot(&'static AtomicUsize);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn worker(
    workers: &'static AtomicUsize,
    operation: impl FnOnce() -> Outcome + Send + 'static,
) -> io::Result<thread::JoinHandle<Outcome>> {
    workers
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 4).then_some(n + 1)
        })
        .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
    let slot = Slot(workers);
    thread::Builder::new()
        .name("usb-most-eject".into())
        .spawn(move || {
            let _slot = slot;
            operation()
        })
}

/// Capture only from the successful attach's port, private listener and alias.
/// The receiver must retain its actual USB proxy/socket and operation ledger
/// while the returned worker runs. A UI deadline must not drop that attachment.
pub struct OwnedImport {
    controller: Controller,
    port: i32,
    local_port: u16,
    alias: Zeroizing<[u8; 32]>,
}
impl OwnedImport {
    pub fn capture(
        controller: &Controller,
        port: i32,
        local_port: u16,
        alias: &[u8; 32],
    ) -> io::Result<Self> {
        if !controller.matches_import(port, local_port, alias)? {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(Self {
            controller: controller.clone(),
            port,
            local_port,
            alias: Zeroizing::new(*alias),
        })
    }
    /// One dedicated thread per accepted operation, bounded to four process-wide.
    /// Dropping a JoinHandle does not cancel/free the operation. Its owned input,
    /// controller handle and veto buffer remain alive through the real OS return.
    pub fn start_eject(self) -> io::Result<thread::JoinHandle<Outcome>> {
        worker(&WORKERS, move || self.eject())
    }
    fn current(&self) -> Result<(), Outcome> {
        match self
            .controller
            .matches_import(self.port, self.local_port, &self.alias)
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(Outcome::Unavailable),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Err(Outcome::Denied),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(Outcome::Busy),
            Err(_) => Err(Outcome::Unavailable),
        }
    }
    fn eject(&self) -> Outcome {
        let run = || -> Result<Outcome, Outcome> {
            self.current()?;
            let candidate = storage(self.controller.instance, self.port as u32)?;
            // Enabling an already-held privilege is scoped to this dedicated
            // worker's impersonation token. The process token is never changed.
            let _privilege = RemovalPrivilege::enable()?;
            self.current()?;
            if storage(self.controller.instance, self.port as u32)? != candidate {
                return Err(Outcome::Unavailable);
            }
            let mut veto = PNP_VetoTypeUnknown;
            let mut name = Zeroizing::new([0u16; 260]);
            // SAFETY: exact validated local DEVINST, synchronous call; bounded
            // writable UTF16 buffer and veto variable remain owned until return.
            // A non-null name buffer suppresses interactive PnP dialogs. Its
            // contents may contain private paths and are zeroized, never logged.
            let result = unsafe {
                CM_Request_Device_EjectW(
                    candidate.node,
                    &mut veto,
                    name.as_mut_ptr(),
                    name.len() as u32,
                    0,
                )
            };
            // Numeric categories only, never the veto string or a device path.
            #[cfg(debug_assertions)]
            eprintln!("USB Most OS ejection: result={result} veto={veto}");
            Ok(outcome(result, veto))
        };
        run().unwrap_or_else(|outcome| outcome)
    }
}

#[derive(PartialEq, Eq)]
struct Candidate {
    node: u32,
    id: String,
    hub: u32,
    hub_id: String,
}
fn config_result(result: CONFIGRET) -> Result<(), Outcome> {
    match result {
        CR_SUCCESS => Ok(()),
        CR_ACCESS_DENIED => Err(Outcome::Denied),
        CR_NO_SUCH_DEVNODE => Err(Outcome::Unavailable),
        _ => Err(Outcome::Unsupported),
    }
}
fn id(node: u32) -> Result<String, Outcome> {
    let mut units = [0u16; 512];
    // SAFETY: fixed writable UTF16 buffer remains live through synchronous CM call.
    config_result(unsafe { CM_Get_Device_IDW(node, units.as_mut_ptr(), units.len() as u32, 0) })?;
    let end = units
        .iter()
        .position(|v| *v == 0)
        .ok_or(Outcome::Unsupported)?;
    if end == 0 {
        return Err(Outcome::Unavailable);
    }
    String::from_utf16(&units[..end])
        .map(|s| s.to_ascii_uppercase())
        .map_err(|_| Outcome::Unsupported)
}
fn number(node: u32, property: u32) -> Result<u32, Outcome> {
    let (mut value, mut kind, mut size) = (0u32, 0u32, 4u32);
    // SAFETY: aligned DWORD and writable type/length, all live through return.
    config_result(unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            property,
            &mut kind,
            (&mut value as *mut u32).cast(),
            &mut size,
            0,
        )
    })?;
    if size != 4 || kind != REG_DWORD {
        return Err(Outcome::Unsupported);
    }
    Ok(value)
}
fn strings(node: u32, property: u32, multi: bool) -> Result<Vec<String>, Outcome> {
    let mut units = [0u16; 2048];
    let (mut kind, mut size) = (0u32, (units.len() * 2) as u32);
    // SAFETY: aligned bounded UTF16 storage and byte length, unchanged until
    // actual return. No pointer to this buffer escapes the function.
    config_result(unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            property,
            &mut kind,
            units.as_mut_ptr().cast(),
            &mut size,
            0,
        )
    })?;
    if size < 2
        || size as usize > units.len() * 2
        || !size.is_multiple_of(2)
        || kind != if multi { REG_MULTI_SZ } else { REG_SZ }
    {
        return Err(Outcome::Unsupported);
    }
    let units = &units[..size as usize / 2];
    if units.last() != Some(&0) || (multi && (units.len() < 2 || units[units.len() - 2] != 0)) {
        return Err(Outcome::Unsupported);
    }
    let mut out = Vec::new();
    for part in units.split(|v| *v == 0) {
        if part.is_empty() {
            break;
        }
        out.push(
            String::from_utf16(part)
                .map_err(|_| Outcome::Unsupported)?
                .to_ascii_uppercase(),
        );
        if out.len() > 64 {
            return Err(Outcome::Unsupported);
        }
    }
    if !multi && out.len() != 1 {
        return Err(Outcome::Unsupported);
    }
    Ok(out)
}
fn children(parent: u32) -> Result<Vec<u32>, Outcome> {
    let mut node = 0;
    // SAFETY: one writable DEVINST output; parent belongs to the local CM tree.
    let result = unsafe { CM_Get_Child(&mut node, parent, 0) };
    if result == CR_NO_SUCH_DEVNODE {
        return Ok(vec![]);
    }
    config_result(result)?;
    let mut result = Vec::new();
    loop {
        if node == 0 || result.len() >= 256 || result.contains(&node) {
            return Err(Outcome::Unsupported);
        }
        result.push(node);
        let mut next = 0;
        // SAFETY: synchronous scalar input/output, no retained pointer.
        let status = unsafe { CM_Get_Sibling(&mut next, node, 0) };
        if status == CR_NO_SUCH_DEVNODE {
            break;
        }
        config_result(status)?;
        node = next;
    }
    Ok(result)
}
fn parent_is(node: u32, parent: u32) -> Result<(), Outcome> {
    let mut value = 0;
    // SAFETY: writable scalar output remains live through synchronous call.
    config_result(unsafe { CM_Get_Parent(&mut value, node, 0) })?;
    if value != parent {
        return Err(Outcome::Unavailable);
    }
    Ok(())
}
fn storage(root: u32, port: u32) -> Result<Candidate, Outcome> {
    if id(root)? != INSTANCE {
        return Err(Outcome::Unavailable);
    }
    let mut found = None;
    for hub in children(root)? {
        let hub_id = id(hub)?;
        if !hub_id.starts_with("USB\\ROOT_HUB30\\") {
            continue;
        }
        if strings(hub, CM_DRP_SERVICE, false)? != ["USBHUB3"] {
            return Err(Outcome::Unsupported);
        }
        parent_is(hub, root)?;
        for node in children(hub)? {
            if number(node, CM_DRP_ADDRESS)? != port {
                continue;
            }
            if found.is_some() {
                return Err(Outcome::Unsupported);
            }
            let device_id = id(node)?;
            if !device_id.starts_with("USB\\VID_") {
                return Err(Outcome::Unsupported);
            }
            if strings(node, CM_DRP_ENUMERATOR_NAME, false)? != ["USB"] {
                return Err(Outcome::Unsupported);
            }
            let service = strings(node, CM_DRP_SERVICE, false)?;
            if service != ["USBSTOR"] && service != ["UASPSTOR"] {
                return Err(Outcome::Unsupported);
            }
            let classes = strings(node, CM_DRP_COMPATIBLEIDS, true)?;
            if !classes
                .iter()
                .any(|s| s == "USB\\CLASS_08" || s.starts_with("USB\\CLASS_08&"))
            {
                return Err(Outcome::Unsupported);
            }
            let capabilities = number(node, CM_DRP_CAPABILITIES)?;
            if capabilities & CM_DEVCAP_DOCKDEVICE != 0 || capabilities & CM_DEVCAP_REMOVABLE == 0 {
                return Err(Outcome::Unsupported);
            }
            let (mut status, mut problem) = (0, 0);
            // SAFETY: both scalar outputs live through synchronous call.
            config_result(unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) })?;
            if status & DN_STARTED == 0 || problem != 0 {
                return Err(Outcome::Unavailable);
            }
            parent_is(node, hub)?;
            found = Some(Candidate {
                node,
                id: device_id,
                hub,
                hub_id: hub_id.clone(),
            });
        }
    }
    found.ok_or(Outcome::Unavailable)
}

struct RemovalPrivilege;
impl RemovalPrivilege {
    fn enable() -> Result<Self, Outcome> {
        // SAFETY: only called on a newly-created dedicated worker. No caller
        // impersonation context is replaced and no async suspension occurs.
        if unsafe { ImpersonateSelf(SecurityImpersonation) } == 0 {
            return Err(Outcome::Denied);
        }
        let guard = Self;
        let mut raw = ptr::null_mut();
        // SAFETY: current-thread pseudo-handle; output immediately RAII-owned.
        if unsafe {
            OpenThreadToken(
                GetCurrentThread(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                0,
                &mut raw,
            )
        } == 0
        {
            return Err(Outcome::Denied);
        }
        // SAFETY: successful OpenThreadToken transfers unique HANDLE ownership.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut luid = Default::default();
        // SAFETY: fixed terminated privilege name and writable LUID output.
        if unsafe { LookupPrivilegeValueW(ptr::null(), SE_LOAD_DRIVER_NAME, &mut luid) } == 0 {
            return Err(Outcome::Denied);
        }
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: one validated privilege in live storage, no previous-state
        // pointer. This can enable only a privilege the token already holds.
        let enabled = unsafe {
            SetLastError(ERROR_SUCCESS);
            AdjustTokenPrivileges(
                token.as_raw_handle(),
                0,
                &privileges,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            ) != 0
                && GetLastError() == ERROR_SUCCESS
        };
        if !enabled {
            return Err(Outcome::Denied);
        }
        Ok(guard)
    }
}
impl Drop for RemovalPrivilege {
    fn drop(&mut self) {
        // SAFETY: same dedicated thread that called ImpersonateSelf. Even if
        // restoration fails, this thread immediately terminates after returning
        // the result; it never executes another operation in a thread pool.
        unsafe {
            RevertToSelf();
        }
    }
}

#[allow(non_upper_case_globals)] // Preserve the SDK's PNP_Veto* constant names.
fn outcome(result: CONFIGRET, veto: PNP_VETO_TYPE) -> Outcome {
    match result {
        CR_SUCCESS => Outcome::Safe,
        CR_ACCESS_DENIED => Outcome::Denied,
        CR_NO_SUCH_DEVNODE => Outcome::Unavailable,
        CR_REMOVE_VETOED => match veto {
            PNP_VetoInsufficientRights => Outcome::Denied,
            PNP_VetoAlreadyRemoved => Outcome::Unavailable,
            PNP_VetoLegacyDevice
            | PNP_VetoLegacyDriver
            | PNP_VetoNonDisableable
            | PNP_VetoIllegalDeviceRequest
            | PNP_VetoInsufficientPower => Outcome::Unsupported,
            _ => Outcome::Busy,
        },
        _ => Outcome::Uncertain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropping_waiter_does_not_release_the_bounded_os_worker() {
        static TEST_WORKERS: AtomicUsize = AtomicUsize::new(0);
        let mut releases = Vec::new();
        let (done, completed) = std::sync::mpsc::channel();
        for _ in 0..4 {
            let (release, wait) = std::sync::mpsc::channel();
            let done = done.clone();
            let handle = worker(&TEST_WORKERS, move || {
                wait.recv().unwrap();
                done.send(()).unwrap();
                Outcome::Safe
            })
            .unwrap();
            drop(handle);
            releases.push(release);
        }
        assert_eq!(TEST_WORKERS.load(Ordering::Acquire), 4);
        assert_eq!(
            worker(&TEST_WORKERS, || Outcome::Safe).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        for release in releases {
            release.send(()).unwrap();
        }
        for _ in 0..4 {
            completed
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while TEST_WORKERS.load(Ordering::Acquire) != 0 {
            assert!(std::time::Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(
            worker(&TEST_WORKERS, || Outcome::Busy)
                .unwrap()
                .join()
                .unwrap(),
            Outcome::Busy
        );
        assert_eq!(TEST_WORKERS.load(Ordering::Acquire), 0);
    }
    #[test]
    fn removal_privilege_is_scoped_to_its_dedicated_thread() {
        thread::spawn(|| {
            let mut raw = ptr::null_mut();
            // SAFETY: dedicated thread with no inherited impersonation; scalar output.
            assert_eq!(
                unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &mut raw) },
                0
            );
            assert_eq!(
                unsafe { GetLastError() },
                windows_sys::Win32::Foundation::ERROR_NO_TOKEN
            );
            // The hosted VM may lack this privilege in a restricted session.
            // Both outcomes must leave the process token and caller untouched.
            match RemovalPrivilege::enable() {
                Ok(guard) => {
                    assert_ne!(
                        unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &mut raw) },
                        0
                    );
                    // SAFETY: successful token query uniquely owns this HANDLE.
                    drop(unsafe { OwnedHandle::from_raw_handle(raw) });
                    drop(guard);
                }
                Err(result) => assert_eq!(result, Outcome::Denied),
            }
            assert_eq!(
                unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &mut raw) },
                0
            );
            assert_eq!(
                unsafe { GetLastError() },
                windows_sys::Win32::Foundation::ERROR_NO_TOKEN
            );
        })
        .join()
        .unwrap();
    }
    #[test]
    fn only_positive_os_reply_means_safe_removal() {
        assert_eq!(outcome(CR_SUCCESS, PNP_VetoTypeUnknown), Outcome::Safe);
        for veto in [
            PNP_VetoOutstandingOpen,
            PNP_VetoPendingClose,
            PNP_VetoWindowsApp,
            PNP_VetoWindowsService,
            PNP_VetoDevice,
        ] {
            assert_eq!(outcome(CR_REMOVE_VETOED, veto), Outcome::Busy);
        }
        assert_eq!(
            outcome(CR_REMOVE_VETOED, PNP_VetoInsufficientRights),
            Outcome::Denied
        );
        assert_eq!(
            outcome(CR_REMOVE_VETOED, PNP_VetoAlreadyRemoved),
            Outcome::Unavailable
        );
        assert_eq!(outcome(CR_FAILURE, PNP_VetoTypeUnknown), Outcome::Uncertain);
    }
}
