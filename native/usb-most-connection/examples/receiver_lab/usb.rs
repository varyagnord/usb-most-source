//! Synthetic HID mouse: no host input capture, no storage, no physical USB.
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
use usb_most_protocol::wire::Frame;
#[derive(Default)]
struct Counts {
    lists: AtomicU64,
    imports: AtomicU64,
    controls: AtomicU64,
    reports: AtomicU64,
    input_events: bool,
}
pub struct Fixture {
    pub address: SocketAddr,
    pub proof: [u8; 32],
    counts: Arc<Counts>,
}
impl Fixture {
    pub fn start(authenticated: bool) -> Self {
        Self::start_with_input(authenticated, true)
    }
    /// A neutral fixture still services real interrupt transfers but cannot
    /// click another test's windows or move the desktop pointer during storage E2E.
    pub fn start_with_input(authenticated: bool, input_events: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proof = usb_most_connection::security::random::<32>().unwrap();
        let counts = Arc::new(Counts {
            input_events,
            ..Counts::default()
        });
        let owned = counts.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let c = owned.clone();
                thread::spawn(move || {
                    if let Err(error) = serve(stream, proof, c, authenticated) {
                        eprintln!("Synthetic fixture socket ended: {:?}", error.kind());
                    }
                });
            }
        });
        Self {
            address,
            proof,
            counts,
        }
    }
    pub fn stats(&self) -> serde_json::Value {
        serde_json::json!({"lists":self.counts.lists.load(Ordering::Relaxed),"imports":self.counts.imports.load(Ordering::Relaxed),"controls":self.counts.controls.load(Ordering::Relaxed),"reports":self.counts.reports.load(Ordering::Relaxed)})
    }
}
const REPORT: &[u8] = &[
    0x05, 1, 0x09, 2, 0xa1, 1, 0x09, 1, 0xa1, 0, 0x05, 9, 0x19, 1, 0x29, 5, 0x15, 0, 0x25, 1, 0x95,
    5, 0x75, 1, 0x81, 2, 0x95, 1, 0x75, 3, 0x81, 1, 0x05, 1, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38,
    0x15, 0x81, 0x25, 0x7f, 0x75, 8, 0x95, 3, 0x81, 6, 0xc0, 0xc0,
];
fn config() -> Vec<u8> {
    let mut v = vec![
        9, 2, 34, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, 2, 0, 9, 0x21, 0x11, 1, 0, 1, 0x22,
    ];
    v.extend((REPORT.len() as u16).to_le_bytes());
    v.extend([7, 5, 0x81, 3, 4, 0, 10]);
    v
}
fn device() -> [u8; 18] {
    [18, 1, 0, 2, 0, 0, 0, 64, 0xfe, 0xca, 1, 0, 0, 1, 1, 2, 0, 1]
}
fn string(n: u8) -> Vec<u8> {
    if n == 0 {
        return vec![4, 3, 9, 4];
    };
    let text = if n == 1 {
        "USB Most Lab"
    } else {
        "USB Most Synthetic Mouse"
    };
    let mut b = vec![0, 3];
    for u in text.encode_utf16() {
        b.extend(u.to_le_bytes())
    }
    b[0] = b.len() as u8;
    b
}
fn descriptor() -> [u8; 312] {
    let mut d = [0; 312];
    d[..18].copy_from_slice(b"/usb-most-fixture\0");
    d[256..260].copy_from_slice(b"1-7\0");
    for (at, n) in [(288, 1u32), (292, 7), (296, 2)] {
        d[at..at + 4].copy_from_slice(&n.to_be_bytes())
    }
    d[300..302].copy_from_slice(&0xcafeu16.to_be_bytes());
    d[302..304].copy_from_slice(&1u16.to_be_bytes());
    d[304..306].copy_from_slice(&0x100u16.to_be_bytes());
    d[309] = 1;
    d[310] = 1;
    d[311] = 1;
    d
}
fn op(s: &mut TcpStream, code: u16, status: u32) -> io::Result<()> {
    s.write_all(&[1, 17])?;
    s.write_all(&code.to_be_bytes())?;
    s.write_all(&status.to_be_bytes())
}
fn serve(mut s: TcpStream, proof: [u8; 32], c: Arc<Counts>, authenticated: bool) -> io::Result<()> {
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    if authenticated {
        let mut auth = [0; 40];
        s.read_exact(&mut auth)?;
        if &auth[..8] != b"UMLOCAL1" || auth[8..] != proof {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        s.write_all(&[1])?;
    }
    let mut header = [0; 8];
    s.read_exact(&mut header)?;
    if header[..2] != [1, 17] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    match u16::from_be_bytes(header[2..4].try_into().unwrap()) {
        0x8005 => {
            c.lists.fetch_add(1, Ordering::Relaxed);
            op(&mut s, 5, 0)?;
            s.write_all(&1u32.to_be_bytes())?;
            s.write_all(&descriptor())?;
            s.write_all(&[3, 1, 2, 0])
        }
        0x8003 => {
            let mut id = [0; 32];
            s.read_exact(&mut id)?;
            if &id[..4] != b"1-7\0" {
                return op(&mut s, 3, 1);
            };
            c.imports.fetch_add(1, Ordering::Relaxed);
            op(&mut s, 3, 0)?;
            s.write_all(&descriptor())?;
            transfers(s, c)
        }
        _ => op(&mut s, 0xdf, 95),
    }
}
fn transfers(mut input: TcpStream, c: Arc<Counts>) -> io::Result<()> {
    let output = Arc::new(Mutex::new(input.try_clone()?));
    let pending = Arc::new(Mutex::new(VecDeque::<Frame>::new()));
    let end = Arc::new(AtomicBool::new(false));
    let (out, queue, stop, count) = (output.clone(), pending.clone(), end.clone(), c.clone());
    let worker = thread::spawn(move || {
        let mut tick = 0u32;
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(20));
            if let Some(request) = queue.lock().unwrap().pop_front() {
                tick += 1;
                let mut report = [
                    match (tick / 2) % 6 {
                        0 => 0,
                        n => 1 << (n - 1),
                    },
                    if tick % 2 == 0 { 1 } else { 255 },
                    if (tick / 2) % 2 == 0 { 1 } else { 255 },
                    if tick % 2 == 0 {
                        1
                    } else if tick % 2 == 1 {
                        255
                    } else {
                        0
                    },
                ];
                if !count.input_events {
                    report.fill(0);
                }
                let bytes = &report[..(request.word(24) as usize).min(4)];
                if Frame::response(&request, 0, bytes)
                    .send(&mut out.lock().unwrap())
                    .is_err()
                {
                    break;
                };
                count.reports.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    let result = (|| {
        let mut trace_left = 128;
        loop {
            let request = Frame::read(&mut input)?;
            if trace_left > 0 {
                trace_left -= 1;
                eprintln!(
                    "Synthetic request cmd={} seq={} ep={} dir={}",
                    request.command(),
                    request.word(4),
                    request.word(16),
                    request.word(12)
                );
            }
            match request.command() {
                1 if request.word(16) == 0 => {
                    c.controls.fetch_add(1, Ordering::Relaxed);
                    let setup = &request.0[40..48];
                    let len = u16::from_le_bytes([setup[6], setup[7]]) as usize;
                    let mut status = 0;
                    let data = match (setup[0] & 0x80, setup[1]) {
                        (0x80, 6) => match setup[3] {
                            1 => device().to_vec(),
                            2 => config(),
                            3 => string(setup[2]),
                            0x21 => config()[18..27].to_vec(),
                            0x22 => REPORT.to_vec(),
                            _ => {
                                status = -32;
                                vec![]
                            }
                        },
                        (0x80, 8) => vec![1],
                        (0x80, 10) => vec![0],
                        (0x80, 2) => vec![0],
                        (0x80, 3) => vec![1],
                        (0x80, 0) => vec![0, 0],
                        (0x80, 1) => vec![0; 4],
                        (0, _) => vec![],
                        _ => {
                            status = -32;
                            vec![]
                        }
                    };
                    Frame::response(&request, status, &data[..data.len().min(len)])
                        .send(&mut output.lock().unwrap())?;
                }
                1 if request.word(16) == 1 && request.word(12) == 1 => {
                    let mut q = pending.lock().unwrap();
                    if q.len() >= 64 {
                        return Err(io::ErrorKind::OutOfMemory.into());
                    }
                    q.push_back(request);
                }
                1 => {
                    Frame::response(&request, -32, &[]).send(&mut output.lock().unwrap())?;
                }
                2 => {
                    let mut q = pending.lock().unwrap();
                    let i = q.iter().position(|r| r.word(4) == request.word(20));
                    let found = i.and_then(|i| q.remove(i)).is_some();
                    Frame::packet(
                        4,
                        request.word(4),
                        request.word(8),
                        request.word(12),
                        request.word(16),
                        if found { -104 } else { 0 },
                        0,
                        0,
                        &[],
                    )
                    .send(&mut output.lock().unwrap())?;
                }
                _ => return Err(io::ErrorKind::InvalidData.into()),
            }
        }
    })();
    end.store(true, Ordering::Release);
    let _ = worker.join();
    result
}
