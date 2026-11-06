//! Kernel VHCI adapter. No open local USB/IP listener, no shell, no mounting.
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use usb_most_protocol::agent::{self, Connector, Policy};
use usb_most_protocol::{
    eject::{ImportKey, Outcome},
    managed::Capabilities,
};
const ROOT: &str = "/sys/devices/platform/vhci_hcd.0";
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
#[derive(Debug, PartialEq, Eq)]
struct Port {
    number: u32,
    super_speed: bool,
    free: bool,
}
fn ports(text: &str) -> io::Result<Vec<Port>> {
    if text.len() > 65536 {
        return Err(invalid());
    }
    let mut lines = text.lines();
    let header = lines.next().ok_or_else(invalid)?;
    if !header.starts_with("hub ") {
        return Err(invalid());
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in lines {
        let f: Vec<_> = line.split_ascii_whitespace().collect();
        if f.len() < 5 || f.len() > 7 || !matches!(f[0], "hs" | "ss") {
            return Err(invalid());
        }
        let number: u32 = f[1].parse().map_err(|_| invalid())?;
        let status: u32 = f[2].parse().map_err(|_| invalid())?;
        if number > 255 || !(4..=7).contains(&status) || !seen.insert(number) {
            return Err(invalid());
        }
        out.push(Port {
            number,
            super_speed: f[0] == "ss",
            free: status == 4,
        });
    }
    Ok(out)
}
fn free_port(root: &Path, speed: u32) -> io::Result<u32> {
    if !(1..=6).contains(&speed) {
        return Err(invalid());
    }
    let controllers: u32 = fs::read_to_string(root.join("nports"))?
        .trim()
        .parse()
        .map_err(|_| invalid())?;
    if controllers == 0 || controllers > 256 {
        return Err(invalid());
    }
    let mut paths = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == "status"
            || name
                .strip_prefix("status.")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            paths.push(item.path());
        }
    }
    paths.sort();
    for path in paths {
        for port in ports(&fs::read_to_string(path)?)? {
            if port.free && port.super_speed == (speed >= 5) {
                return Ok(port.number);
            }
        }
    }
    Err(io::ErrorKind::WouldBlock.into())
}
pub struct Attached {
    socket: Arc<TcpStream>,
    port: u32,
    devid: u32,
    key: Option<ImportKey>,
    done: std::sync::mpsc::Receiver<()>,
    _worker: thread::JoinHandle<()>,
}
impl Attached {
    pub fn import_key(&self) -> Option<ImportKey> {
        self.key
    }
    pub fn finished(&self) -> bool {
        !matches!(
            self.done.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        )
    }
}
impl crate::managed_runtime::Attachment for Attached {
    fn finished(&self) -> bool {
        self.finished()
    }
    fn eject(&mut self) -> Outcome {
        let owner = match usb_most_platform::linux::OwnedUsb::capture(
            self.port,
            self.devid,
            self.socket.clone(),
        ) {
            Ok(owner) => owner,
            Err(error) => {
                #[cfg(feature = "managed-eject-lab")]
                eprintln!("Owned Q35 capture failed: {:?}", error.kind());
                return match error.kind() {
                    io::ErrorKind::PermissionDenied => Outcome::Denied,
                    io::ErrorKind::NotFound => Outcome::Unavailable,
                    io::ErrorKind::Unsupported | io::ErrorKind::InvalidData => Outcome::Unsupported,
                    _ => Outcome::Uncertain,
                };
            }
        };
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(usb_most_platform::linux::eject(&owner)),
            Err(_) => Outcome::Unavailable,
        }
    }
}
impl Drop for Attached {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}
fn pair() -> io::Result<(TcpStream, TcpStream)> {
    // Bind before connect and verify the exact connected 4-tuple. A racing local
    // process cannot substitute its connection. This listener never serves USB.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let client = TcpStream::connect_timeout(&listener.local_addr()?, Duration::from_secs(2))?;
    let peer = client.local_addr()?;
    for _ in 0..32 {
        match listener.accept() {
            Ok((server, address)) if address == peer => return Ok((client, server)),
            Ok((foreign, _)) => drop(foreign),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::ErrorKind::TimedOut.into())
}
pub fn available() -> bool {
    Path::new(ROOT).join("attach").exists()
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
    attach_at(PathBuf::from(ROOT), id, remote, policy, capabilities)
}
fn attach_at(
    root: PathBuf,
    id: &str,
    remote: Arc<dyn Connector>,
    policy: Arc<Mutex<Policy>>,
    capabilities: Option<Capabilities>,
) -> io::Result<Attached> {
    if id.len() > 31 || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err(invalid());
    }
    let (mut kernel, server) = pair()?;
    kernel.set_read_timeout(Some(Duration::from_secs(90)))?;
    kernel.set_write_timeout(Some(Duration::from_secs(15)))?;
    let (tx, done) = std::sync::mpsc::channel();
    let (imported, imported_key) = std::sync::mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("usb-most-import".into())
        .spawn(move || {
            let result = if let Some(capabilities) = capabilities {
                agent::serve_managed(
                    server,
                    remote.as_ref(),
                    &policy,
                    None,
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
                agent::serve_with(server, remote.as_ref(), &policy)
            };
            if let Err(error) = result {
                eprintln!("USB Most import ended: {:?}", error.kind());
                #[cfg(feature = "managed-eject-lab")]
                eprintln!("Owned Q35 transport failure: {error}");
            }
            let _ = tx.send(());
        })?;
    let result = (|| {
        kernel.write_all(&[1, 17, 128, 3, 0, 0, 0, 0])?;
        let mut name = [0; 32];
        name[..id.len()].copy_from_slice(id.as_bytes());
        kernel.write_all(&name)?;
        let mut reply = [0; 8];
        kernel.read_exact(&mut reply)?;
        if reply != [1, 17, 0, 3, 0, 0, 0, 0] {
            return Err(invalid());
        }
        let mut descriptor = [0; 312];
        kernel.read_exact(&mut descriptor)?;
        if descriptor[256..288] != name {
            return Err(invalid());
        }
        let read = |at| u32::from_be_bytes(descriptor[at..at + 4].try_into().unwrap());
        let (bus, dev, speed) = (read(288), read(292), read(296));
        if bus > 65535 || dev > 65535 {
            return Err(invalid());
        }
        let port = free_port(&root, speed)?;
        // Metadata must be present before kernel attachment, not discovered
        // missing after the kernel has started enumerating the device.
        let key = if capabilities.is_some() {
            Some(imported_key.try_recv().map_err(|_| invalid())?)
        } else {
            None
        };
        kernel.set_read_timeout(None)?;
        kernel.set_write_timeout(None)?;
        // The kernel takes its own reference to this already-connected socket.
        // The sysfs operation is one write; partial writes are never replayed.
        let line = format!(
            "{} {} {} {}",
            port,
            kernel.as_raw_fd(),
            (bus << 16) | dev,
            speed
        );
        let mut attr = fs::OpenOptions::new()
            .write(true)
            .open(root.join("attach"))?;
        if attr.write(line.as_bytes())? != line.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        Ok((port, (bus << 16) | dev, key))
    })();
    let (port, devid, key) = match result {
        Ok(identity) => identity,
        Err(error) => {
            let _ = kernel.shutdown(Shutdown::Both);
            return Err(error);
        }
    };
    Ok(Attached {
        socket: Arc::new(kernel),
        port,
        devid,
        key,
        done,
        _worker: worker,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn free_ports_respect_speed_and_reject_ambiguous_kernel_status() {
        let text = "hub port sta spd dev sockfd local_busid\nhs 0000 004 000 00000000 0 0-0\nss 0008 004 000 00000000 0 0-0\nhs 0001 006 003 00010002 9 1-2\n";
        let out = ports(text).unwrap();
        assert_eq!(out.len(), 3);
        assert!(out[0].free);
        assert!(out[1].super_speed);
        assert!(!out[2].free);
        for bad in [
            "",
            "garbage",
            "hub port\nhs 0 4 0 0\nhs 0 4 0 0",
            "hub port\nss 9999 4 0 0",
            "hub port\nhs 0 3 0 0",
        ] {
            assert!(ports(bad).is_err());
        }
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("nports"), "16").unwrap();
        fs::write(temp.path().join("status"), text).unwrap();
        assert_eq!(free_port(temp.path(), 3).unwrap(), 0);
        assert_eq!(free_port(temp.path(), 5).unwrap(), 8);
    }
    #[test]
    fn private_socket_pair_is_connected_only_to_its_creator() {
        let (mut a, mut b) = pair().unwrap();
        a.write_all(b"test").unwrap();
        let mut bytes = [0; 4];
        b.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"test");
    }
}
