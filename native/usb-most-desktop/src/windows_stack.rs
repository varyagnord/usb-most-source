//! Provision the pinned signed packages without installing an extension across
//! every existing physical USB hub. Windows applies the staged extension when
//! our new private virtual hub is enumerated. Shared packages are never removed.
use super::{PrivateFile, digest_hex, invalid};
use std::{
    io,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use usb_most_platform::windows as os;
include!(concat!(env!("OUT_DIR"), "/driver-bundle.rs"));
pub fn prepare(root: &Path) -> io::Result<()> {
    if super::windows::available() {
        return Ok(());
    }
    let directory = root.join("driver-0.9.8.1-x86_64");
    let _pin = os::create_private_directory(&directory)
        .inspect_err(|_| eprintln!("USB Most setup: driver_directory"))?;
    for (name, hash, bytes) in FILES {
        // Verify final linked bytes too; the build verifies the extracted source.
        if digest_hex(bytes) != *hash {
            return Err(invalid());
        }
        let file = PrivateFile::new(directory.join(name), 1024 * 1024)?;
        match file.read()? {
            Some(old) if digest_hex(&old) != *hash => return Err(invalid()),
            Some(_) => {}
            None => file
                .replace(bytes)
                .inspect_err(|_| eprintln!("USB Most setup: driver_file"))?,
        }
    }
    let program = os::system_directory()?.join("pnputil.exe");
    for name in ["usbip2_ude.inf", "usbip2_filter.inf"] {
        let mut child = Command::new(&program)
            .arg("/add-driver")
            .arg(directory.join(name))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.try_wait()? {
                match status.code() {
                    Some(0) => break,
                    Some(3010) => return Err(io::Error::from_raw_os_error(3010)),
                    _ => {
                        eprintln!("USB Most driver code: {}", status.code().unwrap_or(-1));
                        return Err(io::Error::other("signed_driver_staging_failed"));
                    }
                }
            }
            if Instant::now() >= end {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    if os::driver::install_controller(&directory.join("usbip2_ude.inf"))
        .inspect_err(|_| eprintln!("USB Most setup: controller"))?
    {
        return Err(io::Error::from_raw_os_error(3010));
    }
    // Device startup and extension application are asynchronous PnP operations.
    let end = Instant::now() + Duration::from_secs(30);
    while Instant::now() < end {
        if super::windows::available() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(io::ErrorKind::TimedOut.into())
}
