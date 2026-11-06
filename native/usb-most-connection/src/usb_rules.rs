//! Local USB recognition is a convenience, not authentication of USB hardware.
//! Never identify a device by bus address, product label or VID/PID alone.
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_RULES: usize = 64;
pub const MAX_DEVICES: usize = 128;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub vendor: u16,
    pub product: u16,
    pub serial_hash: String,
    pub layout_hash: String,
}

fn digest(domain: &[u8], vendor: u16, product: u16, bytes: &[u8]) -> String {
    let mut hash = Context::new(&SHA256);
    hash.update(domain);
    hash.update(&vendor.to_be_bytes());
    hash.update(&product.to_be_bytes());
    hash.update(bytes);
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in hash.finish().as_ref() {
        write!(out, "{byte:02x}").expect("writing to String");
    }
    out
}

impl Identity {
    pub fn recognize(
        vendor: u16,
        product: u16,
        serial: Option<&str>,
        layout: &str,
    ) -> Option<Self> {
        let serial = serial?;
        if serial.trim().is_empty()
            || serial.len() > 1024
            || serial.chars().any(char::is_control)
            || layout.is_empty()
            || layout.len() > 8192
        {
            return None;
        }
        Some(Self {
            vendor,
            product,
            serial_hash: digest(b"usb-most/serial/v1\0", vendor, product, serial.as_bytes()),
            layout_hash: digest(b"usb-most/layout/v1\0", vendor, product, layout.as_bytes()),
        })
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        for value in [&self.serial_hash, &self.layout_hash] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("invalid_usb_identity");
            }
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub identity: Identity,
    pub name: String,
    pub automatic: bool,
    pub storage: bool,
}

impl Rule {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.identity.validate()?;
        if !self.name.is_empty() && !crate::computers::valid_name(&self.name) {
            return Err("invalid_usb_name");
        }
        if self.storage && self.automatic {
            return Err("storage_requires_consent");
        }
        Ok(())
    }
}

pub fn validate(rules: &[Rule]) -> Result<(), &'static str> {
    if rules.len() > MAX_RULES {
        return Err("usb_rule_limit");
    }
    let mut identities = BTreeSet::new();
    for rule in rules {
        rule.validate()?;
        if !identities.insert(&rule.identity) {
            return Err("duplicate_usb_rule");
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: u32,
    pub generation: i64,
    pub vendor: u16,
    pub product: u16,
    pub identity: Option<Identity>,
    pub storage: bool,
    pub supported: bool,
    pub permission: bool,
}

#[derive(Serialize)]
pub struct Decision<'a> {
    pub id: u32,
    pub generation: i64,
    pub name: &'a str,
    pub automatic: bool,
    pub reason: &'static str,
}

pub fn evaluate<'a>(
    rules: &'a [Rule],
    devices: &'a [Device],
) -> Result<Vec<Decision<'a>>, &'static str> {
    validate(rules)?;
    if devices.len() > MAX_DEVICES {
        return Err("usb_device_limit");
    }
    let mut ids = BTreeSet::new();
    for device in devices {
        if !ids.insert(device.id) {
            return Err("duplicate_usb_device");
        }
        if let Some(identity) = &device.identity {
            identity.validate()?;
            if identity.vendor != device.vendor || identity.product != device.product {
                return Err("invalid_usb_identity");
            }
        }
    }
    Ok(devices
        .iter()
        .map(|device| {
            let mut decision = Decision {
                id: device.id,
                generation: device.generation,
                name: "",
                automatic: false,
                reason: "manual",
            };
            if !device.supported {
                decision.reason = "unsupported";
                return decision;
            }
            if !device.permission {
                decision.reason = "permission";
                return decision;
            }
            let Some(identity) = &device.identity else {
                decision.reason = "no_identity";
                return decision;
            };
            // An unreadable identical model could be a duplicate. Do not guess even
            // when only one of the devices currently has Android permission.
            if devices.iter().any(|other| {
                other.id != device.id
                    && other.vendor == device.vendor
                    && other.product == device.product
                    && (!other.permission
                        || other
                            .identity
                            .as_ref()
                            .is_none_or(|id| id.serial_hash == identity.serial_hash))
            }) {
                decision.reason = "ambiguous";
                return decision;
            }
            if let Some(rule) = rules.iter().find(|rule| rule.identity == *identity) {
                decision.name = &rule.name;
                if device.storage || rule.storage {
                    decision.reason = "storage";
                } else if rule.automatic {
                    decision.automatic = true;
                    decision.reason = "automatic";
                }
            }
            decision
        })
        .collect())
}

#[cfg(test)]
#[path = "usb_rules_tests.rs"]
mod tests;
