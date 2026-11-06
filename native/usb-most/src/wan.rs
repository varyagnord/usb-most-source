//! Opt-in, bounded continuous input pump. Never enabled for opaque USB by class alone.
use crate::engine::{Backend, Completion, Engine, Spec};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const WINDOW: usize = crate::wire::WAN_WINDOW;
const DEPTH: usize = 2;
#[derive(Clone, Copy)]
enum Pending {
    Pass(i32),
    Input,
}
struct Pump {
    ids: u32,
    pending: BTreeMap<i32, Pending>,
    reports: VecDeque<Completion>,
    credits: usize,
    running: bool,
    closed: bool,
    fault: Option<i32>,
    epoch: u32,
    sequence: u32,
    acknowledged: u32,
    received: u64,
    sent: u64,
    highwater: usize,
    max_gap_us: u64,
    last: Option<Instant>,
    overflows: u64,
    #[cfg(feature = "diagnostics")]
    trace: Option<crate::diagnostics::Trace>,
}
impl Pump {
    fn new() -> Self {
        Self {
            ids: 0,
            pending: BTreeMap::new(),
            reports: VecDeque::new(),
            credits: WINDOW,
            running: false,
            closed: false,
            fault: None,
            epoch: 1,
            sequence: 0,
            acknowledged: 0,
            received: 0,
            sent: 0,
            highwater: 0,
            max_gap_us: 0,
            last: None,
            overflows: 0,
            #[cfg(feature = "diagnostics")]
            trace: None,
        }
    }
    fn id(&mut self) -> Result<i32, i32> {
        self.ids = self.ids.checked_add(1).ok_or(libc::EOVERFLOW)?;
        Ok(self.ids as i32)
    }
    fn stats(&self) -> Vec<i64> {
        vec![
            0,
            self.epoch as i64,
            self.running as i64,
            self.pending.len() as i64,
            self.reports.len() as i64,
            self.credits as i64,
            self.received as i64,
            self.sent as i64,
            self.highwater as i64,
            self.max_gap_us as i64,
            self.overflows as i64,
        ]
    }
}
/// The inner Engine remains the sole kernel request owner. This layer never borrows raw pointers.
pub struct Session<B: Backend> {
    inner: Engine<B>,
    pump: Mutex<Option<Pump>>,
}
impl<B: Backend> Session<B> {
    pub fn new(backend: B) -> Self {
        Self {
            inner: Engine::new(backend),
            pump: Mutex::new(None),
        }
    }
    fn fill(&self, p: &mut Pump) -> Result<(), i32> {
        while p.running
            && p.pending
                .values()
                .filter(|k| matches!(k, Pending::Input))
                .count()
                < DEPTH
        {
            if p.pending.len() >= crate::engine::MAX_PENDING {
                return Ok(());
            }
            let id = p.id()?;
            self.inner.submit(
                Spec {
                    id,
                    kind: 1,
                    endpoint: 0x83,
                    flags: 0,
                    length: 32,
                    setup: None,
                },
                |_| Ok(()),
            )?;
            p.pending.insert(id, Pending::Input);
        }
        Ok(())
    }
    pub fn command(&self, op: i32, value: i32) -> Result<Vec<i64>, i32> {
        let mut guard = self.pump.lock().map_err(|_| libc::EIO)?;
        if op == 1 {
            if value != 1 || guard.is_some() {
                return Err(libc::EINVAL);
            }
            if !self.inner.is_idle() {
                return Err(libc::EBUSY);
            }
            *guard = Some(Pump::new());
        }
        let p = guard.as_mut().ok_or(libc::EINVAL)?;
        if p.closed {
            return Err(libc::ENODEV);
        }
        if let Some(e) = p.fault {
            return Err(e);
        }
        match op {
            1 | 4 => {}
            2 => {
                if value < 0 || (value as u32) < p.acknowledged || (value as u32) > p.sequence {
                    return Err(libc::EPROTO);
                }
                let delta = (value as u32 - p.acknowledged) as usize;
                if p.credits + delta > WINDOW {
                    return Err(libc::EPROTO);
                }
                p.acknowledged = value as u32;
                p.credits += delta;
            }
            3 => {
                // configuration barrier: never reconfigure across outstanding client operations.
                if p.pending.values().any(|k| matches!(k, Pending::Pass(_))) {
                    return Err(libc::EBUSY);
                }
                p.running = false;
                for (&id, k) in &p.pending {
                    if matches!(k, Pending::Input) {
                        let _ = self.inner.cancel(id);
                    }
                }
                let deadline = Instant::now() + Duration::from_millis(500);
                while !p.pending.is_empty() {
                    if let Some(c) = self.inner.reap(0)? {
                        p.pending.remove(&c.id).ok_or(libc::EPROTO)?;
                    } else if Instant::now() >= deadline {
                        p.fault = Some(libc::ETIMEDOUT);
                        return Err(libc::ETIMEDOUT);
                    } else {
                        self.inner.wait(5)?;
                    }
                }
                p.reports.clear();
                p.epoch = p
                    .epoch
                    .checked_add(1)
                    .filter(|n| *n <= i32::MAX as u32)
                    .ok_or(libc::EOVERFLOW)?;
                p.sequence = 0;
                p.acknowledged = 0;
                p.credits = WINDOW;
                p.last = None;
            }
            5 => {
                if value != 0 || p.running {
                    return Err(libc::EPROTO);
                }
                p.running = true;
                self.fill(p)?;
            }
            _ => return Err(libc::EINVAL),
        }
        Ok(p.stats())
    }
    pub fn submit(
        &self,
        mut spec: Spec,
        fill: impl FnOnce(&mut [u8]) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let mut guard = self.pump.lock().map_err(|_| libc::EIO)?;
        let Some(p) = guard.as_mut() else {
            return self.inner.submit(spec, fill);
        };
        if p.closed {
            return Err(libc::ENODEV);
        }
        if let Some(e) = p.fault {
            return Err(e);
        }
        // This endpoint is push-owned. A peer must not mix pull and push reads.
        if spec.endpoint == 0x83 {
            return Err(libc::EOPNOTSUPP);
        }
        if p.pending
            .values()
            .any(|k| matches!(k,Pending::Pass(n) if *n==spec.id))
        {
            return Err(libc::EEXIST);
        }
        if p.pending
            .values()
            .filter(|v| matches!(v, Pending::Pass(_)))
            .count()
            >= crate::engine::MAX_PENDING - DEPTH
        {
            return Err(libc::ENOSPC);
        }
        let logical = spec.id;
        let physical = p.id()?;
        spec.id = physical;
        self.inner.submit(spec, fill)?;
        p.pending.insert(physical, Pending::Pass(logical));
        Ok(())
    }
    pub fn cancel(&self, id: i32) -> Result<(), i32> {
        let guard = self.pump.lock().map_err(|_| libc::EIO)?;
        let Some(p) = guard.as_ref() else {
            return self.inner.cancel(id);
        };
        let physical = p
            .pending
            .iter()
            .find_map(|(&n, k)| matches!(k,Pending::Pass(v) if *v==id).then_some(n))
            .ok_or(libc::ENOENT)?;
        self.inner.cancel(physical)
    }
    pub fn reap(&self, timeout: i32) -> Result<Option<Completion>, i32> {
        if !(0..=100).contains(&timeout) {
            return Err(libc::EINVAL);
        }
        let wait;
        {
            let mut guard = self.pump.lock().map_err(|_| libc::EIO)?;
            let Some(p) = guard.as_mut() else {
                drop(guard);
                return self.inner.reap(timeout);
            };
            if p.closed {
                return Err(libc::ENODEV);
            }
            if let Some(e) = p.fault {
                return Err(e);
            }
            // Bound work per call so controls/cancellation cannot be starved by input.
            for _ in 0..crate::engine::MAX_PENDING {
                let Some(mut c) = self.inner.reap(0)? else {
                    break;
                };
                match p.pending.remove(&c.id).ok_or(libc::EPROTO)? {
                    Pending::Pass(id) => {
                        c.id = id;
                        return Ok(Some(c));
                    }
                    Pending::Input => {
                        if c.status != 0 {
                            p.fault = Some(libc::EIO);
                            return Err(libc::EIO);
                        }
                        let now = Instant::now();
                        if let Some(last) = p.last {
                            p.max_gap_us = p
                                .max_gap_us
                                .max(now.duration_since(last).as_micros().min(u64::MAX as u128)
                                    as u64);
                        }
                        p.last = Some(now);
                        p.received += 1;
                        p.reports.push_back(c);
                        p.highwater = p.highwater.max(p.reports.len());
                        if p.reports.len() >= WINDOW {
                            p.overflows += 1;
                            p.fault = Some(libc::ENOBUFS);
                            return Err(libc::ENOBUFS);
                        }
                    }
                }
            }
            self.fill(p)?;
            if p.credits > 0
                && let Some(mut c) = p.reports.pop_front()
            {
                p.sequence = p
                    .sequence
                    .checked_add(1)
                    .filter(|n| *n <= i32::MAX as u32)
                    .ok_or(libc::EOVERFLOW)?;
                p.credits -= 1;
                p.sent += 1;
                c.id = 0;
                c.wan_epoch = p.epoch;
                c.wan_sequence = p.sequence;
                #[cfg(feature = "diagnostics")]
                if let Some(trace) = &mut p.trace {
                    trace.record(0, 0x83, c.status, c.actual, c.data().unwrap_or(&[]));
                }
                return Ok(Some(c));
            }
            wait = if p.reports.is_empty() {
                timeout
            } else {
                timeout.min(5)
            };
        }
        self.inner.wait(wait)?;
        Ok(None)
    }
    #[cfg(feature = "diagnostics")]
    pub fn enable_hid_diagnostics(&self) -> Result<(), i32> {
        let mut guard = self.pump.lock().map_err(|_| libc::EIO)?;
        if let Some(p) = guard.as_mut() {
            if p.trace.is_some() {
                return Err(libc::EALREADY);
            }
            p.trace = Some(crate::diagnostics::Trace::new()?);
            Ok(())
        } else {
            self.inner.enable_hid_diagnostics()
        }
    }
    #[cfg(feature = "diagnostics")]
    pub fn take_hid_diagnostics(&self) -> Result<Vec<i64>, i32> {
        let mut guard = self.pump.lock().map_err(|_| libc::EIO)?;
        if let Some(p) = guard.as_mut() {
            p.trace.as_mut().ok_or(libc::EINVAL)?.drain()
        } else {
            self.inner.take_hid_diagnostics()
        }
    }
    pub fn close(&self) -> usize {
        let mut guard = self.pump.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = guard.as_mut() {
            p.closed = true;
            p.running = false;
            p.reports.clear();
            p.pending.clear();
        }
        self.inner.close()
    }
}

#[cfg(any(test, feature = "diagnostics"))]
pub(crate) mod lab;
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) mod network_lab;
#[cfg(any(test, feature = "diagnostics"))]
pub(crate) mod sim;
#[cfg(test)]
mod tests;
