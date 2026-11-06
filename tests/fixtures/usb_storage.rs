//! Test-only USB MSC/BOT device backed by a newly allocated 64 MiB RAM image.
//! Never opens host disks, device files or mount points. Not a product transport.
//! BOT framing: https://www.usb.org/sites/default/files/usbmassbulk_10.pdf
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
#[path = "write_fence.rs"]
pub mod write_fence;

const SECTORS: usize = 131072;
const BLOCK: usize = 512;
pub const PROBE: &[u8] = b"USB Most isolated RAM disk probe\r\n";
#[derive(Default)]
pub struct Counts {
    pub imports: AtomicU64,
    pub commands: AtomicU64,
    pub reads: AtomicU64,
    pub writes: AtomicU64,
    pub syncs: AtomicU64,
    pub bytes_written: AtomicU64,
    pub trace: AtomicBool,
    pub write_fence: write_fence::WriteFence,
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
            let mut disk = ram_disk();
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
                        if let Err(e) = serve(stream, &mut disk, &count) {
                            eprintln!("RAM storage fixture ended: {:?}", e.kind());
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
        self.counts.write_fence.cancel();
        if let Some(s) = self.socket.lock().unwrap().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
fn ram_disk() -> Vec<u8> {
    // FAT16 superfloppy, 4 sectors/cluster, two FATs and a fixed root directory.
    let mut b = vec![0; SECTORS * BLOCK];
    b[..11].copy_from_slice(b"\xeb\x3c\x90USBMOST ");
    b[11..13].copy_from_slice(&512u16.to_le_bytes());
    b[13] = 4;
    b[14..16].copy_from_slice(&1u16.to_le_bytes());
    b[16] = 2;
    b[17..19].copy_from_slice(&512u16.to_le_bytes());
    b[21] = 0xf8;
    b[22..24].copy_from_slice(&128u16.to_le_bytes());
    b[24..26].copy_from_slice(&32u16.to_le_bytes());
    b[26..28].copy_from_slice(&64u16.to_le_bytes());
    b[32..36].copy_from_slice(&(SECTORS as u32).to_le_bytes());
    b[36] = 0x80;
    b[38] = 0x29;
    b[39..43].copy_from_slice(&0x1e113a01u32.to_le_bytes());
    b[43..54].copy_from_slice(b"USBMOSTLAB ");
    b[54..62].copy_from_slice(b"FAT16   ");
    b[510..512].copy_from_slice(&[0x55, 0xaa]);
    for sector in [1, 129] {
        b[sector * BLOCK..sector * BLOCK + 6]
            .copy_from_slice(&[0xf8, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }
    let root = 257 * BLOCK;
    b[root..root + 11].copy_from_slice(b"PROBE   TXT");
    b[root + 11] = 0x20;
    b[root + 24..root + 26].copy_from_slice(&0x5d64u16.to_le_bytes());
    b[root + 26..root + 28].copy_from_slice(&2u16.to_le_bytes());
    b[root + 28..root + 32].copy_from_slice(&(PROBE.len() as u32).to_le_bytes());
    // Windows reads the volume label from the root-directory label entry;
    // the BPB label alone is not sufficient for GetVolumeInformation/DriveInfo.
    b[root + 32..root + 43].copy_from_slice(b"USBMOSTLAB ");
    b[root + 43] = 0x08;
    b[289 * BLOCK..289 * BLOCK + PROBE.len()].copy_from_slice(PROBE);
    b
}
fn device() -> [u8; 18] {
    [18, 1, 0, 2, 0, 0, 0, 64, 0xfe, 0xca, 2, 0, 0, 1, 1, 2, 3, 1]
}
fn config() -> [u8; 32] {
    [
        9, 2, 32, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x81, 2, 0, 2, 0, 7, 5,
        2, 2, 0, 2, 0,
    ]
}
fn string(n: u8) -> Vec<u8> {
    if n == 0 {
        return vec![4, 3, 9, 4];
    }
    let text = match n {
        1 => "USB Most Lab",
        2 => "USB Most RAM Disk",
        3 => "USBMOSTEJECT113",
        _ => return vec![],
    };
    let mut out = vec![0, 3];
    for u in text.encode_utf16() {
        out.extend(u.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}
fn descriptor() -> [u8; 312] {
    let mut b = [0; 312];
    let path = b"/usb-most-ram-storage\0";
    b[..path.len()].copy_from_slice(path);
    b[256..260].copy_from_slice(b"1-8\0");
    for (at, n) in [(288, 1u32), (292, 8), (296, 3)] {
        b[at..at + 4].copy_from_slice(&n.to_be_bytes());
    }
    b[300..302].copy_from_slice(&0xcafeu16.to_be_bytes());
    b[302..304].copy_from_slice(&2u16.to_be_bytes());
    b[304..306].copy_from_slice(&0x100u16.to_be_bytes());
    b[309..312].copy_from_slice(&[1, 1, 1]);
    b
}
fn op(s: &mut TcpStream, code: u16, status: u32) -> io::Result<()> {
    s.write_all(&[1, 17])?;
    s.write_all(&code.to_be_bytes())?;
    s.write_all(&status.to_be_bytes())
}
fn serve(mut s: TcpStream, disk: &mut [u8], counts: &Counts) -> io::Result<()> {
    // Windows accepts inherit nonblocking mode; read deadlines do not change it.
    s.set_nonblocking(false)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(20)))?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut header = [0; 8];
    s.read_exact(&mut header)?;
    if header[..2] != [1, 17] || header[4..] != [0; 4] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    match u16::from_be_bytes(header[2..4].try_into().unwrap()) {
        0x8005 => {
            op(&mut s, 5, 0)?;
            s.write_all(&1u32.to_be_bytes())?;
            s.write_all(&descriptor())?;
            s.write_all(&[8, 6, 0x50, 0])
        }
        0x8003 => {
            let mut id = [0; 32];
            s.read_exact(&mut id)?;
            if &id[..4] != b"1-8\0" {
                return op(&mut s, 3, 1);
            }
            counts.imports.fetch_add(1, Ordering::Relaxed);
            op(&mut s, 3, 0)?;
            s.write_all(&descriptor())?;
            transfers(s, disk, counts)
        }
        _ => op(&mut s, 0xdf, 95),
    }
}
struct Command {
    tag: [u8; 4],
    expected: usize,
    done: usize,
    data: Vec<u8>,
    write: Option<usize>,
    status: u8,
    input: bool,
}
#[derive(Default)]
struct Bot {
    command: Option<Command>,
    sense: (u8, u8),
}
impl Bot {
    fn cbw(&mut self, b: &[u8], disk: &[u8], counts: &Counts) -> io::Result<()> {
        if self.command.is_some()
            || b.len() != 31
            || &b[..4] != b"USBC"
            || b[12] & 0x7f != 0
            || b[13] != 0
            || !(1..=16).contains(&b[14])
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let expected = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        if expected > 1024 * 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut c = Command {
            tag: b[4..8].try_into().unwrap(),
            expected,
            done: 0,
            data: vec![],
            write: None,
            status: 0,
            input: b[12] == 0x80,
        };
        let scsi = &b[15..31];
        counts.commands.fetch_add(1, Ordering::Relaxed);
        if counts.trace.load(Ordering::Relaxed) {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            eprintln!("RAM command t={:.3} opcode={:02x} expected={expected}", now.as_secs_f64(), scsi[0]);
        }
        let mut reject = None;
        match scsi[0] {
            0x00 | 0x1b | 0x1e | 0x2f => {}
            0x35 | 0x91 => {
                counts.syncs.fetch_add(1, Ordering::Relaxed);
            }
            0x03 => {
                c.data = vec![0; 18];
                c.data[0] = 0x70;
                c.data[2] = self.sense.0;
                c.data[7] = 10;
                c.data[12] = self.sense.1;
                c.data.truncate(scsi[4] as usize);
                self.sense = (0, 0);
            }
            0x12 if scsi[1] & 1 == 0 => {
                c.data = vec![0; 36];
                c.data[..5].copy_from_slice(&[0, 0x80, 5, 2, 31]);
                c.data[8..16].copy_from_slice(b"USBMOST ");
                c.data[16..32].copy_from_slice(b"Isolated RAM    ");
                c.data[32..36].copy_from_slice(b"0113");
                c.data.truncate(scsi[4] as usize);
            }
            0x12 if scsi[2] == 0 => {
                c.data = vec![0, 0, 0, 1, 0];
                c.data.truncate(scsi[4] as usize);
            }
            0x25 => {
                c.data.extend(((SECTORS - 1) as u32).to_be_bytes());
                c.data.extend((BLOCK as u32).to_be_bytes());
            }
            0x9e if scsi[1] & 0x1f == 0x10 => {
                c.data = vec![0; 32];
                c.data[..8].copy_from_slice(&((SECTORS - 1) as u64).to_be_bytes());
                c.data[8..12].copy_from_slice(&(BLOCK as u32).to_be_bytes());
                c.data
                    .truncate(u32::from_be_bytes(scsi[10..14].try_into().unwrap()) as usize);
            }
            0x23 => {
                c.data = vec![0, 0, 0, 8];
                c.data.extend((SECTORS as u32).to_be_bytes());
                c.data.extend([2, 0, 2, 0]);
            }
            0x1a | 0x5a => {
                let page = scsi[2] & 0x3f;
                if !matches!(page, 0 | 8 | 0x3f) {
                    reject = Some((5, 0x24));
                }
                let header = if scsi[0] == 0x1a { 4 } else { 8 };
                c.data = vec![0; header];
                if page != 0 {
                    c.data.extend([8, 18]);
                    c.data.resize(header + 20, 0);
                }
                if header == 4 {
                    c.data[0] = (c.data.len() - 1) as u8;
                    c.data.truncate(scsi[4] as usize);
                } else {
                    let n = (c.data.len() - 2) as u16;
                    c.data[..2].copy_from_slice(&n.to_be_bytes());
                    c.data
                        .truncate(u16::from_be_bytes([scsi[7], scsi[8]]) as usize);
                }
            }
            0xa0 => {
                c.data = vec![0; 16];
                c.data[3] = 8;
            }
            0x28 | 0x2a | 0xa8 | 0xaa | 0x88 | 0x8a => {
                let (lba, blocks) = match scsi[0] {
                    0x28 | 0x2a => (
                        u32::from_be_bytes(scsi[2..6].try_into().unwrap()) as u64,
                        u16::from_be_bytes(scsi[7..9].try_into().unwrap()) as u64,
                    ),
                    0xa8 | 0xaa => (
                        u32::from_be_bytes(scsi[2..6].try_into().unwrap()) as u64,
                        u32::from_be_bytes(scsi[6..10].try_into().unwrap()) as u64,
                    ),
                    _ => (
                        u64::from_be_bytes(scsi[2..10].try_into().unwrap()),
                        u32::from_be_bytes(scsi[10..14].try_into().unwrap()) as u64,
                    ),
                };
                if lba > SECTORS as u64 || blocks > SECTORS as u64 - lba {
                    reject = Some((5, 0x21));
                } else if blocks * BLOCK as u64 != expected as u64 {
                    c.status = 2;
                } else if matches!(scsi[0], 0x2a | 0xaa | 0x8a) {
                    if c.input {
                        c.status = 2;
                    } else {
                        c.write = Some(lba as usize * BLOCK);
                    }
                    counts.writes.fetch_add(1, Ordering::Relaxed);
                } else {
                    if !c.input {
                        c.status = 2;
                    } else {
                        c.data.extend_from_slice(
                            &disk[lba as usize * BLOCK..(lba + blocks) as usize * BLOCK],
                        );
                    }
                    counts.reads.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ => {
                eprintln!("RAM storage unsupported SCSI opcode {:02x}", scsi[0]);
                reject = Some((5, 0x20));
            }
        }
        if let Some(sense) = reject {
            self.sense = sense;
            c.status = 1;
            c.data.clear();
        }
        c.data.truncate(expected);
        self.command = Some(c);
        Ok(())
    }
    fn output(&mut self, data: &[u8], disk: &mut [u8], counts: &Counts) -> io::Result<()> {
        let Some(c) = self.command.as_mut() else {
            return self.cbw(data, disk, counts);
        };
        if c.input || data.len() > c.expected - c.done {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if let Some(at) = c.write {
            disk[at + c.done..at + c.done + data.len()].copy_from_slice(data);
            counts.bytes_written.fetch_add(data.len() as u64, Ordering::Relaxed);
            counts.write_fence.committed(at + c.done, data.len(),
                counts.imports.load(Ordering::Relaxed), u32::from_le_bytes(c.tag),
                Duration::from_secs(60))?;
        }
        c.done += data.len();
        Ok(())
    }
    fn input(&mut self, len: usize) -> io::Result<Option<Vec<u8>>> {
        let Some(c) = self.command.as_mut() else {
            return Ok(None);
        };
        if c.input && c.done < c.data.len() {
            let end = (c.done + len).min(c.data.len());
            let data = c.data[c.done..end].to_vec();
            c.done = end;
            return Ok(Some(data));
        }
        if c.input && c.done == 0 && c.expected > 0 {
            // Unsupported IN command: short zero-length data stage before CSW.
            c.input = false;
            return Ok(Some(vec![]));
        }
        if c.write.is_some() && c.done < c.expected {
            return Ok(None);
        };
        if len < 13 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut csw = b"USBS".to_vec();
        csw.extend(c.tag);
        csw.extend(((c.expected - c.done) as u32).to_le_bytes());
        csw.push(c.status);
        self.command = None;
        Ok(Some(csw))
    }
}
fn complete(s: &mut TcpStream, request: &Frame, status: i32, data: &[u8]) -> io::Result<()> {
    let actual = if request.word(12) == 0 && status == 0 {
        request.data().len() as u32
    } else {
        data.len() as u32
    };
    Frame::packet(
        3,
        request.word(4),
        request.word(8),
        request.word(12),
        request.word(16),
        status,
        actual,
        0,
        data,
    )
    .send(s)
}
fn transfers(mut stream: TcpStream, disk: &mut [u8], counts: &Counts) -> io::Result<()> {
    // A mounted or imported device can be idle indefinitely. Fixture::drop
    // shuts down this exact socket, so teardown does not need an idle deadline.
    stream.set_read_timeout(None)?;
    let mut bot = Bot::default();
    let mut input = VecDeque::<Frame>::new();
    loop {
        let request = Frame::read(&mut stream)?;
        match (request.command(), request.word(16), request.word(12)) {
            (1, 0, _) => {
                let s = &request.0[40..48];
                let len = u16::from_le_bytes([s[6], s[7]]) as usize;
                let (status, data) = match (s[0], s[1]) {
                    (0x80, 6) => match s[3] {
                        1 => (0, device().to_vec()),
                        2 => (0, config().to_vec()),
                        3 => (0, string(s[2])),
                        _ => (-32, vec![]),
                    },
                    (0xa1, 0xfe) => (0, vec![0]),
                    (0x21, 0xff) => {
                        bot = Bot::default();
                        (0, vec![])
                    }
                    (0x80, 8) => (0, vec![1]),
                    (0x81, 10) => (0, vec![0]),
                    (0x80..=0x82, 0) => (0, vec![0, 0]),
                    (0, 9) | (0, 5) | (1, 11) | (2, 1) => (0, vec![]),
                    _ => (-32, vec![]),
                };
                complete(&mut stream, &request, status, &data[..data.len().min(len)])?;
            }
            (1, 2, 0) => {
                bot.output(request.data(), disk, counts)?;
                complete(&mut stream, &request, 0, &[])?;
            }
            (1, 1, 1) => {
                if input.len() >= 32 {
                    return Err(io::ErrorKind::OutOfMemory.into());
                }
                input.push_back(request);
            }
            (1, _, _) => complete(&mut stream, &request, -32, &[])?,
            (2, _, _) => {
                let at = input.iter().position(|r| r.word(4) == request.word(20));
                let found = at.and_then(|at| input.remove(at)).is_some();
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
                .send(&mut stream)?;
            }
            _ => return Err(io::ErrorKind::InvalidData.into()),
        }
        while let Some(request) = input.front() {
            let Some(data) = bot.input(request.word(24) as usize)? else {
                break;
            };
            complete(&mut stream, request, 0, &data)?;
            input.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cbw(tag: u32, bytes: u32, input: bool, cdb: &[u8]) -> Vec<u8> {
        let mut b = b"USBC".to_vec();
        b.extend(tag.to_le_bytes());
        b.extend(bytes.to_le_bytes());
        b.extend([if input { 0x80 } else { 0 }, 0, cdb.len() as u8]);
        b.extend(cdb);
        b.resize(31, 0);
        b
    }
    #[test]
    fn bot_bounds_roundtrip_status_and_ram_only_image() {
        let mut disk = ram_disk();
        let counts = Counts::default();
        let mut bot = Bot::default();
        assert_eq!(&disk[289 * 512..289 * 512 + PROBE.len()], PROBE);
        bot.output(
            &cbw(9, 512, false, &[0x2a, 0, 0, 0, 2, 0, 0, 0, 1, 0]),
            &mut disk,
            &counts,
        )
        .unwrap();
        assert!(bot.input(13).unwrap().is_none());
        bot.output(&[42; 256], &mut disk, &counts).unwrap();
        assert!(bot.input(13).unwrap().is_none());
        bot.output(&[43; 256], &mut disk, &counts).unwrap();
        assert_eq!(counts.bytes_written.load(Ordering::Relaxed), 512);
        assert_eq!(
            bot.input(13).unwrap().unwrap(),
            [b"USBS".as_slice(), &9u32.to_le_bytes(), &[0; 5]].concat()
        );
        bot.output(
            &cbw(10, 512, true, &[0x28, 0, 0, 0, 2, 0, 0, 0, 1, 0]),
            &mut disk,
            &counts,
        )
        .unwrap();
        assert_eq!(bot.input(256).unwrap().unwrap(), vec![42; 256]);
        assert_eq!(bot.input(256).unwrap().unwrap(), vec![43; 256]);
        assert_eq!(bot.input(13).unwrap().unwrap()[12], 0);
        assert!(
            bot.output(&cbw(11, 1024 * 1024 + 1, true, &[0x28]), &mut disk, &counts)
                .is_err()
        );
        bot.output(
            &cbw(
                12,
                512,
                true,
                &[0x28, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 1, 0],
            ),
            &mut disk,
            &counts,
        )
        .unwrap();
        assert!(bot.input(512).unwrap().unwrap().is_empty());
        let csw = bot.input(13).unwrap().unwrap();
        assert_eq!(csw[12], 1);
        assert_eq!(&csw[8..12], &512u32.to_le_bytes());
    }
    #[test]
    fn committed_write_has_no_ack_and_drop_retires_a_held_socket() {
        let fixture = Fixture::start();
        fixture.counts.write_fence.arm().unwrap();
        let counts = fixture.counts.clone();
        let mut stream = TcpStream::connect(fixture.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        stream.write_all(&[1, 17, 0x80, 3, 0, 0, 0, 0]).unwrap();
        let mut id = [0; 32]; id[..4].copy_from_slice(b"1-8\0");
        stream.write_all(&id).unwrap();
        let mut reply = [0; 320]; stream.read_exact(&mut reply).unwrap();
        let cbw = cbw(77, 4096, false, &[0x2a, 0, 0, 0, 0x80, 0, 0, 0, 8, 0]);
        Frame::packet(1, 1, 0x10008, 0, 2, 0, 31, 0, &cbw).send(&mut stream).unwrap();
        assert_eq!(Frame::read(&mut stream).unwrap().word(24), 31);
        Frame::packet(1, 2, 0x10008, 0, 2, 0, 4096, 0, &[42;4096]).send(&mut stream).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while counts.write_fence.snapshot().phase != write_fence::Phase::Held {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(counts.bytes_written.load(Ordering::Relaxed), 4096);
        stream.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        assert!(matches!(stream.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut));
        let began = std::time::Instant::now();
        drop(fixture);
        assert!(began.elapsed() < Duration::from_secs(1));
        assert_eq!(counts.write_fence.snapshot().phase, write_fence::Phase::Cancelled);
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    }
    #[test]
    fn fixture_enumerates_and_imports_over_real_loopback() {
        let fixture = Fixture::start();
        let mut stream = TcpStream::connect(fixture.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Force accept before the handshake bytes, including on Windows.
        thread::sleep(Duration::from_millis(50));
        stream.write_all(&[1, 17, 0x80, 3, 0, 0, 0, 0]).unwrap();
        let mut id = [0; 32];
        id[..4].copy_from_slice(b"1-8\0");
        stream.write_all(&id).unwrap();
        let mut reply = [0; 320];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(&reply[..8], &[1, 17, 0, 3, 0, 0, 0, 0]);
        assert_eq!(&reply[8..], &descriptor());
        let mut request = Frame::packet(1, 1, 0x10008, 1, 0, 0, 18, 0, &[]);
        request.0[40..48].copy_from_slice(&[0x80, 6, 0, 1, 0, 0, 18, 0]);
        request.send(&mut stream).unwrap();
        assert_eq!(Frame::read(&mut stream).unwrap().data(), &device());
        assert_eq!(fixture.counts.imports.load(Ordering::Relaxed), 1);
    }
}
