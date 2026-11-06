//! Actual Windows file/DACL/lock boundaries with a simulated SCM. Real SCM
//! lifecycle and deployment health/rollback are exercised separately by CI.
use super::*;
use crate::deployment::{self, ServiceState, Services, Target};
use std::panic::{AssertUnwindSafe, catch_unwind};
struct Host {
    state: ServiceState,
    fail_health: bool,
}
impl Services for Host {
    fn state(&mut self) -> io::Result<ServiceState> {
        Ok(self.state)
    }
    fn quiesce(&mut self, _: ServiceState) -> io::Result<()> {
        self.state.active = false;
        Ok(())
    }
    fn activate(&mut self) -> io::Result<()> {
        self.state = ServiceState {
            active: true,
            enabled: true,
            windows_start: Some(2),
            windows_sid_type: Some(1),
            windows_firewall_port: Some(3241),
            windows_discovery: true,
        };
        Ok(())
    }
    fn health(&mut self) -> io::Result<()> {
        if self.fail_health {
            Err(invalid())
        } else {
            Ok(())
        }
    }
    fn restore(&mut self, state: ServiceState) -> io::Result<()> {
        self.state = state;
        Ok(())
    }
}
struct Fixture {
    _parent: tempfile::TempDir,
    root: std::path::PathBuf,
    targets: Vec<Target>,
    before: ServiceState,
    host: Host,
    hash: String,
}
impl Fixture {
    fn new(existing: bool) -> Self {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("protected");
        os::create_private_directory(&root).unwrap();
        let targets = (0..4)
            .map(|i| Target {
                path: root.join(format!("target{i}")),
                data: Zeroizing::new(format!("new{i}").into_bytes()),
                mode: 0o600,
                maximum: 4096,
            })
            .collect::<Vec<_>>();
        if existing {
            for (i, target) in targets.iter().enumerate() {
                atomic(&target.path, format!("old{i}").as_bytes(), 0o600).unwrap();
            }
            atomic(&root.join("active"), b"old-activation", 0o600).unwrap();
        }
        let before = ServiceState {
            active: existing,
            enabled: false,
            windows_start: existing.then_some(3),
            windows_sid_type: existing.then_some(0),
            windows_firewall_port: existing.then_some(4241),
            windows_discovery: existing,
        };
        Self {
            _parent: parent,
            root,
            hash: deployment::hash(&targets[1].data),
            targets,
            before,
            host: Host {
                state: before,
                fail_health: false,
            },
        }
    }
    fn install(&mut self, fault: &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> {
        deployment::install(
            &self.root,
            &self.targets,
            &self.root.join("active"),
            &self.hash,
            &mut self.host,
            fault,
        )
    }
    fn check(&self, committed: bool, existing: bool) {
        for (i, target) in self.targets.iter().enumerate() {
            let data = read(&target.path, 4096).unwrap();
            if committed {
                assert_eq!(data.unwrap().0.as_slice(), target.data.as_slice());
            } else if existing {
                assert_eq!(data.unwrap().0.as_slice(), format!("old{i}").as_bytes());
            } else {
                assert!(data.is_none());
            }
        }
        let marker = read(&self.root.join("active"), 32).unwrap();
        if committed {
            assert_eq!(
                marker.unwrap().0.as_slice(),
                deployment::hex32(&self.hash).unwrap()
            );
            assert!(self.host.state.active);
        } else {
            if existing {
                assert_eq!(marker.unwrap().0.as_slice(), b"old-activation");
            } else {
                assert!(marker.is_none());
            }
            assert_eq!(self.host.state, self.before);
        }
        assert!(!self.root.join("install-journal.json").exists());
    }
}
#[test]
fn windows_file_faults_and_process_death_preserve_transaction_boundaries() {
    for existing in [false, true] {
        for fail in 0..17 {
            for crash in [false, true] {
                let mut f = Fixture::new(existing);
                let mut calls = 0;
                let result = catch_unwind(AssertUnwindSafe(|| {
                    f.install(&mut || {
                        let this = calls;
                        calls += 1;
                        if this == fail {
                            if crash {
                                panic!("simulated process death");
                            }
                            return Err(invalid());
                        }
                        Ok(())
                    })
                }));
                if crash && fail < 16 {
                    assert!(result.is_err());
                    deployment::recover(&f.root, &f.targets, &f.root.join("active"), &mut f.host)
                        .unwrap();
                } else {
                    assert_eq!(result.unwrap().is_ok(), fail >= 14);
                }
                f.check(fail >= 14, existing);
            }
        }
    }
}
#[test]
fn windows_health_failure_and_concurrent_install_fail_without_losing_old_state() {
    let mut f = Fixture::new(true);
    let lock = installation_lock(&f.root).unwrap();
    lock.try_lock().unwrap();
    assert_eq!(
        f.install(&mut || Ok(())).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(lock);
    f.host.fail_health = true;
    assert!(f.install(&mut || Ok(())).is_err());
    f.check(false, true);
    f.host.fail_health = false;
    f.install(&mut || Ok(())).unwrap();
    let modified = fs::metadata(&f.targets[0].path)
        .unwrap()
        .modified()
        .unwrap();
    f.install(&mut || panic!("no-op must not mutate")).unwrap();
    assert_eq!(
        modified,
        fs::metadata(&f.targets[0].path)
            .unwrap()
            .modified()
            .unwrap()
    );
    f.check(true, true);
}
