//! Strict non-secret configuration. Credentials are separate zeroizing values.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const MAX_CONFIG: usize = 4096;

/// USB/IP's restricted unencrypted address modes. Shared by inventory policy
/// and saved-computer validation; SSH/TLS use Endpoint instead.
pub fn valid_legacy_host(host: &str, mode: i32) -> bool {
    let Ok(ip) = host.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    if ip.to_string() != host {
        return false;
    }
    let [a, b, _, _] = ip.octets();
    match mode {
        0 => a == 100 && (64..=127).contains(&b),
        1 => a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168),
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}
impl Endpoint {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.port == 0 || self.host.len() > 253 || self.host.is_empty() {
            return Err("invalid_endpoint");
        }
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            let ip = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
                _ => ip,
            };
            // A receiver's link-local interface index is meaningless on a tablet.
            // Pairing uses IPv4 or routable IPv6; scoped literals are not portable.
            let unusable = match ip {
                IpAddr::V4(v4) => v4.is_broadcast(),
                IpAddr::V6(v6) => v6.is_unicast_link_local(),
            };
            if ip.is_unspecified() || ip.is_multicast() || unusable {
                return Err("invalid_endpoint");
            }
            return Ok(());
        }
        if self.host.ends_with('.') || self.host.bytes().any(|b| !b.is_ascii()) {
            return Err("invalid_endpoint");
        }
        if self.host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || label
                    .bytes()
                    .any(|b| !b.is_ascii_alphanumeric() && b != b'-')
        }) {
            return Err("invalid_endpoint");
        }
        // Reject misspelled numeric IPv4 instead of handing it to resolver aliases.
        if self.host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return Err("invalid_endpoint");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Ssh {
        endpoint: Endpoint,
        user: String,
        host_key: Option<String>,
        reverse_port: u16,
        prepare: bool,
    },
    Direct {
        endpoint: Endpoint,
        server_pin: String,
    },
}
impl Profile {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_CONFIG {
            return Err("config_too_large");
        }
        let p: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_config")?;
        p.validate()?;
        Ok(p)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Ssh {
                endpoint,
                user,
                host_key,
                reverse_port,
                ..
            } => {
                endpoint.validate()?;
                // SSH sends a username field; this is never inserted into a shell.
                if user.is_empty() || user.len() > 128 || user.chars().any(char::is_control) {
                    return Err("invalid_user");
                }
                if *reverse_port != 0 && *reverse_port < 1024 {
                    return Err("invalid_reverse_port");
                }
                if let Some(key) = host_key {
                    decode_pin(key)?;
                }
            }
            Self::Direct {
                endpoint,
                server_pin,
            } => {
                endpoint.validate()?;
                decode_pin(server_pin)?;
            }
        }
        Ok(())
    }
}
pub fn decode_pin(pin: &str) -> Result<[u8; 32], &'static str> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(pin)
        .map_err(|_| "invalid_pin")?
        .try_into()
        .map_err(|_| "invalid_pin")
}
pub fn encode_pin(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(ring::digest::digest(&ring::digest::SHA256, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_are_addresses_not_commands_or_implicit_services() {
        for s in [
            "host.example",
            "pc-1.local",
            "192.168.1.2",
            "::1",
            "2001:db8::1",
        ] {
            assert!(
                Endpoint {
                    host: s.into(),
                    port: 22
                }
                .validate()
                .is_ok(),
                "{s}"
            );
        }
        for s in [
            "",
            "host:22",
            "ssh://host",
            "a..b",
            "-host",
            "host.",
            "a b",
            "x\nwhoami",
            "$(id)",
            "x;id",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "255.255.255.255",
            "::ffff:0.0.0.0",
            "::ffff:224.0.0.1",
            "fe80::1",
            "fe80::1%3",
            "[::1]",
            "010.0.0.1",
            "999.0.0.1",
            "хост",
        ] {
            assert!(
                Endpoint {
                    host: s.into(),
                    port: 22
                }
                .validate()
                .is_err(),
                "{s}"
            );
        }
        assert!(
            Endpoint {
                host: "pc".into(),
                port: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            Endpoint {
                host: "pc".into(),
                port: 65535
            }
            .validate()
            .is_ok()
        );
        assert!(
            Endpoint {
                host: "a".repeat(254),
                port: 22
            }
            .validate()
            .is_err()
        );
        assert!(
            Endpoint {
                host: format!("{}.local", "a".repeat(64)),
                port: 22
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn profile_rejects_credentials_unknown_fields_and_unbounded_input() {
        let valid=br#"{"transport":"ssh","endpoint":{"host":"pc","port":22},"user":"user","host_key":null,"reverse_port":0,"prepare":true}"#;
        assert!(Profile::parse(valid).is_ok());
        let mut p = Profile::parse(valid).unwrap();
        if let Profile::Ssh { user, .. } = &mut p {
            *user = "DOMAIN\\user".into();
        }
        assert!(p.validate().is_ok());
        for bad in ["", "user\n", "user\0", "user\t"] {
            if let Profile::Ssh { user, .. } = &mut p {
                *user = bad.into();
            }
            assert!(p.validate().is_err());
        }
        for bad in [
            String::from_utf8(valid.to_vec()).unwrap().replace(
                "\"user\":\"user\"",
                "\"user\":\"user\",\"password\":\"secret\"",
            ),
            " ".repeat(4097),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("\"ssh\"", "\"relay\""),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("\"prepare\":true", "\"preapre\":true"),
        ] {
            assert!(Profile::parse(bad.as_bytes()).is_err());
        }
        assert!(decode_pin(&encode_pin(b"host key")).is_ok());
        for p in ["", "SHA256:x", "AAAA", &"A".repeat(44)] {
            assert!(decode_pin(p).is_err());
        }
    }
}
