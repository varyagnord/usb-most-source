//! Actual loopback TCP / automatic agent / production pump, synthetic USB only.
use super::{Session, sim::Fake};
use crate::{
    agent,
    wire::{self, CREDIT, Frame, PUSH, START},
};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
fn err() -> io::Error {
    wire::invalid()
}
fn pair() -> io::Result<(TcpStream, TcpStream)> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    let a = TcpStream::connect(l.local_addr()?)?;
    let (b, _) = l.accept()?;
    Ok((a, b))
}
fn device() -> [u8; 312] {
    let mut d = [0; 312];
    d[256..259].copy_from_slice(b"1-6");
    for (at, n) in [(288, 1u32), (292, 6), (296, 2)] {
        d[at..at + 4].copy_from_slice(&n.to_be_bytes());
    }
    d[300..304].copy_from_slice(&[4, 109, 197, 43]);
    d[309] = 1;
    d[310] = 1;
    d
}
fn caps() -> [u8; 16] {
    [0x57, 0x41, 0x4e, 0x31, 0, 0, 8, 0, 0, 0, 0, 32, 0, 0, 0, 1]
}
fn remote(mut socket: TcpStream, count: u32) -> io::Result<()> {
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(15)))?;
    socket.set_write_timeout(Some(Duration::from_secs(15)))?;
    let mut h = [0; 8];
    socket.read_exact(&mut h)?;
    let op = u16::from_be_bytes([h[2], h[3]]);
    match op {
        0x8005 => {
            socket.write_all(&[1, 17, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1])?;
            socket.write_all(&device())?;
            return Ok(());
        }
        0x80df => {
            socket.write_all(&[1, 17, 0, 223, 0, 0, 0, 0])?;
            socket.write_all(&caps())?;
            return Ok(());
        }
        0x8003 | 0x80e0 => {}
        _ => return Err(err()),
    }
    let mut bus = [0; 32];
    socket.read_exact(&mut bus)?;
    if &bus[..4] != b"1-6\0" {
        return Err(err());
    }
    let wan = op == 0x80e0;
    if wan {
        let mut p = [0; 4];
        socket.read_exact(&mut p)?;
        if u32::from_be_bytes(p) != 1 {
            return Err(err());
        }
    }
    socket.write_all(&[1, 17, 0, if wan { 224 } else { 3 }, 0, 0, 0, 0])?;
    socket.write_all(&device())?;
    if wan {
        socket.write_all(&caps())?;
    }
    let f = Arc::new(Fake::default());
    let s = Session::new(f.clone());
    if wan {
        s.command(1, 1).map_err(|_| err())?;
    }
    let (mut reader, mut writer) = (socket.try_clone()?, socket);
    let (tx, rx) = mpsc::sync_channel(128);
    thread::scope(|scope| {
        scope.spawn(move || {
            loop {
                let item = Frame::read(&mut reader);
                let end = item.is_err();
                if tx.send(item).is_err() || end {
                    break;
                }
            }
        });
        let result = (|| {
            let mut active = false;
            let mut produced = 0u32;
            let mut next = Instant::now();
            loop {
                while let Ok(item) = rx.try_recv() {
                    let frame = match item {
                        Ok(f) => f,
                        Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => return Ok(()),
                        Err(e) => return Err(e),
                    };
                    if frame.command() == START {
                        if frame.word(28) != 1 {
                            return Err(err());
                        }
                        s.command(5, 0).map_err(|_| err())?;
                        active = true;
                        next = Instant::now();
                    } else if frame.command() == CREDIT {
                        s.command(2, frame.word(24) as i32).map_err(|_| err())?;
                    } else if frame.command() == 1 {
                        let setup = if frame.word(16) == 0 {
                            Some(frame.0[40..48].try_into().unwrap())
                        } else {
                            None
                        };
                        s.submit(
                            crate::engine::Spec {
                                id: frame.word(4) as i32,
                                kind: if setup.is_some() { 2 } else { 1 },
                                endpoint: frame.word(16) as i32
                                    | if frame.word(12) == 1 { 128 } else { 0 },
                                flags: frame.word(20) as i32,
                                length: frame.word(24) as i32,
                                setup,
                            },
                            |b| {
                                b.copy_from_slice(frame.data());
                                Ok(())
                            },
                        )
                        .map_err(|_| err())?;
                        if !wan {
                            let mut data = [0u8; 15];
                            data[..4].copy_from_slice(&produced.to_be_bytes());
                            data[4] = 1;
                            if !f.emit(&data) {
                                return Err(err());
                            }
                            produced += 1;
                        }
                    } else {
                        return Err(err());
                    }
                }
                if active && produced < count && Instant::now() >= next {
                    let mut data = [0u8; 15];
                    data[..4].copy_from_slice(&produced.to_be_bytes());
                    data[4] = 1;
                    if !f.emit(&data) {
                        return Err(err());
                    }
                    produced += 1;
                    next += Duration::from_millis(8);
                }
                for _ in 0..4 {
                    if let Some(c) = s.reap(0).map_err(|_| err())? {
                        let frame = if c.wan_epoch != 0 {
                            Frame::packet(
                                PUSH,
                                c.wan_sequence,
                                65542,
                                1,
                                3,
                                c.status,
                                c.actual as u32,
                                c.wan_epoch,
                                c.data().unwrap_or(&[]),
                            )
                        } else {
                            Frame::packet(
                                3,
                                c.id as u32,
                                65542,
                                1,
                                3,
                                c.status,
                                c.actual as u32,
                                0,
                                c.data().unwrap_or(&[]),
                            )
                        };
                        frame.send(&mut writer)?;
                    } else {
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
        })();
        let _ = writer.shutdown(Shutdown::Both);
        drop(rx);
        s.close();
        result
    })
}
fn pipe(
    mut source: TcpStream,
    mut destination: TcpStream,
    delay: u32,
    jitter: u32,
) -> io::Result<()> {
    source.set_read_timeout(Some(Duration::from_secs(20)))?;
    destination.set_write_timeout(Some(Duration::from_secs(20)))?;
    source.set_nodelay(true)?;
    destination.set_nodelay(true)?;
    let (tx, rx) = mpsc::sync_channel(128);
    thread::scope(|scope| {
        let reader = scope.spawn(move || -> io::Result<()> {
            let mut last = Instant::now();
            let mut seed = 7u64;
            loop {
                let mut bytes = [0; 4096];
                let n = source.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let extra = if jitter == 0 {
                    0
                } else {
                    (seed >> 32) % u64::from(jitter + 1)
                };
                let at =
                    (Instant::now() + Duration::from_millis(u64::from(delay) + extra)).max(last);
                last = at;
                if tx.send((at, bytes[..n].to_vec())).is_err() {
                    break;
                }
            }
            Ok(())
        });
        let result = (|| {
            for (at, bytes) in rx {
                if let Some(wait) = at.checked_duration_since(Instant::now()) {
                    thread::sleep(wait);
                }
                destination.write_all(&bytes)?;
            }
            Ok(())
        })();
        let _ = destination.shutdown(Shutdown::Write);
        reader.join().map_err(|_| err())??;
        result
    })
}
/// Return delivered, auto-mode, elapsed ms, reports seen, buffered high-water.
pub fn scenario(rtt: u32, jitter: u32, count: u32) -> io::Result<Vec<i64>> {
    if rtt > 2000 || jitter > 100 || count == 0 || count > 256 {
        return Err(err());
    }
    let fixture = TcpListener::bind("127.0.0.1:0")?;
    fixture.set_nonblocking(true)?;
    let target = fixture.local_addr()?;
    let proxy_port = std::env::var("USB_MOST_LAB_PROXY_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let proxy = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, proxy_port))?;
    proxy.set_nonblocking(true)?;
    let address = proxy.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let done = stop.clone();
        let remote_thread = scope.spawn(move || {
            let mut jobs = Vec::new();
            while !done.load(Ordering::Relaxed) {
                match fixture.accept() {
                    Ok((s, _)) => jobs.push(thread::spawn(move || remote(s, count))),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
            for job in jobs {
                let _ = job.join();
            }
        });
        let done = stop.clone();
        let proxy_thread = scope.spawn(move || {
            let mut jobs = Vec::new();
            while !done.load(Ordering::Relaxed) {
                match proxy.accept() {
                    Ok((a, _)) => {
                        jobs.push(thread::spawn(move || -> io::Result<()> {
                            let b = TcpStream::connect(target)?;
                            let aa = a.try_clone()?;
                            let bb = b.try_clone()?;
                            let one = thread::spawn(move || pipe(a, bb, rtt / 2, jitter));
                            let result = pipe(b, aa, rtt / 2, jitter);
                            let _ = one.join();
                            result
                        }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
            for job in jobs {
                let _ = job.join();
            }
        });
        let result = (|| {
            let (mut kernel, local) = pair()?;
            let guard = kernel.try_clone()?;
            let (end, timer) = mpsc::channel();
            let deadline = thread::spawn(move || {
                if timer.recv_timeout(Duration::from_secs(45)).is_err() {
                    let _ = guard.shutdown(Shutdown::Both);
                }
            });
            let task = thread::spawn(move || {
                agent::serve(local, address, &Mutex::new(agent::Policy::default()))
            });
            let start = Instant::now();
            kernel.write_all(&[1, 17, 128, 3, 0, 0, 0, 0])?;
            let mut bus = [0u8; 32];
            bus[..3].copy_from_slice(b"1-6");
            kernel.write_all(&bus)?;
            let mut header = [0; 8];
            kernel.read_exact(&mut header)?;
            if header != [1, 17, 0, 3, 0, 0, 0, 0] {
                return Err(err());
            }
            let mut device = [0; 312];
            kernel.read_exact(&mut device)?;
            for index in 0..count {
                Frame::packet(1, index + 1, 65542, 1, 3, 0, 32, 0, &[]).send(&mut kernel)?;
                let reply = Frame::read(&mut kernel)?;
                if reply.command() != 3
                    || reply.word(4) != index + 1
                    || reply.data().len() != 15
                    || wire::u32_at(reply.data(), 0) != index
                    || reply.data()[4] != 1
                {
                    return Err(err());
                }
            }
            kernel.shutdown(Shutdown::Both)?;
            let metrics = task.join().map_err(|_| err())??;
            let _ = end.send(());
            deadline.join().map_err(|_| err())?;
            if rtt >= 100 && metrics.mode != agent::Mode::Wan {
                return Err(err());
            }
            Ok(vec![
                i64::from(count),
                i64::from(metrics.mode == agent::Mode::Wan),
                start.elapsed().as_millis() as i64,
                metrics.reports as i64,
                metrics.highwater as i64,
            ])
        })();
        stop.store(true, Ordering::Relaxed);
        let _ = proxy_thread.join();
        let _ = remote_thread.join();
        result
    })
}
