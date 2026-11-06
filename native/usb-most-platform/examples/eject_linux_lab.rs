//! Explicit test-only import/eject harness. Not linked into a receiver or APK.
//! Requires a marker installed by our isolated QEMU lab runner. Owns only its
//! loopback socket and confirmed VHCI port; never mounts/formats a filesystem.
#![forbid(unsafe_code)]
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        fs,
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        os::fd::AsRawFd,
        sync::Arc,
        time::{Duration, Instant},
    };
    use usb_most_platform::linux::{self, OwnedUsb};
    use usb_most_protocol::agent;
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3
        || args[1] != "--owned-qemu-lab"
        || fs::read_to_string("/run/usb-most-eject-owned-lab")?
            != "USBMOST_EJECT113_SYNTHETIC_ONLY\n"
        || !args[2].split_once('-').is_some_and(|(bus, ports)| {
            let number = |s: &str| {
                !s.is_empty()
                    && s.bytes().all(|b| b.is_ascii_digit())
                    && s.parse::<u32>().is_ok_and(|v| v > 0 && v <= 65535)
            };
            number(bus) && ports.split('.').all(number)
        })
        || args[2].len() > 31
        || args[2].is_empty()
    {
        return Err("owned QEMU fixture marker/busid required".into());
    }
    let product = fs::read_to_string("/sys/class/dmi/id/product_name")?;
    if !product.contains("Q35") {
        return Err("isolated Q35 fixture required".into());
    }
    let id = &args[2];
    let mut name = [0; 32];
    name[..id.len()].copy_from_slice(id.as_bytes());
    let address = "127.0.0.1:3240".parse()?;
    let devices = agent::devices(address)?;
    let descriptor = &devices
        .iter()
        .find(|(d, _)| d[256..288] == name)
        .ok_or("missing synthetic export")?
        .0;
    // QEMU USB storage, not any USB the machine happens to expose.
    if descriptor[300..304] != [0x46, 0xf4, 0, 1] {
        return Err("not QEMU USB storage".into());
    }
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.write_all(&[1, 17, 128, 3, 0, 0, 0, 0])?;
    socket.write_all(&name)?;
    let mut reply = [0; 8];
    socket.read_exact(&mut reply)?;
    if reply != [1, 17, 0, 3, 0, 0, 0, 0] {
        return Err("import failed".into());
    }
    let mut imported = [0; 312];
    socket.read_exact(&mut imported)?;
    if imported != descriptor.as_slice() {
        return Err("import descriptor changed".into());
    }
    let read = |at| u32::from_be_bytes(imported[at..at + 4].try_into().unwrap());
    let (bus, dev, speed) = (read(288), read(292), read(296));
    if bus > 65535 || dev > 65535 || !(1..=6).contains(&speed) {
        return Err("invalid fixture descriptor".into());
    }
    let root = "/sys/devices/platform/vhci_hcd.0";
    let status = fs::read_to_string(format!("{root}/status"))?;
    let port: u32 = status
        .lines()
        .skip(1)
        .find_map(|line| {
            let f: Vec<_> = line.split_ascii_whitespace().collect();
            (f.len() == 7 && f[2] == "004" && f[0] == if speed >= 5 { "ss" } else { "hs" })
                .then(|| f[1].parse().ok())
                .flatten()
        })
        .ok_or("no free owned VHCI port")?;
    socket.set_read_timeout(None)?;
    let line = format!(
        "{port} {} {} {speed}",
        socket.as_raw_fd(),
        (bus << 16) | dev
    );
    let mut attach = fs::OpenOptions::new()
        .write(true)
        .open(format!("{root}/attach"))?;
    if attach.write(line.as_bytes())? != line.len() {
        return Err("partial attach not retried".into());
    }
    let socket = Arc::new(socket);
    let deadline = Instant::now() + Duration::from_secs(30);
    let owned = loop {
        match OwnedUsb::capture(port, (bus << 16) | dev, socket.clone()) {
            Ok(owned) => break owned,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.into()),
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    println!("READY {port}");
    std::io::stdout().flush()?;
    loop {
        let mut command = String::new();
        if std::io::stdin().read_line(&mut command)? == 0 {
            // A crashed test runner is not permission to detach a possibly
            // mounted disk. Retain this owned import for explicit VM cleanup.
            let _ = writeln!(std::io::stderr(), "RETAINED_IMPORT_AFTER_RUNNER_EOF");
            loop {
                std::thread::park();
            }
        }
        if command == "eject\n" {
            let result = runtime.block_on(linux::eject(&owned));
            println!("RESULT {result:?}");
            std::io::stdout().flush()?;
        } else if command == "quit\n" {
            break;
        } else {
            return Err("unknown fixture command".into());
        }
    }
    socket.shutdown(Shutdown::Both)?;
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux-only isolated lab example");
}
