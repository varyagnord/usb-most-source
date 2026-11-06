use super::*;
use std::os::unix::fs::PermissionsExt;
#[derive(Clone)]
struct Host {
    state: ServiceState,
    fail_health: bool,
}

#[test]
fn unchanged_files_still_repair_drifted_external_configuration() {
    struct Drift(Host, bool, usize);
    impl Services for Drift {
        fn state(&mut self) -> io::Result<ServiceState> {
            self.0.state()
        }
        fn configuration_current(&mut self) -> io::Result<bool> {
            Ok(self.1)
        }
        fn quiesce(&mut self, state: ServiceState) -> io::Result<()> {
            self.0.quiesce(state)
        }
        fn activate(&mut self) -> io::Result<()> {
            self.2 += 1;
            self.1 = true;
            self.0.activate()
        }
        fn health(&mut self) -> io::Result<()> {
            self.0.health()
        }
        fn restore(&mut self, state: ServiceState) -> io::Result<()> {
            self.0.restore(state)
        }
    }
    let f = Fixture::new(false);
    let mut host = Drift(f.host(), true, 0);
    let run = |host: &mut Drift| {
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            host,
            &mut || Ok(()),
        )
        .unwrap();
    };
    run(&mut host);
    assert_eq!(host.2, 1);
    run(&mut host);
    assert_eq!(host.2, 1, "healthy no-op must not reactivate");
    host.1 = false;
    run(&mut host);
    assert_eq!(
        host.2, 2,
        "external drift must use the recoverable install path"
    );
    assert!(host.1);
}
impl Services for Host {
    fn state(&mut self) -> io::Result<ServiceState> {
        Ok(self.state)
    }
    fn quiesce(&mut self, previous: ServiceState) -> io::Result<()> {
        self.state.active = false;
        if !previous.enabled {
            self.state.enabled = false;
        }
        Ok(())
    }
    fn activate(&mut self) -> io::Result<()> {
        self.state = ServiceState {
            windows_start: None,
            windows_sid_type: None,
            windows_firewall_port: None,
            windows_discovery: false,
            active: true,
            enabled: true,
        };
        Ok(())
    }
    fn health(&mut self) -> io::Result<()> {
        if self.fail_health {
            Err(io::Error::other("fixture_health"))
        } else {
            Ok(())
        }
    }
    fn restore(&mut self, previous: ServiceState) -> io::Result<()> {
        self.state = previous;
        Ok(())
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    targets: Vec<Target>,
    activation: PathBuf,
    activation_hash: String,
    old: ServiceState,
}
impl Fixture {
    fn new(existing: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let targets = (0..4)
            .map(|i| Target {
                path: dir.path().join(format!("target{i}")),
                data: Zeroizing::new(format!("new{i}").into_bytes()),
                mode: if i == 0 { 0o755 } else { 0o600 },
                maximum: 4096,
            })
            .collect::<Vec<_>>();
        let activation = dir.path().join("active");
        let activation_hash = hash(&targets[1].data);
        if existing {
            for (i, t) in targets.iter().enumerate() {
                fs::write(&t.path, format!("old{i}")).unwrap();
                fs::set_permissions(&t.path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            fs::write(&activation, b"old-activation").unwrap();
        }
        Self {
            dir,
            targets,
            activation,
            activation_hash,
            old: ServiceState {
                windows_start: None,
                windows_sid_type: None,
                windows_firewall_port: None,
                windows_discovery: false,
                active: existing,
                enabled: existing,
            },
        }
    }
    fn host(&self) -> Host {
        Host {
            state: self.old,
            fail_health: false,
        }
    }
    fn assert_old(&self, existing: bool) {
        for (i, t) in self.targets.iter().enumerate() {
            if existing {
                assert_eq!(fs::read(&t.path).unwrap(), format!("old{i}").as_bytes());
            } else {
                assert!(!t.path.exists());
            }
        }
        if existing {
            assert_eq!(fs::read(&self.activation).unwrap(), b"old-activation");
        } else {
            assert!(!self.activation.exists());
        }
    }
    fn assert_new(&self) {
        for t in &self.targets {
            assert_eq!(fs::read(&t.path).unwrap(), t.data.as_slice());
        }
        assert_eq!(
            fs::read(&self.activation).unwrap(),
            hex32(&self.activation_hash).unwrap()
        );
    }
}
#[test]
fn fault_at_every_boundary_rolls_back_or_finishes_durable_commit() {
    for existing in [false, true] {
        for fail in 0..17 {
            let f = Fixture::new(existing);
            let mut host = f.host();
            let mut calls = 0;
            let result = install(
                f.dir.path(),
                &f.targets,
                &f.activation,
                &f.activation_hash,
                &mut host,
                &mut || {
                    let this = calls;
                    calls += 1;
                    if this == fail {
                        Err(io::Error::other("fixture_fault"))
                    } else {
                        Ok(())
                    }
                },
            );
            // 5 backups + quiesce gate + stop +4 file replacements + activation,
            // health, durable commit, activation marker, cleanup.
            if fail < 14 {
                assert!(result.is_err(), "failure {fail}");
                f.assert_old(existing);
                assert_eq!(host.state, f.old);
            } else {
                assert!(result.is_ok());
                f.assert_new();
                assert!(host.state.active);
            }
            assert!(!f.dir.path().join("install-journal.json").exists());
        }
    }
}
#[test]
fn health_failure_restores_files_activation_and_service() {
    for existing in [false, true] {
        let f = Fixture::new(existing);
        let mut host = f.host();
        host.fail_health = true;
        assert!(
            install(
                f.dir.path(),
                &f.targets,
                &f.activation,
                &f.activation_hash,
                &mut host,
                &mut || Ok(())
            )
            .is_err()
        );
        f.assert_old(existing);
        assert_eq!(host.state, f.old);
    }
}
#[test]
fn process_death_is_recovered_without_replaying_install_commands() {
    for die in 0..16 {
        let f = Fixture::new(true);
        let mut host = f.host();
        let mut calls = 0;
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            install(
                f.dir.path(),
                &f.targets,
                &f.activation,
                &f.activation_hash,
                &mut host,
                &mut || {
                    let this = calls;
                    calls += 1;
                    if this == die {
                        panic!("fixture process death");
                    }
                    Ok(())
                },
            )
        }));
        assert!(panic.is_err());
        recover(f.dir.path(), &f.targets, &f.activation, &mut host).unwrap();
        if die < 14 {
            f.assert_old(true);
            assert_eq!(host.state, f.old);
        } else {
            f.assert_new();
            assert!(host.state.active);
        }
    }
}
#[test]
fn symlink_unknown_edit_and_lock_contention_fail_closed() {
    let f = Fixture::new(false);
    std::os::unix::fs::symlink("/etc/passwd", &f.targets[0].path).unwrap();
    let mut host = f.host();
    assert!(
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || Ok(())
        )
        .is_err()
    );
    assert_eq!(host.state, f.old);
    let f = Fixture::new(true);
    let mut host = f.host();
    let mut calls = 0;
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || {
                calls += 1;
                if calls == 10 {
                    panic!("fixture");
                }
                Ok(())
            },
        )
    }));
    fs::write(&f.targets[0].path, b"administrator-edit").unwrap();
    assert!(recover(f.dir.path(), &f.targets, &f.activation, &mut host).is_err());
    assert_eq!(fs::read(&f.targets[0].path).unwrap(), b"administrator-edit");
    assert!(f.dir.path().join("install-journal.json").is_file());
    assert!(hex32(&"€".repeat(21)).is_err());
}

#[test]
fn process_lock_rejects_concurrent_installer_and_modes_roll_back_too() {
    let mut f = Fixture::new(true);
    let mut host = f.host();
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(f.dir.path().join("install.lock"))
        .unwrap();
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .unwrap();
    lock.lock().unwrap();
    assert_eq!(
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!f.dir.path().join("install-journal.json").exists());
    f.assert_old(true);
    drop(lock);
    f.targets[0].data = Zeroizing::new(b"old0".to_vec());
    host.fail_health = true;
    assert!(
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || Ok(())
        )
        .is_err()
    );
    f.assert_old(true);
    assert_eq!(
        fs::metadata(&f.targets[0].path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn permissions_only_failure_restores_modes_and_noop_preserves_service() {
    let mut f = Fixture::new(true);
    let mut host = f.host();
    fs::remove_file(&f.activation).unwrap();
    for (i, target) in f.targets.iter_mut().enumerate() {
        target.data = Zeroizing::new(format!("old{i}").into_bytes());
    }
    host.fail_health = true;
    assert!(
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || Ok(())
        )
        .is_err()
    );
    assert_eq!(
        fs::metadata(&f.targets[0].path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    host.fail_health = false;
    install(
        f.dir.path(),
        &f.targets,
        &f.activation,
        &f.activation_hash,
        &mut host,
        &mut || Ok(()),
    )
    .unwrap();
    let before = fs::metadata(&f.targets[0].path)
        .unwrap()
        .modified()
        .unwrap();
    install(
        f.dir.path(),
        &f.targets,
        &f.activation,
        &f.activation_hash,
        &mut host,
        &mut || Err(io::Error::other("no writes allowed")),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(&f.targets[0].path)
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    // A new session changes only the configuration. Replacing the multi-MB
    // executable (and its inode) again is unnecessary and hurts slow links.
    use std::os::unix::fs::MetadataExt;
    let unchanged: Vec<_> = [0, 2, 3]
        .map(|i| fs::metadata(&f.targets[i].path).unwrap())
        .into_iter()
        .collect();
    f.targets[1].data = Zeroizing::new(b"new-session-config".to_vec());
    f.activation_hash = hash(&f.targets[1].data);
    install(
        f.dir.path(),
        &f.targets,
        &f.activation,
        &f.activation_hash,
        &mut host,
        &mut || Ok(()),
    )
    .unwrap();
    for (index, previous) in [0, 2, 3].into_iter().zip(unchanged) {
        let current = fs::metadata(&f.targets[index].path).unwrap();
        assert_eq!(current.ino(), previous.ino());
        assert_eq!(current.modified().unwrap(), previous.modified().unwrap());
    }
}
#[test]
fn busy_foreign_service_is_not_stopped_by_rollback() {
    struct Busy {
        attempts: usize,
    }
    impl Services for Busy {
        fn state(&mut self) -> io::Result<ServiceState> {
            Ok(ServiceState {
                windows_start: None,
                windows_sid_type: None,
                windows_firewall_port: None,
                windows_discovery: false,
                active: true,
                enabled: true,
            })
        }
        fn quiesce(&mut self, _: ServiceState) -> io::Result<()> {
            self.attempts += 1;
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn activate(&mut self) -> io::Result<()> {
            panic!("must not activate")
        }
        fn health(&mut self) -> io::Result<()> {
            panic!("must not health")
        }
        fn restore(&mut self, _: ServiceState) -> io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new(true);
    let mut host = Busy { attempts: 0 };
    assert_eq!(
        install(
            f.dir.path(),
            &f.targets,
            &f.activation,
            &f.activation_hash,
            &mut host,
            &mut || Ok(())
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(host.attempts, 1);
    f.assert_old(true);
    assert!(!f.dir.path().join("install-journal.json").exists());
}

#[test]
fn legacy_service_journal_preserves_unknown_sid_and_new_state_records_exact_identity() {
    let legacy = br#"{"active":true,"enabled":true,"windows_start":2,"windows_firewall_port":3241,"windows_discovery":true}"#;
    let mut state: ServiceState = serde_json::from_slice(legacy).unwrap();
    assert_eq!(state.windows_sid_type, None);
    for identity in [0, 1] {
        state.windows_sid_type = Some(identity);
        let encoded = serde_json::to_vec(&state).unwrap();
        assert_eq!(
            serde_json::from_slice::<ServiceState>(&encoded).unwrap(),
            state
        );
    }
}
