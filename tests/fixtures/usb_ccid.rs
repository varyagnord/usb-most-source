//! Minimal deterministic CCID fixture, with no cryptography or host USB access.
//! Framing/descriptor reference: USB CCID rev1.1 sections5.1,6.1 and6.2:
//! https://www.usb.org/sites/default/files/DWG_Smart-Card_CCID_Rev110.pdf
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
use usb_most_protocol::wire::Frame;

#[derive(Default)]
pub struct Counts {
    pub imports: AtomicU64,
    pub commands: AtomicU64,
    pub exchanges: AtomicU64,
}
pub struct Fixture {
    pub address: SocketAddr,
    pub counts: Arc<Counts>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(Mutex::new(None));
        let counts = Arc::new(Counts::default());
        let (end, active, count) = (stop.clone(), socket.clone(), counts.clone());
        let worker = thread::spawn(move || {
            while !end.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        {
                            let mut slot = active.lock().unwrap();
                            if end.load(Ordering::Acquire) {
                                break;
                            }
                            *slot = Some(stream.try_clone().unwrap());
                        }
                        if let Err(e) = serve(stream, &count) {
                            eprintln!("CCID fixture connection ended: {:?}", e.kind());
                        }
                        active.lock().unwrap().take();
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            counts,
            stop,
            socket,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn device() -> [u8; 18] {
    [18, 1, 0, 2, 0, 0, 0, 64, 0xfe, 0xca, 3, 0, 0, 1, 1, 2, 3, 1]
}
fn config() -> Vec<u8> {
    let mut c = vec![9, 2, 86, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 0x0b, 0, 0, 0];
    let mut ccid = vec![54, 0x21, 0x10, 1, 0, 1];
    for n in [1u32, 4000, 4000] {
        ccid.extend(n.to_le_bytes());
    }
    ccid.push(0);
    ccid.extend(9600u32.to_le_bytes());
    ccid.extend(9600u32.to_le_bytes());
    ccid.push(0);
    for n in [254u32, 0, 0, 0x0002_0002, 4096] {
        ccid.extend(n.to_le_bytes());
    }
    ccid.extend([0, 0, 0, 0, 0, 1]);
    assert_eq!(ccid.len(), 54);
    c.extend(ccid);
    c.extend([7, 5, 0x82, 2, 0, 2, 0, 7, 5, 2, 2, 0, 2, 0]);
    c
}
fn string(n: u8) -> Vec<u8> {
    if n == 0 {
        return vec![4, 3, 9, 4];
    }
    let text = match n {
        1 => "USB Most Lab",
        2 => "USB Most Synthetic CCID",
        3 => "USBMOSTCCID113",
        _ => return vec![],
    };
    let mut data = vec![0, 3];
    for ch in text.encode_utf16() {
        data.extend(ch.to_le_bytes());
    }
    data[0] = data.len() as u8;
    data
}
fn descriptor() -> [u8; 312] {
    let mut d = [0; 312];
    let path = b"/usb-most-synthetic-ccid\0";
    d[..path.len()].copy_from_slice(path);
    d[256..260].copy_from_slice(b"1-9\0");
    for (at, n) in [(288, 1u32), (292, 9), (296, 3)] {
        d[at..at + 4].copy_from_slice(&n.to_be_bytes());
    }
    d[300..302].copy_from_slice(&0xcafeu16.to_be_bytes());
    d[302..304].copy_from_slice(&3u16.to_be_bytes());
    d[304..306].copy_from_slice(&0x100u16.to_be_bytes());
    d[309..312].copy_from_slice(&[1, 1, 1]);
    d
}
fn op(s: &mut TcpStream, code: u16, status: u32) -> io::Result<()> {
    s.write_all(&[1, 17])?;
    s.write_all(&code.to_be_bytes())?;
    s.write_all(&status.to_be_bytes())
}
#[derive(Default)]
struct Card {
    powered: bool,
    replies: VecDeque<Vec<u8>>,
}
impl Card {
    fn output(&mut self, request: &[u8], counts: &Counts) -> io::Result<()> {
        // The echo appends two status bytes; both request and response must fit
        // the descriptor's maximum 4096-byte CCID message.
        if request.len() < 10 || request.len() > 4094 || self.replies.len() >= 16 {
            return Err(invalid());
        }
        let length = u32::from_le_bytes(request[1..5].try_into().unwrap()) as usize;
        if length != request.len() - 10 || request[5] != 0 {
            return Err(invalid());
        }
        let mut status = if self.powered { 0 } else { 1 };
        let mut error = 0;
        let (kind, data) = match request[0] {
            0x62 if length == 0 => {
                self.powered = true;
                status = 0;
                (0x80, vec![0x3b, 0])
            }
            0x63 if length == 0 => {
                self.powered = false;
                status = 1;
                (0x81, vec![])
            }
            0x65 if length == 0 => (0x81, vec![]),
            0x6f if self.powered => {
                counts.exchanges.fetch_add(1, Ordering::Relaxed);
                let mut bytes = request[10..].to_vec();
                bytes.extend([0x90, 0]);
                (0x80, bytes)
            }
            _ => {
                status |= 0x40;
                error = 0xfe;
                (0x81, vec![])
            }
        };
        let mut reply = vec![kind];
        reply.extend((data.len() as u32).to_le_bytes());
        reply.extend([0, request[6], status, error, 0]);
        reply.extend(data);
        self.replies.push_back(reply);
        counts.commands.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn input(&mut self, capacity: usize) -> io::Result<Option<Vec<u8>>> {
        let Some(reply) = self.replies.front() else {
            return Ok(None);
        };
        if capacity < reply.len() {
            return Err(invalid());
        }
        Ok(self.replies.pop_front())
    }
}
fn complete(stream: &mut TcpStream, request: &Frame, status: i32, data: &[u8]) -> io::Result<()> {
    let actual = if request.word(12) == 0 && status == 0 {
        request.data().len()
    } else {
        data.len()
    };
    Frame::packet(
        3,
        request.word(4),
        request.word(8),
        request.word(12),
        request.word(16),
        status,
        actual as u32,
        0,
        data,
    )
    .send(stream)
}
fn serve(mut stream: TcpStream, counts: &Counts) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut header = [0; 8];
    stream.read_exact(&mut header)?;
    if header[..2] != [1, 17] || header[4..] != [0; 4] {
        return Err(invalid());
    }
    match u16::from_be_bytes(header[2..4].try_into().unwrap()) {
        0x8005 => {
            op(&mut stream, 5, 0)?;
            stream.write_all(&1u32.to_be_bytes())?;
            stream.write_all(&descriptor())?;
            stream.write_all(&[0x0b, 0, 0, 0])
        }
        0x8003 => {
            let mut id = [0; 32];
            stream.read_exact(&mut id)?;
            if id[..4] != *b"1-9\0" || id[4..].iter().any(|b| *b != 0) {
                return op(&mut stream, 3, 1);
            }
            op(&mut stream, 3, 0)?;
            stream.write_all(&descriptor())?;
            counts.imports.fetch_add(1, Ordering::Relaxed);
            transfers(stream, counts)
        }
        _ => op(&mut stream, 0xdf, 95),
    }
}
fn transfers(mut stream: TcpStream, counts: &Counts) -> io::Result<()> {
    stream.set_read_timeout(None)?; // Fixture::drop interrupts the owned socket.
    let mut card = Card::default();
    let mut pending = VecDeque::<Frame>::new();
    loop {
        let request = Frame::read(&mut stream)?;
        match (request.command(), request.word(16), request.word(12)) {
            (1, 0, _) => {
                let s = &request.0[40..48];
                let len = u16::from_le_bytes([s[6], s[7]]) as usize;
                let (status, data) = match (s[0], s[1]) {
                    (0x80, 6) => match s[3] {
                        1 => (0, device().to_vec()),
                        2 => (0, config()),
                        3 => (0, string(s[2])),
                        _ => (-32, vec![]),
                    },
                    (0x80, 8) => (0, vec![1]),
                    (0x81, 10) => (0, vec![0]),
                    (0x80..=0x82, 0) => (0, vec![0, 0]),
                    (0, 9) | (0, 5) | (1, 11) | (2, 1) => (0, vec![]),
                    (0xa1, 2) => (0, 4000u32.to_le_bytes().to_vec()),
                    (0xa1, 3) => (0, 9600u32.to_le_bytes().to_vec()),
                    _ => (-32, vec![]),
                };
                complete(&mut stream, &request, status, &data[..len.min(data.len())])?;
            }
            (1, 2, 0) => {
                card.output(request.data(), counts)?;
                complete(&mut stream, &request, 0, &[])?;
            }
            (1, 2, 1) => {
                if pending.len() >= 16 {
                    return Err(invalid());
                }
                pending.push_back(request);
            }
            (1, _, _) => complete(&mut stream, &request, -32, &[])?,
            (2, _, _) => {
                let removed = pending
                    .iter()
                    .position(|r| r.word(4) == request.word(20))
                    .and_then(|at| pending.remove(at))
                    .is_some();
                Frame::packet(
                    4,
                    request.word(4),
                    request.word(8),
                    request.word(12),
                    request.word(16),
                    if removed { -104 } else { 0 },
                    0,
                    0,
                    &[],
                )
                .send(&mut stream)?;
            }
            _ => return Err(invalid()),
        }
        while let Some(request) = pending.front() {
            let Some(reply) = card.input(request.word(24) as usize)? else {
                break;
            };
            complete(&mut stream, request, 0, &reply)?;
            pending.pop_front();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn command(kind: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![kind];
        b.extend((payload.len() as u32).to_le_bytes());
        b.extend([0, seq, 0, 0, 0]);
        b.extend(payload);
        b
    }
    #[test]
    fn ccid_sequences_payloads_power_and_bounds() {
        let counts = Counts::default();
        let mut card = Card::default();
        card.output(&command(0x6f, 0, b"test"), &counts).unwrap();
        assert_eq!(card.input(4098).unwrap().unwrap()[7] & 0x40, 0x40);
        card.output(&command(0x62, 1, &[]), &counts).unwrap();
        assert_eq!(card.input(4098).unwrap().unwrap()[10..], [0x3b, 0]);
        for seq in 2..12 {
            card.output(&command(0x6f, seq, &[seq; 256]), &counts)
                .unwrap();
        }
        for seq in 2..12 {
            let reply = card.input(4098).unwrap().unwrap();
            assert_eq!(reply[6], seq);
            assert_eq!(&reply[10..266], &[seq; 256]);
            assert_eq!(&reply[266..], &[0x90, 0]);
        }
        assert_eq!(counts.exchanges.load(Ordering::Relaxed), 10);
        card.output(&command(0x6f, 12, &[7; 4084]), &counts)
            .unwrap();
        assert_eq!(card.input(4096).unwrap().unwrap().len(), 4096);
        assert!(
            card.output(&command(0x6f, 13, &[7; 4085]), &counts)
                .is_err()
        );
        assert!(card.output(&[0; 9], &counts).is_err());
        let mut malformed = command(0x6f, 12, &[]);
        malformed[1] = 1;
        assert!(card.output(&malformed, &counts).is_err());
        for _ in 0..16 {
            card.output(&command(0x65, 1, &[]), &counts).unwrap();
        }
        assert!(card.output(&command(0x65, 2, &[]), &counts).is_err());
    }
    #[test]
    fn ccid_descriptor_lengths_and_real_loopback_enumeration() {
        assert_eq!(config().len(), 86);
        assert_eq!(config()[18], 54);
        let fixture = Fixture::start();
        let mut stream = TcpStream::connect(fixture.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&[1, 17, 0x80, 5, 0, 0, 0, 0]).unwrap();
        let mut list = [0; 328];
        stream.read_exact(&mut list).unwrap();
        assert_eq!(&list[12..324], &descriptor());
        assert_eq!(&list[324..], &[0x0b, 0, 0, 0]);
    }
}
