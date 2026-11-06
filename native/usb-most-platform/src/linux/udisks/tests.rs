//! Real private D-Bus transport with synthetic UDisks and ordinary sysfs files.
//! These tests never connect to the host system bus or enumerate real USB.
use super::*;
use crate::linux::owned_usb::tests::Fixture;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};
const DISK: &str = "/org/freedesktop/UDisks2/block_devices/fixture";
const CHILD1: &str = "/org/freedesktop/UDisks2/block_devices/fixture1";
const CHILD2: &str = "/org/freedesktop/UDisks2/block_devices/fixture2";
const DISK_DRIVE: &str = "/org/freedesktop/UDisks2/drives/fixture";
fn array(mounted: bool) -> OwnedVariant {
    let paths = if mounted {
        vec![b"/fixture-only\0".to_vec()]
    } else {
        vec![]
    };
    OwnedVariant::try_from(Variant::from(paths)).unwrap()
}
fn obj(path: &str) -> OwnedVariant {
    ObjectPath::try_from(path).unwrap().into()
}
fn string_value(s: &str) -> OwnedVariant {
    zbus::Str::from(s).into()
}
fn objects() -> ManagedObjects {
    HashMap::from([
        (
            DISK.try_into().unwrap(),
            HashMap::from([
                (
                    BLOCK.try_into().unwrap(),
                    HashMap::from([
                        ("DeviceNumber".into(), rustix::fs::makedev(8, 0).into()),
                        ("Drive".into(), obj(DISK_DRIVE)),
                        ("IdUsage".into(), string_value("filesystem")),
                        ("MDRaid".into(), obj("/")),
                        ("MDRaidMember".into(), obj("/")),
                        ("CryptoBackingDevice".into(), obj("/")),
                    ]),
                ),
                (
                    FILESYSTEM.try_into().unwrap(),
                    HashMap::from([("MountPoints".into(), array(true))]),
                ),
            ]),
        ),
        (
            DISK_DRIVE.try_into().unwrap(),
            HashMap::from([(
                DRIVE.try_into().unwrap(),
                HashMap::from([
                    ("ConnectionBus".into(), string_value("usb")),
                    ("CanPowerOff".into(), true.into()),
                    ("SiblingId".into(), string_value("usb3/3-1")),
                    ("TimeDetected".into(), 42u64.into()),
                ]),
            )]),
        ),
    ])
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.UDisks2.Error")]
enum Rejection {
    DeviceBusy(String),
    NotAuthorized(String),
    Failed(String),
    NotSupported(String),
}
#[derive(Default)]
struct State {
    objects: ManagedObjects,
    calls: Vec<String>,
    failure: Option<Outcome>,
    power_delay: u64,
    remove: Option<std::path::PathBuf>,
    snapshots: usize,
    hide_at_snapshot: Option<usize>,
    metadata_delay_ms: u64,
}
#[derive(Clone)]
struct Manager(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl Manager {
    async fn get_managed_objects(&self) -> ManagedObjects {
        let (objects, delay) = {
            let mut state = self.0.lock().unwrap();
            state.snapshots += 1;
            if state.hide_at_snapshot == Some(state.snapshots) {
                state.objects.clear();
            }
            (state.objects.clone(), state.metadata_delay_ms)
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        objects
    }
}
#[derive(Clone)]
struct Filesystem(Arc<Mutex<State>>, &'static str);
#[zbus::interface(name = "org.freedesktop.UDisks2.Filesystem")]
impl Filesystem {
    fn unmount(&self, options: HashMap<String, OwnedVariant>) -> Result<(), Rejection> {
        assert_eq!(options.len(), 1);
        assert!(bool::try_from(&options["auth.no_user_interaction"]).unwrap());
        let mut s = self.0.lock().unwrap();
        s.calls.push("unmount".into());
        match s.failure {
            Some(Outcome::Busy) => {
                return Err(Rejection::DeviceBusy("private path must not escape".into()));
            }
            Some(Outcome::Denied) => {
                return Err(Rejection::NotAuthorized("private identity".into()));
            }
            Some(Outcome::Unsupported) => {
                return Err(Rejection::NotSupported("unsupported fixture".into()));
            }
            Some(Outcome::Uncertain) => {
                return Err(Rejection::Failed("unknown partial progress".into()));
            }
            _ => (),
        }
        s.objects
            .get_mut(&ObjectPath::try_from(self.1).unwrap())
            .unwrap()
            .get_mut(FILESYSTEM)
            .unwrap()
            .insert("MountPoints".into(), array(false));
        Ok(())
    }
}
#[derive(Clone)]
struct DriveService(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.UDisks2.Drive")]
impl DriveService {
    async fn power_off(&self, options: HashMap<String, OwnedVariant>) -> Result<(), Rejection> {
        assert_eq!(options.len(), 1);
        assert!(bool::try_from(&options["auth.no_user_interaction"]).unwrap());
        let (delay, remove) = {
            let mut s = self.0.lock().unwrap();
            s.calls.push("poweroff".into());
            (s.power_delay, s.remove.clone())
        };
        tokio::time::sleep(Duration::from_millis(delay)).await;
        if let Some(path) = remove {
            std::fs::write(
                path,
                "hub port sta spd dev sockfd local_busid\nhs 0000 004 000 00000000 000000 0-0\n",
            )
            .unwrap();
        }
        Ok(())
    }
}
struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}
impl Bus {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new("/usr/bin/dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}/bus", dir.path().display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(address.starts_with("unix:"));
        Self {
            child,
            address: address.trim().into(),
            _dir: dir,
        }
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
async fn server(bus: &Bus, state: Arc<Mutex<State>>) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .name(SERVICE)
        .serve_at(ROOT, Manager(state.clone()))
        .serve_at(DISK, Filesystem(state.clone(), DISK))
        .serve_at(CHILD1, Filesystem(state.clone(), CHILD1))
        .serve_at(CHILD2, Filesystem(state.clone(), CHILD2))
        .serve_at(DISK_DRIVE, DriveService(state))
        .build()
        .await
        .unwrap()
}
async fn host<'a>(bus: &Bus, fixture: &'a Fixture) -> UDisks<'a> {
    let connection = zbus::connection::Builder::address(bus.address.as_str())
        .build()
        .await
        .unwrap();
    let owner = service_owner(&connection).await.unwrap();
    UDisks {
        connection,
        owner,
        device: &fixture.device,
        pins: BTreeMap::new(),
        expected: None,
    }
}
fn run(f: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_success_veto_denial_and_uncertain_result() {
    run(async {
        let bus = Bus::new();
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            ..State::default()
        }));
        let _server = server(&bus, state.clone()).await;
        for failure in [
            None,
            Some(Outcome::Busy),
            Some(Outcome::Denied),
            Some(Outcome::Unsupported),
            Some(Outcome::Uncertain),
        ] {
            let fixture = Fixture::new();
            *state.lock().unwrap() = State {
                objects: objects(),
                failure,
                ..State::default()
            };
            let mut host = host(&bus, &fixture).await;
            assert_eq!(
                eject::execute(&mut host).await,
                failure.unwrap_or(Outcome::Safe)
            );
            let calls = state.lock().unwrap().calls.clone();
            assert_eq!(
                calls,
                if failure.is_none() {
                    vec!["unmount", "poweroff"]
                } else {
                    vec!["unmount"]
                }
            );
        }
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_initial_probe_lag_waits_without_executing_or_replaying_mutations() {
    run(async {
        let bus = Bus::new();
        for outcome in [Outcome::Safe, Outcome::Busy] {
            let fixture = Fixture::new();
            let state = Arc::new(Mutex::new(State {
                failure: (outcome == Outcome::Busy).then_some(outcome),
                ..State::default()
            }));
            let service = server(&bus, state.clone()).await;
            let mut host = host(&bus, &fixture).await;
            let future = eject::execute(&mut host);
            tokio::pin!(future);
            assert!(
                tokio::time::timeout(Duration::from_millis(180), &mut future)
                    .await
                    .is_err()
            );
            {
                let mut snapshot = state.lock().unwrap();
                assert!(snapshot.snapshots >= 2);
                assert!(snapshot.calls.is_empty());
                snapshot.objects = objects();
            }
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), future)
                    .await
                    .unwrap(),
                outcome
            );
            assert_eq!(
                state.lock().unwrap().calls,
                if outcome == Outcome::Safe {
                    vec!["unmount", "poweroff"]
                } else {
                    vec!["unmount"]
                }
            );
            service.close().await.unwrap();
        }
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_probe_deadline_does_not_claim_usb_loss_or_operate_on_foreign_devices() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::new();
        let mut foreign = objects();
        foreign
            .get_mut(&ObjectPath::try_from(DISK).unwrap())
            .unwrap()
            .get_mut(BLOCK)
            .unwrap()
            .insert("DeviceNumber".into(), rustix::fs::makedev(99, 0).into());
        let state = Arc::new(Mutex::new(State {
            objects: foreign,
            ..State::default()
        }));
        let _service = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        assert!(matches!(
            host.wait_for_owned_snapshot(Duration::from_millis(120))
                .await,
            Err(Outcome::Unsupported)
        ));
        fixture.device.current().unwrap();
        assert!(state.lock().unwrap().calls.is_empty());
        assert!(state.lock().unwrap().snapshots >= 2);
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_probe_deadline_cancels_only_a_readonly_query() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::new();
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            metadata_delay_ms: 2000,
            ..State::default()
        }));
        let _service = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        let result = tokio::time::timeout(
            Duration::from_millis(600),
            host.wait_for_owned_snapshot(Duration::from_millis(60)),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(Outcome::Unsupported)));
        fixture.device.current().unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.snapshots, 1);
        assert!(state.calls.is_empty());
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_probe_wait_rejects_replaced_usb_and_restarted_service() {
    run(async {
        for replace_usb in [true, false] {
            let bus = Bus::new();
            let fixture = Fixture::new();
            let state = Arc::new(Mutex::new(State::default()));
            let service = server(&bus, state.clone()).await;
            let mut host = host(&bus, &fixture).await;
            let future = eject::execute(&mut host);
            tokio::pin!(future);
            assert!(
                tokio::time::timeout(Duration::from_millis(80), &mut future)
                    .await
                    .is_err()
            );
            let replacement = if replace_usb {
                std::fs::write(fixture.temp.path().join("vhci/status"),
                    "hub port sta spd dev sockfd local_busid\nhs 0000 006 003 00010003 999999 3-1\n").unwrap();
                state.lock().unwrap().objects = objects();
                None
            } else {
                service.close().await.unwrap();
                state.lock().unwrap().objects = objects();
                Some(server(&bus, state.clone()).await)
            };
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), future)
                    .await
                    .unwrap(),
                Outcome::Unavailable
            );
            assert!(state.lock().unwrap().calls.is_empty());
            drop(replacement);
        }
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_disappearing_objects_after_plan_never_restart_discovery_or_repeat_unmount() {
    run(async {
        for hide_at in [2, 3] {
            let bus = Bus::new();
            let fixture = Fixture::new();
            let state = Arc::new(Mutex::new(State {
                objects: objects(),
                hide_at_snapshot: Some(hide_at),
                ..State::default()
            }));
            let _service = server(&bus, state.clone()).await;
            let mut host = host(&bus, &fixture).await;
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), eject::execute(&mut host))
                    .await
                    .unwrap(),
                Outcome::Unavailable
            );
            let state = state.lock().unwrap();
            assert_eq!(state.snapshots, hide_at);
            assert_eq!(
                state.calls,
                if hide_at == 2 {
                    vec![]
                } else {
                    vec!["unmount"]
                }
            );
        }
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_delayed_return_keeps_future_and_handles_os_disappearance() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::new();
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            power_delay: 250,
            remove: Some(fixture.temp.path().join("vhci/status")),
            ..State::default()
        }));
        let _server = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        let future = eject::execute(&mut host);
        tokio::pin!(future);
        assert!(
            tokio::time::timeout(Duration::from_millis(80), &mut future)
                .await
                .is_err()
        );
        assert_eq!(future.await, Outcome::Safe);
        assert_eq!(state.lock().unwrap().calls, ["unmount", "poweroff"]);
        assert_eq!(fixture.device.current(), Err(Outcome::Unavailable));
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_restarted_service_cannot_receive_old_import_operation() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::new();
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            ..State::default()
        }));
        let first = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        host.snapshot().await.unwrap();
        first.close().await.unwrap();
        let _new = server(&bus, state.clone()).await;
        assert_eq!(eject::execute(&mut host).await, Outcome::Unavailable);
        assert!(state.lock().unwrap().calls.is_empty());
    });
}

#[test]
fn malformed_or_foreign_metadata_cannot_become_an_owned_plan() {
    for kind in 0..5 {
        let mut objects = objects();
        match kind {
            0 => {
                objects
                    .get_mut(&ObjectPath::try_from(DISK).unwrap())
                    .unwrap()
                    .get_mut(BLOCK)
                    .unwrap()
                    .remove("DeviceNumber");
            }
            1 => {
                objects
                    .get_mut(&ObjectPath::try_from(DISK).unwrap())
                    .unwrap()
                    .get_mut(BLOCK)
                    .unwrap()
                    .insert("Drive".into(), string_value(DISK_DRIVE));
            }
            2 => {
                objects
                    .get_mut(&ObjectPath::try_from(DISK_DRIVE).unwrap())
                    .unwrap()
                    .get_mut(DRIVE)
                    .unwrap()
                    .insert("CanPowerOff".into(), 1u32.into());
            }
            3 => {
                objects
                    .get_mut(&ObjectPath::try_from(DISK).unwrap())
                    .unwrap()
                    .get_mut(FILESYSTEM)
                    .unwrap()
                    .insert(
                        "MountPoints".into(),
                        Variant::from(Vec::<String>::new()).try_into().unwrap(),
                    );
            }
            4 => {
                let value = objects
                    .remove(&ObjectPath::try_from(DISK).unwrap())
                    .unwrap();
                objects.insert("/foreign/device".try_into().unwrap(), value);
            }
            _ => unreachable!(),
        }
        assert!(parse(objects, |_| Ok(true)).is_err(), "case {kind}");
    }
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_missing_service_requests_activation_without_inventing_an_owner() {
    run(async {
        let bus = Bus::new();
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .build()
            .await
            .unwrap();
        // This isolated session bus has no UDisks activatable service file.
        // ServiceUnknown proves StartServiceByName was attempted; simply asking
        // GetNameOwner would instead return NameHasNoOwner.
        let error = service_owner(&connection).await.unwrap_err();
        assert!(
            matches!(error, zbus::Error::FDO(ref error)
            if matches!(**error, zbus::fdo::Error::ServiceUnknown(_))),
            "{error:?}"
        );
        assert_eq!(classify(&error, false), Outcome::Unavailable);
    });
}

// The parent is visible first, while both child block devices already exist
// in the kernel. This reproduces the actual delayed two-volume QEMU failure.
fn partitioned_objects() -> ManagedObjects {
    let mut all = objects();
    let path = ObjectPath::try_from(DISK).unwrap();
    let child = all[&path].clone();
    let parent = all.get_mut(&path).unwrap();
    parent.remove(FILESYSTEM);
    parent
        .get_mut(BLOCK)
        .unwrap()
        .insert("IdUsage".into(), string_value(""));
    for (name, minor) in [(CHILD1, 1), (CHILD2, 2)] {
        let mut child = child.clone();
        child
            .get_mut(BLOCK)
            .unwrap()
            .insert("DeviceNumber".into(), rustix::fs::makedev(8, minor).into());
        all.insert(ObjectPath::try_from(name).unwrap().into(), child);
    }
    all
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_parent_before_partitions_waits_for_every_kernel_child_and_filesystem() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::with_blocks(3);
        let all = partitioned_objects();
        let mut parent = all.clone();
        parent.remove(&ObjectPath::try_from(CHILD1).unwrap());
        parent.remove(&ObjectPath::try_from(CHILD2).unwrap());
        let state = Arc::new(Mutex::new(State {
            objects: parent,
            ..State::default()
        }));
        let _service = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        let operation = eject::execute(&mut host);
        tokio::pin!(operation);
        for stage in 0..3 {
            assert!(
                tokio::time::timeout(Duration::from_millis(180), &mut operation)
                    .await
                    .is_err()
            );
            let mut s = state.lock().unwrap();
            assert!(
                s.calls.is_empty(),
                "no mutation at incomplete stage {stage}"
            );
            if stage == 0 {
                s.objects.insert(
                    CHILD1.try_into().unwrap(),
                    all[&ObjectPath::try_from(CHILD1).unwrap()].clone(),
                );
            } else {
                s.objects = all.clone();
                if stage == 1 {
                    s.objects
                        .get_mut(&ObjectPath::try_from(CHILD2).unwrap())
                        .unwrap()
                        .remove(FILESYSTEM);
                }
            }
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), operation)
                .await
                .unwrap(),
            Outcome::Safe
        );
        assert_eq!(
            state.lock().unwrap().calls,
            ["unmount", "unmount", "poweroff"]
        );
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_missing_partition_deadline_is_manual_without_mutation() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::with_blocks(3);
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            ..State::default()
        }));
        let _service = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        assert!(matches!(
            host.wait_for_owned_snapshot(Duration::from_millis(120))
                .await,
            Err(Outcome::Unsupported)
        ));
        fixture.device.current().unwrap();
        assert_eq!(
            host.pins.len(),
            3,
            "kernel children are pinned before D-Bus sees them"
        );
        assert!(state.lock().unwrap().calls.is_empty());
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_kernel_child_changed_after_plan_is_not_waited_for_or_powered_off() {
    run(async {
        for replace in [false, true] {
            let bus = Bus::new();
            let fixture = Fixture::with_blocks(3);
            let state = Arc::new(Mutex::new(State {
                objects: partitioned_objects(),
                ..State::default()
            }));
            let _service = server(&bus, state.clone()).await;
            let mut host = host(&bus, &fixture).await;
            host.initial_snapshot().await.unwrap();
            // Plain fixture directories only, never real devices/mounts.
            let block = fixture.temp.path().join("blocks/8:2");
            let path = block.canonicalize().unwrap();
            if replace {
                std::fs::rename(&path, path.with_file_name("old-child")).unwrap();
                std::fs::create_dir_all(path.join("holders")).unwrap();
            } else {
                std::fs::remove_file(&block).unwrap();
            }
            assert_eq!(host.power_off(DISK_DRIVE).await, Err(Outcome::Unavailable));
            assert!(state.lock().unwrap().calls.is_empty());
        }
    });
}

#[test]
#[ignore = "requires an isolated dbus-daemon executable; run explicitly"]
fn real_dbus_new_kernel_child_between_snapshot_and_mutation_prevents_unmount() {
    run(async {
        let bus = Bus::new();
        let fixture = Fixture::new();
        let state = Arc::new(Mutex::new(State {
            objects: objects(),
            ..State::default()
        }));
        let _service = server(&bus, state.clone()).await;
        let mut host = host(&bus, &fixture).await;
        host.initial_snapshot().await.unwrap();
        let path = fixture.device.pin.path.join("3-1:1.0/block/new-child");
        std::fs::create_dir_all(path.join("holders")).unwrap();
        std::os::unix::fs::symlink(path, fixture.temp.path().join("blocks/8:1")).unwrap();
        assert_eq!(host.unmount(DISK).await, Err(Outcome::Unavailable));
        assert!(state.lock().unwrap().calls.is_empty());
    });
}
