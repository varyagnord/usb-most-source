//! Desired-state first-use preparation. Shell adapters cannot invent task types.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Linux,
    Windows,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86_64,
    Aarch64,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModuleAvailability {
    Available,
    Unavailable,
    Unknown,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinuxPrerequisites {
    pub systemd: bool,
    pub module: ModuleAvailability,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub schema: u32,
    pub nonce: String,
    pub os: Os,
    pub arch: Arch,
    pub elevated: bool,
    pub usb_stack: bool,
    pub agent_version: Option<String>,
    #[serde(default)]
    pub agent_sha256: Option<String>,
    pub service_running: bool,
    pub reboot_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_prerequisites: Option<LinuxPrerequisites>,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Stage,
    Verify,
    InstallStack,
    InstallAgent,
    Configure,
    Health,
    Commit,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub needs_elevation: bool,
}
impl Probe {
    /// Advisory preflight only. The privileged installer still validates the
    /// real stack independently; an SSH user's answer never authorizes it.
    pub fn check_prerequisites(&self) -> Result<(), &'static str> {
        if self.os == Os::Linux
            && let Some(linux) = &self.linux_prerequisites
        {
            if !linux.systemd {
                return Err("linux_systemd_required");
            }
            if !self.usb_stack && linux.module == ModuleAvailability::Unavailable {
                return Err("linux_usb_driver_unavailable");
            }
        }
        Ok(())
    }
    pub fn parse(bytes: &[u8], expected_nonce: &str) -> Result<Self, &'static str> {
        if bytes.len() > 16384 || expected_nonce.len() != 32 {
            return Err("invalid_probe");
        }
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        let p: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_probe")?;
        if p.schema != 1
            || p.nonce != expected_nonce
            || !p.nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || (p.os == Os::Windows && p.linux_prerequisites.is_some())
        {
            return Err("invalid_probe");
        }
        if p.agent_version.as_ref().is_some_and(|v| {
            v.is_empty()
                || v.len() > 32
                || v.bytes()
                    .any(|b| !b.is_ascii_alphanumeric() && b != b'.' && b != b'-')
        }) {
            return Err("invalid_probe");
        }
        if p.agent_sha256.as_ref().is_some_and(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err("invalid_probe");
        }
        Ok(p)
    }
    pub fn plan(&self, version: &str) -> Result<Plan, &'static str> {
        self.check_prerequisites()?;
        if version.is_empty()
            || version.len() > 32
            || version
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && b != b'.' && b != b'-')
        {
            return Err("invalid_version");
        }
        if self.usb_stack && self.agent_version.as_deref() == Some(version) && self.service_running
        {
            return Ok(Plan {
                steps: vec![Step::Health],
                needs_elevation: false,
            });
        }
        // An unrelated pending OS reboot must not disable an already usable
        // driver. A stack needing replacement cannot be verified until reboot.
        if self.reboot_pending && !self.usb_stack {
            return Err("reboot_required");
        }
        let mut steps = vec![Step::Stage, Step::Verify];
        if !self.usb_stack {
            steps.push(Step::InstallStack);
        }
        if self.agent_version.as_deref() != Some(version) {
            steps.push(Step::InstallAgent);
        }
        steps.extend([Step::Configure, Step::Health, Step::Commit]);
        Ok(Plan {
            steps,
            needs_elevation: !self.elevated,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Planned,
    Applying,
    Committed,
    RollbackRequired,
    RolledBack,
}
/// Successful health is required before commit; failures retain rollback state.
pub struct Transaction {
    plan: Plan,
    next: usize,
    state: State,
    changed: bool,
}
impl Transaction {
    pub fn new(plan: Plan) -> Result<Self, &'static str> {
        let s = plan.steps.as_slice();
        let standard = s.starts_with(&[Step::Stage, Step::Verify])
            && s.ends_with(&[Step::Configure, Step::Health, Step::Commit]);
        if s != [Step::Health]
            && (!standard
                || !matches!(
                    s.get(2..s.len().saturating_sub(3)),
                    Some(
                        [] | [Step::InstallStack]
                            | [Step::InstallAgent]
                            | [Step::InstallStack, Step::InstallAgent]
                    )
                ))
        {
            return Err("invalid_plan");
        }
        Ok(Self {
            plan,
            next: 0,
            state: State::Planned,
            changed: false,
        })
    }
    pub fn cancel(&mut self) {
        if matches!(self.state, State::Planned | State::Applying) {
            self.state = if self.changed {
                State::RollbackRequired
            } else {
                State::RolledBack
            };
        }
    }
    pub fn next(&self) -> Option<Step> {
        if matches!(
            self.state,
            State::Committed | State::RollbackRequired | State::RolledBack
        ) {
            None
        } else {
            self.plan.steps.get(self.next).copied()
        }
    }
    pub fn complete(&mut self, step: Step, success: bool) -> Result<(), &'static str> {
        if self.next() != Some(step) {
            return Err("unexpected_step");
        }
        self.state = State::Applying;
        // A failed mutating operation may already have changed persistent state.
        self.changed |= matches!(
            step,
            Step::InstallStack | Step::InstallAgent | Step::Configure
        );
        if !success {
            self.state = if self.changed {
                State::RollbackRequired
            } else {
                State::RolledBack
            };
            return Ok(());
        }
        self.next += 1;
        if self.next == self.plan.steps.len() {
            self.state = State::Committed;
        }
        Ok(())
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn rollback_complete(&mut self, success: bool) -> Result<(), &'static str> {
        if self.state != State::RollbackRequired {
            return Err("unexpected_rollback");
        }
        if success {
            self.state = State::RolledBack;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Probe {
        Probe {
            schema: 1,
            nonce: "0123456789abcdef0123456789abcdef".into(),
            os: Os::Linux,
            arch: Arch::X86_64,
            elevated: false,
            usb_stack: false,
            agent_version: None,
            agent_sha256: None,
            service_running: false,
            reboot_pending: false,
            linux_prerequisites: None,
        }
    }
    #[test]
    fn preflight_distinguishes_unknown_from_unavailable_and_loaded_stack() {
        let mut probe = fixture();
        assert_eq!(probe.check_prerequisites(), Ok(()));
        probe.linux_prerequisites = Some(LinuxPrerequisites {
            systemd: true,
            module: ModuleAvailability::Unknown,
        });
        assert_eq!(probe.check_prerequisites(), Ok(()));
        probe.linux_prerequisites.as_mut().unwrap().module = ModuleAvailability::Unavailable;
        assert_eq!(
            probe.check_prerequisites(),
            Err("linux_usb_driver_unavailable")
        );
        assert_eq!(probe.plan("1.0.11"), Err("linux_usb_driver_unavailable"));
        probe.usb_stack = true;
        assert_eq!(probe.check_prerequisites(), Ok(()));
        probe.linux_prerequisites.as_mut().unwrap().systemd = false;
        assert_eq!(probe.check_prerequisites(), Err("linux_systemd_required"));
    }
    #[test]
    fn optional_linux_probe_fields_are_strict_and_old_probes_remain_valid() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        let nonce = fixture().nonce;
        assert!(Probe::parse(&serde_json::to_vec(&value).unwrap(), &nonce).is_ok());
        value["linux_prerequisites"] = serde_json::json!({"systemd":true,"module":"unavailable"});
        let parsed = Probe::parse(&serde_json::to_vec(&value).unwrap(), &nonce).unwrap();
        assert_eq!(
            parsed.check_prerequisites(),
            Err("linux_usb_driver_unavailable")
        );
        value["linux_prerequisites"]["module"] = serde_json::json!("made_up");
        assert!(Probe::parse(&serde_json::to_vec(&value).unwrap(), &nonce).is_err());
        value["linux_prerequisites"]["module"] = serde_json::json!("available");
        value["os"] = serde_json::json!("windows");
        assert!(Probe::parse(&serde_json::to_vec(&value).unwrap(), &nonce).is_err());
        value.as_object_mut().unwrap().remove("linux_prerequisites");
        assert!(Probe::parse(&serde_json::to_vec(&value).unwrap(), &nonce).is_ok());
    }
    #[test]
    fn current_installation_is_checked_not_reinstalled() {
        let mut p = fixture();
        p.usb_stack = true;
        p.agent_version = Some("1.0.11".into());
        p.service_running = true;
        assert_eq!(
            p.plan("1.0.11").unwrap(),
            Plan {
                steps: vec![Step::Health],
                needs_elevation: false
            }
        );
        p.service_running = false;
        assert!(
            !p.plan("1.0.11")
                .unwrap()
                .steps
                .contains(&Step::InstallAgent)
        );
        p.reboot_pending = true;
        assert!(p.plan("1.0.11").is_ok());
        p.usb_stack = false;
        assert_eq!(p.plan("1.0.11"), Err("reboot_required"));
        assert_eq!(p.plan(""), Err("invalid_version"));
    }
    #[test]
    fn all_failures_before_and_after_mutation_preserve_rollback_contract() {
        let p = fixture().plan("1.0.11").unwrap();
        assert!(p.needs_elevation);
        for failure in 0..p.steps.len() {
            let mut t = Transaction::new(p.clone()).unwrap();
            assert_eq!(t.complete(Step::Commit, true), Err("unexpected_step"));
            for (index, step) in p.steps.iter().enumerate().take(failure + 1) {
                t.complete(*step, index != failure).unwrap();
            }
            assert_eq!(
                t.state(),
                if failure < 2 {
                    State::RolledBack
                } else {
                    State::RollbackRequired
                }
            );
            assert!(t.next().is_none());
            if failure >= 2 {
                t.rollback_complete(false).unwrap();
                assert_eq!(t.state(), State::RollbackRequired);
                t.rollback_complete(true).unwrap();
            }
            assert_eq!(t.state(), State::RolledBack);
        }
        let mut t = Transaction::new(p.clone()).unwrap();
        for step in p.steps {
            t.complete(step, true).unwrap();
        }
        assert_eq!(t.state(), State::Committed);
        assert!(t.next().is_none());
    }
    #[test]
    fn invalid_and_cancelled_plans_never_commit() {
        for steps in [
            vec![],
            vec![Step::Commit],
            vec![Step::Stage, Step::Verify, Step::Commit],
            vec![Step::Stage, Step::Verify, Step::Configure, Step::Commit],
        ] {
            assert!(
                Transaction::new(Plan {
                    steps,
                    needs_elevation: false
                })
                .is_err()
            );
        }
        let p = fixture().plan("1.0.11").unwrap();
        for n in 0..p.steps.len() {
            let mut t = Transaction::new(p.clone()).unwrap();
            for step in p.steps.iter().take(n) {
                t.complete(*step, true).unwrap();
            }
            t.cancel();
            assert_eq!(
                t.state(),
                if n < 3 {
                    State::RolledBack
                } else {
                    State::RollbackRequired
                }
            );
            assert!(t.next().is_none());
        }
    }
    #[test]
    fn untrusted_probe_cannot_select_apple_architecture_or_replay_response() {
        let p = fixture();
        let bytes = serde_json::to_vec(&p).unwrap();
        assert_eq!(Probe::parse(&bytes, &p.nonce).unwrap(), p);
        for bad in [
            String::from_utf8(bytes.clone())
                .unwrap()
                .replace("linux", "macos"),
            String::from_utf8(bytes.clone())
                .unwrap()
                .replace("x86_64", "x86"),
            String::from_utf8(bytes.clone())
                .unwrap()
                .replace("\"schema\":1", "\"schema\":2"),
        ] {
            assert!(Probe::parse(bad.as_bytes(), &p.nonce).is_err());
        }
        assert!(Probe::parse(&bytes, "00000000000000000000000000000000").is_err());
        assert!(Probe::parse(&vec![b' '; 16385], &p.nonce).is_err());
        let mut hashed = p.clone();
        hashed.agent_sha256 = Some("a".repeat(64));
        assert_eq!(
            Probe::parse(&serde_json::to_vec(&hashed).unwrap(), &p.nonce).unwrap(),
            hashed
        );
        for bad in [
            "A".repeat(64),
            "a".repeat(63),
            "g".repeat(64),
            "\n".repeat(64),
        ] {
            hashed.agent_sha256 = Some(bad);
            assert!(Probe::parse(&serde_json::to_vec(&hashed).unwrap(), &p.nonce).is_err());
        }
        // Previously installed receivers did not report a hash; they must
        // remain upgradeable through the fully verified artifact path.
        let mut older = serde_json::to_value(&p).unwrap();
        older.as_object_mut().unwrap().remove("agent_sha256");
        assert_eq!(
            Probe::parse(&serde_json::to_vec(&older).unwrap(), &p.nonce).unwrap(),
            p
        );
    }
}
