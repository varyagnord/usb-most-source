//! Single own-process SCM entry. Callback state lives until process exit: stop,
//! shutdown and interrogate cannot race a freed callback context. USB and async
//! runtime ownership remain in the safe receiver; SCM never kills its threads.
pub mod install;
use std::{
    ffi::{OsStr, c_void},
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicPtr, Ordering},
    },
};
use windows_sys::Win32::{
    Foundation::{ERROR_CALL_NOT_IMPLEMENTED, ERROR_SERVICE_SPECIFIC_ERROR},
    System::Services::*,
};

pub const NAME: &str = "USBMostReceiver";
struct Report {
    state: u32,
    exit: u32,
}
struct Shared {
    handle: AtomicPtr<c_void>,
    stop: AtomicBool,
    report: Mutex<Report>,
}
struct Entry {
    callback: fn(Control) -> io::Result<()>,
    shared: Arc<Shared>,
}
static ENTRY: OnceLock<Entry> = OnceLock::new();

#[derive(Clone)]
pub struct Control(Arc<Shared>);
impl Control {
    pub fn stopping(&self) -> bool {
        self.0.stop.load(Ordering::Acquire)
    }
    /// Called only after configuration, listeners and owner IPC are ready.
    /// Once STOP_PENDING was published this can never publish RUNNING again.
    pub fn ready(&self) -> io::Result<()> {
        let mut report = self.0.report.lock().map_err(|_| io::ErrorKind::Other)?;
        if self.stopping() || report.state != SERVICE_START_PENDING {
            return Err(io::ErrorKind::Interrupted.into());
        }
        report.state = SERVICE_RUNNING;
        publish(&self.0, &report)
    }
}
fn publish(shared: &Shared, report: &Report) -> io::Result<()> {
    let pending = matches!(report.state, SERVICE_START_PENDING | SERVICE_STOP_PENDING);
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: report.state,
        dwControlsAccepted: if report.state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: if report.exit == 0 {
            0
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
        dwServiceSpecificExitCode: report.exit,
        dwCheckPoint: u32::from(pending),
        dwWaitHint: match report.state {
            SERVICE_START_PENDING => 30_000,
            SERVICE_STOP_PENDING => 120_000,
            _ => 0,
        },
    };
    // SAFETY: handle is published only by successful handler registration and
    // lives through ServiceMain; status is fully initialized, borrowed for call.
    if unsafe { SetServiceStatus(shared.handle.load(Ordering::Acquire), &status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
unsafe extern "system" fn handler(event: u32, _: u32, _: *mut c_void, _: *mut c_void) -> u32 {
    // Never dereference SCM context/event pointers or allow a Rust panic into FFI.
    catch_unwind(|| {
        let Some(entry) = ENTRY.get() else {
            return ERROR_CALL_NOT_IMPLEMENTED;
        };
        let shared = &entry.shared;
        let Ok(mut report) = shared.report.lock() else {
            return ERROR_CALL_NOT_IMPLEMENTED;
        };
        match event {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                shared.stop.store(true, Ordering::Release);
                if report.state != SERVICE_STOPPED {
                    report.state = SERVICE_STOP_PENDING;
                }
            }
            SERVICE_CONTROL_INTERROGATE => {}
            _ => return ERROR_CALL_NOT_IMPLEMENTED,
        }
        publish(shared, &report).map_or_else(|e| e.raw_os_error().unwrap_or(1) as u32, |_| 0)
    })
    .unwrap_or(ERROR_CALL_NOT_IMPLEMENTED)
}
unsafe extern "system" fn service_main(_: u32, _: *mut *mut u16) {
    // Configuration is from our protected fixed file, never SCM startup argv.
    let Some(entry) = ENTRY.get() else {
        return;
    };
    let result = catch_unwind(AssertUnwindSafe(|| -> io::Result<()> {
        let name = super::wide(OsStr::new(NAME))?;
        // SAFETY: terminated name lives through call; static handler uses no
        // borrowed context. No unregister/free while Windows can call it.
        let handle =
            unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(handler), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        entry.shared.handle.store(handle, Ordering::Release);
        publish(
            &entry.shared,
            &*entry
                .shared
                .report
                .lock()
                .map_err(|_| io::ErrorKind::Other)?,
        )?;
        (entry.callback)(Control(entry.shared.clone()))
    }));
    if !entry.shared.handle.load(Ordering::Acquire).is_null() {
        // Callback has returned only after teardown. Recover poison to report
        // failure rather than unwinding across SCM's ServiceMain boundary.
        let mut report = entry
            .shared
            .report
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        report.state = SERVICE_STOPPED;
        report.exit = u32::from(!matches!(result, Ok(Ok(()))));
        let _ = publish(&entry.shared, &report);
    }
}
/// Must run on the process main thread before constructing the async runtime.
/// No fallback to console mode if launched outside SCM or under a wrong name.
pub fn dispatch(callback: fn(Control) -> io::Result<()>) -> io::Result<()> {
    ENTRY
        .set(Entry {
            callback,
            shared: Arc::new(Shared {
                handle: AtomicPtr::new(ptr::null_mut()),
                stop: AtomicBool::new(false),
                report: Mutex::new(Report {
                    state: SERVICE_START_PENDING,
                    exit: 0,
                }),
            }),
        })
        .map_err(|_| io::ErrorKind::AlreadyExists)?;
    let mut name = super::wide(OsStr::new(NAME))?;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: terminated table and writable terminated name remain live until
    // synchronous dispatcher returns after ServiceMain; callbacks are static.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
