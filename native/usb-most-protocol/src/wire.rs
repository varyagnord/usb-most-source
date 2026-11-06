//! Bounded USB/IP + negotiated WAN frames. No payload is logged.
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    time::Duration,
};
use zeroize::Zeroize;
pub const MAX: usize = 1024 * 1024;
pub const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
pub const WAN_WINDOW: usize = 2048;
pub const PUSH: u32 = 16;
pub const START: u32 = 17;
pub const CREDIT: u32 = 18;
pub const BARRIER: u32 = 19;
pub fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid USB/WAN frame")
}
pub fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
pub struct Frame(pub Vec<u8>);
impl Drop for Frame {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl Frame {
    pub fn word(&self, at: usize) -> u32 {
        u32_at(&self.0, at)
    }
    pub fn command(&self) -> u32 {
        self.word(0)
    }
    pub fn data(&self) -> &[u8] {
        &self.0[48..]
    }
    pub fn read(s: &mut TcpStream) -> io::Result<Self> {
        let mut header = [0u8; 48];
        s.set_read_timeout(None)?;
        if s.read(&mut header[..1])? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "peer closed",
            ));
        }
        s.set_read_timeout(Some(Duration::from_secs(15)))?;
        s.read_exact(&mut header[1..])?;
        let len = body_size(&header)?;
        let mut data = Vec::new();
        data.try_reserve_exact(48 + len)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        data.extend(header);
        data.resize(48 + len, 0);
        s.read_exact(&mut data[48..])?;
        Ok(Self(data))
    }
    pub fn send(&self, s: &mut TcpStream) -> io::Result<()> {
        s.write_all(&self.0)
    }
    #[allow(clippy::too_many_arguments)] // Fixed USB/IP header fields.
    pub fn packet(
        command: u32,
        seq: u32,
        dev: u32,
        dir: u32,
        ep: u32,
        status: i32,
        actual: u32,
        epoch: u32,
        data: &[u8],
    ) -> Self {
        let mut b = Vec::with_capacity(48 + data.len());
        for n in [
            command,
            seq,
            dev,
            dir,
            ep,
            status as u32,
            actual,
            epoch,
            0,
            0,
            0,
            0,
        ] {
            b.extend(n.to_be_bytes());
        }
        b.extend(data);
        Self(b)
    }
    pub fn response(request: &Frame, status: i32, data: &[u8]) -> Self {
        Self::packet(
            3,
            request.word(4),
            request.word(8),
            request.word(12),
            request.word(16),
            status,
            data.len() as u32,
            0,
            data,
        )
    }
    pub fn control(command: u32, dev: u32, epoch: u32, value: u32) -> Self {
        Self::packet(command, 0, dev, 0, 3, 0, value, epoch, &[])
    }
    pub fn is_barrier_request(&self) -> bool {
        self.command() == 1
            && self.word(16) == 0
            && matches!((self.0[40], self.0[41]), (0, 9) | (1, 11))
    }
}
pub fn body_size(h: &[u8; 48]) -> io::Result<usize> {
    let command = u32_at(h, 0);
    let dir = u32_at(h, 12);
    let len = u32_at(h, 24) as usize;
    if dir > 1 || u32_at(h, 16) > 15 {
        return Err(invalid());
    }
    match command {
        1 | 3 => {
            if len > MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("USB transfer size {len} exceeds {MAX}"),
                ));
            }
            if !matches!(u32_at(h, 32), 0 | u32::MAX) {
                return Err(invalid());
            }
            Ok(
                if (command == 1 && dir == 0) || (command == 3 && dir == 1) {
                    len
                } else {
                    0
                },
            )
        }
        2 | 4 => Ok(0),
        PUSH => {
            if (32..48).step_by(4).any(|at| u32_at(h, at) != 0)
                || u32_at(h, 4) == 0
                || u32_at(h, 4) > i32::MAX as u32
                || u32_at(h, 28) > i32::MAX as u32
            {
                return Err(invalid());
            }
            if dir != 1 || u32_at(h, 16) != 3 || len > 32 || u32_at(h, 28) == 0 {
                return Err(invalid());
            }
            Ok(len)
        }
        START | CREDIT | BARRIER => {
            if u32_at(h, 4) != 0
                || u32_at(h, 16) != 3
                || u32_at(h, 20) != 0
                || u32_at(h, 28) == 0
                || u32_at(h, 28) > i32::MAX as u32
                || (32..48).step_by(4).any(|at| u32_at(h, at) != 0)
                || (command == BARRIER && (dir != 1 || len != 0))
                || (command != BARRIER && dir != 0)
                || (command == START && len != 0)
                || (command == CREDIT && (len == 0 || len > i32::MAX as usize))
            {
                return Err(invalid());
            }
            Ok(0)
        }
        _ => Err(invalid()),
    }
}
