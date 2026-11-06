//! Synthetic kernel, only compiled for tests/internal diagnostics. Never opens USB.
use crate::engine::{Backend, Urb};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pointer(*mut Urb);
// SAFETY: accesses are serialized by Kernel's mutex and follow Engine's borrow contract.
unsafe impl Send for Pointer {}
#[derive(Default)]
struct Kernel {
    pending: Vec<Pointer>,
    ready: VecDeque<Pointer>,
    controls: usize,
}
#[derive(Default)]
pub struct Fake(Mutex<Kernel>);
impl Fake {
    pub fn emit(&self, data: &[u8]) -> bool {
        let mut k = self.0.lock().unwrap();
        let Some(p) = k
            .pending
            .iter()
            .copied()
            .find(|p| !k.ready.contains(p) && unsafe { (*p.0).endpoint == 0x83 })
        else {
            return false;
        };
        // SAFETY: the submitted allocation is still borrowed by this fake kernel.
        unsafe {
            let u = &mut *p.0;
            assert!(data.len() <= u.buffer_length as usize);
            std::ptr::copy_nonoverlapping(data.as_ptr(), u.buffer.cast(), data.len());
            u.actual_length = data.len() as i32;
            u.status = 0;
        }
        k.ready.push_back(p);
        true
    }
    #[cfg(test)]
    pub fn controls(&self) -> usize {
        self.0.lock().unwrap().controls
    }
}
// SAFETY: only matched, live requests are written, and retirement ends all writes.
unsafe impl Backend for Arc<Fake> {
    unsafe fn submit(&self, p: *mut Urb) -> Result<(), i32> {
        let mut k = self.0.lock().unwrap();
        k.pending.push(Pointer(p));
        // SAFETY: Engine just submitted this initialized request and retains its allocation.
        if unsafe { (*p).kind == 2 } {
            k.controls += 1;
            unsafe {
                (*p).actual_length = (*p).buffer_length - 8;
                (*p).status = 0;
            }
            k.ready.push_back(Pointer(p));
        }
        Ok(())
    }
    fn discard(&self, p: *mut Urb) -> Result<(), i32> {
        let mut k = self.0.lock().unwrap();
        let p = Pointer(p);
        if !k.pending.contains(&p) {
            return Err(libc::EINVAL);
        }
        if !k.ready.contains(&p) {
            unsafe {
                (*p.0).status = -libc::ECONNRESET;
                (*p.0).actual_length = 0;
            }
            k.ready.push_back(p);
        }
        Ok(())
    }
    fn reap(&self) -> Result<Option<*mut Urb>, i32> {
        let mut k = self.0.lock().unwrap();
        let Some(p) = k.ready.pop_front() else {
            return Ok(None);
        };
        k.pending.retain(|n| *n != p);
        Ok(Some(p.0))
    }
    fn poll(&self, _: i32) -> Result<(), i32> {
        Ok(())
    }
}
