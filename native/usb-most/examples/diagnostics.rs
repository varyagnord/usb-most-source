//! Bounded client for the explicit diagnostic APK. Does not import USB devices.
use std::{
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 256 * 1024;

fn read_before(stream: &mut TcpStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "diagnostic deadline"))?;
        stream.set_read_timeout(Some(remaining))?;
        let n = stream.read(bytes)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated diagnostic frame",
            ));
        }
        bytes = &mut bytes[n..];
    }
    Ok(())
}

fn decode_header(header: &[u8; 12], expected: u16) -> io::Result<(u32, usize)> {
    let version = u16::from_be_bytes([header[0], header[1]]);
    let code = u16::from_be_bytes([header[2], header[3]]);
    let status = u32::from_be_bytes(header[4..8].try_into().unwrap());
    let size = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
    if version != 0x111 || code != expected || status > 1 || size == 0 || size > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid diagnostic reply",
        ));
    }
    Ok((status, size))
}

fn exchange(address: SocketAddr, command: &str) -> io::Result<(u32, String)> {
    let (op, mode) = match command {
        "get" => (0x80f0u16, None),
        "rust" => (0x80f1, Some(0u32)),
        "sdk" => (0x80f1, Some(1)),
        "clear" => (0x80f2, None),
        "selftest" => (0x80f3, None),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown diagnostic command",
            ));
        }
    };
    let deadline =
        Instant::now() + Duration::from_secs(if command == "selftest" { 120 } else { 15 });
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::with_capacity(12);
    request.extend(0x111u16.to_be_bytes());
    request.extend(op.to_be_bytes());
    request.extend(0u32.to_be_bytes());
    if let Some(mode) = mode {
        request.extend(mode.to_be_bytes());
    }
    stream.write_all(&request)?;
    let mut header = [0; 12];
    read_before(&mut stream, &mut header, deadline)?;
    let (status, size) = decode_header(&header, op & 0x7fff)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| io::Error::other("diagnostic allocation failed"))?;
    bytes.resize(size, 0);
    read_before(&mut stream, &mut bytes, deadline)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "diagnostic UTF-8"))?;
    Ok((status, text))
}

fn main() -> ExitCode {
    let result = (|| -> Result<u32, Box<dyn std::error::Error>> {
        let mut args = std::env::args().skip(1);
        let host = args.next().ok_or(
            "usage: diagnostics IPv4 [get|rust|sdk|clear|selftest] [--port N] [--output FILE]",
        )?;
        let ip: Ipv4Addr = host.parse()?;
        if ip.to_string() != host || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
            return Err("canonical unicast IPv4 required".into());
        }
        let mut command = "get".to_string();
        let mut port = 3240;
        let mut output = None;
        let mut have_command = false;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--port" => {
                    port = args.next().ok_or("port required")?.parse::<u16>()?;
                    if port == 0 {
                        return Err("port zero".into());
                    }
                }
                "--output" => {
                    if output.is_some() {
                        return Err("duplicate output".into());
                    }
                    output = Some(PathBuf::from(args.next().ok_or("output required")?));
                }
                _ if !have_command => {
                    command = arg;
                    have_command = true;
                }
                _ => return Err("unexpected argument".into()),
            }
        }
        // Refuse an existing/unwritable destination before changing remote mode.
        let mut destination = if let Some(path) = output {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            Some(options.open(path)?)
        } else {
            None
        };
        let (status, body) = exchange(SocketAddr::new(IpAddr::V4(ip), port), &command)?;
        if let Some(file) = destination.as_mut() {
            writeln!(file, "{body}")?;
            file.sync_all()?;
        } else {
            println!("{body}");
        }
        Ok(status)
    })();
    match result {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => {
            eprintln!("diagnostic operation busy/rejected; inspect snapshot");
            ExitCode::from(3)
        }
        Err(error) => {
            eprintln!("diagnostic client: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_oversized_and_wrong_replies_before_allocation() {
        let mut header = [0u8; 12];
        header[..2].copy_from_slice(&0x111u16.to_be_bytes());
        header[2..4].copy_from_slice(&0xf0u16.to_be_bytes());
        header[8..].copy_from_slice(&10u32.to_be_bytes());
        assert_eq!(decode_header(&header, 0xf0).unwrap(), (0, 10));
        assert!(decode_header(&header, 0xf1).is_err());
        header[8..].copy_from_slice(&((MAX_BYTES + 1) as u32).to_be_bytes());
        assert!(decode_header(&header, 0xf0).is_err());
        header[8..].fill(0);
        assert!(decode_header(&header, 0xf0).is_err());
    }
    #[test]
    fn real_tcp_framing_and_busy_status() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 12];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(&request[..8], &[1, 17, 128, 241, 0, 0, 0, 0]);
            assert_eq!(&request[8..], &[0, 0, 0, 1]);
            let body = b"{\"active_hid\":1}";
            let mut reply = Vec::new();
            reply.extend([1, 17, 0, 241, 0, 0, 0, 1]);
            reply.extend((body.len() as u32).to_be_bytes());
            reply.extend(body);
            for chunk in reply.chunks(3) {
                stream.write_all(chunk).unwrap();
            }
        });
        assert_eq!(
            exchange(address, "sdk").unwrap(),
            (1, "{\"active_hid\":1}".into())
        );
        peer.join().unwrap();
    }
}
