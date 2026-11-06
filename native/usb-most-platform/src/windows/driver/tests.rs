use super::*;
#[test]
#[ignore = "requires the isolated Windows signed-driver CI lab"]
fn protected_signed_controller_denies_other_local_principals() {
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1"),
        "driver lab requires an explicit isolated VM"
    );
    let package = std::env::var_os("USB_MOST_DRIVER_DIR").expect("verified package directory");
    assert!(
        !install_controller(&std::path::PathBuf::from(package).join("usbip2_ude.inf")).unwrap(),
        "fresh virtual controller must not require a reboot"
    );
    let controller =
        Controller::open().expect("exact private controller and actual device-object ACL");
    assert_eq!(
        controller.detach(0).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        controller.detach(-1).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        controller.detach(-2).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut raw = ptr::null_mut();
    // SAFETY: borrowed current-process pseudo-handle and writable token output.
    assert_ne!(
        unsafe {
            windows_sys::Win32::System::Threading::OpenProcessToken(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                &mut raw,
            )
        },
        0
    );
    // SAFETY: successful OpenProcessToken uniquely transfers its owned HANDLE.
    let original = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
    let sid = super::super::wide(std::ffi::OsStr::new("S-1-5-32-545")).unwrap();
    let mut users = ptr::null_mut();
    // SAFETY: terminated fixed SID literal and valid output, immediately guarded.
    assert_ne!(
        unsafe { ConvertStringSidToSidW(sid.as_ptr(), &mut users) },
        0
    );
    let users = Local(users);
    let restricted_sid = SID_AND_ATTRIBUTES {
        Sid: users.0,
        Attributes: 0,
    };
    let mut raw = ptr::null_mut();
    // SAFETY: one live SID, current token and valid output; no deleted-privilege array.
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
                &restricted_sid,
                &mut raw,
            )
        },
        0
    );
    // SAFETY: successful token creation uniquely transfers this HANDLE.
    let token = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // SAFETY: same test thread which impersonated; no async suspension in scope.
            assert_ne!(unsafe { RevertToSelf() }, 0);
        }
    }
    // SAFETY: live owned restricted token; impersonation limited to this synchronous test thread.
    assert_ne!(unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) }, 0);
    let revert = Revert;
    // SAFETY: copied terminated interface path, normal synchronous CreateFile.
    let handle = unsafe {
        CreateFileW(
            controller.path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            3,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    let error = io::Error::last_os_error();
    drop(revert);
    if handle != INVALID_HANDLE_VALUE {
        // SAFETY: close any unexpected successful handle before reporting failure.
        drop(unsafe { File::from_raw_handle(handle) });
        panic!("restricted local user opened the controller");
    }
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "private driver ACL must deny access at CreateFile"
    );
}
