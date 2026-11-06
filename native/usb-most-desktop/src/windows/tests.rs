use super::*;
#[path = "../../../usb-most-connection/examples/receiver_lab/usb.rs"]
mod fixture;
use crate::managed_runtime::transport_tests::managed_fixture;
use crate::managed_runtime::transport_tests::storage;
fn present() -> bool {
    let output=std::process::Command::new("powershell.exe").args(["-NoLogo","-NoProfile","-NonInteractive","-Command","$ErrorActionPreference='Stop';$d=@(Get-PnpDevice -PresentOnly | Where-Object {$_.InstanceId -like 'HID\\VID_CAFE&PID_0001*'});[Console]::Write($d.Count)"]).output().unwrap();
    assert!(output.status.success(), "synthetic HID PnP query failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap()
        > 0
}
#[test]
#[ignore = "requires the isolated Windows VM with our protected signed controller"]
fn signed_driver_imports_synthetic_hid_with_private_alias() {
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
    );
    assert!(available(), "private signed controller must be ready");
    let fixture = fixture::Fixture::start(false);
    assert_ne!(fixture.proof, [0; 32]);
    let attached = attach(
        "1-7",
        Arc::new(agent::Direct(fixture.address)),
        Arc::new(Mutex::new(Policy::default())),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        assert!(
            !attached.finished(),
            "USB proxy ended during Windows HID enumeration"
        );
        if fixture.stats()["reports"].as_u64().unwrap() >= 10 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Windows HID enumeration/interrupt deadline; stats={}",
            fixture.stats()
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(present(), "actual Windows PnP HID child must exist");
    assert_eq!(fixture.stats()["imports"], 1);
    assert!(fixture.stats()["controls"].as_u64().unwrap() > 0);
    drop(attached);
    let deadline = Instant::now() + Duration::from_secs(15);
    while present() {
        assert!(Instant::now() < deadline, "own HID device did not detach");
        thread::sleep(Duration::from_millis(200));
    }
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        fixture.stats()["imports"],
        1,
        "PLUGIN_ONCE must not reattach or replay after detach"
    );
    println!(
        "PASS real Windows driver/PnP HID enum, private alias, control/interrupt data, own detach and no reattach"
    );
}

fn storage_volume() -> Option<std::path::PathBuf> {
    // Discovery only: no format, mount, global unplug or arbitrary path input.
    // All writes below additionally require the exact RAM fixture marker.
    // Avoid invoking the storage-management provider (Get-Partition/Get-Disk):
    // its retained handles can themselves veto an otherwise idle disk removal.
    // This isolated VM has no physical USB. Unique label + exact probe contents
    // authorize fixture file writes; the adapter separately verifies driver/PnP
    // ownership before any OS eject operation.
    const QUERY: &str = r#"$ErrorActionPreference='Stop';
$d=@([IO.DriveInfo]::GetDrives() | Where-Object {$_.DriveType -eq [IO.DriveType]::Removable -and $_.IsReady -and $_.VolumeLabel -eq 'USBMOSTLAB'});
if($d.Count -gt 1){throw 'Ambiguous RAM fixture volumes'}
if($d.Count -eq 1){[Console]::Write($d[0].Name.Substring(0,1))}"#;
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            QUERY,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = output;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            // Read-only helper process only; no OS removal thread is cancelled.
            let _ = child.kill();
            let _ = child.wait();
            panic!("RAM fixture read-only volume query deadline");
        }
        thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "RAM fixture discovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let letter = text.trim().as_bytes();
    if letter.is_empty() {
        return None;
    }
    assert!(letter.len() == 1 && letter[0].is_ascii_uppercase());
    Some(std::path::PathBuf::from(format!(
        "{}:\\",
        letter[0] as char
    )))
}
fn wait_storage(attached: &Attached, fixture: &storage::Fixture) -> std::path::PathBuf {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(
            !attached.finished(),
            "storage proxy ended during enumeration"
        );
        if let Some(root) = storage_volume() {
            assert_eq!(
                std::fs::read(root.join("PROBE.TXT")).unwrap(),
                storage::PROBE
            );
            return root;
        }
        if Instant::now() >= deadline {
            // Read-only, synthetic/OS volume metadata only: no labels or contents.
            let diagnostic = std::process::Command::new("powershell.exe").args([
                "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                r#"[IO.DriveInfo]::GetDrives() | Where-Object {$_.DriveType -ne [IO.DriveType]::Network} | ForEach-Object { $v=$_;[pscustomobject]@{Type=[string]$v.DriveType;Ready=$v.IsReady;FixtureLabel=($v.IsReady -and $v.VolumeLabel -eq 'USBMOSTLAB');FixtureProbe=($v.IsReady -and [IO.File]::Exists([IO.Path]::Combine($v.Name,'PROBE.TXT')))} } | ConvertTo-Json -Compress"#,
            ]).output().unwrap();
            eprintln!(
                "RAM fixture volume metadata: {}",
                String::from_utf8_lossy(&diagnostic.stdout)
            );
            panic!(
                "Windows RAM disk enumeration deadline: {} SCSI commands",
                fixture.counts.commands.load(Ordering::Relaxed)
            );
        }
        thread::sleep(Duration::from_millis(300));
    }
}
fn capture(attached: &Attached) -> usb_most_platform::windows::eject::OwnedImport {
    usb_most_platform::windows::eject::OwnedImport::capture(
        &attached.controller,
        attached.port,
        attached.local_port,
        &attached.alias,
    )
    .unwrap()
}
fn explicitly_retry_idle_ejection(attached: &Attached) {
    use usb_most_protocol::eject::Outcome;
    // Each attempt models a new explicit Retry after a known Busy result. This
    // is a lab sequence, not automatic replay or retry logic in the product.
    // Give OS antivirus/indexing/close notifications time to retire their handles.
    for attempt in 1..=10 {
        let outcome = capture(attached).start_eject().unwrap().join().unwrap();
        println!("RAM fixture explicit idle-eject attempt {attempt}: {outcome:?}");
        if outcome == Outcome::Safe {
            return;
        }
        assert_eq!(outcome, Outcome::Busy);
        thread::sleep(Duration::from_secs(1));
    }
    panic!("Windows continued vetoing the idle RAM fixture; no force removal attempted");
}
fn managed_ejection_once(
    fixture: &managed_fixture::Fixture,
    key: usb_most_protocol::eject::ImportKey,
) -> (u64, usb_most_protocol::eject::Outcome) {
    use usb_most_protocol::eject::State;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !fixture.ready(key) {
        assert!(
            Instant::now() < deadline,
            "managed receiver readiness deadline"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let sequence = fixture.request(key); // one explicit lab UI action
    loop {
        if let Some(State::Completed {
            sequence: actual,
            outcome,
        }) = fixture.state(key)
            && actual == sequence
        {
            println!("Managed RAM fixture operation {sequence}: {outcome:?}");
            return (sequence, outcome);
        }
        assert!(Instant::now() < deadline, "managed OS result deadline");
        thread::sleep(Duration::from_millis(50));
    }
}
fn explicitly_retry_managed_ejection(
    fixture: &managed_fixture::Fixture,
    key: usb_most_protocol::eject::ImportKey,
) -> u64 {
    use usb_most_protocol::eject::Outcome;
    for _ in 0..10 {
        let (sequence, outcome) = managed_ejection_once(fixture, key);
        if outcome == Outcome::Safe {
            return sequence;
        }
        assert_eq!(outcome, Outcome::Busy);
        thread::sleep(Duration::from_secs(1));
    }
    panic!("Windows continued vetoing the managed RAM fixture; no force removal");
}
#[test]
#[ignore = "requires isolated Windows signed-driver VM; only synthetic RAM storage"]
fn signed_driver_ejects_only_owned_storage_after_os_confirmation() {
    use std::{
        fs::OpenOptions,
        io::{Read, Seek, SeekFrom, Write},
        os::windows::fs::OpenOptionsExt,
    };
    use usb_most_protocol::eject::Outcome;
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
    );
    assert!(available());
    assert!(
        storage_volume().is_none(),
        "another storage lab already owns the fixture identity"
    );
    let fixture = storage::Fixture::start();
    let management = managed_fixture::Fixture::start(fixture.address);
    let hid = fixture::Fixture::start_with_input(false, false);
    let mouse = attach(
        "1-7",
        Arc::new(agent::Direct(hid.address)),
        Arc::new(Mutex::new(Policy::default())),
    )
    .unwrap();
    let disk = attach_with_management(
        "1-8",
        management.connector(),
        Arc::new(Mutex::new(Policy::default())),
        Some(management.capabilities()),
    )
    .unwrap();
    let root = wait_storage(&disk, &fixture);
    assert_eq!(
        capture(&mouse).start_eject().unwrap().join().unwrap(),
        Outcome::Unsupported,
        "HID must never be ejected as storage"
    );
    let mut wrong = *disk.alias;
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    assert!(
        !disk
            .controller
            .matches_import(disk.port, disk.local_port, &wrong)
            .unwrap()
    );
    assert!(
        usb_most_platform::windows::eject::OwnedImport::capture(
            &disk.controller,
            disk.port,
            disk.local_port,
            &wrong
        )
        .is_err()
    );
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(1)
        .create_new(true)
        .open(root.join("EJECT.TXT"))
        .unwrap();
    let payload = b"Owned RAM fixture write must survive OS-confirmed ejection.\r\n";
    file.write_all(payload).unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        capture(&disk).start_eject().unwrap().join().unwrap(),
        Outcome::Busy,
        "open file must veto removal without forced detach"
    );
    assert!(!disk.finished());
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut actual = Vec::new();
    file.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, payload);
    let obsolete = capture(&disk);
    let reports = hid.stats()["reports"].as_u64().unwrap();
    let old_port = disk.port;
    let key = disk
        .import_key()
        .expect("managed IMPORT key must precede driver enumeration");
    struct Counted(Attached, Arc<std::sync::atomic::AtomicUsize>);
    impl crate::managed_runtime::Attachment for Counted {
        fn finished(&self) -> bool {
            self.0.finished()
        }
        fn eject(&mut self) -> Outcome {
            self.1.fetch_add(1, Ordering::AcqRel);
            crate::managed_runtime::Attachment::eject(&mut self.0)
        }
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let retired_disk = crate::managed_runtime::Managed::start(
        crate::managed_runtime::Permit::acquire().unwrap(),
        Counted(disk, calls.clone()),
        key,
        management.connector(),
    )
    .unwrap();
    assert_eq!(managed_ejection_once(&management, key), (1, Outcome::Busy));
    file.seek(SeekFrom::Start(0)).unwrap();
    actual.clear();
    file.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, payload);
    drop(file);
    let sequence = explicitly_retry_managed_ejection(&management, key);
    println!("Managed OS Safe observed; checking volume disappearance");
    assert_eq!(
        calls.load(Ordering::Acquire),
        sequence as usize,
        "lost/duplicate metadata must not repeat an OS operation"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while storage_volume().is_some() {
        assert!(
            Instant::now() < deadline,
            "OS-confirmed ejected storage volume remained visible"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(!mouse.finished());
    assert!(
        hid.stats()["reports"].as_u64().unwrap() > reports,
        "other USB must continue during disk ejection"
    );
    println!("Reimporting RAM storage while old result owner remains retained");
    let disk = attach(
        "1-8",
        Arc::new(agent::Direct(fixture.address)),
        Arc::new(Mutex::new(Policy::default())),
    )
    .unwrap();
    println!("Replacement attached; checking enumeration");
    let root = wait_storage(&disk, &fixture);
    assert_eq!(old_port, disk.port, "exercise actual port reuse");
    assert!(
        !disk
            .controller
            .detach_owned(disk.port, disk.local_port, &wrong)
            .unwrap(),
        "wrong alias must not detach an occupied port"
    );
    assert!(
        !retired_disk.finished(),
        "keep Safe until exact lease acknowledgment"
    );
    assert_eq!(management.losses(), (1, 1));
    management.acknowledge(key);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !retired_disk.finished() {
        assert!(
            Instant::now() < deadline,
            "managed retire/old owner cleanup deadline"
        );
        thread::sleep(Duration::from_millis(50));
    }
    drop(retired_disk);
    assert!(
        disk.controller
            .matches_import(disk.port, disk.local_port, &disk.alias)
            .unwrap(),
        "dropping an old attachment must leave the replacement import alive"
    );
    assert_eq!(
        obsolete.start_eject().unwrap().join().unwrap(),
        Outcome::Unavailable,
        "old import cannot eject replacement on a reused port"
    );
    assert!(!disk.finished());
    assert_eq!(std::fs::read(root.join("EJECT.TXT")).unwrap(), payload);
    explicitly_retry_idle_ejection(&disk);
    assert_eq!(fixture.counts.imports.load(Ordering::Relaxed), 2);
    assert!(fixture.counts.reads.load(Ordering::Relaxed) > 0);
    assert!(fixture.counts.writes.load(Ordering::Relaxed) > 0);
    drop(disk);
    drop(mouse);
    println!(
        "PASS authenticated managed IMPORT/SYNC, lost command/result, at-most-once OS Safe, exact old-owner cleanup after port reuse, Busy with open file, unaffected HID, stale import rejection and RAM data survived reimport"
    );
}
