//! Bounded, transactional saved computers. No I/O or ambient active connection.
//!
//! The Android repository owns encryption, atomic persistence and the start/stop
//! barrier. A successful mutation returns a NEW plaintext snapshot; callers must
//! persist it before publishing its view or acknowledging a session event.
//! Neither secrets nor commands implement Debug. Errors contain fixed codes only.
use crate::{
    config::{Profile, decode_pin, valid_legacy_host},
    pairing::{PairCode, token},
    security::random,
    session::Secret,
    usb_rules::{self, Device, Identity, Rule},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use zeroize::{Zeroize, Zeroizing};

pub const MAX_CATALOG: usize = 4 * 1024 * 1024;
pub const MAX_REQUEST: usize = 144 * 1024;
const MAX_COMPUTERS: usize = 32;
const MAX_COUNTER: u64 = i64::MAX as u64;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u8,
    revision: u64,
    active_id: Option<String>,
    activation_epoch: u64,
    computers: Vec<Computer>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Computer {
    id: String,
    name: String,
    connection_generation: u64,
    connection: Connection,
    #[serde(default = "one")]
    usb_epoch: u64,
    #[serde(default)]
    usb_rules: Vec<Rule>,
}

fn one() -> u64 {
    1
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Connection {
    Secure {
        profile: Profile,
        credentials: Box<Secret>,
    },
    Legacy {
        host: String,
        mode: i32,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Lease {
    id: String,
    connection_generation: u64,
    activation_epoch: u64,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    UsbName {
        name: String,
    },
    UsbIdentity {
        vendor: u16,
        product: u16,
        serial: Option<String>,
        layout: String,
    },
    UsbEvaluate {
        rules: Vec<Rule>,
        devices: Vec<Device>,
    },
    SetUsbRule {
        revision: u64,
        id: String,
        rule: Rule,
    },
    ForgetUsbRule {
        revision: u64,
        id: String,
        identity: Identity,
    },
    Create {},
    Migrate {
        name: String,
        expected_profile: String,
        old: LegacyEnvelope,
    },
    View {},
    Active {},
    Begin {
        revision: u64,
    },
    Edit {
        revision: u64,
        id: String,
    },
    Add {
        revision: u64,
        name: String,
        connection: Connection,
        activate: bool,
    },
    Replace {
        revision: u64,
        id: String,
        name: String,
        connection: Connection,
    },
    Rename {
        revision: u64,
        id: String,
        name: String,
    },
    Activate {
        revision: u64,
        id: String,
    },
    Forget {
        revision: u64,
        id: String,
    },
    Trust {
        lease: Lease,
        pin: String,
    },
    Paired {
        lease: Lease,
        token: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEnvelope {
    profile: String,
    credentials: Box<Secret>,
}

impl Drop for Request {
    fn drop(&mut self) {
        if let Self::Paired { token, .. } = self {
            token.zeroize();
        }
        if let Self::UsbIdentity {
            serial: Some(serial),
            ..
        } = self
        {
            serial.zeroize();
        }
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && name.len() <= 256
        && name.chars().count() <= 64
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}
fn next(value: u64) -> Result<u64, &'static str> {
    value
        .checked_add(1)
        .filter(|v| *v <= MAX_COUNTER)
        .ok_or("counter_exhausted")
}
fn encode(value: &impl Serialize, max: usize) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    let data = Zeroizing::new(serde_json::to_vec(value).map_err(|_| "invalid_catalog")?);
    if data.len() > max {
        return Err("catalog_too_large");
    }
    Ok(data)
}

impl Connection {
    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Legacy { host, mode } => {
                if !valid_legacy_host(host, *mode) {
                    return Err("invalid_legacy_address");
                }
            }
            Self::Secure {
                profile,
                credentials: s,
            } => {
                profile.validate()?;
                s.validate()?;
                // Also enforce the wire bound for heavily escaped credentials.
                let _ = encode(s, 131072)?;
                if s.client_id.as_ref().is_some_and(|id| !valid_id(id)) {
                    return Err("invalid_client_id");
                }
                match profile {
                    Profile::Ssh { prepare, .. } => {
                        if (s.password.is_none() && s.private_key.is_none())
                            || (s.passphrase.is_some() && s.private_key.is_none())
                            || s.token.is_some()
                            || s.pair_code.is_some()
                            || s.pair_nonce.is_some()
                            || s.private_key.as_ref().is_some_and(String::is_empty)
                            || (*prepare && s.client_id.is_none())
                        {
                            return Err("invalid_credentials");
                        }
                    }
                    Profile::Direct {
                        endpoint,
                        server_pin,
                    } => {
                        if s.password.is_some()
                            || s.sudo_password.is_some()
                            || s.private_key.is_some()
                            || s.passphrase.is_some()
                        {
                            return Err("invalid_credentials");
                        }
                        if let Some(value) = &s.token {
                            token(value).map_err(|_| "invalid_credentials")?;
                            if s.pair_code.is_some() || s.pair_nonce.is_some() {
                                return Err("invalid_credentials");
                            }
                        } else {
                            let code =
                                PairCode::decode(s.pair_code.as_deref().ok_or("pairing_required")?)
                                    .map_err(|_| "invalid_pairing_code")?;
                            if code.endpoint != *endpoint || code.server_pin != *server_pin {
                                return Err("invalid_pairing_code");
                            }
                            let nonce = Zeroizing::new(
                                base64::engine::general_purpose::URL_SAFE_NO_PAD
                                    .decode(s.pair_nonce.as_deref().ok_or("invalid_pairing_nonce")?)
                                    .map_err(|_| "invalid_pairing_nonce")?,
                            );
                            if nonce.len() != 16 || nonce.iter().all(|b| *b == 0) {
                                return Err("invalid_pairing_nonce");
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Catalog {
    fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_CATALOG {
            return Err("catalog_too_large");
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_catalog")?;
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> Result<(), &'static str> {
        if self.schema != 2
            || self.revision > MAX_COUNTER
            || self.activation_epoch > MAX_COUNTER
            || self.computers.len() > MAX_COMPUTERS
        {
            return Err("invalid_catalog");
        }
        let mut ids = BTreeSet::new();
        for computer in &self.computers {
            if !valid_id(&computer.id)
                || !ids.insert(&computer.id)
                || !valid_name(&computer.name)
                || computer.connection_generation == 0
                || computer.connection_generation > MAX_COUNTER
                || computer.usb_epoch == 0
                || computer.usb_epoch > MAX_COUNTER
            {
                return Err("invalid_catalog");
            }
            computer.connection.validate()?;
            usb_rules::validate(&computer.usb_rules)?;
        }
        if let Some(id) = &self.active_id
            && (!ids.contains(id) || self.activation_epoch == 0)
        {
            return Err("invalid_catalog");
        }
        Ok(())
    }
    fn check_revision(&self, revision: u64) -> Result<(), &'static str> {
        if self.revision != revision {
            return Err("catalog_changed");
        }
        Ok(())
    }
    fn index(&self, id: &str) -> Result<usize, &'static str> {
        self.computers
            .iter()
            .position(|c| c.id == id)
            .ok_or("computer_missing")
    }
    fn check_idle(&self, affected: &str, running: bool) -> Result<(), &'static str> {
        if running && self.active_id.as_deref() == Some(affected) {
            return Err("connection_running");
        }
        Ok(())
    }
    fn lease(&self, at: usize) -> Lease {
        let c = &self.computers[at];
        Lease {
            id: c.id.clone(),
            connection_generation: c.connection_generation,
            activation_epoch: self.activation_epoch,
        }
    }
    fn check_lease(&self, lease: &Lease, running: bool) -> Result<usize, &'static str> {
        if !running || self.active_id.as_ref() != Some(&lease.id) {
            return Err("stale_connection");
        }
        let at = self.index(&lease.id)?;
        if self.lease(at) != *lease {
            return Err("stale_connection");
        }
        Ok(at)
    }
    fn activate(&mut self, id: &str, running: bool) -> Result<bool, &'static str> {
        self.index(id)?;
        if self.active_id.as_deref() == Some(id) {
            return Ok(false);
        }
        if running {
            return Err("connection_running");
        }
        self.activation_epoch = next(self.activation_epoch)?;
        self.active_id = Some(id.into());
        Ok(true)
    }
    fn fresh_id(&self) -> Result<String, &'static str> {
        for _ in 0..3 {
            let bytes = random::<16>().map_err(|_| "random_failed")?;
            use std::fmt::Write;
            let mut id = String::with_capacity(32);
            for byte in bytes {
                write!(id, "{byte:02x}").map_err(|_| "random_failed")?;
            }
            if !self.computers.iter().any(|c| c.id == id) {
                return Ok(id);
            }
        }
        Err("random_failed")
    }
    fn apply(&mut self, request: &mut Request, running: bool) -> Result<(), &'static str> {
        let changed = match request {
            Request::SetUsbRule { revision, id, rule } => {
                self.check_revision(*revision)?;
                rule.validate()?;
                let at = self.index(id)?;
                let rules = &mut self.computers[at].usb_rules;
                if let Some(old) = rules.iter_mut().find(|old| old.identity == rule.identity) {
                    if old == rule {
                        false
                    } else {
                        *old = rule.clone();
                        true
                    }
                } else {
                    if rules.len() >= usb_rules::MAX_RULES {
                        return Err("usb_rule_limit");
                    }
                    rules.push(rule.clone());
                    true
                }
            }
            Request::ForgetUsbRule {
                revision,
                id,
                identity,
            } => {
                self.check_revision(*revision)?;
                identity.validate()?;
                let at = self.index(id)?;
                let rules = &mut self.computers[at].usb_rules;
                let before = rules.len();
                rules.retain(|rule| rule.identity != *identity);
                before != rules.len()
            }
            Request::Begin { revision } => {
                self.check_revision(*revision)?;
                if running {
                    return Err("connection_running");
                }
                if self.active_id.is_none() {
                    return Err("computer_not_selected");
                }
                self.activation_epoch = next(self.activation_epoch)?;
                true
            }
            Request::Add {
                revision,
                name,
                connection,
                activate,
            } => {
                self.check_revision(*revision)?;
                if self.computers.len() >= MAX_COMPUTERS {
                    return Err("computer_limit");
                }
                if !valid_name(name) {
                    return Err("invalid_computer_name");
                }
                connection.validate()?;
                if *activate && running {
                    return Err("connection_running");
                }
                let id = self.fresh_id()?;
                self.computers.push(Computer {
                    id: id.clone(),
                    name: std::mem::take(name),
                    connection_generation: 1,
                    usb_epoch: 1,
                    usb_rules: Vec::new(),
                    connection: std::mem::replace(
                        connection,
                        Connection::Legacy {
                            host: String::new(),
                            mode: 0,
                        },
                    ),
                });
                if *activate {
                    self.activate(&id, running)?;
                }
                true
            }
            Request::Replace {
                revision,
                id,
                name,
                connection,
            } => {
                self.check_revision(*revision)?;
                self.check_idle(id, running)?;
                let at = self.index(id)?;
                if !valid_name(name) {
                    return Err("invalid_computer_name");
                }
                connection.validate()?;
                let c = &mut self.computers[at];
                c.connection_generation = next(c.connection_generation)?;
                c.usb_epoch = next(c.usb_epoch)?;
                c.usb_rules.clear();
                c.name = std::mem::take(name);
                c.connection = std::mem::replace(
                    connection,
                    Connection::Legacy {
                        host: String::new(),
                        mode: 0,
                    },
                );
                true
            }
            Request::Rename { revision, id, name } => {
                self.check_revision(*revision)?;
                if !valid_name(name) {
                    return Err("invalid_computer_name");
                }
                let at = self.index(id)?;
                if self.computers[at].name == *name {
                    false
                } else {
                    self.computers[at].name = std::mem::take(name);
                    true
                }
            }
            Request::Activate { revision, id } => {
                self.check_revision(*revision)?;
                self.activate(id, running)?
            }
            Request::Forget { revision, id } => {
                self.check_revision(*revision)?;
                self.check_idle(id, running)?;
                let at = self.index(id)?;
                if self.active_id.as_ref() == Some(id) {
                    self.activation_epoch = next(self.activation_epoch)?;
                    self.active_id = None;
                }
                self.computers.remove(at);
                true
            }
            Request::Trust { lease, pin } => {
                let at = self.check_lease(lease, running)?;
                decode_pin(pin)?;
                let c = &mut self.computers[at];
                let Connection::Secure {
                    profile: Profile::Ssh { host_key, .. },
                    ..
                } = &mut c.connection
                else {
                    return Err("wrong_transport");
                };
                if host_key.as_ref().is_some_and(|old| old != pin) {
                    return Err("identity_changed");
                }
                if host_key.is_some() {
                    false
                } else {
                    c.connection_generation = next(c.connection_generation)?;
                    *host_key = Some(pin.clone());
                    true
                }
            }
            Request::Paired {
                lease,
                token: value,
            } => {
                let at = self.check_lease(lease, running)?;
                token(value).map_err(|_| "invalid_credentials")?;
                let c = &mut self.computers[at];
                let Connection::Secure {
                    profile: Profile::Direct { .. },
                    credentials,
                } = &mut c.connection
                else {
                    return Err("wrong_transport");
                };
                if let Some(old) = &credentials.token {
                    if old != value {
                        return Err("identity_changed");
                    }
                    false
                } else {
                    c.connection_generation = next(c.connection_generation)?;
                    credentials.token = Some(std::mem::take(value));
                    for pending in [&mut credentials.pair_code, &mut credentials.pair_nonce] {
                        if let Some(mut secret) = pending.take() {
                            secret.zeroize();
                        }
                    }
                    true
                }
            }
            _ => return Err("invalid_catalog_request"),
        };
        if changed {
            self.revision = next(self.revision)?;
        }
        self.validate()
    }
    fn view(&self) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        // Dedicated types: adding a credential field to Connection cannot leak it
        // through a generic Value clone or a blacklist of fields to remove.
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum PublicConnection<'a> {
            Secure { profile: &'a Profile },
            Legacy { host: &'a str, mode: i32 },
        }
        #[derive(Serialize)]
        struct PublicComputer<'a> {
            id: &'a str,
            name: &'a str,
            connection_generation: u64,
            connection: PublicConnection<'a>,
            usb_epoch: u64,
            usb_rules: &'a [Rule],
        }
        #[derive(Serialize)]
        struct View<'a> {
            schema: u8,
            revision: u64,
            active_id: &'a Option<String>,
            activation_epoch: u64,
            computers: Vec<PublicComputer<'a>>,
        }
        let computers = self
            .computers
            .iter()
            .map(|c| PublicComputer {
                id: &c.id,
                name: &c.name,
                connection_generation: c.connection_generation,
                usb_epoch: c.usb_epoch,
                usb_rules: &c.usb_rules,
                connection: match &c.connection {
                    Connection::Secure { profile, .. } => PublicConnection::Secure { profile },
                    Connection::Legacy { host, mode } => {
                        PublicConnection::Legacy { host, mode: *mode }
                    }
                },
            })
            .collect();
        encode(
            &View {
                schema: 2,
                revision: self.revision,
                active_id: &self.active_id,
                activation_epoch: self.activation_epoch,
                computers,
            },
            MAX_CATALOG,
        )
    }
}

/// create requires explicitly empty input; corrupt/nonempty input never resets.
/// view returns ONLY public fields; active/edit intentionally return secrets to
/// their owning service/editor. Mutations return the candidate snapshot, and
/// leave the supplied bytes untouched on both success and failure.
pub fn execute(
    input: &[u8],
    request: &[u8],
    running: bool,
) -> Result<Zeroizing<Vec<u8>>, &'static str> {
    if input.len() > MAX_CATALOG {
        return Err("catalog_too_large");
    }
    if request.len() > MAX_REQUEST {
        return Err("catalog_request_too_large");
    }
    let mut request: Request =
        serde_json::from_slice(request).map_err(|_| "invalid_catalog_request")?;
    // Pure recognition/decision requests never receive encrypted catalog data.
    match &request {
        Request::UsbName { name } => {
            if !input.is_empty() || running {
                return Err("invalid_catalog_request");
            }
            return encode(&(name.is_empty() || valid_name(name)), MAX_REQUEST);
        }
        Request::UsbIdentity {
            vendor,
            product,
            serial,
            layout,
        } => {
            if !input.is_empty() || running {
                return Err("invalid_catalog_request");
            }
            return encode(
                &Identity::recognize(*vendor, *product, serial.as_deref(), layout),
                MAX_REQUEST,
            );
        }
        Request::UsbEvaluate { rules, devices } => {
            if !input.is_empty() || running {
                return Err("invalid_catalog_request");
            }
            return encode(&usb_rules::evaluate(rules, devices)?, MAX_REQUEST);
        }
        _ => (),
    }
    if matches!(request, Request::Create {} | Request::Migrate { .. }) {
        if !input.is_empty() || running {
            return Err("catalog_exists");
        }
        let mut catalog = Catalog {
            schema: 2,
            revision: 0,
            active_id: None,
            activation_epoch: 0,
            computers: Vec::new(),
        };
        if let Request::Migrate {
            name,
            expected_profile,
            old,
        } = &mut request
        {
            if old.profile != *expected_profile {
                return Err("profile_mismatch");
            }
            let profile = Profile::parse(expected_profile.as_bytes())?;
            let mut add = Request::Add {
                revision: 0,
                name: std::mem::take(name),
                connection: Connection::Secure {
                    profile,
                    credentials: Box::new((*old.credentials).clone()),
                },
                activate: true,
            };
            catalog.apply(&mut add, false)?;
        }
        return encode(&catalog, MAX_CATALOG);
    }
    let mut catalog = Catalog::parse(input)?;
    match &request {
        Request::View {} => return catalog.view(),
        Request::Active {} => {
            #[derive(Serialize)]
            struct Active<'a> {
                lease: Lease,
                connection: &'a Connection,
            }
            let id = catalog.active_id.as_ref().ok_or("computer_not_selected")?;
            let at = catalog.index(id)?;
            return encode(
                &Active {
                    lease: catalog.lease(at),
                    connection: &catalog.computers[at].connection,
                },
                MAX_REQUEST,
            );
        }
        Request::Edit { revision, id } => {
            catalog.check_revision(*revision)?;
            let at = catalog.index(id)?;
            #[derive(Serialize)]
            struct Edit<'a> {
                id: &'a str,
                name: &'a str,
                connection_generation: u64,
                connection: &'a Connection,
            }
            let computer = &catalog.computers[at];
            return encode(
                &Edit {
                    id: &computer.id,
                    name: &computer.name,
                    connection_generation: computer.connection_generation,
                    connection: &computer.connection,
                },
                MAX_REQUEST,
            );
        }
        _ => (),
    }
    catalog.apply(&mut request, running)?;
    encode(&catalog, MAX_CATALOG)
}

#[cfg(test)]
#[path = "computers_tests.rs"]
mod tests;
