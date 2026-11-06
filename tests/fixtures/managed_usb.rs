//! Isolated test gateway: authenticated UMM1 metadata + a caller-owned synthetic
//! legacy USB fixture. Never discovers or opens host USB or filesystem devices.
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use usb_most_connection::{direct::GatewayConnector, gateway};
use usb_most_protocol::{
    agent::{self, Connector},
    eject::{ImportKey, Outcome, State},
    managed::{self, Action, Book, Capabilities, Directive, ImportRequest, Report},
};
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
struct Shared {
    book: Mutex<Book>,
    descriptor: Vec<u8>,
    backend_descriptor: Vec<u8>,
    linux_headers: bool,
    interfaces: Vec<u8>,
    backend: SocketAddr,
    proof: [u8; 32],
    lose_eject: AtomicUsize,
    lose_safe: AtomicUsize,
    pub lost_commands: AtomicUsize,
    pub lost_results: AtomicUsize,
}
pub struct Fixture {
    shared: Arc<Shared>,
    remote: Arc<dyn Connector>,
    stop: tokio::sync::watch::Sender<bool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    pub fn start(backend: SocketAddr) -> Self {
        Self::start_device(backend, None)
    }
    pub fn start_device(backend: SocketAddr, busid: Option<&str>) -> Self {
        assert!(backend.ip().is_loopback());
        let mut devices = agent::devices(backend).unwrap();
        if let Some(id) = busid {
            assert!(
                !id.is_empty()
                    && id.len() < 32
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'.'))
            );
            let mut name = [0; 32];
            name[..id.len()].copy_from_slice(id.as_bytes());
            devices.retain(|(d, _)| d[256..288] == name);
        }
        assert_eq!(devices.len(), 1, "one caller-owned synthetic device only");
        let (backend_descriptor, interfaces) = devices.pop().unwrap();
        let mut descriptor = backend_descriptor.clone();
        if busid.is_some() {
            // Real QEMU/usbip_host IDs can include hub dots. Expose the same
            // bounded numeric Android busid used by this product's publisher.
            // The exact upstream descriptor remains separately pinned below.
            descriptor[256..288].fill(0);
            descriptor[256..259].copy_from_slice(b"1-8");
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proof = usb_most_connection::security::random().unwrap();
        let remote =
            Arc::new(GatewayConnector::new(listener.local_addr().unwrap(), proof).unwrap());
        let shared = Arc::new(Shared {
            book: Mutex::new(Book::new(usb_most_connection::security::random().unwrap()).unwrap()),
            descriptor,
            backend_descriptor,
            linux_headers: busid.is_some(),
            interfaces,
            backend,
            proof,
            lose_eject: AtomicUsize::new(1),
            lose_safe: AtomicUsize::new(1),
            lost_commands: AtomicUsize::new(0),
            lost_results: AtomicUsize::new(0),
        });
        let state = shared.clone();
        let (stop, mut end) = tokio::sync::watch::channel(false);
        let worker = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let mut jobs = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = end.changed() => break,
                        Some(result) = jobs.join_next(), if !jobs.is_empty() => { result.unwrap(); },
                        incoming = listener.accept(), if jobs.len() < 16 => {
                            let (stream, _) = incoming.unwrap();
                            let state = state.clone();
                            jobs.spawn(async move { let _ = serve(stream, state).await; });
                        }
                    }
                }
                jobs.abort_all();
                while jobs.join_next().await.is_some() {}
            });
        });
        Self {
            shared,
            remote,
            stop,
            worker: Some(worker),
        }
    }
    pub fn connector(&self) -> Arc<dyn Connector> {
        self.remote.clone()
    }
    pub fn capabilities(&self) -> Capabilities {
        agent::management_capabilities(self.remote.as_ref())
            .unwrap()
            .unwrap()
    }
    pub fn ready(&self, key: ImportKey) -> bool {
        self.shared
            .book
            .lock()
            .unwrap()
            .execute(10, &managed::encode_key(key).unwrap())
            == Some(vec![1])
    }
    pub fn request(&self, key: ImportKey) -> u64 {
        let b = self
            .shared
            .book
            .lock()
            .unwrap()
            .execute(5, &managed::encode_key(key).unwrap())
            .unwrap();
        u64::from_be_bytes(b.try_into().unwrap())
    }
    pub fn state(&self, key: ImportKey) -> Option<State> {
        self.shared
            .book
            .lock()
            .unwrap()
            .execute(7, &managed::encode_key(key).unwrap())
            .map(|b| Report::decode(&b).unwrap().state)
    }
    pub fn acknowledge(&self, key: ImportKey) {
        self.shared
            .book
            .lock()
            .unwrap()
            .execute(8, &managed::encode_key(key).unwrap())
            .unwrap();
    }
    pub fn losses(&self) -> (usize, usize) {
        (
            self.shared.lost_commands.load(Ordering::Acquire),
            self.shared.lost_results.load(Ordering::Acquire),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
struct Released(Arc<Shared>, Vec<u8>);
impl Drop for Released {
    fn drop(&mut self) {
        self.0.book.lock().unwrap().execute(3, &self.1);
    }
}
async fn reply(stream: &mut tokio::net::TcpStream, op: u16, body: &[u8]) -> io::Result<()> {
    let mut header = [0; 8];
    header[..2].copy_from_slice(&[1, 17]);
    header[2..4].copy_from_slice(&(op & 0x7fff).to_be_bytes());
    stream.write_all(&header).await?;
    stream.write_all(body).await
}
async fn serve(mut stream: tokio::net::TcpStream, state: Arc<Shared>) -> io::Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        gateway::server(&mut stream, &state.proof),
    )
    .await
    .map_err(|_| io::ErrorKind::TimedOut)??;
    stream.write_all(&[1]).await?;
    let mut header = [0; 8];
    stream.read_exact(&mut header).await?;
    if header[..2] != [1, 17] || header[4..] != [0; 4] {
        return Err(invalid());
    }
    let op = u16::from_be_bytes(header[2..4].try_into().unwrap());
    match op {
        managed::CAPS => {
            let bytes = state.book.lock().unwrap().execute(0, &[]).unwrap();
            let mut caps = Capabilities::decode(&bytes).unwrap();
            caps.features = managed::GENERIC | managed::EJECT;
            reply(&mut stream, op, &caps.encode().unwrap()).await
        }
        0x8005 => {
            let mut bytes = 1u32.to_be_bytes().to_vec();
            bytes.extend_from_slice(&state.descriptor);
            bytes.extend_from_slice(&state.interfaces);
            reply(&mut stream, op, &bytes).await
        }
        managed::SYNC => {
            let mut body = [0; 40];
            stream.read_exact(&mut body).await?;
            let report = Report::decode(&body).map_err(|_| invalid())?;
            let response = state
                .book
                .lock()
                .unwrap()
                .execute(6, &body)
                .ok_or_else(invalid)?;
            let directive = Directive::decode(&response).map_err(|_| invalid())?;
            if directive.action == Action::Eject && state.lose_eject.swap(0, Ordering::AcqRel) == 1
            {
                state.lost_commands.fetch_add(1, Ordering::AcqRel);
                return Ok(());
            }
            if matches!(
                report.state,
                State::Completed {
                    outcome: Outcome::Safe,
                    ..
                }
            ) && state.lose_safe.swap(0, Ordering::AcqRel) == 1
            {
                state.lost_results.fetch_add(1, Ordering::AcqRel);
                return Ok(());
            }
            reply(&mut stream, op, &response).await
        }
        managed::IMPORT => {
            let mut body = [0; 52];
            stream.read_exact(&mut body).await?;
            let request = ImportRequest::decode(&body).map_err(|_| invalid())?;
            if request.wan || request.busid != state.descriptor[256..288] {
                return Err(invalid());
            }
            let key = {
                let mut book = state.book.lock().unwrap();
                book.execute(1, &body).ok_or_else(invalid)?;
                book.execute(2, &[]).ok_or_else(invalid)?
            };
            let _released = Released(state.clone(), key.clone());
            let mut upstream = tokio::net::TcpStream::connect(state.backend).await?;
            upstream.write_all(&[1, 17, 128, 3, 0, 0, 0, 0]).await?;
            upstream
                .write_all(&state.backend_descriptor[256..288])
                .await?;
            let mut response = [0; 320];
            upstream.read_exact(&mut response).await?;
            if response[..8] != [1, 17, 0, 3, 0, 0, 0, 0]
                || response[8..] != state.backend_descriptor
            {
                return Err(invalid());
            }
            let mut output = state.descriptor.clone();
            output.extend_from_slice(&key);
            reply(&mut stream, op, &output).await?;
            if state.linux_headers {
                linux_to_android_headers(&mut stream, &mut upstream).await?;
            } else {
                tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}

/// Test-only adaptation for the QEMU usbip_host backend. Linux replies zero the
/// devid/direction/endpoint fields; Android replies carry the matched request's
/// values. See https://docs.kernel.org/usb/usbip_protocol.html, basic header.
/// Restore only these metadata fields from a bounded exact-sequence map. Payload
/// bytes/status/ordering are copied unchanged; no command is replayed or invented.
async fn linux_to_android_headers(
    client: &mut tokio::net::TcpStream,
    upstream: &mut tokio::net::TcpStream,
) -> io::Result<()> {
    use std::collections::BTreeMap;
    use usb_most_protocol::wire::{body_size, u32_at};
    let pending = Mutex::new(BTreeMap::<u32, [u8; 48]>::new());
    let (mut requests, mut responses) = tokio::io::split(client);
    let (mut input, mut output) = tokio::io::split(upstream);
    let forward = async {
        loop {
            let mut header = [0; 48];
            requests.read_exact(&mut header).await?;
            if !matches!(u32_at(&header, 0), 1 | 2) {
                return Err(invalid());
            }
            let len = body_size(&header)?;
            let mut bytes = vec![0; len];
            requests.read_exact(&mut bytes).await?;
            {
                let mut map = pending.lock().unwrap();
                if map.len() >= 128 || map.insert(u32_at(&header, 4), header).is_some() {
                    return Err(invalid());
                }
            }
            output.write_all(&header).await?;
            output.write_all(&bytes).await?;
        }
    };
    let reverse = async {
        loop {
            let mut header = [0; 48];
            input.read_exact(&mut header).await?;
            let command = u32_at(&header, 0);
            if !matches!(command, 3 | 4) || header[8..20] != [0; 12] {
                return Err(invalid());
            }
            let request = {
                let mut map = pending.lock().unwrap();
                let request = map.remove(&u32_at(&header, 4)).ok_or_else(invalid)?;
                if command != u32_at(&request, 0) + 2 {
                    return Err(invalid());
                }
                if command == 4 && u32_at(&header, 20) as i32 == -104 {
                    map.remove(&u32_at(&request, 20));
                }
                request
            };
            if command == 3 && u32_at(&header, 24) > u32_at(&request, 24) {
                return Err(invalid());
            }
            header[8..20].copy_from_slice(&request[8..20]);
            let len = body_size(&header)?;
            let mut bytes = vec![0; len];
            input.read_exact(&mut bytes).await?;
            responses.write_all(&header).await?;
            responses.write_all(&bytes).await?;
        }
    };
    tokio::try_join!(forward, reverse).map(|_: ((), ())| ())
}
