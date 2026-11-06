//! Private signed-driver instance + single-use import capability. No CLI secrets.
use std::{
    io,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use usb_most_platform::windows::driver::Controller;
use usb_most_protocol::agent::{self, Connector, ImportAlias, Policy};
use usb_most_protocol::{
    eject::{ImportKey, Outcome},
    managed::Capabilities,
};
pub fn available() -> bool {
    Controller::open().is_ok()
}
pub struct Attached {
    controller: Controller,
    port: i32,
    local_port: u16,
    alias: zeroize::Zeroizing<[u8; 32]>,
    key: Option<ImportKey>,
    halt: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    done: mpsc::Receiver<()>,
    _worker: thread::JoinHandle<()>,
}
impl Attached {
    pub fn import_key(&self) -> Option<ImportKey> {
        self.key
    }
    pub fn finished(&self) -> bool {
        !matches!(self.done.try_recv(), Err(mpsc::TryRecvError::Empty))
    }
    fn close_data(&self) {
        self.halt.store(true, Ordering::Release);
        if let Ok(mut slot) = self.socket.lock()
            && let Some(socket) = slot.take()
        {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}
impl crate::managed_runtime::Attachment for Attached {
    fn finished(&self) -> bool {
        self.finished()
    }
    fn eject(&mut self) -> Outcome {
        let owner = match usb_most_platform::windows::eject::OwnedImport::capture(
            &self.controller,
            self.port,
            self.local_port,
            &self.alias,
        ) {
            Ok(owner) => owner,
            Err(error) => {
                return match error.kind() {
                    io::ErrorKind::PermissionDenied => Outcome::Denied,
                    io::ErrorKind::WouldBlock => Outcome::Busy,
                    io::ErrorKind::NotFound => Outcome::Unavailable,
                    _ => Outcome::Uncertain,
                };
            }
        };
        let outcome = match owner.start_eject() {
            Ok(worker) => worker.join().unwrap_or(Outcome::Uncertain),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Outcome::Busy,
            Err(_) => Outcome::Unavailable,
        };
        if outcome == Outcome::Safe {
            // The OS call has returned and confirmed removal. Release the data
            // connection now; retaining an otherwise idle proxy can prevent the
            // exporter accepting a new import while result delivery is retried.
            // The coordinator still owns this exact alias/controller and ledger
            // until acknowledgement. Its eventual Drop must not detach a reuse.
            self.close_data();
        }
        outcome
    }
}
impl Drop for Attached {
    fn drop(&mut self) {
        self.halt.store(true, Ordering::Release);
        // Query before closing our socket: EOF can release/reuse the driver port.
        // This is ordinary connection teardown, never evidence of OS Safe.
        let _ = self
            .controller
            .detach_owned(self.port, self.local_port, &self.alias);
        self.close_data();
    }
}
pub fn attach(
    id: &str,
    remote: Arc<dyn Connector>,
    policy: Arc<Mutex<Policy>>,
) -> io::Result<Attached> {
    attach_with_management(id, remote, policy, None)
}
pub fn attach_with_management(
    id: &str,
    remote: Arc<dyn Connector>,
    policy: Arc<Mutex<Policy>>,
    capabilities: Option<Capabilities>,
) -> io::Result<Attached> {
    let controller = Controller::open()?;
    let random = zeroize::Zeroizing::new(
        usb_most_connection::security::random::<15>().map_err(|_| io::ErrorKind::Other)?,
    );
    let name = zeroize::Zeroizing::new(
        random
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect::<String>(),
    );
    let alias = ImportAlias::new(&name, id)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let local_port = listener.local_addr()?.port();
    let halt = Arc::new(AtomicBool::new(false));
    let stop = halt.clone();
    let socket = Arc::new(Mutex::new(None));
    let guard = socket.clone();
    let (tx, done) = mpsc::sync_channel(1);
    let (imported, imported_key) = mpsc::sync_channel(1);
    let encoded = zeroize::Zeroizing::new(*alias.encoded());
    let worker = thread::Builder::new()
        .name("usb-most-win-proxy".into())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(90);
            while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => {
                        drop(listener);
                        if let Ok(copy) = stream.try_clone()
                            && let Ok(mut slot) = guard.lock()
                        {
                            *slot = Some(copy);
                        }
                        // A guessed/late/replayed alias is rejected before opening ANY remote hop.
                        // The driver uses PLUGIN_HARDWARE_ONCE; a new attempt creates a new alias.
                        let result = if let Some(capabilities) = capabilities {
                            agent::serve_managed(
                                stream,
                                remote.as_ref(),
                                &policy,
                                Some(&alias),
                                &agent::Management {
                                    capabilities,
                                    imported: &|key| {
                                        imported
                                            .try_send(key)
                                            .map_err(|_| io::ErrorKind::InvalidData.into())
                                    },
                                },
                            )
                        } else {
                            agent::serve_alias(stream, remote.as_ref(), &policy, &alias)
                        };
                        if let Err(error) = result {
                            eprintln!("USB Most import ended: {:?}", error.kind());
                        }
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(());
        })?;
    match controller.attach_once(local_port, &encoded) {
        Ok(port) => {
            let mut attached = Attached {
                controller,
                port,
                local_port,
                alias: encoded,
                key: None,
                halt,
                socket,
                done,
                _worker: worker,
            };
            if capabilities.is_some() {
                attached.key = Some(
                    imported_key
                        // The proxy publishes the key before returning the
                        // descriptor consumed by the driver's attach IOCTL.
                        .try_recv()
                        .map_err(|_| io::ErrorKind::InvalidData)?,
                );
            }
            Ok(attached)
        }
        Err(error) => {
            halt.store(true, Ordering::Release);
            if let Ok(mut slot) = socket.lock()
                && let Some(s) = slot.take()
            {
                let _ = s.shutdown(Shutdown::Both);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
