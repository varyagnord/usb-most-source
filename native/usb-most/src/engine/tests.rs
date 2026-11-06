use super::*;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Pointer(*mut Urb);
// SAFETY: the fake kernel only touches URBs under its lock; tests retain Engine.
unsafe impl Send for Pointer {}
#[derive(Default)]
struct Kernel {
    pending: Vec<Pointer>,
    ready: VecDeque<Pointer>,
    submitted: usize,
    discarded: usize,
    submit_error: i32,
    discard_error: i32,
    reap_error: i32,
    poll_error: i32,
}
#[derive(Default)]
struct Mock {
    kernel: Mutex<Kernel>,
    polls: AtomicUsize,
    gate: Mutex<Option<(Arc<Barrier>, Arc<Barrier>)>>,
}
impl Mock {
    fn last(&self) -> Pointer {
        *self.kernel.lock().unwrap().pending.last().unwrap()
    }
    fn finish(&self, p: Pointer, status: i32, actual: i32, data: &[u8]) {
        let mut k = self.kernel.lock().unwrap();
        assert!(k.pending.contains(&p));
        // SAFETY: these emulate kernel writes during the borrow; no engine
        // method reads them until reap. Bounds are checked before writing.
        unsafe {
            let urb = &mut *p.0;
            urb.status = status;
            urb.actual_length = actual;
            let offset = if urb.kind == 2 { 8 } else { 0 };
            assert!(data.len() + offset <= urb.buffer_length as usize);
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                urb.buffer.cast::<u8>().add(offset),
                data.len(),
            );
        }
        k.ready.push_back(p);
    }
}
// SAFETY: same borrowing contract as usbfs, with synchronous test-controlled completion.
unsafe impl Backend for Arc<Mock> {
    unsafe fn submit(&self, p: *mut Urb) -> Result<(), i32> {
        let mut k = self.kernel.lock().unwrap();
        k.submitted += 1;
        if k.submit_error != 0 {
            return Err(k.submit_error);
        }
        k.pending.push(Pointer(p));
        Ok(())
    }
    fn discard(&self, p: *mut Urb) -> Result<(), i32> {
        let mut k = self.kernel.lock().unwrap();
        k.discarded += 1;
        if k.discard_error != 0 {
            return Err(k.discard_error);
        }
        let p = Pointer(p);
        if !k.pending.contains(&p) {
            return Err(libc::EINVAL);
        }
        if !k.ready.contains(&p) {
            // SAFETY: this is a submitted, still-live kernel-owned URB.
            unsafe {
                (*p.0).status = -libc::ECONNRESET;
                (*p.0).actual_length = 0;
            }
            k.ready.push_back(p);
        }
        Ok(())
    }
    fn reap(&self) -> Result<Option<*mut Urb>, i32> {
        let mut k = self.kernel.lock().unwrap();
        if k.reap_error != 0 {
            return Err(k.reap_error);
        }
        let Some(p) = k.ready.pop_front() else {
            return Ok(None);
        };
        k.pending.retain(|v| *v != p);
        Ok(Some(p.0))
    }
    fn poll(&self, _: i32) -> Result<(), i32> {
        self.polls.fetch_add(1, Ordering::Relaxed);
        if let Some((entered, release)) = self.gate.lock().unwrap().clone() {
            entered.wait();
            release.wait();
        }
        let e = self.kernel.lock().unwrap().poll_error;
        if e == 0 { Ok(()) } else { Err(e) }
    }
}
fn spec(id: i32, length: i32) -> Spec {
    Spec {
        id,
        kind: 3,
        endpoint: 0x82,
        flags: 0x40200,
        length,
        setup: None,
    }
}
fn fixture() -> (Arc<Mock>, Engine<Arc<Mock>>) {
    let m = Arc::new(Mock::default());
    let e = Engine::new(m.clone());
    (m, e)
}

#[cfg(feature = "diagnostics")]
#[test]
fn trace_reads_only_retired_bytes_and_never_changes_completion() {
    let (m, e) = fixture();
    e.enable_hid_diagnostics().unwrap();
    assert_eq!(e.enable_hid_diagnostics(), Err(libc::EALREADY));
    let mut request = spec(123, 32);
    request.kind = 1;
    request.endpoint = 0x83;
    e.submit(request, |_| panic!("IN payload copy")).unwrap();
    assert_eq!(e.take_hid_diagnostics().unwrap()[2], 0);
    let packet = [0x20, 1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    m.finish(m.last(), 0, packet.len() as i32, &packet);
    let completion = e.reap(0).unwrap().unwrap();
    let records = e.take_hid_diagnostics().unwrap();
    assert_eq!(&records[..6], &[1, 9, 1, 1, 1, 0]);
    assert_eq!(&records[6..9], &[123, 0, 15]);
    assert_eq!(&records[10..13], &[1, 1, 1]);
    assert_eq!(records[13] as u64, crate::diagnostics::digest(&packet));
    assert_eq!(completion.data(), Some(packet.as_slice()));
    assert_eq!(e.take_hid_diagnostics().unwrap()[2], 0);
    e.close();
    assert_eq!(completion.data(), Some(packet.as_slice()));
}
fn input(e: &Engine<Arc<Mock>>, id: i32, length: i32) -> Result<(), i32> {
    e.submit(spec(id, length), |_| panic!("IN must not copy payload"))
}

#[test]
fn lengths_and_partial_errors() {
    let (m, e) = fixture();
    for size in [
        0, 1, 16, 63, 64, 65, 127, 128, 129, 130, 256, 270, 271, 512, 65535, 65536, 65556, 131072,
    ] {
        input(&e, size, size.max(271)).unwrap();
        let p = m.last();
        // SAFETY: inspecting initialized fields; no fake kernel write is concurrent.
        assert_eq!(unsafe { (*p.0).flags }, 0); // unsupported USB/IP flags masked
        m.finish(p, -libc::ETIMEDOUT, size, &vec![0x5a; size as usize]);
        let r = e.reap(100).unwrap().unwrap();
        assert_eq!((r.id, r.status, r.actual), (size, -libc::ETIMEDOUT, size));
        assert_eq!(r.data().unwrap_or_default(), vec![0x5a; size as usize]);
    }
    assert_eq!(m.kernel.lock().unwrap().submitted, 18);
}
#[test]
#[cfg_attr(miri, ignore)] // Large byte loops are covered separately by ASan.
fn large_bulk_transfers_preserve_input_output_partial_errors_without_replay() {
    let (m, e) = fixture();
    for length in [131073, 520192, 524288, MAX_BYTES as i32] {
        let data: Vec<_> = (0..length).map(|i| (i ^ (i >> 8)) as u8).collect();
        input(&e, length, length).unwrap();
        m.finish(m.last(), 0, length, &data);
        let reply = e.reap(0).unwrap().unwrap();
        assert_eq!(reply.data(), Some(data.as_slice()));
        assert_eq!(e.bytes.load(Ordering::Acquire), length as usize);
        drop(reply);
        let mut out = spec(length, length);
        out.endpoint = 2;
        e.submit(out, |bytes| {
            bytes.copy_from_slice(&data);
            Ok(())
        })
        .unwrap();
        let p = m.last();
        // SAFETY: the mock does not write concurrently; it owns this live URB.
        assert_eq!(
            unsafe { std::slice::from_raw_parts((*p.0).buffer.cast::<u8>(), length as usize) },
            data
        );
        m.finish(p, -libc::ETIMEDOUT, length - 1, &[]);
        let reply = e.reap(0).unwrap().unwrap();
        assert_eq!((reply.status, reply.actual), (-libc::ETIMEDOUT, length - 1));
        assert!(reply.data().is_none());
        drop(reply);
        assert_eq!(e.bytes.load(Ordering::Acquire), 0);
    }
    assert_eq!(m.kernel.lock().unwrap().submitted, 8);
    assert_eq!(input(&e, 1, MAX_BYTES as i32 + 1), Err(libc::EINVAL));
}

#[test]
fn byte_budget_survives_cancel_reap_and_close_until_completion_drop() {
    let (m, e) = fixture();
    // Reserve most of the budget without allocating it: same boundary under Miri.
    let reservation = ByteLease::reserve(&e.bytes, MAX_BUFFERED_BYTES - 1024).unwrap();
    input(&e, 1, 1024).unwrap();
    assert_eq!(input(&e, 2, 1), Err(libc::ENOMEM));
    e.cancel(1).unwrap();
    assert_eq!(input(&e, 2, 1), Err(libc::ENOMEM));
    let completion = e.reap(0).unwrap().unwrap();
    assert_eq!(completion.status, -libc::ECONNRESET);
    assert_eq!(input(&e, 2, 1), Err(libc::ENOMEM));
    assert_eq!(m.kernel.lock().unwrap().submitted, 1);
    drop(completion);
    input(&e, 2, 1024).unwrap();
    m.finish(m.last(), 0, 1, &[47]);
    let completion = e.reap(0).unwrap().unwrap();
    assert_eq!(e.close(), 0);
    drop(reservation);
    let budget = e.bytes.clone();
    drop(e);
    assert_eq!(budget.load(Ordering::Acquire), 1024);
    assert_eq!(completion.data(), Some([47].as_slice()));
    drop(completion);
    assert_eq!(budget.load(Ordering::Acquire), 0);
}

#[test]
fn byte_budget_refunds_failed_preparation_and_submit_and_charges_control_setup() {
    let (m, e) = fixture();
    let reservation = ByteLease::reserve(&e.bytes, MAX_BUFFERED_BYTES - 1024).unwrap();
    let mut out = spec(1, 1024);
    out.endpoint = 2;
    assert_eq!(e.submit(out, |_| Err(libc::EFAULT)), Err(libc::EFAULT));
    assert_eq!(e.bytes.load(Ordering::Acquire), MAX_BUFFERED_BYTES - 1024);
    m.kernel.lock().unwrap().submit_error = libc::EIO;
    assert_eq!(input(&e, 1, 1024), Err(libc::EIO));
    assert_eq!(e.bytes.load(Ordering::Acquire), MAX_BUFFERED_BYTES - 1024);
    m.kernel.lock().unwrap().submit_error = 0;
    let mut control = spec(1, 1024);
    control.kind = 2;
    control.setup = Some([0x80, 6, 0, 1, 0, 0, 0, 4]);
    assert_eq!(e.submit(control, |_| panic!("IN copy")), Err(libc::ENOMEM));
    let before = e.bytes.load(Ordering::Acquire);
    assert!(matches!(
        ByteLease::reserve(&e.bytes, usize::MAX),
        Err(libc::ENOMEM)
    ));
    assert_eq!(e.bytes.load(Ordering::Acquire), before);
    input(&e, 1, 1024).unwrap();
    assert_eq!(e.close(), 0);
    drop(reservation);
    assert_eq!(e.bytes.load(Ordering::Acquire), 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn byte_budget_caps_real_large_allocations_and_reopens_after_retirement() {
    let (_, e) = fixture();
    for id in 0..8 {
        input(&e, id, MAX_BYTES as i32).unwrap();
    }
    assert_eq!(input(&e, 8, 1), Err(libc::ENOMEM));
    e.cancel(0).unwrap();
    drop(e.reap(0).unwrap().unwrap());
    input(&e, 8, MAX_BYTES as i32).unwrap();
    assert_eq!(e.bytes.load(Ordering::Acquire), MAX_BUFFERED_BYTES);
    assert_eq!(e.close(), 0);
    assert_eq!(e.bytes.load(Ordering::Acquire), 0);
}
#[test]
fn out_not_retried_and_control_setup() {
    let (m, e) = fixture();
    let mut s = spec(1, 130);
    s.endpoint = 2;
    e.submit(s, |bytes| {
        bytes.fill(0xa5);
        Ok(())
    })
    .unwrap();
    let p = m.last();
    assert_eq!(
        unsafe { std::slice::from_raw_parts((*p.0).buffer.cast::<u8>(), 130) },
        [0xa5; 130]
    );
    m.finish(p, -libc::ETIMEDOUT, 64, &[]);
    let r = e.reap(0).unwrap().unwrap();
    assert_eq!((r.status, r.actual), (-libc::ETIMEDOUT, 64));
    assert!(r.data().is_none());
    assert_eq!(m.kernel.lock().unwrap().submitted, 1);
    for direction in [0u8, 0x80] {
        let setup = [direction, 6, 0, 1, 0, 0, 18, 0];
        let mut s = spec(2, 18);
        s.kind = 2;
        s.setup = Some(setup);
        e.submit(s, |data| {
            data.fill(0xee);
            Ok(())
        })
        .unwrap();
        let p = m.last();
        assert_eq!(unsafe { (*p.0).buffer_length }, 26);
        assert_eq!(
            unsafe { std::slice::from_raw_parts((*p.0).buffer.cast::<u8>(), 8) },
            setup
        );
        m.finish(p, 0, 18, &[0xee; 18]);
        let r = e.reap(0).unwrap().unwrap();
        assert_eq!(r.data().is_some(), direction != 0);
        if let Some(data) = r.data() {
            assert_eq!(data, [0xee; 18]);
        }
    }
}
#[test]
fn validation_and_fill_failure() {
    let (m, e) = fixture();
    for length in [-1, 1048577, i32::MAX] {
        assert_eq!(input(&e, 1, length), Err(libc::EINVAL));
    }
    for endpoint in [-1, 256, i32::MAX] {
        let mut s = spec(1, 0);
        s.endpoint = endpoint;
        assert_eq!(e.submit(s, |_| Ok(())), Err(libc::EINVAL));
    }
    for kind in [-1, 0, 4, 255] {
        let mut s = spec(1, 0);
        s.kind = kind;
        assert_eq!(e.submit(s, |_| Ok(())), Err(libc::EINVAL));
    }
    let mut s = spec(1, 65536);
    s.kind = 2;
    s.setup = Some([0; 8]);
    assert_eq!(e.submit(s, |_| Ok(())), Err(libc::EINVAL));
    let mut s = spec(1, 0);
    s.kind = 2;
    assert_eq!(e.submit(s, |_| Ok(())), Err(libc::EINVAL));
    let mut s = spec(1, 10);
    s.endpoint = 1;
    assert_eq!(e.submit(s, |_| Err(libc::EFAULT)), Err(libc::EFAULT));
    assert_eq!(m.kernel.lock().unwrap().submitted, 0);
    assert!(matches!(e.reap(-1), Err(libc::EINVAL)));
    assert!(matches!(e.reap(101), Err(libc::EINVAL)));
}
#[test]
fn queue_duplicate_cancellation_and_close() {
    let (m, e) = fixture();
    for id in 0..64 {
        input(&e, id, 16).unwrap();
    }
    assert_eq!(input(&e, 0, 16), Err(libc::EEXIST));
    assert_eq!(input(&e, 64, 16), Err(libc::ENOSPC));
    e.cancel(1).unwrap();
    e.cancel(1).unwrap();
    assert_eq!(m.kernel.lock().unwrap().discarded, 1);
    let r = e.reap(0).unwrap().unwrap();
    assert_eq!((r.id, r.status), (1, -libc::ECONNRESET));
    assert_eq!(e.cancel(1), Err(libc::ENOENT));
    input(&e, 64, 16).unwrap();
    assert_eq!(e.close(), 0);
    assert!(m.kernel.lock().unwrap().pending.is_empty());
    assert_eq!(e.close(), 0);
    assert_eq!(input(&e, 66, 0), Err(libc::ENODEV));
    assert!(matches!(e.reap(0), Err(libc::ENODEV)));
}
#[test]
fn corrupt_completion_and_backend_errors() {
    let (m, e) = fixture();
    for actual in [-1, 2, i32::MAX] {
        input(&e, 1, 1).unwrap();
        m.finish(m.last(), 0, actual, &[]);
        let r = e.reap(0).unwrap().unwrap();
        assert_eq!((r.status, r.actual), (-libc::EOVERFLOW, 0));
        assert!(r.data().is_none());
    }
    m.kernel.lock().unwrap().submit_error = libc::ENODEV;
    assert_eq!(input(&e, 1, 1), Err(libc::ENODEV));
    m.kernel.lock().unwrap().submit_error = 0;
    input(&e, 1, 1).unwrap();
    m.kernel.lock().unwrap().discard_error = libc::EINVAL;
    assert_eq!(e.cancel(1), Err(libc::EINVAL));
    m.kernel.lock().unwrap().discard_error = 0;
    e.cancel(1).unwrap();
    e.reap(0).unwrap();
    m.kernel.lock().unwrap().reap_error = libc::ENODEV;
    assert!(matches!(e.reap(0), Err(libc::ENODEV)));
    m.kernel.lock().unwrap().reap_error = 0;
    m.kernel.lock().unwrap().poll_error = libc::ENODEV;
    assert!(matches!(e.reap(0), Err(libc::ENODEV)));
}
#[test]
fn foreign_pointer_is_never_dereferenced() {
    let (m, e) = fixture();
    m.kernel
        .lock()
        .unwrap()
        .ready
        .push_back(Pointer(std::ptr::dangling_mut()));
    assert!(matches!(e.reap(0), Err(libc::EPROTO)));
}
#[test]
fn cancel_does_not_wait_for_poll() {
    let (m, e) = fixture();
    input(&e, 1, 1).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    *m.gate.lock().unwrap() = Some((entered.clone(), release.clone()));
    thread::scope(|scope| {
        let waiter = scope.spawn(|| e.reap(100).unwrap().is_none());
        entered.wait();
        e.cancel(1).unwrap();
        release.wait();
        assert!(waiter.join().unwrap());
    });
    *m.gate.lock().unwrap() = None;
    assert_eq!(e.reap(0).unwrap().unwrap().status, -libc::ECONNRESET);
}
#[test]
fn concurrent_completion_cancel_close() {
    for _ in 0..if cfg!(miri) { 4 } else { 200 } {
        let (m, e) = fixture();
        input(&e, 1, 8).unwrap();
        m.finish(m.last(), 0, 8, &[7; 8]);
        thread::scope(|s| {
            s.spawn(|| {
                let _ = e.cancel(1);
            });
            s.spawn(|| {
                let _ = e.reap(0);
            });
            s.spawn(|| {
                assert_eq!(e.close(), 0);
            });
        });
        assert!(m.kernel.lock().unwrap().pending.is_empty());
    }
}
#[test]
fn randomized_bounded_traces() {
    let (m, e) = fixture();
    let mut seed = 0x12345678u64;
    for id in 0..if cfg!(miri) { 20 } else { 2000 } {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let size = (seed % 272) as i32;
        input(&e, id, size).unwrap();
        let p = m.last();
        if seed & 1 == 0 {
            e.cancel(id).unwrap();
        } else {
            m.finish(p, 0, size, &vec![(seed >> 32) as u8; size as usize]);
        }
        let c = e.reap(0).unwrap().unwrap();
        assert_eq!(c.id, id);
        assert!(c.actual <= size);
    }
    assert_eq!(e.close(), 0);
}
#[test]
fn uapi_layout() {
    if size_of::<usize>() == 8 {
        assert_eq!(size_of::<Urb>(), 56);
        assert_eq!(std::mem::offset_of!(Urb, buffer), 16);
        assert_eq!(std::mem::offset_of!(Urb, usercontext), 48);
    } else {
        assert_eq!(size_of::<Urb>(), 44);
        assert_eq!(std::mem::offset_of!(Urb, buffer), 12);
        assert_eq!(std::mem::offset_of!(Urb, usercontext), 40);
    }
}

#[test]
fn reference_trace() {
    let (m, e) = fixture();
    for i in 0..96 {
        let size = if i == 0 {
            65556
        } else if i == 1 {
            131072
        } else {
            (i * 97) % 1024
        };
        let input = i % 2 != 0;
        let kind = if i > 1 && i % 3 == 0 { 2 } else { 3 };
        let mut s = spec(i, size);
        s.endpoint = if input { 130 } else { 2 };
        s.kind = kind;
        s.flags = 0x40041;
        if kind == 2 {
            s.setup = Some([if input { 0x80 } else { 0 }, 6, 0, 1, 0, 0, 0, 0]);
        }
        e.submit(s, |bytes| {
            bytes.fill(i as u8);
            Ok(())
        })
        .unwrap();
        let actual = if size == 0 { 0 } else { size - i % (size + 1) };
        let status = if i % 4 == 0 { -libc::ETIMEDOUT } else { 0 };
        m.finish(
            m.last(),
            status,
            actual,
            &vec![i as u8; if input { actual as usize } else { 0 }],
        );
        let r = e.reap(100).unwrap().unwrap();
        let checksum: u32 = r.data().unwrap_or_default().iter().map(|b| *b as u32).sum();
        println!("TRACE {} {} {} {}", r.id, r.status, r.actual, checksum);
    }
}

#[test]
fn failed_close_keeps_kernel_memory_alive() {
    let mock = Arc::new(Mock::default());
    let engine = Engine::new(Arc::clone(&mock));
    engine
        .submit(
            Spec {
                id: 7,
                kind: 3,
                endpoint: 0x81,
                flags: 0,
                length: 8,
                setup: None,
            },
            |_| Ok(()),
        )
        .unwrap();
    let urb = mock.last();
    // Retain the allocation identity solely to reclaim the intentional quarantine
    // after the fake kernel explicitly retires it. Production cannot make this claim.
    let allocation = {
        let mut state = engine.state.lock().unwrap();
        unsafe {
            state.requests[0]
                .as_mut()
                .unwrap()
                .as_mut()
                .get_unchecked_mut() as *mut Request
        }
    };
    {
        let mut k = mock.kernel.lock().unwrap();
        k.discard_error = libc::ENODEV;
        k.reap_error = libc::ENODEV;
    }
    assert_eq!(engine.close(), 1);
    assert_eq!(engine.close(), 0);
    assert_eq!(engine.cancel(7), Err(libc::ENODEV));
    // A delayed kernel write must still be safe after failed teardown.
    mock.finish(urb, 0, 8, b"delayed!");
    mock.kernel.lock().unwrap().reap_error = 0;
    assert_eq!(mock.reap().unwrap(), Some(urb.0));
    // SAFETY: close forgot precisely this pinned Box; mock reap ended its only
    // outstanding borrow, and there are no other owners or references.
    unsafe {
        let mut request = Box::from_raw(allocation);
        request.in_flight = false;
        assert_eq!(request.bytes_mut(), b"delayed!");
    }
}
