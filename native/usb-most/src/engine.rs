use std::{
    cell::UnsafeCell,
    marker::PhantomPinned,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroize;

// One modern host bulk URB can exceed 128 KiB (observed 520192 bytes on
// Fedora USB3). The aggregate allocation bound remains the previous 8 MiB.
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_BUFFERED_BYTES: usize = 8 * 1024 * 1024;

struct ByteLease {
    used: Arc<AtomicUsize>,
    bytes: usize,
}
impl ByteLease {
    fn reserve(used: &Arc<AtomicUsize>, bytes: usize) -> Result<Self, i32> {
        used.try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(bytes)
                .filter(|total| *total <= MAX_BUFFERED_BYTES)
        })
        .map_err(|_| libc::ENOMEM)?;
        Ok(Self {
            used: used.clone(),
            bytes,
        })
    }
}
impl Drop for ByteLease {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
pub const MAX_PENDING: usize = 64;

/// Linux UAPI usbdevfs_urb, excluding the unused flexible ISO descriptor array.
/// Offsets and ioctl numbers are checked against NDK headers for every ABI.
#[repr(C)]
#[derive(Default)]
pub struct Urb {
    pub kind: u8,
    pub endpoint: u8,
    pub status: i32,
    pub flags: u32,
    pub buffer: *mut libc::c_void,
    pub buffer_length: i32,
    pub actual_length: i32,
    pub start_frame: i32,
    pub number_of_packets: i32,
    pub error_count: i32,
    pub signr: u32,
    pub usercontext: *mut libc::c_void,
}

/// # Safety
/// A successful submit borrows the URB and its buffer until that exact pointer
/// is reaped. Failed submits retain nothing. Reap/submit/discard are serialized
/// by Engine. poll may run concurrently but must not access request memory.
/// Returned pointers must not be dereferenced until matched against owned URBs.
/// Implementations must not panic after borrowing memory or consuming a completion.
pub unsafe trait Backend: Send + Sync {
    unsafe fn submit(&self, urb: *mut Urb) -> Result<(), i32>;
    fn discard(&self, urb: *mut Urb) -> Result<(), i32>;
    fn reap(&self) -> Result<Option<*mut Urb>, i32>;
    fn poll(&self, timeout_ms: i32) -> Result<(), i32>;
}

pub struct Spec {
    pub id: i32,
    pub kind: i32,
    pub endpoint: i32,
    pub flags: i32,
    pub length: i32,
    pub setup: Option<[u8; 8]>,
}

struct Request {
    urb: UnsafeCell<Urb>,
    buffer: Vec<UnsafeCell<u8>>,
    id: i32,
    capacity: usize,
    offset: usize,
    input: bool,
    in_flight: bool,
    cancelled: bool,
    _pin: PhantomPinned,
    _bytes: ByteLease,
}
// SAFETY: all memory accesses are serialized by Engine's mutex; the allocation
// never moves and UnsafeCell explicitly permits the kernel's writes during I/O.
unsafe impl Send for Request {}

impl Request {
    fn prepare(spec: &Spec, used: &Arc<AtomicUsize>) -> Result<Pin<Box<Self>>, i32> {
        if !(0..=MAX_BYTES as i32).contains(&spec.length)
            || !(0..=255).contains(&spec.endpoint)
            || !matches!(spec.kind, 1..=3)
            || (spec.kind == 2 && (spec.setup.is_none() || spec.length > 65535))
        {
            return Err(libc::EINVAL);
        }
        let offset = if spec.kind == 2 { 8 } else { 0 };
        let input = if offset != 0 {
            spec.setup.as_ref().is_some_and(|s| s[0] & 0x80 != 0)
        } else {
            spec.endpoint & 0x80 != 0
        };
        // Charge before allocation, retain through the actual kernel completion
        // AND any returned Completion buffer, including close/quarantine paths.
        let bytes = ByteLease::reserve(used, spec.length as usize + offset)?;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(spec.length as usize + offset)
            .map_err(|_| libc::ENOMEM)?;
        buffer.resize_with(spec.length as usize + offset, || UnsafeCell::new(0));
        let request = Box::try_new(Self {
            urb: UnsafeCell::new(Urb {
                kind: spec.kind as u8,
                endpoint: spec.endpoint as u8,
                flags: (spec.flags as u32) & (1 | 0x40), // SHORT_NOT_OK | ZERO_PACKET
                buffer: std::ptr::null_mut(),
                buffer_length: buffer.len() as i32,
                ..Urb::default()
            }),
            buffer,
            id: spec.id,
            capacity: spec.length as usize,
            offset,
            input,
            in_flight: false,
            cancelled: false,
            _pin: PhantomPinned,
            _bytes: bytes,
        })
        .map_err(|_| libc::ENOMEM)?;
        if offset != 0 {
            for (dst, src) in request.buffer.iter().zip(spec.setup.unwrap()) {
                // SAFETY: preparation happens before publication to the kernel.
                unsafe {
                    *dst.get() = src;
                }
            }
        }
        Ok(Box::into_pin(request))
    }

    fn pointer(&self) -> *mut Urb {
        self.urb.get()
    }

    // Only before submission or after reaping. Never accessed while in flight.
    fn bytes_mut(&mut self) -> &mut [u8] {
        assert!(!self.in_flight);
        // SAFETY: UnsafeCell<u8> has u8 layout; exclusive access and no kernel borrow.
        unsafe {
            std::slice::from_raw_parts_mut(self.buffer.as_mut_ptr().cast(), self.buffer.len())
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.bytes_mut().zeroize();
    }
}

pub struct Completion {
    pub id: i32,
    pub status: i32,
    pub actual: i32,
    pub wan_epoch: u32,
    pub wan_sequence: u32,
    request: Option<Pin<Box<Request>>>,
}
impl Completion {
    pub fn data(&self) -> Option<&[u8]> {
        let r = self.request.as_ref()?;
        if !r.input || self.actual == 0 {
            return None;
        }
        // SAFETY: the URB was reaped and matched; actual was checked against
        // capacity. No kernel or mutable Rust reference can touch this buffer.
        Some(unsafe {
            std::slice::from_raw_parts(
                r.buffer.as_ptr().cast::<u8>().add(r.offset),
                self.actual as usize,
            )
        })
    }
}

struct State {
    requests: [Option<Pin<Box<Request>>>; MAX_PENDING],
    closed: bool,
    #[cfg(feature = "diagnostics")]
    trace: Option<crate::diagnostics::Trace>,
}
pub struct Engine<B: Backend> {
    backend: B,
    bytes: Arc<AtomicUsize>,
    state: Mutex<State>,
}
impl<B: Backend> Engine<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            bytes: Arc::new(AtomicUsize::new(0)),
            state: Mutex::new(State {
                requests: std::array::from_fn(|_| None),
                closed: false,
                #[cfg(feature = "diagnostics")]
                trace: None,
            }),
        }
    }
    pub fn is_idle(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|s| !s.closed && s.requests.iter().all(Option::is_none))
    }
    pub fn wait(&self, timeout: i32) -> Result<(), i32> {
        if !(0..=100).contains(&timeout) {
            return Err(libc::EINVAL);
        }
        self.backend.poll(timeout)
    }
    pub fn submit(
        &self,
        spec: Spec,
        fill: impl FnOnce(&mut [u8]) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let mut state = self.state.lock().map_err(|_| libc::EIO)?;
        if state.closed {
            return Err(libc::ENODEV);
        }
        if state.requests.iter().flatten().any(|r| r.id == spec.id) {
            return Err(libc::EEXIST);
        }
        let slot = state
            .requests
            .iter()
            .position(Option::is_none)
            .ok_or(libc::ENOSPC)?;
        let mut request = Request::prepare(&spec, &self.bytes)?;
        // SAFETY: modifying metadata and buffer does not move the pinned URB.
        let r = unsafe { request.as_mut().get_unchecked_mut() };
        if !r.input && r.capacity != 0 {
            let offset = r.offset;
            fill(&mut r.bytes_mut()[offset..])?;
        }
        // Establish the kernel pointer after the last exclusive buffer access.
        // Vec owns a fixed allocation without Box's unique-pointer move retags.
        // SAFETY: unpublished URB; UnsafeCell permits later kernel writes.
        unsafe {
            (*r.pointer()).buffer = r.buffer.as_ptr().cast_mut().cast();
        }
        // SAFETY: pinned allocations stay live in state until matched reap.
        unsafe {
            self.backend.submit(r.pointer())?;
        }
        r.in_flight = true;
        state.requests[slot] = Some(request);
        Ok(())
    }
    pub fn cancel(&self, id: i32) -> Result<(), i32> {
        let mut state = self.state.lock().map_err(|_| libc::EIO)?;
        if state.closed {
            return Err(libc::ENODEV);
        }
        let r = state
            .requests
            .iter_mut()
            .flatten()
            .find(|r| r.id == id)
            .ok_or(libc::ENOENT)?;
        // SAFETY: only metadata changes; pinned allocations remain stationary.
        let r = unsafe { r.as_mut().get_unchecked_mut() };
        if !r.cancelled {
            self.backend.discard(r.pointer())?;
            r.cancelled = true;
        }
        Ok(())
    }
    fn take(state: &mut State, pointer: *mut Urb) -> Result<Pin<Box<Request>>, i32> {
        let slot = state
            .requests
            .iter()
            .position(|r| r.as_ref().is_some_and(|r| r.pointer() == pointer))
            .ok_or(libc::EPROTO)?;
        let mut r = state.requests[slot].take().ok_or(libc::EPROTO)?;
        // SAFETY: matched reap ends the kernel's borrow. Pinned allocation is not moved.
        unsafe {
            r.as_mut().get_unchecked_mut().in_flight = false;
        }
        Ok(r)
    }
    pub fn reap(&self, timeout: i32) -> Result<Option<Completion>, i32> {
        if !(0..=100).contains(&timeout) {
            return Err(libc::EINVAL);
        }
        {
            let mut state = self.state.lock().map_err(|_| libc::EIO)?;
            if state.closed {
                return Err(libc::ENODEV);
            }
            if let Some(pointer) = self.backend.reap()? {
                let request = Self::take(&mut state, pointer)?;
                // SAFETY: the backend has ended its borrow, and take verified pointer identity.
                let urb = unsafe { &*request.pointer() };
                let (status, actual) =
                    if urb.actual_length < 0 || urb.actual_length as usize > request.capacity {
                        (-libc::EOVERFLOW, 0)
                    } else {
                        (urb.status, urb.actual_length)
                    };
                #[cfg(feature = "diagnostics")]
                let endpoint = urb.endpoint;
                let completion = Completion {
                    id: request.id,
                    status,
                    actual,
                    wan_epoch: 0,
                    wan_sequence: 0,
                    request: Some(request),
                };
                #[cfg(feature = "diagnostics")]
                if let Some(trace) = &mut state.trace {
                    // Only after matched REAP and checked lengths; no kernel borrow remains.
                    trace.record(
                        completion.id,
                        endpoint,
                        status,
                        actual,
                        completion.data().unwrap_or(&[]),
                    );
                }
                return Ok(Some(completion));
            }
        }
        // Poll does not hold the state lock: submit/cancel remain responsive.
        self.backend.poll(timeout)?;
        Ok(None)
    }
    #[cfg(feature = "diagnostics")]
    pub fn enable_hid_diagnostics(&self) -> Result<(), i32> {
        let mut state = self.state.lock().map_err(|_| libc::EIO)?;
        if state.closed {
            return Err(libc::ENODEV);
        }
        if state.trace.is_some() {
            return Err(libc::EALREADY);
        }
        if state.requests.iter().any(Option::is_some) {
            return Err(libc::EBUSY);
        }
        state.trace = Some(crate::diagnostics::Trace::new()?);
        Ok(())
    }
    #[cfg(feature = "diagnostics")]
    pub fn take_hid_diagnostics(&self) -> Result<Vec<i64>, i32> {
        let mut state = self.state.lock().map_err(|_| libc::EIO)?;
        state.trace.as_mut().ok_or(libc::EINVAL)?.drain()
    }
    /// Close is idempotent. Cancel then reap before releasing any pending memory.
    /// If a broken/disconnected kernel cannot confirm retirement within 500 ms,
    /// quarantine those allocations rather than risking a use-after-free. This
    /// exceptional safety fallback is bounded by 64 requests per session.
    pub fn close(&self) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.closed {
            return 0;
        }
        state.closed = true;
        for r in state.requests.iter().flatten() {
            let _ = self.backend.discard(r.pointer());
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while state.requests.iter().any(Option::is_some) {
            match self.backend.reap() {
                Ok(Some(p)) => {
                    if Self::take(&mut state, p).is_err() {
                        break;
                    }
                }
                Ok(None) if Instant::now() < deadline => {
                    if self.backend.poll(5).is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        let mut quarantined = 0;
        for slot in &mut state.requests {
            if let Some(request) = slot.take() {
                std::mem::forget(request);
                quarantined += 1;
            }
        }
        quarantined
    }
}
impl<B: Backend> Drop for Engine<B> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests;
