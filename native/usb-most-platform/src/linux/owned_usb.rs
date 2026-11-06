use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    net::TcpStream,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use usb_most_protocol::eject::Outcome;

const VHCI: &str = "/sys/devices/platform/vhci_hcd.0";

pub(super) fn read(path: &Path, limit: usize) -> io::Result<String> {
    let mut text = String::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_string(&mut text)?;
    if text.len() > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(text)
}

#[derive(Debug)]
pub(super) struct PinnedPath {
    pub path: PathBuf,
    file: File,
}
impl PinnedPath {
    fn new(path: PathBuf) -> io::Result<Self> {
        let file = File::open(&path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let pin = Self { path, file };
        pin.check()?;
        Ok(pin)
    }
    pub fn check(&self) -> io::Result<()> {
        let old = self.file.metadata()?;
        let now = fs::metadata(&self.path)?;
        if old.dev() != now.dev() || old.ino() != now.ino() {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(())
    }
}

/// Captured from the successful sysfs attach, never from a remote device path.
/// The original socket FD stays owned: its number cannot be reused in this
/// process while checking the kernel's port/devid/sockfd record.
pub struct OwnedUsb {
    root: PathBuf,
    block_root: PathBuf,
    port: u32,
    devid: u32,
    socket: Arc<TcpStream>,
    name: String,
    pub(super) pin: PinnedPath,
}

fn local_name(text: &str, port: u32, devid: u32, fd: i32) -> io::Result<String> {
    let mut lines = text.lines();
    let head: Vec<_> = lines
        .next()
        .unwrap_or("")
        .split_ascii_whitespace()
        .collect();
    if head != ["hub", "port", "sta", "spd", "dev", "sockfd", "local_busid"] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut found = None;
    for line in lines {
        let f: Vec<_> = line.split_ascii_whitespace().collect();
        if f.len() != 7 || !matches!(f[0], "hs" | "ss") {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let number = f[1]
            .parse::<u32>()
            .map_err(|_| io::ErrorKind::InvalidData)?;
        if number > 255 || !seen.insert(number) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if number != port {
            continue;
        }
        if f[2] != "006"
            || u32::from_str_radix(f[4], 16).ok() != Some(devid)
            || f[5].parse::<i32>().ok() != Some(fd)
            || !valid_name(f[6])
        {
            return Err(io::ErrorKind::NotFound.into());
        }
        found = Some(f[6].to_owned());
    }
    found.ok_or_else(|| io::ErrorKind::NotFound.into())
}

fn valid_name(name: &str) -> bool {
    // A VHCI import is a root-port device, never a descendant of a USB hub.
    let Some((bus, port)) = name.split_once('-') else {
        return false;
    };
    let number = |s: &str| {
        !s.is_empty()
            && s.len() <= 5
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u32>().is_ok_and(|v| v > 0 && v <= 65535)
    };
    name.len() <= 11 && number(bus) && number(port)
}

impl OwnedUsb {
    pub fn capture(port: u32, devid: u32, socket: Arc<TcpStream>) -> io::Result<Self> {
        Self::capture_at(
            Path::new(VHCI),
            Path::new("/sys/dev/block"),
            port,
            devid,
            socket,
        )
    }
    fn capture_at(
        root: &Path,
        block_root: &Path,
        port: u32,
        devid: u32,
        socket: Arc<TcpStream>,
    ) -> io::Result<Self> {
        if port > 255 || devid == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let root = root.canonicalize()?;
        let name = find_port(&root, port, devid, socket.as_raw_fd())?;
        let bus = name.split_once('-').unwrap().0;
        let path = root.join(format!("usb{bus}")).join(&name);
        // No symlink substitution or nested hubs in the selected sysfs path.
        if path.canonicalize()? != path {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let device = Self {
            root,
            block_root: block_root.to_owned(),
            port,
            devid,
            socket,
            name,
            pin: PinnedPath::new(path)?,
        };
        device.current().map_err(|_| io::ErrorKind::NotFound)?;
        Ok(device)
    }
    pub(super) fn current(&self) -> Result<(), Outcome> {
        // A matching numeric FD in sysfs is insufficient if this original
        // stream has already closed and another process reused the port. Poll
        // without consuming URBs or changing the kernel-shared blocking mode.
        use rustix::event::{PollFd, PollFlags};
        let mut poll = [PollFd::new(self.socket.as_ref(), PollFlags::RDHUP)];
        rustix::event::poll(&mut poll, Some(&rustix::event::Timespec::default()))
            .map_err(|_| Outcome::Unavailable)?;
        if poll[0]
            .revents()
            .intersects(PollFlags::RDHUP | PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL)
        {
            return Err(Outcome::Unavailable);
        }
        self.pin.check().map_err(|_| Outcome::Unavailable)?;
        let name = find_port(&self.root, self.port, self.devid, self.socket.as_raw_fd())
            .map_err(|_| Outcome::Unavailable)?;
        if name != self.name {
            return Err(Outcome::Unavailable);
        }
        self.pin.check().map_err(|_| Outcome::Unavailable)
    }
    pub(super) fn storage_only(&self) -> Result<(), Outcome> {
        self.current()?;
        let attr =
            |name: &str| read(&self.pin.path.join(name), 16).map_err(|_| Outcome::Unsupported);
        let class = attr("bDeviceClass")?;
        if !matches!(class.trim(), "00" | "08") {
            return Err(Outcome::Unsupported);
        }
        let expected = attr("bNumInterfaces")?
            .trim()
            .parse::<u8>()
            .map_err(|_| Outcome::Unsupported)?;
        if expected == 0 || expected > 32 {
            return Err(Outcome::Unsupported);
        }
        let mut count = 0;
        for (index, entry) in fs::read_dir(&self.pin.path)
            .map_err(|_| Outcome::Unavailable)?
            .enumerate()
        {
            if index >= 256 {
                return Err(Outcome::Unsupported);
            }
            let entry = entry.map_err(|_| Outcome::Unavailable)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|s| s.starts_with(&format!("{}:", self.name)))
            {
                count += 1;
                let class = read(&entry.path().join("bInterfaceClass"), 16)
                    .map_err(|_| Outcome::Unsupported)?;
                if class.trim() != "08" {
                    return Err(Outcome::Unsupported);
                }
            }
        }
        if count != expected {
            return Err(Outcome::Unsupported);
        }
        self.current()
    }
    /// Enumerate the kernel's block devices independently of UDisks. A disk
    /// object can appear on D-Bus before its already-present partitions do.
    /// Bound the global scan, but retain only descendants of this pinned USB.
    pub(super) fn blocks(&self) -> Result<BTreeMap<u64, PinnedPath>, Outcome> {
        self.current()?;
        let mut blocks = BTreeMap::new();
        for (index, entry) in fs::read_dir(&self.block_root)
            .map_err(|_| Outcome::Unavailable)?
            .enumerate()
        {
            if index >= 4096 {
                return Err(Outcome::Unsupported);
            }
            let entry = entry.map_err(|_| Outcome::Unavailable)?;
            let name = entry.file_name();
            let (major, minor) = name
                .to_str()
                .and_then(|name| name.split_once(':'))
                .ok_or(Outcome::Unsupported)?;
            let major = major.parse::<u32>().map_err(|_| Outcome::Unsupported)?;
            let minor = minor.parse::<u32>().map_err(|_| Outcome::Unsupported)?;
            let number = rustix::fs::makedev(major, minor);
            if name != format!("{major}:{minor}").as_str()
                || rustix::fs::major(number) != major
                || rustix::fs::minor(number) != minor
            {
                return Err(Outcome::Unsupported);
            }
            if let Some(pin) = self.block(number)?
                && (blocks.len() >= 128 || blocks.insert(number, pin).is_some())
            {
                return Err(Outcome::Unsupported);
            }
        }
        self.current()?;
        Ok(blocks)
    }
    pub(super) fn block(&self, device_number: u64) -> Result<Option<PinnedPath>, Outcome> {
        let major = rustix::fs::major(device_number);
        let minor = rustix::fs::minor(device_number);
        let path = match self
            .block_root
            .join(format!("{major}:{minor}"))
            .canonicalize()
        {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Outcome::Unavailable),
        };
        if !path.starts_with(&self.pin.path) {
            return Ok(None);
        }
        let pin = PinnedPath::new(path).map_err(|_| Outcome::Unavailable)?;
        // Device mapper, RAID and other block consumers need a separate flow.
        if fs::read_dir(pin.path.join("holders"))
            .map_err(|_| Outcome::Unavailable)?
            .next()
            .is_some()
        {
            return Err(Outcome::Busy);
        }
        self.current()?;
        Ok(Some(pin))
    }
}

fn find_port(root: &Path, port: u32, devid: u32, fd: i32) -> io::Result<String> {
    let mut names = Vec::new();
    for (index, item) in fs::read_dir(root)?.enumerate() {
        if index >= 256 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let item = item?;
        let name = item.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "status"
            || name
                .strip_prefix("status.")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            names.push(item.path());
        }
    }
    if names.is_empty() || names.len() > 32 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut found = None;
    for path in names {
        match local_name(&read(&path, 65536)?, port, devid, fd) {
            Ok(name) if found.is_none() => found = Some(name),
            Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    found.ok_or_else(|| io::ErrorKind::NotFound.into())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(crate) struct Fixture {
        // All paths are plain, newly-created test directories, NEVER mounts.
        pub temp: tempfile::TempDir,
        pub device: OwnedUsb,
        _peer: TcpStream,
    }
    impl Fixture {
        pub fn new() -> Self {
            Self::with_blocks(1)
        }
        pub fn with_blocks(count: u32) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let socket = Arc::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
            let (peer, _) = listener.accept().unwrap();
            let root = temp.path().join("vhci");
            let usb = root.join("usb3/3-1");
            let interface = usb.join("3-1:1.0");
            fs::create_dir_all(&interface).unwrap();
            fs::write(
                root.join("status"),
                format!(
                    "hub port sta spd dev sockfd local_busid\nhs 0000 006 003 00010002 {:06} 3-1\n",
                    socket.as_raw_fd()
                ),
            )
            .unwrap();
            fs::write(usb.join("bDeviceClass"), "00\n").unwrap();
            fs::write(usb.join("bNumInterfaces"), " 1\n").unwrap();
            fs::write(interface.join("bInterfaceClass"), "08\n").unwrap();
            let blocks = temp.path().join("blocks");
            fs::create_dir(&blocks).unwrap();
            for minor in 0..count {
                let disk = interface.join(format!("block/disk{minor}"));
                fs::create_dir_all(disk.join("holders")).unwrap();
                std::os::unix::fs::symlink(disk, blocks.join(format!("8:{minor}"))).unwrap();
            }
            let device = OwnedUsb::capture_at(&root, &blocks, 0, 0x10002, socket).unwrap();
            Self {
                temp,
                device,
                _peer: peer,
            }
        }
    }
    #[test]
    fn inventory_includes_every_owned_child_but_not_foreign_disks() {
        let f = Fixture::with_blocks(3);
        let foreign = f.temp.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, f.temp.path().join("blocks/9:0")).unwrap();
        assert_eq!(
            f.device.blocks().unwrap().into_keys().collect::<Vec<_>>(),
            (0..3)
                .map(|minor| rustix::fs::makedev(8, minor))
                .collect::<Vec<_>>()
        );
        // A holder on a child absent from UDisks still vetoes the operation.
        let child = f.temp.path().join("blocks/8:2").canonicalize().unwrap();
        fs::write(child.join("holders/dm-0"), "").unwrap();
        assert!(matches!(f.device.blocks(), Err(Outcome::Busy)));
    }
    #[test]
    fn inventory_is_bounded_and_rejects_noncanonical_device_numbers() {
        let f = Fixture::with_blocks(129);
        assert!(matches!(f.device.blocks(), Err(Outcome::Unsupported)));
        let f = Fixture::new();
        let child = f.temp.path().join("blocks/8:0").canonicalize().unwrap();
        std::os::unix::fs::symlink(child, f.temp.path().join("blocks/08:0")).unwrap();
        assert!(matches!(f.device.blocks(), Err(Outcome::Unsupported)));
    }
    #[test]
    fn actual_sysfs_shape_limits_storage_to_owned_blocks() {
        let f = Fixture::new();
        f.device.storage_only().unwrap();
        assert!(f.device.block(rustix::fs::makedev(8, 0)).unwrap().is_some());
        let foreign = f.temp.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, f.temp.path().join("blocks/9:0")).unwrap();
        assert!(f.device.block(rustix::fs::makedev(9, 0)).unwrap().is_none());
        fs::write(f.device.pin.path.join("3-1:1.0/bInterfaceClass"), "03\n").unwrap();
        assert_eq!(f.device.storage_only(), Err(Outcome::Unsupported));
    }
    #[test]
    fn block_holders_and_reused_socket_records_are_rejected() {
        let f = Fixture::new();
        let pin = f.device.block(rustix::fs::makedev(8, 0)).unwrap().unwrap();
        fs::write(pin.path.join("holders/dm-0"), "").unwrap();
        assert!(matches!(
            f.device.block(rustix::fs::makedev(8, 0)),
            Err(Outcome::Busy)
        ));
        fs::write(
            f.device.root.join("status"),
            "hub port sta spd dev sockfd local_busid\nhs 0000 006 003 00010002 999999 3-1\n",
        )
        .unwrap();
        assert_eq!(f.device.current(), Err(Outcome::Unavailable));
    }
    #[test]
    fn disconnected_original_socket_cannot_authorize_a_reused_port_record() {
        let f = Fixture::new();
        f.device.current().unwrap();
        f.device.socket.shutdown(std::net::Shutdown::Both).unwrap();
        // The status text and pinned directory are deliberately still present.
        assert_eq!(f.device.current(), Err(Outcome::Unavailable));
    }
    #[test]
    fn exact_port_socket_and_device_are_required() {
        let status = "hub port sta spd dev sockfd local_busid\nhs 0000 006 003 00010002 000009 3-1\nss 0008 004 000 00000000 000000 0-0\n";
        assert_eq!(local_name(status, 0, 0x10002, 9).unwrap(), "3-1");
        for (port, dev, fd) in [(1, 0x10002, 9), (0, 0x10003, 9), (0, 0x10002, 10)] {
            assert!(local_name(status, port, dev, fd).is_err());
        }
        for bad in [
            status.replace("006", "004"),
            status.replace("3-1", "../disk"),
            status.replace("3-1", "3-1.2"),
            status.replace("0008", "0000"),
            status.replace("sockfd", "pointer"),
        ] {
            assert!(local_name(&bad, 0, 0x10002, 9).is_err());
        }
    }
    #[test]
    fn a_recreated_path_is_not_the_pinned_device() {
        // Ordinary directories only; this fixture never creates a mountpoint.
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("device");
        fs::create_dir(&path).unwrap();
        let pin = PinnedPath::new(path.clone()).unwrap();
        pin.check().unwrap();
        fs::remove_dir(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(pin.check().is_err());
    }
}
