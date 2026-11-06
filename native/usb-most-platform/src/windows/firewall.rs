//! Owned LAN exceptions only. Never enable/disable the firewall, broaden a
//! profile, or overwrite an administrator's edited/foreign rule.
//! API contract: https://learn.microsoft.com/windows/win32/api/netfw/nn-netfw-inetfwrule
use std::io;
use windows::{
    Win32::{
        NetworkManagement::WindowsFirewall::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::BSTR,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub tls: Option<u16>,
    pub discovery: bool,
}
impl State {
    pub fn direct(port: u16) -> Self {
        Self {
            tls: Some(port),
            discovery: true,
        }
    }
}
const TLS: &str = "USB Most Receiver TLS v1";
const MDNS: &str = "USB Most Receiver Discovery v1";
const DESCRIPTION: &str = "USB Most managed LAN rule v1";
const PROFILES: i32 = NET_FW_PROFILE2_DOMAIN.0 | NET_FW_PROFILE2_PRIVATE.0;
fn failure(e: windows::core::Error) -> io::Error {
    let n = e.code().0 as u32;
    if n & 0xffff0000 == 0x80070000 {
        io::Error::from_raw_os_error((n & 0xffff) as i32)
    } else {
        io::Error::other("firewall_api_unavailable")
    }
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
fn worker<T: Send + 'static>(
    f: impl FnOnce(&INetFwRules) -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    std::thread::spawn(move || {
        // SAFETY: owned thread; guard outlives every COM object and balances init.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(failure)?;
        let _apartment = Apartment;
        let policy: INetFwPolicy2 =
            unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }
                .map_err(failure)?;
        let rules = unsafe { policy.Rules() }.map_err(failure)?;
        f(&rules)
    })
    .join()
    .map_err(|_| io::Error::other("firewall_worker_failed"))?
}
fn executable() -> io::Result<BSTR> {
    let path = super::service::install::executable()?;
    let text = path.to_str().ok_or_else(super::invalid)?;
    Ok(BSTR::from(text))
}
fn get(rules: &INetFwRules, name: &str) -> io::Result<Option<INetFwRule>> {
    // SAFETY: live apartment-bound interface and owned string.
    match unsafe { rules.Item(&BSTR::from(name)) } {
        Ok(rule) => Ok(Some(rule)),
        Err(e) if e.code().0 as u32 == 0x80070002 => Ok(None),
        Err(e) => Err(failure(e)),
    }
}
fn validate(rule: &INetFwRule, name: &str, protocol: i32) -> io::Result<u16> {
    let path = executable()?;
    // SAFETY: getters copy COM-owned values into lifetime-managed Rust wrappers.
    unsafe {
        let port = rule.LocalPorts().map_err(failure)?.to_string();
        let number: u16 = port.parse().map_err(|_| super::invalid())?;
        if number < 1024
            || number.to_string() != port
            || rule.Name().map_err(failure)? != name
            || rule.Description().map_err(failure)? != DESCRIPTION
            || rule.ApplicationName().map_err(failure)? != path
            || rule.ServiceName().map_err(failure)? != super::service::NAME
            || rule.Protocol().map_err(failure)? != protocol
            || rule.Profiles().map_err(failure)? != PROFILES
            || rule.RemoteAddresses().map_err(failure)? != "LocalSubnet"
            || rule.LocalAddresses().map_err(failure)? != "*"
            || rule.RemotePorts().map_err(failure)? != "*"
            || !rule
                .InterfaceTypes()
                .map_err(failure)?
                .to_string()
                .eq_ignore_ascii_case("All")
            || !rule.Interfaces().map_err(failure)?.is_empty()
            || rule.Direction().map_err(failure)? != NET_FW_RULE_DIR_IN
            || rule.Action().map_err(failure)? != NET_FW_ACTION_ALLOW
            || !rule.Enabled().map_err(failure)?.as_bool()
            || rule.EdgeTraversal().map_err(failure)?.as_bool()
            || (protocol == 17 && number != 5353)
        {
            return Err(super::denied());
        }
        Ok(number)
    }
}
fn snapshot(rules: &INetFwRules) -> io::Result<State> {
    Ok(State {
        tls: get(rules, TLS)?.map(|r| validate(&r, TLS, 6)).transpose()?,
        discovery: get(rules, MDNS)?
            .map(|r| validate(&r, MDNS, 17))
            .transpose()?
            .is_some(),
    })
}
pub fn state() -> io::Result<State> {
    worker(snapshot)
}
fn set_rule(rules: &INetFwRules, name: &str, protocol: i32, port: Option<u16>) -> io::Result<()> {
    let previous = get(rules, name)?;
    if let Some(ref rule) = previous {
        validate(rule, name, protocol)?;
    }
    // SAFETY: synchronous operations on apartment-local interfaces. A new rule
    // is fully constrained before Add; existing rules change only their port.
    unsafe {
        match (previous, port) {
            (Some(_), None) => rules.Remove(&BSTR::from(name)).map_err(failure),
            (None, None) => Ok(()),
            (Some(rule), Some(port)) => rule
                .SetLocalPorts(&BSTR::from(port.to_string()))
                .map_err(failure),
            (None, Some(port)) => {
                let rule: INetFwRule =
                    CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER).map_err(failure)?;
                rule.SetName(&BSTR::from(name)).map_err(failure)?;
                rule.SetDescription(&BSTR::from(DESCRIPTION))
                    .map_err(failure)?;
                rule.SetApplicationName(&executable()?).map_err(failure)?;
                rule.SetServiceName(&BSTR::from(super::service::NAME))
                    .map_err(failure)?;
                rule.SetProtocol(protocol).map_err(failure)?;
                rule.SetLocalPorts(&BSTR::from(port.to_string()))
                    .map_err(failure)?;
                rule.SetRemotePorts(&BSTR::from("*")).map_err(failure)?;
                rule.SetLocalAddresses(&BSTR::from("*")).map_err(failure)?;
                rule.SetRemoteAddresses(&BSTR::from("LocalSubnet"))
                    .map_err(failure)?;
                rule.SetInterfaceTypes(&BSTR::from("All"))
                    .map_err(failure)?;
                rule.SetProfiles(PROFILES).map_err(failure)?;
                rule.SetDirection(NET_FW_RULE_DIR_IN).map_err(failure)?;
                rule.SetAction(NET_FW_ACTION_ALLOW).map_err(failure)?;
                rule.SetEdgeTraversal(false.into()).map_err(failure)?;
                rule.SetEnabled(true.into()).map_err(failure)?;
                rules.Add(&rule).map_err(failure)
            }
        }
    }
}
/// Called under the installer's exclusive lock, with a persisted before-state.
/// On partial failure the same API restores that snapshot without broad cleanup.
pub fn apply(wanted: State) -> io::Result<()> {
    if wanted.tls.is_some_and(|p| p < 1024) {
        return Err(super::invalid());
    }
    worker(move |rules| {
        snapshot(rules)?; // Validate both before modifying either.
        set_rule(rules, TLS, 6, wanted.tls)?;
        set_rule(rules, MDNS, 17, wanted.discovery.then_some(5353))?;
        if snapshot(rules)? != wanted {
            return Err(super::invalid());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "changes only owned rules on an explicitly disposable elevated Windows VM"]
    fn owned_lan_rules_snapshot_repair_and_foreign_refusal() {
        assert!(
            std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
                || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
        );
        assert_eq!(
            state().unwrap(),
            State::default(),
            "fixture must start without product rules"
        );
        apply(State::direct(3241)).unwrap();
        assert_eq!(state().unwrap(), State::direct(3241));
        apply(State::direct(3241)).unwrap();
        apply(State::direct(4241)).unwrap();
        assert_eq!(state().unwrap(), State::direct(4241));
        apply(State {
            tls: None,
            discovery: true,
        })
        .unwrap();
        assert_eq!(
            state().unwrap(),
            State {
                tls: None,
                discovery: true
            }
        );
        apply(State::direct(3241)).unwrap();
        worker(|rules| {
            let rule = get(rules, TLS)?.unwrap();
            // SAFETY: edit the fixture's own rule to emulate an administrator.
            unsafe { rule.SetDescription(&BSTR::from("Administrator override")) }.map_err(failure)
        })
        .unwrap();
        assert!(state().is_err());
        assert!(apply(State::default()).is_err());
        worker(|rules| {
            let rule = get(rules, TLS)?.unwrap();
            assert_eq!(
                unsafe { rule.Description() }.unwrap(),
                BSTR::from("Administrator override")
            );
            // Fixture ownership was established above; restore the exact edit.
            unsafe { rule.SetDescription(&BSTR::from(DESCRIPTION)) }.map_err(failure)
        })
        .unwrap();
        apply(State::default()).unwrap();
        assert_eq!(state().unwrap(), State::default());
    }
}
