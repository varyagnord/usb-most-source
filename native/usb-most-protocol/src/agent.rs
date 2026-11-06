//! Loopback USB/IP companion. Mode is selected before IMPORT and never switched mid-operation.
use crate::wire::{self, BARRIER, CREDIT, Frame, PUSH, START, invalid};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
const HEADER: [u8; 2] = [1, 17];
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Generic,
    Wan,
}
#[derive(Default)]
pub struct Policy {
    last: BTreeMap<String, Mode>,
}
impl Policy {
    pub fn choose(&mut self, busid: &str, supported: bool, rtt: Duration) -> Mode {
        if !supported {
            return Mode::Generic;
        }
        let last = *self.last.get(busid).unwrap_or(&Mode::Generic);
        let next = if rtt >= Duration::from_millis(60) {
            Mode::Wan
        } else if rtt <= Duration::from_millis(30) {
            Mode::Generic
        } else {
            last
        };
        if self.last.len() >= 128 && !self.last.contains_key(busid) {
            self.last.pop_first();
        }
        self.last.insert(busid.to_owned(), next);
        next
    }
}
pub trait Connector: Send + Sync {
    /// Return a fully prepared stream before RTT probing starts.
    /// The protocol owns it and normalizes blocking I/O; no async borrower remains.
    fn connect(&self) -> io::Result<TcpStream>;
    /// True only for a mutually authenticated hop to the selected receiver/app.
    /// A LAN address, loopback address or Tailscale route alone is not evidence.
    fn authenticated(&self) -> bool {
        false
    }
}
pub struct Direct(pub SocketAddr);
impl Connector for Direct {
    fn connect(&self) -> io::Result<TcpStream> {
        connect(self.0)
    }
}
fn blocking_stream(remote: &dyn Connector) -> io::Result<TcpStream> {
    let stream = remote.connect()?;
    // Windows accept can preserve a nonblocking listener's socket mode. Merely
    // setting SO_RCVTIMEO does not make read_exact wait on a nonblocking socket.
    stream.set_nonblocking(false)?;
    Ok(stream)
}
fn connect(addr: SocketAddr) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.set_write_timeout(Some(Duration::from_secs(15)))?;
    Ok(s)
}
fn request(s: &mut TcpStream, op: u16) -> io::Result<()> {
    let mut h = [0u8; 8];
    h[..2].copy_from_slice(&HEADER);
    h[2..4].copy_from_slice(&op.to_be_bytes());
    s.write_all(&h)
}
fn reply(s: &mut TcpStream, op: u16) -> io::Result<u32> {
    let mut h = [0u8; 8];
    s.read_exact(&mut h)?;
    if h[..2] != HEADER || h[2..4] != op.to_be_bytes() {
        return Err(invalid());
    }
    Ok(wire::u32_at(&h, 4))
}
/// Read-only negotiation. A legacy peer can close/reject CAPS without claiming USB.
/// Errors remain distinguishable from a positive, version-checked capability.
pub fn management_capabilities(
    remote: &dyn Connector,
) -> io::Result<Option<crate::managed::Capabilities>> {
    if !remote.authenticated() {
        return Ok(None);
    }
    let mut stream = blocking_stream(remote)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    request(&mut stream, crate::managed::CAPS)?;
    if reply(&mut stream, crate::managed::CAPS & 0x7fff)? != 0 {
        return Ok(None);
    }
    let mut bytes = [0; 32];
    stream.read_exact(&mut bytes)?;
    crate::managed::Capabilities::decode(&bytes)
        .map(Some)
        .map_err(|_| invalid())
}
/// Exchange metadata only on the existing authenticated path. Neither delivery
/// nor a reply executes an OS operation: the caller must use the retained Ledger.
pub fn sync_ejection(
    remote: &dyn Connector,
    report: crate::managed::Report,
) -> io::Result<crate::managed::Directive> {
    if !remote.authenticated() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let bytes = report.encode().map_err(|_| invalid())?;
    let mut stream = blocking_stream(remote)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    request(&mut stream, crate::managed::SYNC)?;
    stream.write_all(&bytes)?;
    if reply(&mut stream, crate::managed::SYNC & 0x7fff)? != 0 {
        return Err(invalid());
    }
    let mut bytes = [0; 40];
    stream.read_exact(&mut bytes)?;
    let directive = crate::managed::Directive::decode(&bytes).map_err(|_| invalid())?;
    if directive.import != report.import {
        return Err(invalid());
    }
    Ok(directive)
}
pub fn devices(addr: SocketAddr) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    devices_with(&Direct(addr))
}
pub fn devices_with(remote: &dyn Connector) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut s = blocking_stream(remote)?;
    request(&mut s, 0x8005)?;
    if reply(&mut s, 5)? != 0 {
        return Err(invalid());
    }
    let mut n = [0; 4];
    s.read_exact(&mut n)?;
    let count = u32::from_be_bytes(n);
    if count > 128 {
        return Err(invalid());
    }
    let mut out = Vec::new();
    for _ in 0..count {
        let mut d = vec![0; 312];
        s.read_exact(&mut d)?;
        let mut i = vec![0; usize::from(d[311]) * 4];
        s.read_exact(&mut i)?;
        out.push((d, i));
    }
    Ok(out)
}
fn capability(remote: &dyn Connector) -> io::Result<Duration> {
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let mut s = blocking_stream(remote)?;
        let start = Instant::now();
        request(&mut s, 0x80df)?;
        if reply(&mut s, 0xdf)? != 0 {
            return Err(invalid());
        }
        let mut b = [0; 16];
        s.read_exact(&mut b)?;
        if b != [0x57, 0x41, 0x4e, 0x31, 0, 0, 8, 0, 0, 0, 0, 32, 0, 0, 0, 1] {
            return Err(invalid());
        }
        min = min.min(start.elapsed());
    }
    Ok(min)
}
fn busid(d: &[u8]) -> io::Result<String> {
    let b = &d[256..288];
    let end = b.iter().position(|c| *c == 0).ok_or_else(invalid)?;
    if b[end..].iter().any(|c| *c != 0) {
        return Err(invalid());
    }
    String::from_utf8(b[..end].to_vec()).map_err(|_| invalid())
}
#[derive(Default, Debug)]
pub struct Metrics {
    pub mode: Mode,
    pub reports: u64,
    pub delivered: u64,
    pub highwater: usize,
    pub controls: u64,
    pub cancels: u64,
}
pub fn serve(local: TcpStream, remote: SocketAddr, policy: &Mutex<Policy>) -> io::Result<Metrics> {
    serve_with(local, &Direct(remote), policy)
}
/// A single-use, 120-bit opaque bus name for a protected kernel import hop.
/// The platform gives it directly to its own driver through IOCTL, never argv.
pub struct ImportAlias {
    name: zeroize::Zeroizing<[u8; 32]>,
    real: [u8; 32],
    used: std::sync::atomic::AtomicBool,
}
impl ImportAlias {
    pub fn new(name: &str, real: &str) -> io::Result<Self> {
        if name.len() != 30
            || !name.bytes().all(|b| b.is_ascii_hexdigit())
            || real.is_empty()
            || real.len() > 31
            || !real.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid());
        }
        let mut alias = zeroize::Zeroizing::new([0; 32]);
        alias[..30].copy_from_slice(name.as_bytes());
        let mut target = [0; 32];
        target[..real.len()].copy_from_slice(real.as_bytes());
        Ok(Self {
            name: alias,
            real: target,
            used: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn encoded(&self) -> &[u8; 32] {
        &self.name
    }
    fn authorize(&self, request: &[u8; 32]) -> bool {
        use subtle::ConstantTimeEq;
        bool::from(request.ct_eq(self.name.as_ref()))
            && self
                .used
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_ok()
    }
}
pub fn serve_alias(
    local: TcpStream,
    remote: &dyn Connector,
    policy: &Mutex<Policy>,
    alias: &ImportAlias,
) -> io::Result<Metrics> {
    serve_internal(local, remote, policy, Some(alias), None)
}
pub fn serve_with(
    local: TcpStream,
    remote: &dyn Connector,
    policy: &Mutex<Policy>,
) -> io::Result<Metrics> {
    serve_internal(local, remote, policy, None, None)
}
/// Explicit opt-in only: the receiver must retain this metadata with its exact
/// successful OS attachment. The callback runs once, before the kernel sees the
/// successful descriptor and can submit URBs. A failure closes this import; it
/// never replays the claimed device on a legacy transport.
pub struct Management<'a> {
    pub capabilities: crate::managed::Capabilities,
    pub imported: &'a dyn Fn(crate::eject::ImportKey) -> io::Result<()>,
}
pub fn serve_managed(
    local: TcpStream,
    remote: &dyn Connector,
    policy: &Mutex<Policy>,
    alias: Option<&ImportAlias>,
    management: &Management<'_>,
) -> io::Result<Metrics> {
    if !remote.authenticated() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    management.capabilities.encode().map_err(|_| invalid())?;
    serve_internal(local, remote, policy, alias, Some(management))
}
fn import_request(
    upstream: &mut TcpStream,
    name: &[u8; 32],
    mode: Mode,
    management: Option<&Management<'_>>,
) -> io::Result<u32> {
    let op = if management.is_some() {
        crate::managed::IMPORT
    } else if mode == Mode::Wan {
        0x80e0
    } else {
        0x8003
    };
    request(upstream, op)?;
    if let Some(management) = management {
        upstream.write_all(
            &crate::managed::ImportRequest {
                busid: *name,
                wan: mode == Mode::Wan,
                session: management.capabilities.session,
            }
            .encode()
            .map_err(|_| invalid())?,
        )?;
    } else {
        upstream.write_all(name)?;
        if mode == Mode::Wan {
            upstream.write_all(&1u32.to_be_bytes())?;
        }
    }
    reply(upstream, op & 0x7fff)
}
fn serve_internal(
    mut local: TcpStream,
    remote: &dyn Connector,
    policy: &Mutex<Policy>,
    alias: Option<&ImportAlias>,
    management: Option<&Management<'_>>,
) -> io::Result<Metrics> {
    local.set_nonblocking(false)?;
    local.set_nodelay(true)?;
    local.set_read_timeout(Some(Duration::from_secs(15)))?;
    local.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut h = [0u8; 8];
    local.read_exact(&mut h)?;
    if h[..2] != HEADER || wire::u32_at(&h, 4) != 0 {
        return Err(invalid());
    }
    let op = u16::from_be_bytes([h[2], h[3]]);
    if op == 0x8005 && alias.is_none() && management.is_none() {
        let ds = devices_with(remote)?;
        local.write_all(&[1, 17, 0, 5, 0, 0, 0, 0])?;
        local.write_all(&(ds.len() as u32).to_be_bytes())?;
        for (d, i) in ds {
            local.write_all(&d)?;
            local.write_all(&i)?;
        }
        return Ok(Metrics::default());
    }
    if op != 0x8003 {
        return Err(invalid());
    }
    let mut name = [0u8; 32];
    local.read_exact(&mut name)?;
    if let Some(alias) = alias {
        if !alias.authorize(&name) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        name.copy_from_slice(&alias.real);
    }
    let end = name.iter().position(|v| *v == 0).ok_or_else(invalid)?;
    if name[end..].iter().any(|v| *v != 0) {
        return Err(invalid());
    }
    let id = std::str::from_utf8(&name[..end]).map_err(|_| invalid())?;
    if id.is_empty() || !id.bytes().all(|v| v.is_ascii_digit() || v == b'-') {
        return Err(invalid());
    }
    let ds = devices_with(remote)?;
    let known = ds
        .iter()
        .any(|(d, _)| d[300..304] == [4, 109, 197, 43] && busid(d).is_ok_and(|s| s == id));
    // Probing never claims USB. An older server closing CAPS can safely use generic IMPORT.
    let rtt = if known { capability(remote).ok() } else { None };
    let mut mode =
        policy
            .lock()
            .map_err(|_| invalid())?
            .choose(id, rtt.is_some(), rtt.unwrap_or_default());
    if management.is_some_and(|m| m.capabilities.features & crate::managed::WAN == 0) {
        mode = Mode::Generic;
    }
    let mut upstream = blocking_stream(remote)?;
    let mut status = import_request(&mut upstream, &name, mode, management)?;
    // EOPNOTSUPP is the protocol's pre-claim rejection. Only this explicit
    // result permits a fresh generic import; never replay an active USB session.
    if mode == Mode::Wan && status == 95 {
        let _ = upstream.shutdown(Shutdown::Both);
        upstream = blocking_stream(remote)?;
        mode = Mode::Generic;
        status = import_request(&mut upstream, &name, mode, management)?;
    }
    local.write_all(&[1, 17, 0, 3])?;
    local.write_all(&status.to_be_bytes())?;
    if status != 0 {
        return Ok(Metrics {
            mode,
            ..Metrics::default()
        });
    }
    let mut device = [0u8; 312];
    upstream.read_exact(&mut device)?;
    if busid(&device)? != id || (mode == Mode::Wan && device[300..304] != [4, 109, 197, 43]) {
        return Err(invalid());
    }
    let key = if let Some(management) = management {
        let mut bytes = [0; 24];
        upstream.read_exact(&mut bytes)?;
        let key = crate::managed::decode_key(&bytes).map_err(|_| invalid())?;
        if key.session != management.capabilities.session {
            return Err(invalid());
        }
        Some(key)
    } else {
        None
    };
    if mode == Mode::Wan {
        let mut caps = [0u8; 16];
        upstream.read_exact(&mut caps)?;
        if caps != [0x57, 0x41, 0x4e, 0x31, 0, 0, 8, 0, 0, 0, 0, 32, 0, 0, 0, 1] {
            return Err(invalid());
        }
    }
    if let Some(alias) = alias {
        device[256..288].copy_from_slice(alias.encoded());
    }
    if let (Some(management), Some(key)) = (management, key) {
        (management.imported)(key)?;
    }
    local.write_all(&device)?;
    let dev = (wire::u32_at(&device, 288) << 16) | wire::u32_at(&device, 292);
    relay(local, upstream, dev, mode)
}
/// Both readers are bounded and share no USB state. A failed connection is never resumed/replayed.
pub fn relay(
    mut local: TcpStream,
    mut remote: TcpStream,
    device: u32,
    mode: Mode,
) -> io::Result<Metrics> {
    // Handshake deadlines must not become idle-device deadlines. A pending USB
    // IN may legitimately wait indefinitely; peer close still wakes both readers.
    local.set_nonblocking(false)?;
    remote.set_nonblocking(false)?;
    local.set_read_timeout(None)?;
    remote.set_read_timeout(None)?;
    local.set_write_timeout(Some(Duration::from_secs(15)))?;
    remote.set_write_timeout(Some(Duration::from_secs(15)))?;
    local.set_nodelay(true)?;
    remote.set_nodelay(true)?;
    // Bound reader staging separately from the 8 MiB submitted-request budget.
    let (tx, rx) = mpsc::sync_channel(8);
    let mut left = local.try_clone()?;
    let mut right = remote.try_clone()?;
    thread::scope(|scope| {
        let txl = tx.clone();
        scope.spawn(move || {
            loop {
                let r = Frame::read(&mut left);
                let ended = r.is_err();
                if txl.send((true, r)).is_err() || ended {
                    break;
                }
            }
        });
        scope.spawn(move || {
            loop {
                let r = Frame::read(&mut right);
                let ended = r.is_err();
                if tx.send((false, r)).is_err() || ended {
                    break;
                }
            }
        });
        let result = (|| {
            let mut metrics = Metrics {
                mode,
                ..Metrics::default()
            };
            let mut pending = BTreeMap::<u32, Frame>::new();
            let mut pending_bytes = 0usize;
            let mut unlinks = BTreeMap::<u32, u32>::new();
            let mut waiting = VecDeque::<Frame>::new();
            let mut input = VecDeque::<Frame>::new();
            let mut epoch = 1;
            let mut sequence = 0;
            let mut started = false;
            let mut barrier: Option<u32> = None;
            while let Ok((from_local, item)) = rx.recv() {
                let frame = match item {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => break,
                    Err(e) => return Err(e),
                };
                if frame.word(8) != device {
                    return Err(invalid());
                }
                if from_local {
                    let seq = frame.word(4);
                    match frame.command() {
                        1 => {
                            if pending.contains_key(&seq)
                                || unlinks.contains_key(&seq)
                                || waiting.iter().any(|f| f.word(4) == seq)
                                || pending.len() + waiting.len() >= 64
                            {
                                return Err(invalid());
                            }
                            let bytes = frame.word(24) as usize;
                            if bytes > wire::MAX_PENDING_BYTES - pending_bytes {
                                // Known pre-submit refusal: no USB OUT replay or
                                // delivery occurs, and unrelated imports survive.
                                Frame::packet(
                                    3,
                                    seq,
                                    device,
                                    frame.word(12),
                                    frame.word(16),
                                    -12,
                                    0,
                                    0,
                                    &[],
                                )
                                .send(&mut local)?;
                                continue;
                            }
                            if mode == Mode::Wan && frame.word(12) == 1 && frame.word(16) == 3 {
                                if frame.word(24) != 32 {
                                    Frame::response(&frame, -95, &[]).send(&mut local)?;
                                    continue;
                                }
                                if barrier.is_some() {
                                    Frame::response(&frame, -16, &[]).send(&mut local)?;
                                    continue;
                                }
                                if !started {
                                    Frame::control(START, device, epoch, 0).send(&mut remote)?;
                                    started = true;
                                }
                                waiting.push_back(frame);
                            } else {
                                if mode == Mode::Wan && frame.is_barrier_request() {
                                    if barrier.is_some() || !waiting.is_empty() {
                                        Frame::response(&frame, -16, &[]).send(&mut local)?;
                                        continue;
                                    }
                                    barrier = Some(seq);
                                }
                                if frame.word(16) == 0 {
                                    metrics.controls += 1;
                                }
                                frame.send(&mut remote)?;
                                pending_bytes += bytes;
                                pending.insert(seq, frame);
                            }
                        }
                        2 => {
                            if pending.contains_key(&seq)
                                || unlinks.contains_key(&seq)
                                || waiting.iter().any(|f| f.word(4) == seq)
                                || unlinks.len() >= 64
                            {
                                return Err(invalid());
                            }
                            let target = frame.word(20);
                            if let Some(i) = waiting.iter().position(|f| f.word(4) == target) {
                                waiting.remove(i);
                                Frame::packet(
                                    4,
                                    seq,
                                    device,
                                    frame.word(12),
                                    frame.word(16),
                                    -104,
                                    0,
                                    0,
                                    &[],
                                )
                                .send(&mut local)?;
                                metrics.cancels += 1;
                            } else if pending.contains_key(&target) {
                                frame.send(&mut remote)?;
                                unlinks.insert(seq, target);
                            } else {
                                Frame::packet(
                                    4,
                                    seq,
                                    device,
                                    frame.word(12),
                                    frame.word(16),
                                    0,
                                    0,
                                    0,
                                    &[],
                                )
                                .send(&mut local)?;
                            }
                        }
                        _ => return Err(invalid()),
                    }
                } else {
                    match frame.command() {
                        PUSH if mode == Mode::Wan => {
                            if !started
                                || frame.word(28) != epoch
                                || frame.word(4) != sequence + 1
                                || frame.word(20) != 0
                                || input.len() >= wire::WAN_WINDOW
                            {
                                return Err(invalid());
                            }
                            sequence += 1;
                            metrics.reports += 1;
                            input.push_back(frame);
                            metrics.highwater = metrics.highwater.max(input.len());
                        }
                        BARRIER if mode == Mode::Wan => {
                            if barrier.is_none()
                                || frame.word(28) != epoch + 1
                                || frame.word(4) != 0
                                || frame.word(20) != 0
                                || frame.word(24) != 0
                            {
                                return Err(invalid());
                            }
                            epoch += 1;
                            sequence = 0;
                            started = false;
                            input.clear();
                        }
                        3 => {
                            let request = pending.remove(&frame.word(4)).ok_or_else(invalid)?;
                            pending_bytes -= request.word(24) as usize;
                            if frame.word(12) != request.word(12)
                                || frame.word(16) != request.word(16)
                                || frame.word(24) > request.word(24)
                            {
                                return Err(invalid());
                            }
                            if barrier == Some(frame.word(4)) {
                                barrier = None;
                            }
                            frame.send(&mut local)?;
                        }
                        4 => {
                            let target = unlinks.remove(&frame.word(4)).ok_or_else(invalid)?;
                            if frame.word(20) as i32 == -104 {
                                let cancelled = pending.remove(&target).ok_or_else(invalid)?;
                                pending_bytes -= cancelled.word(24) as usize;
                            } else if frame.word(20) != 0 {
                                return Err(invalid());
                            }
                            frame.send(&mut local)?;
                        }
                        _ => return Err(invalid()),
                    }
                }
                let mut ack = 0;
                while barrier.is_none() && !waiting.is_empty() && !input.is_empty() {
                    let request = waiting.pop_front().unwrap();
                    let report = input.pop_front().unwrap();
                    let status = if request.word(20) & 1 != 0 && report.data().len() < 32 {
                        -121
                    } else {
                        0
                    };
                    Frame::response(&request, status, report.data()).send(&mut local)?;
                    ack = report.word(4);
                    metrics.delivered += 1;
                }
                if ack != 0 {
                    Frame::control(CREDIT, device, epoch, ack).send(&mut remote)?;
                }
            }
            Ok(metrics)
        })();
        let _ = local.shutdown(Shutdown::Both);
        let _ = remote.shutdown(Shutdown::Both);
        drop(rx);
        result
    })
}
pub fn listen(address: SocketAddr, remote: SocketAddr) -> io::Result<()> {
    listen_with_ready(address, remote, || {})
}

/// The hook runs only after the exclusive loopback listener has been bound.
pub fn listen_with_ready(
    address: SocketAddr,
    remote: SocketAddr,
    ready: impl FnOnce(),
) -> io::Result<()> {
    listen_with_connector(address, Arc::new(Direct(remote)), ready)
}
pub fn listen_with_connector(
    address: SocketAddr,
    remote: Arc<dyn Connector>,
    ready: impl FnOnce(),
) -> io::Result<()> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "agent listens on loopback only",
        ));
    }
    let listener = TcpListener::bind(address)?;
    ready();
    let policy = Arc::new(Mutex::new(Policy::default()));
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for local in listener.incoming() {
        let local = local?;
        if active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 8 {
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            continue;
        }
        let policy = policy.clone();
        let active = active.clone();
        let remote = remote.clone();
        thread::spawn(move || {
            struct Active(std::sync::Arc<std::sync::atomic::AtomicUsize>);
            impl Drop for Active {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _active = Active(active);
            match serve_with(local, remote.as_ref(), &policy) {
                Ok(m) => eprintln!(
                    "USB Most agent: {:?}, reports={}, delivered={}, queue_highwater={}",
                    m.mode, m.reports, m.delivered, m.highwater
                ),
                Err(e) => eprintln!("USB Most agent: session ended ({:?})", e.kind()),
            };
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
