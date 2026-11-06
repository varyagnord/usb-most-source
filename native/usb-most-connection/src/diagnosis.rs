//! Read-only explanation of bounded, current observations. No credentials,
//! addresses, device identifiers, logs or USB payloads enter the report.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    None,
    Legacy,
    Ssh,
    Direct,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Connecting,
    Trust,
    Checking,
    Preparing,
    Connected,
    Reconnecting,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    HostKeyRejected,
    AuthenticationFailed,
    InvalidKey,
    ForwardingDenied,
    PreparationFailed,
    MissingReceiver,
    InvalidReceiver,
    RestartRequired,
    WindowsAdministratorRequired,
    LinuxUsbDriverUnavailable,
    LinuxSystemdRequired,
    ComputerBusy,
    PairingRequired,
    InvalidPairingCode,
    PairingFailed,
    StorageFailed,
    InvalidProfile,
    InvalidCredentials,
    CredentialsRequired,
    Timeout,
    ConnectionLost,
    NetworkUnavailable,
    ProbeFailed,
    HealthFailed,
    ProtocolError,
    InternalError,
    #[serde(other)]
    Unknown,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Usb {
    pub total: u16,
    pub selected: u16,
    pub ready: u16,
    pub in_use: u16,
    pub permission_missing: u16,
    pub consent_missing: u16,
    pub unsupported: u16,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    pub schema: u8,
    pub configured: bool,
    pub transport: Transport,
    pub running: bool,
    /// Link presence only, never a claim about internet/endpoint reachability.
    pub network: Option<bool>,
    pub phase: Phase,
    pub receiver_verified: bool,
    pub failure: Option<Failure>,
    pub usb: Usb,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Good,
    Attention,
    Unknown,
    Progress,
}
#[derive(Debug, Serialize)]
pub struct Check {
    pub area: &'static str,
    pub level: Level,
    pub message: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u8,
    pub summary: &'static str,
    pub action: &'static str,
    pub checks: Vec<Check>,
    pub observations: Observations,
}

pub fn diagnose(bytes: &[u8]) -> Result<String, &'static str> {
    serde_json::to_string(&Observations::parse(bytes)?.evaluate())
        .map_err(|_| "diagnostic_report_failed")
}

impl Observations {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 4096 {
            return Err("invalid_diagnostic_observations");
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid_diagnostic_observations")?;
        let usb = &value.usb;
        if value.schema != 1
            || usb.total > 512
            || usb.selected > usb.total
            || usb.ready > usb.selected
            || usb.in_use > usb.total
            || usb.permission_missing > usb.selected
            || usb.consent_missing > usb.selected
            || usb.unsupported > usb.total
        {
            return Err("invalid_diagnostic_observations");
        }
        Ok(value)
    }
    pub fn evaluate(mut self) -> Report {
        use Level::*;
        // A remembered state must not survive stopping/changing a destination.
        if !self.configured {
            self.failure = None;
        }
        if !self.running
            || !self.configured
            || self.failure.is_some()
            || matches!(self.transport, Transport::Legacy | Transport::None)
        {
            self.phase = Phase::Idle;
            self.receiver_verified = false;
        }
        let usb = &self.usb;
        let mut checks = vec![
            check(
                "configuration",
                if self.configured { Good } else { Attention },
                if self.configured {
                    "computer_saved"
                } else {
                    "choose_computer"
                },
            ),
            if usb.total == 0 {
                check("usb", Attention, "usb_absent")
            } else if usb.selected == 0 {
                check(
                    "usb",
                    Attention,
                    if usb.unsupported == usb.total {
                        "usb_unsupported"
                    } else {
                        "select_usb"
                    },
                )
            } else if usb.permission_missing > 0 {
                check("usb", Attention, "usb_permission")
            } else if usb.consent_missing > 0 {
                check("usb", Attention, "storage_consent")
            } else if usb.ready < usb.selected {
                check("usb", Attention, "usb_not_ready")
            } else {
                check("usb", Good, "usb_ready")
            },
            match self.network {
                Some(true) => check("network", Good, "network_present"),
                Some(false) => check("network", Attention, "network_absent"),
                None => check("network", Unknown, "network_unknown"),
            },
            match self.phase {
                Phase::Trust => check("computer", Attention, "identity_confirmation"),
                Phase::Checking | Phase::Preparing | Phase::Connected => {
                    check("computer", Good, "computer_authenticated")
                }
                Phase::Connecting => check("computer", Progress, "computer_connecting"),
                Phase::Reconnecting => check("computer", Progress, "computer_reconnecting"),
                Phase::Idle => check("computer", Unknown, "computer_unchecked"),
            },
            if self.phase == Phase::Connected && self.receiver_verified {
                check("receiver", Good, "receiver_verified")
            } else if self.phase == Phase::Preparing {
                check("receiver", Progress, "receiver_preparing")
            } else {
                check("receiver", Unknown, "receiver_unchecked")
            },
            if self.running && self.configured && self.failure.is_none() && usb.in_use > 0 {
                check("transfer", Good, "usb_transferring")
            } else {
                check("transfer", Unknown, "usb_not_imported")
            },
        ];
        let (mut summary, mut action) = if !self.configured {
            ("choose_computer", "setup")
        } else if checks[1].level == Attention {
            (checks[1].message, "usb")
        } else if self.network == Some(false) {
            ("network_absent", "network")
        } else if !self.running {
            ("bridge_stopped", "home")
        } else if self.phase == Phase::Trust {
            ("identity_confirmation", "home")
        } else if self.phase == Phase::Reconnecting {
            ("computer_reconnecting", "none")
        } else if usb.in_use > 0 {
            ("usb_transferring", "none")
        } else if self.phase == Phase::Connected || self.transport == Transport::Legacy {
            ("waiting_for_import", "help")
        } else {
            ("computer_connecting", "none")
        };
        if let Some(failure) = self.failure {
            let (area, message, next) = explain(failure);
            for check in &mut checks {
                if check.area == area {
                    check.level = Attention;
                    check.message = message;
                }
            }
            summary = message;
            action = next;
        }
        Report {
            schema: 1,
            summary,
            action,
            checks,
            observations: self,
        }
    }
}

fn check(area: &'static str, level: Level, message: &'static str) -> Check {
    Check {
        area,
        level,
        message,
    }
}
fn explain(failure: Failure) -> (&'static str, &'static str, &'static str) {
    use Failure::*;
    match failure {
        HostKeyRejected => ("computer", "identity_not_confirmed", "setup"),
        AuthenticationFailed => ("computer", "login_failed", "setup"),
        InvalidKey => ("configuration", "private_key_invalid", "setup"),
        InvalidProfile | InvalidCredentials | CredentialsRequired => {
            ("configuration", "configuration_invalid", "setup")
        }
        ForwardingDenied => ("computer", "ssh_forwarding_denied", "help"),
        LinuxUsbDriverUnavailable => ("receiver", "linux_usb_driver_unavailable", "help"),
        LinuxSystemdRequired => ("receiver", "linux_systemd_required", "help"),
        WindowsAdministratorRequired => ("receiver", "administrator_required", "setup"),
        PreparationFailed => ("receiver", "preparation_failed", "help"),
        MissingReceiver | InvalidReceiver => ("receiver", "receiver_artifact_unavailable", "help"),
        RestartRequired => ("receiver", "computer_restart_required", "help"),
        ComputerBusy => ("receiver", "computer_busy", "help"),
        PairingRequired | InvalidPairingCode | PairingFailed => ("computer", "pair_again", "setup"),
        StorageFailed => ("configuration", "protected_storage_failed", "setup"),
        Timeout | NetworkUnavailable | ConnectionLost => {
            ("computer", "computer_unreachable", "home")
        }
        ProbeFailed | HealthFailed => ("receiver", "receiver_health_failed", "help"),
        ProtocolError => ("receiver", "protocol_incompatible", "help"),
        InternalError | Unknown => ("computer", "connection_failed", "home"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Observations {
        Observations {
            schema: 1,
            configured: true,
            transport: Transport::Ssh,
            running: true,
            network: Some(true),
            phase: Phase::Connected,
            receiver_verified: true,
            failure: None,
            usb: Usb {
                total: 1,
                selected: 1,
                ready: 1,
                in_use: 0,
                permission_missing: 0,
                consent_missing: 0,
                unsupported: 0,
            },
        }
    }
    #[test]
    fn network_and_authentication_do_not_claim_an_imported_device() {
        let report = fixture().evaluate();
        assert_eq!(report.summary, "waiting_for_import");
        assert_eq!(report.checks[5].level, Level::Unknown);
        let mut input = fixture();
        input.phase = Phase::Idle;
        input.receiver_verified = false;
        let report = input.evaluate();
        assert_eq!(report.checks[2].level, Level::Good);
        assert_eq!(report.checks[3].level, Level::Unknown);
        assert_eq!(report.checks[4].level, Level::Unknown);
    }
    #[test]
    fn stopped_or_changed_computer_cannot_keep_stale_success() {
        for configured in [true, false] {
            let mut input = fixture();
            input.configured = configured;
            input.running = false;
            input.usb.in_use = 1;
            let report = input.evaluate();
            assert_eq!(report.checks[3].level, Level::Unknown);
            assert_eq!(report.checks[4].level, Level::Unknown);
            assert_eq!(report.checks[5].level, Level::Unknown);
        }
    }
    #[test]
    fn timeout_does_not_invent_firewall_or_bandwidth_causes() {
        let mut input = fixture();
        input.failure = Some(Failure::Timeout);
        let report = input.evaluate();
        assert_eq!(report.summary, "computer_unreachable");
        assert_eq!(report.checks[2].message, "network_present");
        assert_eq!(report.checks[3].level, Level::Attention);
        assert_eq!(report.checks[4].level, Level::Unknown);
        assert!(!report.observations.receiver_verified);
    }
    #[test]
    fn failures_and_reconfiguration_do_not_leave_stale_transfer_success() {
        let mut input = fixture();
        input.usb.in_use = 1;
        input.failure = Some(Failure::ConnectionLost);
        let report = input.clone().evaluate();
        assert_eq!(report.checks[5].level, Level::Unknown);
        input.configured = false;
        let report = input.evaluate();
        assert_eq!(report.checks[3].level, Level::Unknown);
        assert_eq!(report.checks[5].level, Level::Unknown);
        let mut input = fixture();
        input.transport = Transport::Legacy;
        let report = input.evaluate();
        assert_eq!(report.checks[3].level, Level::Unknown);
        assert_eq!(report.checks[4].level, Level::Unknown);
    }
    #[test]
    fn storage_consent_and_permission_are_distinct() {
        let mut input = fixture();
        input.usb.consent_missing = 1;
        input.usb.ready = 0;
        assert_eq!(input.clone().evaluate().summary, "storage_consent");
        input.usb.permission_missing = 1;
        assert_eq!(input.evaluate().summary, "usb_permission");
    }
    #[test]
    fn no_secrets_can_be_echoed_from_unknown_errors_or_fields() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["failure"] = serde_json::json!("password=synthetic-secret host=private-host");
        let report = Observations::parse(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .evaluate();
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("synthetic-secret") && !encoded.contains("private-host"));
        value["password"] = serde_json::json!("synthetic-secret");
        assert!(Observations::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(Observations::parse(&vec![0; 4097]).is_err());
    }
    #[test]
    fn impossible_counts_and_unknown_phases_are_rejected() {
        let mut input = fixture();
        input.usb.selected = 2;
        assert!(Observations::parse(&serde_json::to_vec(&input).unwrap()).is_err());
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["phase"] = serde_json::json!("ready_guessed");
        assert!(Observations::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn known_failure_has_a_targeted_action_and_clears_when_profile_changes() {
        let mut input = fixture();
        input.failure = Some(Failure::LinuxUsbDriverUnavailable);
        let report = input.clone().evaluate();
        assert_eq!(report.summary, "linux_usb_driver_unavailable");
        assert_eq!(report.action, "help");
        input.configured = false;
        assert_eq!(input.evaluate().summary, "choose_computer");
    }
}
