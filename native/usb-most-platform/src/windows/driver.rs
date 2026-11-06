//! Pinned usbip-win2 0.9.8.1 ABI. Our private PnP instance only; no global port operations.
use super::{Local, administrator_descriptor, denied, invalid};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::{Authorization::*, *},
        Storage::FileSystem::{CreateFileW, OPEN_EXISTING, READ_CONTROL},
        System::IO::{CancelSynchronousIo, DeviceIoControl},
    },
    core::GUID,
};
use zeroize::{Zeroize, Zeroizing};
pub const INSTANCE: &str = "ROOT\\USBMOST\\0000";
pub const HARDWARE: &str = "ROOT\\USBIP_WIN2\\UDE";
pub const INTERFACE: GUID = GUID::from_u128(0xb4030c06_dc5f_4fcc_87eb_e5515a0935c0);
pub const USB_CLASS: GUID = GUID::from_u128(0x36fc9e60_c465_11cf_8056_444553540000);
const PLUGIN_ONCE: u32 = 0x22e018;
const PLUGOUT: u32 = 0x22e004;
const GET_IMPORTED: u32 = 0x22e008;
static QUARANTINED: AtomicBool = AtomicBool::new(false);
static CONTROL: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Set(HDEVINFO);
impl Drop for Set {
    fn drop(&mut self) {
        // SAFETY: exactly one ownership of a successful SetupDi information set.
        unsafe {
            SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}
fn info() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    }
}
#[derive(Clone)]
pub struct Controller {
    file: Arc<File>,
    #[cfg(feature = "windows-eject")]
    pub(super) instance: u32,
    #[cfg(test)]
    path: Arc<Vec<u16>>,
}
impl Controller {
    pub fn open() -> io::Result<Self> {
        if QUARANTINED.load(Ordering::Acquire) {
            return Err(io::Error::other("driver_restart_required"));
        }
        // SAFETY: fixed interface GUID and valid flags; result immediately RAII-owned.
        let raw = unsafe {
            SetupDiGetClassDevsW(
                &INTERFACE,
                ptr::null(),
                ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
        };
        if raw == -1 {
            return Err(io::Error::last_os_error());
        }
        let set = Set(raw);
        for index in 0..64 {
            let mut interface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: live set, correct initialized structure and writable output.
            if unsafe {
                SetupDiEnumDeviceInterfaces(set.0, ptr::null(), &INTERFACE, index, &mut interface)
            } == 0
            {
                break;
            }
            let mut needed = 0;
            // SAFETY: size query only; API permits a null detail buffer with size zero.
            unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &interface,
                    ptr::null_mut(),
                    0,
                    &mut needed,
                    ptr::null_mut(),
                );
            }
            if !(8..=65536).contains(&needed) {
                return Err(invalid());
            }
            // u64 storage provides sufficient alignment for both supported 64-bit ABIs.
            let mut data = vec![0u64; (needed as usize).div_ceil(8)];
            let detail = data
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            let mut dev = info();
            // SAFETY: buffer is aligned, zeroed and at least needed bytes; cbSize is fixed ABI.
            unsafe {
                (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            }
            // SAFETY: both output buffers remain live and writable through this synchronous call.
            if unsafe {
                SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &interface,
                    detail,
                    needed,
                    ptr::null_mut(),
                    &mut dev,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut id = [0u16; 512];
            // SAFETY: initialized dev info and bounded writable UTF16 buffer.
            if unsafe {
                SetupDiGetDeviceInstanceIdW(
                    set.0,
                    &dev,
                    id.as_mut_ptr(),
                    id.len() as u32,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let end = id.iter().position(|v| *v == 0).ok_or_else(invalid)?;
            let id = String::from_utf16(&id[..end]).map_err(|_| invalid())?;
            if !id.eq_ignore_ascii_case(INSTANCE) {
                continue;
            }
            // SAFETY: validated detail buffer length and fixed offset; UTF16 is copied before free.
            let units = unsafe {
                std::slice::from_raw_parts(
                    data.as_ptr().cast::<u8>().add(4).cast::<u16>(),
                    (needed as usize - 4) / 2,
                )
            };
            let end = units.iter().position(|v| *v == 0).ok_or_else(invalid)?;
            let path = &units[..=end];
            // SAFETY: terminated validated interface path and synchronous non-inheritable handle.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                    3,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful CreateFile transfers unique handle ownership to File.
            let file = unsafe { File::from_raw_handle(handle) };
            let mut sd = ptr::null_mut();
            // SAFETY: borrowed live kernel handle; descriptor ownership transferred to guard.
            let error = unsafe {
                GetSecurityInfo(
                    file.as_raw_handle(),
                    SE_KERNEL_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut sd,
                )
            };
            let sd = Local(sd);
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            if sd.0.is_null() {
                return Err(invalid());
            }
            administrator_descriptor(sd.0, false)?;
            return Ok(Self {
                file: Arc::new(file),
                #[cfg(feature = "windows-eject")]
                instance: dev.DevInst,
                #[cfg(test)]
                path: Arc::new(path.to_vec()),
            });
        }
        Err(io::ErrorKind::NotFound.into())
    }
    pub fn attach_once(&self, local_port: u16, alias: &[u8; 32]) -> io::Result<i32> {
        if local_port == 0
            || alias[30..] != [0, 0]
            || !alias[..30].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(invalid());
        }
        let mut data = Zeroizing::new(vec![0u8; 1124]);
        data[..4].copy_from_slice(&1124u32.to_le_bytes());
        data[12..44].copy_from_slice(alias);
        let port = local_port.to_string();
        data[44..44 + port.len()].copy_from_slice(port.as_bytes());
        data[76..85].copy_from_slice(b"127.0.0.1");
        let result = self.control(PLUGIN_ONCE, data, Duration::from_secs(90))?;
        let port = i32::from_le_bytes(result[4..8].try_into().unwrap());
        if !(1..=1024).contains(&port) {
            return Err(invalid());
        }
        Ok(port)
    }
    pub fn detach(&self, port: i32) -> io::Result<()> {
        if !(1..=1024).contains(&port) {
            return Err(denied());
        }
        let mut data = Zeroizing::new(vec![0u8; 8]);
        data[..4].copy_from_slice(&8u32.to_le_bytes());
        data[4..8].copy_from_slice(&port.to_le_bytes());
        self.control(PLUGOUT, data, Duration::from_secs(10))
            .map(|_| ())
    }
    /// Read-only ownership check before OS ejection. The driver must still
    /// associate this exact one-use alias/private proxy with the confirmed port.
    /// Never treat a matching model, serial, port alone or location_hash as proof.
    pub fn matches_import(&self, port: i32, local_port: u16, alias: &[u8; 32]) -> io::Result<bool> {
        use crate::import_identity;
        // Validate caller dimensions before opening a driver operation.
        import_identity::validate(port, local_port, alias)?;
        let mut bytes = Zeroizing::new(vec![0; import_identity::CAPACITY]);
        bytes[..4].copy_from_slice(&(import_identity::STRUCT_SIZE as u32).to_le_bytes());
        let result = self.control(GET_IMPORTED, bytes, Duration::from_secs(5))?;
        import_identity::matches(&result, port, local_port, alias)
    }
    /// Teardown of an exact live import, NOT OS-confirmed safe storage removal.
    /// Hold the process-wide control gate through identity query and detach so
    /// our concurrent imports cannot reuse the numeric port between those calls.
    /// A departed/replaced import is already gone and must not be detached again.
    pub fn detach_owned(&self, port: i32, local_port: u16, alias: &[u8; 32]) -> io::Result<bool> {
        use crate::import_identity;
        import_identity::validate(port, local_port, alias)?;
        let serial = CONTROL
            .try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        let mut bytes = Zeroizing::new(vec![0; import_identity::CAPACITY]);
        bytes[..4].copy_from_slice(&(import_identity::STRUCT_SIZE as u32).to_le_bytes());
        let imports = self.control_locked(GET_IMPORTED, bytes, Duration::from_secs(5), &serial)?;
        if !import_identity::matches(&imports, port, local_port, alias)? {
            return Ok(false);
        }
        let mut data = Zeroizing::new(vec![0u8; 8]);
        data[..4].copy_from_slice(&8u32.to_le_bytes());
        data[4..8].copy_from_slice(&port.to_le_bytes());
        self.control_locked(PLUGOUT, data, Duration::from_secs(10), &serial)?;
        Ok(true)
    }
    fn control(
        &self,
        code: u32,
        data: Zeroizing<Vec<u8>>,
        deadline: Duration,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        let serial = CONTROL
            .try_lock()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        self.control_locked(code, data, deadline, &serial)
    }
    fn control_locked(
        &self,
        code: u32,
        data: Zeroizing<Vec<u8>>,
        deadline: Duration,
        _serial: &std::sync::MutexGuard<'_, ()>,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        if QUARANTINED.load(Ordering::Acquire) {
            return Err(io::Error::other("driver_restart_required"));
        }
        let file = self.file.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let job = thread::Builder::new()
            .name("usb-most-driver".into())
            .spawn(move || {
                let result = ioctl(&file, code, data);
                if let Err(late) = tx.send(result)
                    && code == PLUGIN_ONCE
                    && let Ok(data) = late.0
                {
                    let port = i32::from_le_bytes(data[4..8].try_into().unwrap());
                    if (1..=1024).contains(&port) {
                        let mut cleanup = Zeroizing::new(vec![0u8; 8]);
                        cleanup[..4].copy_from_slice(&8u32.to_le_bytes());
                        cleanup[4..8].copy_from_slice(&port.to_le_bytes());
                        let _ = ioctl(&file, PLUGOUT, cleanup);
                    }
                }
            })?;
        match rx.recv_timeout(deadline) {
            Ok(result) => {
                let _ = job.join();
                result
            }
            Err(_) => {
                // SAFETY: thread handle belongs to the live JoinHandle. Cancellation does NOT
                // free its data: the worker retains the File/buffer until DeviceIoControl returns.
                unsafe {
                    CancelSynchronousIo(job.as_raw_handle());
                }
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(result) => {
                        let _ = job.join();
                        result
                    }
                    Err(_) => {
                        // Detached worker owns its outstanding memory/handle, plus late-port cleanup.
                        QUARANTINED.store(true, Ordering::Release);
                        Err(io::ErrorKind::TimedOut.into())
                    }
                }
            }
        }
    }
}
fn ioctl(file: &File, code: u32, mut data: Zeroizing<Vec<u8>>) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut returned = 0;
    let output_size = if code == GET_IMPORTED {
        data.len() as u32
    } else {
        8
    };
    // SAFETY: synchronous METHOD_BUFFERED call. Owned input/output storage and the
    // byte-count variable stay live, exclusively borrowed until actual return.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            data.as_ptr().cast(),
            data.len() as u32,
            data.as_mut_ptr().cast(),
            output_size,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if returned > output_size || (code == PLUGIN_ONCE && returned != 8) {
        return Err(invalid());
    }
    if code == GET_IMPORTED {
        // DeviceIoControl has returned, so the kernel no longer borrows data.
        // Truncate only now; the decoder independently checks the exact layout.
        data[returned as usize..].zeroize();
        data.truncate(returned as usize);
    }
    Ok(data)
}

/// Installer-only: create exactly our root-enumerated instance with its private
/// DACL present BEFORE registration/driver start. Never select another VHCI.
/// The caller stages/verifies signed packages and journals ownership first.
pub fn install_controller(inf: &std::path::Path) -> io::Result<bool> {
    use super::wide;
    use std::ffi::OsStr;
    let inf = std::fs::canonicalize(inf)?;
    if inf.file_name() != Some(OsStr::new("usbip2_ude.inf")) {
        return Err(invalid());
    }
    let inf = wide(inf.as_os_str())?;
    if inf.len() > 260 {
        return Err(invalid());
    }
    let instance = wide(OsStr::new(INSTANCE))?;
    // SAFETY: fixed class GUID, no parent window; unique set is immediately owned.
    let raw = unsafe { SetupDiCreateDeviceInfoList(&USB_CLASS, ptr::null_mut()) };
    if raw == -1 {
        return Err(io::Error::last_os_error());
    }
    let set = Set(raw);
    let mut dev = info();
    // SAFETY: terminated fixed instance and valid initialized structures.
    if unsafe { SetupDiOpenDeviceInfoW(set.0, instance.as_ptr(), ptr::null_mut(), 0, &mut dev) }
        != 0
    {
        // Reuse only a verifiably protected live instance. No broad driver update.
        Controller::open()?;
        return Ok(false);
    }
    let description = wide(OsStr::new("USB Most virtual controller"))?;
    // SAFETY: full explicit root instance ID, initialized class/output and no UI.
    if unsafe {
        SetupDiCreateDeviceInfoW(
            set.0,
            instance.as_ptr(),
            &USB_CLASS,
            description.as_ptr(),
            ptr::null_mut(),
            0,
            &mut dev,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    #[cfg(test)]
    eprintln!("controller: create private device info succeeded");
    let result = (|| {
        let mut hardware = wide(OsStr::new(HARDWARE))?;
        hardware.push(0);
        let acl = wide(OsStr::new("O:SYG:SYD:P(A;;GA;;;SY)(A;;GA;;;BA)"))?;
        for (property, value) in [(SPDRP_HARDWAREID, hardware), (SPDRP_SECURITY_SDS, acl)] {
            // SAFETY: live UTF16 REG_MULTI_SZ/REG_SZ buffers with terminators and exact byte length.
            if unsafe {
                SetupDiSetDeviceRegistryPropertyW(
                    set.0,
                    &mut dev,
                    property,
                    value.as_ptr().cast(),
                    (value.len() * 2) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(test)]
        eprintln!("controller: hardware/security registry properties set before registration");
        // SAFETY: registers only the previously initialized explicit instance.
        if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &dev) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut params = SP_DEVINSTALL_PARAMS_W {
            cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            ..Default::default()
        };
        // SAFETY: correctly sized initialized output for our device.
        if unsafe { SetupDiGetDeviceInstallParamsW(set.0, &dev, &mut params) } == 0 {
            return Err(io::Error::last_os_error());
        }
        params.Flags |= DI_ENUMSINGLEINF | DI_QUIETINSTALL;
        params.DriverPath[..inf.len()].copy_from_slice(&inf);
        // SAFETY: fixed-size struct and bounded terminated DriverPath.
        if unsafe { SetupDiSetDeviceInstallParamsW(set.0, &dev, &params) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: compatible driver list restricted to our HWID and exact signed INF.
        if unsafe { SetupDiBuildDriverInfoList(set.0, &mut dev, SPDIT_COMPATDRIVER) } == 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(test)]
        eprintln!("controller: registration and signed INF enumeration succeeded");
        let installed = (|| {
            // SAFETY: all class-installer calls target only the owned device info.
            if unsafe { SetupDiCallClassInstaller(DIF_SELECTBESTCOMPATDRV, set.0, &dev) } == 0 {
                return Err(io::Error::last_os_error());
            }
            #[cfg(test)]
            eprintln!("controller: compatible signed driver selected");
            let mut selected = SP_DRVINFO_DATA_V2_W {
                cbSize: size_of::<SP_DRVINFO_DATA_V2_W>() as u32,
                ..Default::default()
            };
            // SAFETY: initialized selected-driver output for the exact restricted list.
            if unsafe { SetupDiGetSelectedDriverW(set.0, &dev, &mut selected) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut reboot = 0;
            // SAFETY: documented full installation API for one explicit device/
            // selected signed driver. It runs the complete Windows installation
            // sequence, rather than invoking only the DIF_INSTALLDEVICE phase.
            if unsafe { DiInstallDevice(ptr::null_mut(), set.0, &dev, &selected, 0, &mut reboot) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            if unsafe { SetupDiGetDeviceInstallParamsW(set.0, &dev, &mut params) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(reboot != 0 || params.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0)
        })();
        // SAFETY: paired with successful BuildDriverInfoList, before set destruction.
        unsafe {
            SetupDiDestroyDriverInfoList(set.0, &dev, SPDIT_COMPATDRIVER);
        }
        installed
    })();
    if result.is_err() {
        // SAFETY: this call created this exact instance. Shared driver packages
        // and other controllers/USB hubs are never removed here.
        unsafe {
            SetupDiCallClassInstaller(DIF_REMOVE, set.0, &dev);
        }
    }
    result
}

#[cfg(test)]
mod tests;
