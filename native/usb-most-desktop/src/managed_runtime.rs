//! One bounded owning worker per managed import. UI/transport shutdown must not
//! drop the actual OS attachment while a synchronous removal is still running.
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use usb_most_protocol::{
    agent::{self, Connector},
    eject::{Decision, ImportKey, Ledger, Operation, Outcome},
    managed::{Action, Directive, Report},
};

static OWNERS: AtomicUsize = AtomicUsize::new(0);
static WORKERS: AtomicUsize = AtomicUsize::new(0);
const LIMIT: usize = 4;

/// Graceful service shutdown waits for actual OS calls, not just UI observers.
/// Service-manager forced termination remains an OS policy, never a Safe result.
pub async fn drain() {
    while WORKERS.load(Ordering::Acquire) != 0 {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
struct Worker;
impl Worker {
    fn reserve() -> Self {
        WORKERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Reserve before attaching, including old workers retained across profile changes.
pub struct Permit(&'static AtomicUsize);
impl Permit {
    pub fn active() -> usize {
        OWNERS.load(Ordering::Acquire)
    }
    pub fn acquire() -> io::Result<Self> {
        Self::acquire_at(&OWNERS)
    }
    fn acquire_at(counter: &'static AtomicUsize) -> io::Result<Self> {
        counter
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < LIMIT).then(|| n + 1)
            })
            .map(|_| Self(counter))
            .map_err(|_| io::ErrorKind::WouldBlock.into())
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub trait Attachment: Send + 'static {
    fn finished(&self) -> bool;
    /// Returns only after the local OS adapter has retired its actual worker.
    /// Retain the socket/driver attachment throughout the call. No force removal.
    fn eject(&mut self) -> Outcome;
}

/// The import loop observes managed workers; it never borrows their attachment.
/// A transient/empty device list cannot discard the only copy of an OS result.
pub enum Import<A: Attachment> {
    Legacy { attachment: A, _permit: Permit },
    Managed(Managed),
}
impl<A: Attachment> Import<A> {
    pub fn new(
        attachment: A,
        permit: Permit,
        key: Option<ImportKey>,
        remote: Arc<dyn Connector>,
    ) -> io::Result<Self> {
        match key {
            Some(key) => Managed::start(permit, attachment, key, remote).map(Self::Managed),
            None => Ok(Self::Legacy {
                attachment,
                _permit: permit,
            }),
        }
    }
    pub fn finished(&self) -> bool {
        match self {
            Self::Legacy { attachment, .. } => attachment.finished(),
            Self::Managed(worker) => worker.finished(),
        }
    }
    pub fn retain(&self, listed: bool, session: Option<[u8; 16]>) -> bool {
        match self {
            Self::Legacy { .. } => listed,
            Self::Managed(worker) => session.is_none_or(|session| session == worker.session()),
        }
    }
}

pub struct Managed {
    session: [u8; 16],
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    worker: thread::JoinHandle<()>,
}

struct Finished(Arc<AtomicBool>, bool);
impl Drop for Finished {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(true, Ordering::Release);
        }
    }
}

impl Managed {
    pub fn start<A: Attachment>(
        permit: Permit,
        attachment: A,
        key: ImportKey,
        remote: Arc<dyn Connector>,
    ) -> io::Result<Self> {
        Self::spawn(
            permit,
            attachment,
            key,
            move |report| agent::sync_ejection(remote.as_ref(), report),
            Duration::from_secs(1),
        )
    }
    fn spawn<A, P>(
        permit: Permit,
        mut attachment: A,
        key: ImportKey,
        mut sync: P,
        interval: Duration,
    ) -> io::Result<Self>
    where
        A: Attachment,
        P: FnMut(Report) -> io::Result<Directive> + Send + 'static,
    {
        let mut ledger = Ledger::new(key).map_err(|_| io::ErrorKind::InvalidInput)?;
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let finished = Finished(done.clone(), true);
        let running = Worker::reserve();
        let worker = thread::Builder::new()
            .name("usb-most-managed".into())
            .spawn(move || {
                let _running = running;
                let mut finished = finished;
                while !stopping.load(Ordering::Acquire) {
                    let report = Report {
                        import: key,
                        state: ledger.state(),
                    };
                    let result = sync(report);
                    // A command which returns after explicit withdrawal cannot
                    // start a new OS operation. Already running calls are owned
                    // by this thread and never interrupted by the stop flag.
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    match result {
                        // Malformed/stale metadata cannot authorize discarding
                        // a terminal ledger or starting a fresh automatic import.
                        Ok(command) if command.import != key => {}
                        Ok(command) => match command.action {
                            Action::Retire => break,
                            Action::Wait => {}
                            Action::Eject => {
                                match ledger.begin(Operation {
                                    import: key,
                                    sequence: command.sequence,
                                }) {
                                    Ok(Decision::Execute(operation)) => {
                                        let result = std::panic::catch_unwind(
                                            std::panic::AssertUnwindSafe(|| attachment.eject()),
                                        );
                                        let Ok(outcome) = result else {
                                            // An unexpected adapter panic cannot
                                            // establish retirement. Keep the
                                            // owner AND its bound until process
                                            // exit; do not make a fresh slot.
                                            std::mem::forget(attachment);
                                            std::mem::forget(permit);
                                            // Leave the observer as a tombstone:
                                            // no new import of this bus ID until
                                            // explicit stop/session replacement.
                                            finished.1 = false;
                                            eprintln!("USB Most removal owner quarantined");
                                            return;
                                        };
                                        if ledger.complete(&operation, outcome).is_err() {
                                            break;
                                        }
                                        continue; // Deliver the actual result promptly.
                                    }
                                    Ok(Decision::Pending | Decision::Result(_)) => {}
                                    Err(_) => {}
                                }
                            }
                        },
                        Err(_) if attachment.finished() && !ledger.suppress_reimport() => break,
                        Err(_) => {}
                    }
                    // USB EOF is not Safe. A completed/uncertain operation
                    // stays here until exact-key Retire or explicit shutdown.
                    thread::park_timeout(interval);
                }
                drop(attachment);
                drop(permit);
            })?;
        Ok(Self {
            session: key.session,
            stop,
            done,
            worker,
        })
    }
    pub fn session(&self) -> [u8; 16] {
        self.session
    }
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}
impl Drop for Managed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker.thread().unpark();
        // JoinHandle drop only detaches the observer. The worker owns its actual
        // attachment/permit until an in-progress OS call returns.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};
    const KEY: ImportKey = ImportKey {
        session: [7; 16],
        number: 1,
    };
    struct Mock {
        drops: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        reply: mpsc::Receiver<Outcome>,
        entered: mpsc::Sender<()>,
        eof: bool,
    }
    impl Attachment for Mock {
        fn finished(&self) -> bool {
            self.eof
        }
        fn eject(&mut self) -> Outcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send(()).unwrap();
            self.reply.recv().unwrap()
        }
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn mock(eof: bool) -> (Mock, mpsc::Sender<Outcome>, mpsc::Receiver<()>) {
        let (tx, reply) = mpsc::channel();
        let (entered, rx) = mpsc::channel();
        (
            Mock {
                drops: Arc::new(AtomicUsize::new(0)),
                calls: Arc::new(AtomicUsize::new(0)),
                reply,
                entered,
                eof,
            },
            tx,
            rx,
        )
    }
    fn command(sequence: u64, action: Action) -> Directive {
        Directive {
            import: KEY,
            sequence,
            action,
        }
    }
    fn wait(ready: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !ready() && std::time::Instant::now() < end {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(ready());
    }
    #[test]
    fn dropping_observer_retains_socket_and_global_slot_through_os_return() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, reply, entered) = mock(false);
        let drops = device.drops.clone();
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            |_| Ok(command(1, Action::Eject)),
            Duration::from_millis(1),
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(task);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(COUNT.load(Ordering::Acquire), 1);
        let remaining: Vec<_> = (0..3)
            .map(|_| Permit::acquire_at(&COUNT).unwrap())
            .collect();
        assert!(Permit::acquire_at(&COUNT).is_err());
        reply.send(Outcome::Safe).unwrap();
        wait(|| drops.load(Ordering::Acquire) == 1 && COUNT.load(Ordering::Acquire) == 3);
        drop(remaining);
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
    }
    #[test]
    fn duplicate_delivery_executes_once_and_safe_waits_for_ack_despite_usb_eof() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, reply, _entered) = mock(true);
        let calls = device.calls.clone();
        let drops = device.drops.clone();
        reply.send(Outcome::Busy).unwrap();
        reply.send(Outcome::Safe).unwrap();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let saved = reports.clone();
        let mut commands = [
            command(1, Action::Eject),
            command(1, Action::Eject),
            command(2, Action::Eject),
            command(2, Action::Eject),
            command(2, Action::Retire),
        ]
        .into_iter();
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            move |report| {
                saved.lock().unwrap().push(report.state);
                Ok(commands.next().unwrap())
            },
            Duration::from_millis(1),
        )
        .unwrap();
        wait(|| task.finished());
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
        assert_eq!(reports.lock().unwrap().len(), 5);
    }
    #[test]
    fn withdrawal_while_metadata_is_delayed_prevents_new_os_execution() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, _reply, _entered) = mock(false);
        let calls = device.calls.clone();
        let drops = device.drops.clone();
        let (started, ready) = mpsc::channel();
        let (release, barrier) = mpsc::channel();
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            move |_| {
                started.send(()).unwrap();
                barrier.recv().unwrap();
                Ok(command(1, Action::Eject))
            },
            Duration::from_millis(1),
        )
        .unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(task);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        release.send(()).unwrap();
        wait(|| drops.load(Ordering::Acquire) == 1);
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }
    #[test]
    fn terminal_result_survives_lost_reply_stale_commands_and_empty_devlist() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, reply, _entered) = mock(true);
        let calls = device.calls.clone();
        let drops = device.drops.clone();
        reply.send(Outcome::Uncertain).unwrap();
        let polls = Arc::new(AtomicUsize::new(0));
        let observed = polls.clone();
        let (retire, retirement) = mpsc::channel();
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            move |_| {
                let n = observed.fetch_add(1, Ordering::AcqRel);
                match n {
                    0 => Ok(command(1, Action::Eject)),
                    1 => Err(io::ErrorKind::ConnectionReset.into()),
                    2 => Ok(Directive {
                        import: ImportKey { number: 99, ..KEY },
                        ..command(1, Action::Retire)
                    }),
                    3 => Ok(command(2, Action::Eject)), // no new operation after Uncertain
                    _ => {
                        retirement.recv().unwrap();
                        Ok(command(1, Action::Retire))
                    }
                }
            },
            Duration::from_millis(1),
        )
        .unwrap();
        let import: Import<Mock> = Import::Managed(task);
        wait(|| polls.load(Ordering::Acquire) >= 5);
        assert!(import.retain(false, None));
        assert!(import.retain(false, Some(KEY.session)));
        assert!(!import.retain(true, Some([8; 16])));
        assert!(!import.finished());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        retire.send(()).unwrap();
        wait(|| import.finished());
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
    }
    #[test]
    fn legacy_device_disappearance_still_releases_only_its_own_permit() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, _reply, _) = mock(false);
        let drops = device.drops.clone();
        let import = Import::new(
            device,
            Permit::acquire_at(&COUNT).unwrap(),
            None,
            Arc::new(agent::Direct("127.0.0.1:1".parse().unwrap())),
        )
        .unwrap();
        assert!(import.retain(true, None));
        assert!(!import.retain(false, None));
        assert!(!import.finished());
        drop(import);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
    }
    #[test]
    fn graceful_shutdown_does_not_complete_while_os_borrows_attachment() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, reply, entered) = mock(false);
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            |_| Ok(command(1, Action::Eject)),
            Duration::from_millis(1),
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(task);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), drain())
                    .await
                    .is_err()
            );
            reply.send(Outcome::Safe).unwrap();
            tokio::time::timeout(Duration::from_secs(10), drain())
                .await
                .unwrap();
        });
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
    }
    #[test]
    fn adapter_panic_quarantines_owner_and_observer_instead_of_reimporting() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let (device, reply, entered) = mock(false);
        let drops = device.drops.clone();
        let (finished, ended) = mpsc::channel();
        struct End(mpsc::Sender<()>);
        impl Drop for End {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let marker = End(finished);
        drop(reply); // mock OS adapter panics when the response channel closes
        let task = Managed::spawn(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            move |_| {
                let _ = &marker;
                Ok(command(1, Action::Eject))
            },
            Duration::from_millis(1),
        )
        .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        ended.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!task.finished());
        assert_eq!(COUNT.load(Ordering::Acquire), 1);
        drop(task);
        assert_eq!(drops.load(Ordering::Acquire), 0);
    }
    #[test]
    fn real_authenticated_tcp_and_book_recover_lost_eject_and_safe_ack_without_replay() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use usb_most_connection::{direct::GatewayConnector, gateway};
        use usb_most_protocol::{
            eject::State,
            managed::{self, Book},
        };
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(20), async {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let mut book = Book::new(KEY.session).unwrap();
                    let key = book.execute(2, &[]).unwrap();
                    assert_eq!(managed::decode_key(&key).unwrap(), KEY);
                    for step in 0..5 {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        gateway::server(&mut socket, &[9; 32]).await.unwrap();
                        socket.write_all(&[1]).await.unwrap();
                        let mut header = [0; 8];
                        socket.read_exact(&mut header).await.unwrap();
                        assert_eq!(header, [1, 17, 128, 227, 0, 0, 0, 0]);
                        let mut report = [0; 40];
                        socket.read_exact(&mut report).await.unwrap();
                        let state = Report::decode(&report).unwrap().state;
                        let mut response = book.execute(6, &report).unwrap();
                        if step == 0 {
                            assert_eq!(state, State::Idle);
                            book.execute(5, &key).unwrap(); // explicit fixture UI action
                        }
                        if step >= 3 {
                            assert_eq!(
                                state,
                                State::Completed {
                                    sequence: 1,
                                    outcome: Outcome::Safe
                                }
                            );
                        }
                        if step == 3 {
                            book.execute(8, &key).unwrap(); // exact fixture lease applied Safe
                            response = book.execute(6, &report).unwrap();
                        }
                        if step == 1 || step == 3 {
                            continue;
                        } // lose command/result ACK
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        socket
                            .write_all(&[1, 17, 0, 227, 0, 0, 0, 0])
                            .await
                            .unwrap();
                        // Deliberately fragment metadata instead of one packet.
                        for chunk in response.chunks(7) {
                            socket.write_all(chunk).await.unwrap();
                        }
                    }
                })
                .await
                .unwrap();
            });
        });
        let (device, reply, _entered) = mock(false);
        let calls = device.calls.clone();
        let drops = device.drops.clone();
        reply.send(Outcome::Safe).unwrap();
        let task = Managed::start(
            Permit::acquire_at(&COUNT).unwrap(),
            device,
            KEY,
            Arc::new(GatewayConnector::new(address, [9; 32]).unwrap()),
        )
        .unwrap();
        server.join().unwrap();
        wait(|| task.finished());
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(COUNT.load(Ordering::Acquire), 0);
    }
}

#[cfg(test)]
#[path = "managed_runtime/transport_tests.rs"]
pub(crate) mod transport_tests;
