//! Versioned fixed-size metadata for explicitly negotiated, authenticated imports.
//!
//! Decoding is NOT authorization. The caller must first authenticate the existing
//! SSH/TLS + app-local proof path. Plain USB/IP must never expose these operations.
//! No filesystem path, disk letter, PnP ID or shell argument crosses this boundary.
use crate::eject::{ImportKey, Outcome, State};
mod publisher;
pub use publisher::Publisher;
mod book;
pub use book::Book;

pub const CAPS: u16 = 0x80e1;
pub const IMPORT: u16 = 0x80e2;
pub const SYNC: u16 = 0x80e3;
pub const MAX_IMPORTS: u32 = 4;
pub const GENERIC: u32 = 1;
pub const WAN: u32 = 2;
pub const EJECT: u32 = 4;
const FEATURES: u32 = GENERIC | WAN | EJECT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;
type Result<T> = std::result::Result<T, Invalid>;
fn fixed<const N: usize>(bytes: &[u8]) -> Result<&[u8; N]> {
    bytes.try_into().map_err(|_| Invalid)
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn valid_session(session: &[u8; 16]) -> Result<()> {
    if *session == [0; 16] {
        Err(Invalid)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub session: [u8; 16],
    pub features: u32,
    pub max_imports: u32,
}
impl Capabilities {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let b = fixed::<32>(bytes)?;
        let value = Self {
            session: b[16..].try_into().unwrap(),
            features: u32_at(b, 8),
            max_imports: u32_at(b, 12),
        };
        if &b[..4] != b"UMM1"
            || u32_at(b, 4) != 1
            || value.features & !FEATURES != 0
            || value.features & GENERIC == 0
            || !(1..=MAX_IMPORTS).contains(&value.max_imports)
        {
            return Err(Invalid);
        }
        valid_session(&value.session)?;
        Ok(value)
    }
    pub fn encode(self) -> Result<[u8; 32]> {
        let mut b = [0; 32];
        b[..4].copy_from_slice(b"UMM1");
        b[4..8].copy_from_slice(&1u32.to_be_bytes());
        b[8..12].copy_from_slice(&self.features.to_be_bytes());
        b[12..16].copy_from_slice(&self.max_imports.to_be_bytes());
        b[16..].copy_from_slice(&self.session);
        Self::decode(&b)?;
        Ok(b)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRequest {
    pub busid: [u8; 32],
    pub wan: bool,
    pub session: [u8; 16],
}
impl ImportRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let b = fixed::<52>(bytes)?;
        let end = b[..32].iter().position(|v| *v == 0).ok_or(Invalid)?;
        if end == 0
            || b[end..32].iter().any(|v| *v != 0)
            || !b[..end].iter().all(|v| v.is_ascii_digit() || *v == b'-')
            || u32_at(b, 32) > 1
        {
            return Err(Invalid);
        }
        let value = Self {
            busid: b[..32].try_into().unwrap(),
            wan: u32_at(b, 32) == 1,
            session: b[36..].try_into().unwrap(),
        };
        valid_session(&value.session)?;
        Ok(value)
    }
    pub fn encode(&self) -> Result<[u8; 52]> {
        let mut b = [0; 52];
        b[..32].copy_from_slice(&self.busid);
        b[32..36].copy_from_slice(&u32::from(self.wan).to_be_bytes());
        b[36..].copy_from_slice(&self.session);
        Self::decode(&b)?;
        Ok(b)
    }
}
pub fn decode_key(bytes: &[u8]) -> Result<ImportKey> {
    let b = fixed::<24>(bytes)?;
    let key = ImportKey {
        session: b[..16].try_into().unwrap(),
        number: u64_at(b, 16),
    };
    valid_session(&key.session)?;
    if key.number == 0 {
        return Err(Invalid);
    }
    Ok(key)
}
pub fn encode_key(key: ImportKey) -> Result<[u8; 24]> {
    let mut b = [0; 24];
    b[..16].copy_from_slice(&key.session);
    b[16..].copy_from_slice(&key.number.to_be_bytes());
    decode_key(&b)?;
    Ok(b)
}

/// Receiver -> Android, a snapshot of one retained exact-import ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub import: ImportKey,
    pub state: State,
}
impl Report {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let b = fixed::<40>(bytes)?;
        let import = decode_key(&b[..24])?;
        let sequence = u64_at(b, 24);
        let code = u32_at(b, 32);
        if b[36..] != [0; 4] || (sequence == 0) != (code == 0) {
            return Err(Invalid);
        }
        let state = match code {
            0 => State::Idle,
            1 => State::Running { sequence },
            2..=7 => State::Completed {
                sequence,
                outcome: match code {
                    2 => Outcome::Safe,
                    3 => Outcome::Busy,
                    4 => Outcome::Denied,
                    5 => Outcome::Unsupported,
                    6 => Outcome::Unavailable,
                    7 => Outcome::Uncertain,
                    _ => unreachable!(),
                },
            },
            _ => return Err(Invalid),
        };
        Ok(Self { import, state })
    }
    pub fn encode(self) -> Result<[u8; 40]> {
        let (sequence, code) = match self.state {
            State::Idle => (0, 0u32),
            State::Running { sequence } => (sequence, 1),
            State::Completed { sequence, outcome } => (
                sequence,
                match outcome {
                    Outcome::Safe => 2,
                    Outcome::Busy => 3,
                    Outcome::Denied => 4,
                    Outcome::Unsupported => 5,
                    Outcome::Unavailable => 6,
                    Outcome::Uncertain => 7,
                },
            ),
        };
        let mut b = [0; 40];
        b[..24].copy_from_slice(&encode_key(self.import)?);
        b[24..32].copy_from_slice(&sequence.to_be_bytes());
        b[32..36].copy_from_slice(&code.to_be_bytes());
        Self::decode(&b)?;
        Ok(b)
    }
}
/// Android -> receiver. Retire withdraws this import, never cancels an OS worker.
/// A receiver must keep its attachment/ledger until that worker actually returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    Eject,
    Retire,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Directive {
    pub import: ImportKey,
    pub sequence: u64,
    pub action: Action,
}
impl Directive {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let b = fixed::<40>(bytes)?;
        let import = decode_key(&b[..24])?;
        let sequence = u64_at(b, 24);
        if b[36..] != [0; 4] {
            return Err(Invalid);
        }
        let action = match u32_at(b, 32) {
            0 if sequence == 0 => Action::Wait,
            1 if sequence > 0 => Action::Eject,
            2 => Action::Retire,
            _ => return Err(Invalid),
        };
        Ok(Self {
            import,
            sequence,
            action,
        })
    }
    pub fn encode(self) -> Result<[u8; 40]> {
        let mut b = [0; 40];
        b[..24].copy_from_slice(&encode_key(self.import)?);
        b[24..32].copy_from_slice(&self.sequence.to_be_bytes());
        let code: u32 = match self.action {
            Action::Wait => 0,
            Action::Eject => 1,
            Action::Retire => 2,
        };
        b[32..36].copy_from_slice(&code.to_be_bytes());
        Self::decode(&b)?;
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: ImportKey = ImportKey {
        session: [0x37; 16],
        number: 0x0102030405060708,
    };
    #[test]
    fn exact_fixed_framing_and_byte_order() {
        let key = encode_key(KEY).unwrap();
        assert_eq!(&key[16..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(decode_key(&key), Ok(KEY));
        let caps = Capabilities {
            session: KEY.session,
            features: FEATURES,
            max_imports: 4,
        };
        let b = caps.encode().unwrap();
        assert_eq!(Capabilities::decode(&b), Ok(caps));
        let mut busid = [0; 32];
        busid[..4].copy_from_slice(b"1-8\0");
        for wan in [false, true] {
            let request = ImportRequest {
                busid,
                wan,
                session: KEY.session,
            };
            assert_eq!(
                ImportRequest::decode(&request.encode().unwrap()),
                Ok(request)
            );
        }
        for state in [
            State::Idle,
            State::Running { sequence: 1 },
            State::Completed {
                sequence: u64::MAX,
                outcome: Outcome::Safe,
            },
            State::Completed {
                sequence: 3,
                outcome: Outcome::Busy,
            },
            State::Completed {
                sequence: 4,
                outcome: Outcome::Denied,
            },
            State::Completed {
                sequence: 5,
                outcome: Outcome::Unsupported,
            },
            State::Completed {
                sequence: 6,
                outcome: Outcome::Unavailable,
            },
            State::Completed {
                sequence: 7,
                outcome: Outcome::Uncertain,
            },
        ] {
            let report = Report { import: KEY, state };
            assert_eq!(Report::decode(&report.encode().unwrap()), Ok(report));
        }
        for (sequence, action) in [
            (0, Action::Wait),
            (1, Action::Eject),
            (u64::MAX, Action::Eject),
            (0, Action::Retire),
            (u64::MAX, Action::Retire),
        ] {
            let directive = Directive {
                import: KEY,
                sequence,
                action,
            };
            assert_eq!(
                Directive::decode(&directive.encode().unwrap()),
                Ok(directive)
            );
        }
    }
    #[test]
    fn all_lengths_reserved_fields_invalid_ids_and_states_fail_closed() {
        let caps = Capabilities {
            session: KEY.session,
            features: FEATURES,
            max_imports: 4,
        }
        .encode()
        .unwrap();
        let request = ImportRequest {
            busid: {
                let mut b = [0; 32];
                b[..3].copy_from_slice(b"1-8");
                b
            },
            wan: false,
            session: KEY.session,
        }
        .encode()
        .unwrap();
        let report = Report {
            import: KEY,
            state: State::Running { sequence: 1 },
        }
        .encode()
        .unwrap();
        let directive = Directive {
            import: KEY,
            sequence: 1,
            action: Action::Eject,
        }
        .encode()
        .unwrap();
        for n in 0..=256 {
            let expand = |b: &[u8]| {
                let mut out = vec![0; n];
                let len = n.min(b.len());
                out[..len].copy_from_slice(&b[..len]);
                out
            };
            if n != 32 {
                assert!(Capabilities::decode(&expand(&caps)).is_err());
            }
            if n != 52 {
                assert!(ImportRequest::decode(&expand(&request)).is_err());
            }
            if n != 40 {
                assert!(Report::decode(&expand(&report)).is_err());
                assert!(Directive::decode(&expand(&directive)).is_err());
            }
        }
        for at in 36..40 {
            let mut b = report;
            b[at] = 1;
            assert!(Report::decode(&b).is_err());
            let mut b = directive;
            b[at] = 1;
            assert!(Directive::decode(&b).is_err());
        }
        for (start, end) in [(0, 16), (16, 24), (24, 32)] {
            let mut b = report;
            b[start..end].fill(0);
            assert!(Report::decode(&b).is_err());
        }
        for code in [8, u32::MAX] {
            let mut b = report;
            b[32..36].copy_from_slice(&code.to_be_bytes());
            assert!(Report::decode(&b).is_err());
        }
        let mut b = request;
        b[31] = b'x';
        assert!(ImportRequest::decode(&b).is_err());
        for name in [
            b"/etc/passwd".as_slice(),
            b"C:\\disk",
            b"1-8\0x",
            b"",
            b"\xff",
        ] {
            let mut b = request;
            b[..32].fill(0);
            b[..name.len()].copy_from_slice(name);
            assert!(ImportRequest::decode(&b).is_err());
        }
        for (at, value) in [(4, 2u32), (8, 8), (8, 0), (12, 0), (12, 5)] {
            let mut b = caps;
            b[at..at + 4].copy_from_slice(&value.to_be_bytes());
            assert!(Capabilities::decode(&b).is_err());
        }
    }
}
