use crate::engine::{Backend, Urb};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

// Linux asm-generic ioctl encoding, valid for all four supported Android ABIs.
const fn ioctl_code(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'U' as u32) << 8) | nr) as libc::c_ulong
}
pub const SUBMIT: libc::c_ulong = ioctl_code(2, 10, size_of::<Urb>());
pub const DISCARD: libc::c_ulong = ioctl_code(0, 11, 0);
pub const REAP: libc::c_ulong = ioctl_code(1, 13, size_of::<*mut Urb>());
pub const RELEASE_INTERFACE: libc::c_ulong = ioctl_code(2, 16, size_of::<u32>());
#[repr(C)]
struct InterfaceIoctl {
    interface: i32,
    code: i32,
    data: *mut libc::c_void,
}
pub const INTERFACE_IOCTL: libc::c_ulong = ioctl_code(3, 18, size_of::<InterfaceIoctl>());
const CONNECT_DRIVER: i32 = ioctl_code(0, 23, 0) as i32;
// Compile-time ABI assertions run for each Cargo Android target.
const _: () = {
    let wide = size_of::<usize>() == 8;
    assert!(size_of::<Urb>() == if wide { 56 } else { 44 });
    assert!(std::mem::offset_of!(Urb, buffer) == if wide { 16 } else { 12 });
    assert!(std::mem::offset_of!(Urb, usercontext) == if wide { 48 } else { 40 });
    assert!(SUBMIT == if wide { 0x8038550a } else { 0x802c550a });
    assert!(DISCARD == 0x550b);
    assert!(REAP == if wide { 0x4008550d } else { 0x4004550d });
    assert!(RELEASE_INTERFACE == 0x80045510);
    assert!(size_of::<InterfaceIoctl>() == if wide { 16 } else { 12 });
    assert!(std::mem::offset_of!(InterfaceIoctl, data) == 8);
    assert!(INTERFACE_IOCTL == if wide { 0xc0105512 } else { 0xc00c5512 });
};

struct Interfaces(i32);
impl crate::interfaces::Control for Interfaces {
    fn release(&mut self, mut interface: u32) -> Result<(), i32> {
        // SAFETY: live caller-owned fd; ioctl borrows only this stack u32 synchronously.
        if unsafe { libc::ioctl(self.0, RELEASE_INTERFACE as _, &mut interface) } < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
    fn reconnect(&mut self, interface: u32) -> Result<(), i32> {
        let mut request = InterfaceIoctl {
            interface: interface as i32,
            code: CONNECT_DRIVER,
            data: std::ptr::null_mut(),
        };
        // SAFETY: CONNECT has no payload; request is a valid initialized ABI struct,
        // retained only during this synchronous ioctl. No pointer/fd is stored.
        if unsafe { libc::ioctl(self.0, INTERFACE_IOCTL as _, &mut request) } < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
}
pub fn release_interface(fd: i32, interface: i32, reconnect: bool) -> Result<(), i32> {
    if fd < 0 {
        return Err(libc::EINVAL);
    }
    crate::interfaces::release(&mut Interfaces(fd), interface, reconnect)
}
fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
pub struct Linux {
    fd: OwnedFd,
}
impl Linux {
    pub fn open(fd: i32) -> Result<Self, i32> {
        // SAFETY: fcntl validates arbitrary integers; only a successful duplicate is owned.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            Err(errno())
        } else {
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(duplicate) },
            })
        }
    }
}
// SAFETY: usbfs kernel ABI borrows pinned request allocations until REAP; all
// data-accessing calls are serialized by Engine. OwnedFd stays live across poll.
unsafe impl Backend for Linux {
    unsafe fn submit(&self, urb: *mut Urb) -> Result<(), i32> {
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), SUBMIT as _, urb) } < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
    fn discard(&self, urb: *mut Urb) -> Result<(), i32> {
        // SAFETY: kernel treats this as the previously submitted user address.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), DISCARD as _, urb) } < 0 {
            Err(errno())
        } else {
            Ok(())
        }
    }
    fn reap(&self) -> Result<Option<*mut Urb>, i32> {
        let mut urb = std::ptr::null_mut::<Urb>();
        // SAFETY: output points to a valid initialized pointer-sized stack slot.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), REAP as _, &mut urb) } >= 0 {
            return Ok(Some(urb));
        }
        match errno() {
            libc::EAGAIN | libc::EINTR => Ok(None),
            error => Err(error),
        }
    }
    fn poll(&self, timeout: i32) -> Result<(), i32> {
        let mut fd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: valid stack array of one pollfd; bounded timeout.
        let rc = unsafe { libc::poll(&mut fd, 1, timeout) };
        if rc < 0 {
            let e = errno();
            if e != libc::EINTR {
                return Err(e);
            }
        }
        if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(libc::ENODEV);
        }
        Ok(())
    }
}
pub fn configure_socket(fd: i32) -> Result<(), i32> {
    for (level, option, value) in [
        (libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1_i32),
        (libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, 30),
        (libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 10),
        (libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 3),
        (libc::IPPROTO_TCP, libc::TCP_USER_TIMEOUT, 60000),
    ] {
        // SAFETY: value points to a live i32, with its exact byte size.
        if unsafe {
            libc::setsockopt(
                fd,
                level,
                option,
                (&value as *const i32).cast(),
                size_of::<i32>() as _,
            )
        } < 0
        {
            return Err(errno());
        }
    }
    Ok(())
}
