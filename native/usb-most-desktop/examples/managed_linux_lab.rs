//! Test-only Q35 harness exercising the actual receiver adapter/owning worker,
//! authenticated TCP and Book. The external lab runner owns all synthetic disks.
#![forbid(unsafe_code)]
#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../../../tests/fixtures/managed_usb.rs"]
#[cfg(not(test))]
mod fixture;
#[cfg(all(target_os = "linux", test))]
use managed_runtime::transport_tests::managed_fixture as fixture;
#[cfg(target_os = "linux")]
#[allow(dead_code)] // Reuse product modules without adding a production lab entry.
#[path = "../src/linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "../src/managed_runtime.rs"]
mod managed_runtime;
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        fs,
        io::{self, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    use usb_most_protocol::{
        agent,
        eject::{Outcome, State},
    };
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3
        || args[1] != "--owned-qemu-lab"
        || fs::read_to_string("/run/usb-most-eject-owned-lab")?
            != "USBMOST_EJECT113_SYNTHETIC_ONLY\n"
        || !fs::read_to_string("/sys/class/dmi/id/product_name")?.contains("Q35")
    {
        return Err("owned Q35 synthetic fixture required".into());
    }
    let id = &args[2];
    if id.is_empty()
        || id.len() > 31
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'.'))
    {
        return Err("bounded synthetic busid required".into());
    }
    let mut name = [0; 32];
    name[..id.len()].copy_from_slice(id.as_bytes());
    let backend = "127.0.0.1:3240".parse()?;
    let descriptor = agent::devices(backend)?
        .into_iter()
        .find(|(d, _)| d[256..288] == name)
        .ok_or("missing synthetic export")?
        .0;
    if descriptor[300..304] != [0x46, 0xf4, 0, 1] {
        return Err("not QEMU storage".into());
    }
    let read = |at| u32::from_be_bytes(descriptor[at..at + 4].try_into().unwrap());
    let devid = (read(288) << 16) | read(292);
    let fixture = fixture::Fixture::start_device(backend, Some(id));
    let permit = managed_runtime::Permit::acquire()?;
    let attached = linux::attach_with_management(
        "1-8",
        fixture.connector(),
        Arc::new(Mutex::new(agent::Policy::default())),
        Some(fixture.capabilities()),
    )?;
    let key = attached
        .import_key()
        .ok_or("missing managed import identity")?;
    // Read-only lab routing for the existing runner. Production ejection later
    // verifies port+devid+original socket FD and pinned sysfs identities itself.
    let deadline = Instant::now() + Duration::from_secs(30);
    let port = loop {
        let status = fs::read_to_string("/sys/devices/platform/vhci_hcd.0/status")?;
        let rows: Vec<_> = status
            .lines()
            .skip(1)
            .map(|l| l.split_ascii_whitespace().collect::<Vec<_>>())
            .filter(|f| {
                f.len() == 7 && f[2] == "006" && u32::from_str_radix(f[4], 16).ok() == Some(devid)
            })
            .collect();
        if rows.len() == 1 {
            let path = std::path::Path::new("/sys/bus/usb/devices").join(rows[0][6]);
            // Kernel status precedes udev/sysfs publication. The external mount
            // runner must not see READY until the actual VHCI child exists.
            if fs::canonicalize(&path).is_ok_and(|path| {
                path.starts_with("/sys/devices/platform/vhci_hcd.0")
                    && path.join("idVendor").is_file()
            }) {
                break rows[0][1].parse::<u32>()?;
            }
        }
        if Instant::now() >= deadline {
            return Err("VHCI enumeration deadline".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    struct Counted(linux::Attached, Arc<AtomicUsize>);
    impl managed_runtime::Attachment for Counted {
        fn finished(&self) -> bool {
            self.0.finished()
        }
        fn eject(&mut self) -> Outcome {
            self.1.fetch_add(1, Ordering::AcqRel);
            managed_runtime::Attachment::eject(&mut self.0)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let task = managed_runtime::Managed::start(
        permit,
        Counted(attached, calls.clone()),
        key,
        fixture.connector(),
    )?;
    println!("READY {port}");
    io::stdout().flush()?;
    loop {
        let mut command = String::new();
        if io::stdin().read_line(&mut command)? == 0 {
            eprintln!("RETAINED_IMPORT_AFTER_RUNNER_EOF");
            loop {
                std::thread::park();
            }
        }
        if command == "quit\n" {
            break;
        }
        if command != "eject\n" {
            return Err("unknown fixture command".into());
        }
        let deadline = Instant::now() + Duration::from_secs(35);
        while !fixture.ready(key) {
            if Instant::now() >= deadline {
                return Err("managed readiness deadline".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let sequence = fixture.request(key);
        let outcome = loop {
            if let Some(State::Completed {
                sequence: actual,
                outcome,
            }) = fixture.state(key)
                && actual == sequence
            {
                break outcome;
            }
            if Instant::now() >= deadline {
                return Err("managed OS result deadline".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(calls.load(Ordering::Acquire), sequence as usize);
        if outcome == Outcome::Safe {
            assert_eq!(fixture.losses(), (1, 1));
            assert!(!task.finished());
            fixture.acknowledge(key);
            while !task.finished() {
                if Instant::now() >= deadline {
                    return Err("managed retire deadline".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        println!("RESULT {outcome:?}");
        io::stdout().flush()?;
    }
    drop(task);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(managed_runtime::drain());
    eprintln!("MANAGED_GATEWAY_OK lost_command_and_result=true at_most_once=true");
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux-only isolated Q35 harness");
}
