use crate::{linux::Linux, wan::Session as Engine};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI64, Ordering},
};
// Matches the Android USB/IP server's four-client limit. No hashing or growth
// allocation is needed on the JNI hot path.
// A closing slot remains reserved until draining completes.
type Session = Option<(i64, Option<Arc<Engine<Linux>>>)>;
static SESSIONS: Mutex<[Session; 4]> = Mutex::new([const { None }; 4]);
static NEXT: AtomicI64 = AtomicI64::new(1);
static QUARANTINED: AtomicBool = AtomicBool::new(false);
pub fn open(fd: i32) -> i64 {
    if QUARANTINED.load(Ordering::Acquire) {
        return 0;
    }
    let Ok(backend) = Linux::open(fd) else {
        return 0;
    };
    let Ok(mut sessions) = SESSIONS.lock() else {
        return 0;
    };
    if QUARANTINED.load(Ordering::Acquire) {
        return 0;
    }
    let Some(slot) = sessions.iter_mut().find(|s| s.is_none()) else {
        return 0;
    };
    let Ok(engine) = Arc::try_new(Engine::new(backend)) else {
        return 0;
    };
    let Ok(id) = NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)) else {
        return 0;
    };
    *slot = Some((id, Some(engine)));
    id
}
pub fn get(id: i64) -> Result<Arc<Engine<Linux>>, i32> {
    if id <= 0 {
        return Err(libc::EINVAL);
    }
    SESSIONS
        .lock()
        .map_err(|_| libc::EIO)?
        .iter()
        .flatten()
        .find(|(key, _)| *key == id)
        .and_then(|(_, e)| e.as_ref().map(Arc::clone))
        .ok_or(libc::EINVAL)
}
pub fn close(id: i64) {
    let engine = {
        let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        sessions
            .iter_mut()
            .flatten()
            .find(|(key, _)| *key == id)
            .and_then(|(_, engine)| engine.take())
    };
    let Some(engine) = engine else {
        return;
    };
    let quarantined = engine.close();
    let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if quarantined != 0 {
        QUARANTINED.store(true, Ordering::Release);
        eprintln!(
            "USB Most: kernel did not retire requests; native sessions disabled until process restart"
        );
    }
    if let Some(slot) = sessions
        .iter_mut()
        .find(|s| s.as_ref().is_some_and(|(key, _)| *key == id))
    {
        *slot = None;
    }
}
