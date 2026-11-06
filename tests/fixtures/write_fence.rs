//! Test-only commit-before-acknowledgement fence for the isolated RAM disk.
//! The fixed range is disjoint from its FAT metadata and normal mixed workload.
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use std::io;

pub const OFFSET: usize = 16 * 1024 * 1024;
pub const LENGTH: usize = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Dormant,
    Armed,
    Held,
    Released,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub phase: Phase,
    pub commits: u64,
    pub bytes: u64,
    pub first_import: u64,
    pub first_tag: u32,
}

#[derive(Default)]
pub struct WriteFence {
    state: Mutex<Snapshot>,
    changed: Condvar,
}

impl WriteFence {
    pub fn snapshot(&self) -> Snapshot { *self.state.lock().unwrap() }

    pub fn arm(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.phase != Phase::Dormant {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        state.phase = Phase::Armed;
        Ok(())
    }

    pub fn release(&self) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.phase != Phase::Held {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        state.phase = Phase::Released;
        self.changed.notify_all();
        Ok(())
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(state.phase, Phase::Armed | Phase::Held) {
            state.phase = Phase::Cancelled;
            self.changed.notify_all();
        }
    }

    /// Called only after the copy to RAM, before sending RET_SUBMIT.
    pub fn committed(&self, offset: usize, length: usize, import: u64, tag: u32,
                     budget: Duration) -> io::Result<()> {
        let end = offset.checked_add(length).ok_or(io::ErrorKind::InvalidInput)?;
        if offset >= OFFSET + LENGTH || end <= OFFSET { return Ok(()); }
        let mut state = self.state.lock().unwrap();
        if state.phase == Phase::Dormant { return Ok(()); }
        state.commits += 1;
        state.bytes += length as u64;
        if state.commits == 1 {
            state.first_import = import;
            state.first_tag = tag;
        }
        if state.phase != Phase::Armed { return Ok(()); }
        // Fail rather than silently weakening this precise single-URB scenario.
        if offset != OFFSET || length != LENGTH {
            return Err(io::ErrorKind::InvalidData.into());
        }
        state.phase = Phase::Held;
        let end = Instant::now() + budget;
        while state.phase == Phase::Held {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.phase = Phase::TimedOut;
                return Err(io::ErrorKind::TimedOut.into());
            }
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
        match state.phase {
            Phase::Released => Ok(()),
            _ => Err(io::ErrorKind::Interrupted.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread};

    fn held(fence: &WriteFence) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while fence.snapshot().phase != Phase::Held {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn release_tracks_duplicate_commit_across_imports_without_holding_again() {
        let fence = Arc::new(WriteFence::default());
        assert!(fence.release().is_err());
        fence.arm().unwrap();
        assert!(fence.arm().is_err());
        fence.committed(8 * 1024 * 1024, LENGTH, 1, 4, Duration::ZERO).unwrap();
        assert_eq!(fence.snapshot().commits, 0);
        let worker = { let f = fence.clone(); thread::spawn(move ||
            f.committed(OFFSET, LENGTH, 1, 5, Duration::from_secs(2))) };
        held(&fence);
        assert!(!worker.is_finished());
        assert_eq!(fence.snapshot().bytes, LENGTH as u64);
        fence.release().unwrap();
        worker.join().unwrap().unwrap();
        assert!(fence.release().is_err());
        fence.committed(OFFSET, LENGTH, 2, 6, Duration::ZERO).unwrap();
        let state = fence.snapshot();
        assert_eq!((state.commits, state.bytes, state.first_import, state.first_tag),
                   (2, 8192, 1, 5));
    }

    #[test]
    fn timeout_and_cancellation_never_report_success() {
        let fence = WriteFence::default();
        fence.arm().unwrap();
        assert_eq!(fence.committed(OFFSET, LENGTH, 1, 1, Duration::ZERO)
                   .unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(fence.snapshot().phase, Phase::TimedOut);
        let fence = Arc::new(WriteFence::default());
        fence.arm().unwrap();
        let worker = { let f = fence.clone(); thread::spawn(move ||
            f.committed(OFFSET, LENGTH, 1, 1, Duration::from_secs(2))) };
        held(&fence);
        fence.cancel();
        assert_eq!(worker.join().unwrap().unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(fence.release().is_err());
    }
}
