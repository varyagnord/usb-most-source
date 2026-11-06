//! Owner-issued, expiring pairing claims and durable per-device capabilities.
use crate::{
    config::{Endpoint, decode_pin},
    security::random,
    vault::PrivateFile,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use zeroize::{Zeroize, Zeroizing};
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
pub const LIMIT: usize = 32;
pub type PeerId = [u8; 16];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Denied,
    Expired,
    Capacity,
    Storage,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    name: String,
    secret: String,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema: u32,
    peers: Vec<Record>,
}
struct Grant {
    name: String,
    token: Zeroizing<[u8; 32]>,
    revoke: watch::Sender<bool>,
}
pub struct Lease {
    pub id: PeerId,
    pub revoked: watch::Receiver<bool>,
}
struct Claim {
    token: Zeroizing<[u8; 32]>,
    expires: Instant,
    consumed: Option<([u8; 16], PeerId, String)>,
}
struct State {
    grants: Vec<Grant>,
    claim: Option<Claim>,
}
pub struct Store {
    state: Mutex<State>,
    file: PrivateFile,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairCode {
    pub endpoint: Endpoint,
    pub server_pin: String,
    pub claim: String,
}
impl Drop for PairCode {
    fn drop(&mut self) {
        self.claim.zeroize();
    }
}
impl PairCode {
    pub fn encode(&self) -> Result<Zeroizing<String>, Error> {
        self.endpoint.validate().map_err(|_| Error::Invalid)?;
        decode_pin(&self.server_pin).map_err(|_| Error::Invalid)?;
        token(&self.claim)?;
        let bytes = Zeroizing::new(serde_json::to_vec(self).map_err(|_| Error::Invalid)?);
        Ok(Zeroizing::new(format!(
            "usbmost:1:{}",
            B64.encode(bytes.as_slice())
        )))
    }
    pub fn decode(code: &str) -> Result<Self, Error> {
        if code.len() > 2048 {
            return Err(Error::Invalid);
        }
        let code = code.strip_prefix("usbmost:1:").ok_or(Error::Invalid)?;
        let bytes = Zeroizing::new(B64.decode(code).map_err(|_| Error::Invalid)?);
        let p: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
        p.endpoint.validate().map_err(|_| Error::Invalid)?;
        decode_pin(&p.server_pin).map_err(|_| Error::Invalid)?;
        token(&p.claim)?;
        Ok(p)
    }
}
pub fn token(encoded: &str) -> Result<Zeroizing<[u8; 32]>, Error> {
    let bytes = Zeroizing::new(B64.decode(encoded).map_err(|_| Error::Invalid)?);
    Ok(Zeroizing::new(
        bytes.as_slice().try_into().map_err(|_| Error::Invalid)?,
    ))
}
fn peer_id(secret: &[u8; 32]) -> PeerId {
    let hash = ring::digest::digest(&ring::digest::SHA256, secret);
    hash.as_ref()[..16].try_into().unwrap()
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && !name.chars().any(char::is_control)
}
impl Store {
    pub fn open(file: PrivateFile) -> Result<Arc<Self>, Error> {
        let mut grants = Vec::new();
        if let Some(bytes) = file.read().map_err(|_| Error::Storage)? {
            let snapshot: Snapshot = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
            if snapshot.schema != 1 || snapshot.peers.len() > LIMIT {
                return Err(Error::Storage);
            }
            for p in &snapshot.peers {
                if !valid_name(&p.name) {
                    return Err(Error::Storage);
                }
                let token = token(&p.secret).map_err(|_| Error::Storage)?;
                if grants
                    .iter()
                    .any(|p: &Grant| peer_id(&p.token) == peer_id(&token))
                {
                    return Err(Error::Storage);
                }
                grants.push(Grant {
                    name: p.name.clone(),
                    token,
                    revoke: watch::channel(false).0,
                });
            }
        }
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                grants,
                claim: None,
            }),
            file,
        }))
    }
    fn persist(&self, grants: &[Grant]) -> Result<(), Error> {
        let snapshot = Snapshot {
            schema: 1,
            peers: grants
                .iter()
                .map(|p| Record {
                    name: p.name.clone(),
                    secret: B64.encode(p.token.as_slice()),
                })
                .collect(),
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&snapshot).map_err(|_| Error::Storage)?);
        self.file.replace(&bytes).map_err(|_| Error::Storage)
    }
    pub fn challenge(
        &self,
        endpoint: Endpoint,
        server_pin: String,
        lifetime: Duration,
    ) -> Result<PairCode, Error> {
        endpoint.validate().map_err(|_| Error::Invalid)?;
        decode_pin(&server_pin).map_err(|_| Error::Invalid)?;
        if lifetime.is_zero() || lifetime > Duration::from_secs(300) {
            return Err(Error::Invalid);
        }
        let token = Zeroizing::new(random::<32>().map_err(|_| Error::Invalid)?);
        let code = PairCode {
            endpoint,
            server_pin,
            claim: B64.encode(token.as_slice()),
        };
        let mut s = self.state.lock().map_err(|_| Error::Storage)?;
        s.claim = Some(Claim {
            token,
            expires: Instant::now() + lifetime,
            consumed: None,
        });
        Ok(code)
    }
    /// A successful mutation is persisted before the capability is returned.
    /// Only an exact retry nonce can recover a response lost in transit.
    pub fn redeem(
        &self,
        claim: &[u8; 32],
        nonce: [u8; 16],
        name: &str,
    ) -> Result<Zeroizing<[u8; 32]>, Error> {
        if !valid_name(name) || nonce == [0; 16] {
            return Err(Error::Invalid);
        }
        let mut s = self.state.lock().map_err(|_| Error::Storage)?;
        let c = s.claim.as_ref().ok_or(Error::Denied)?;
        if Instant::now() >= c.expires {
            return Err(Error::Expired);
        }
        if !bool::from(c.token.as_slice().ct_eq(claim)) {
            return Err(Error::Denied);
        }
        if let Some((previous, id, previous_name)) = &c.consumed {
            if previous != &nonce || previous_name != name {
                return Err(Error::Denied);
            }
            return s
                .grants
                .iter()
                .find(|g| peer_id(&g.token) == *id)
                .map(|g| Zeroizing::new(*g.token))
                .ok_or(Error::Denied);
        }
        if s.grants.len() >= LIMIT {
            return Err(Error::Capacity);
        }
        let token = Zeroizing::new(random::<32>().map_err(|_| Error::Invalid)?);
        let id = peer_id(&token);
        s.grants.push(Grant {
            name: name.into(),
            token: Zeroizing::new(*token),
            revoke: watch::channel(false).0,
        });
        if let Err(error) = self.persist(&s.grants) {
            s.grants.pop();
            return Err(error);
        }
        s.claim.as_mut().unwrap().consumed = Some((nonce, id, name.into()));
        Ok(token)
    }
    pub fn authenticate(&self, token: &[u8; 32]) -> Result<Lease, Error> {
        let s = self.state.lock().map_err(|_| Error::Storage)?;
        let g = s
            .grants
            .iter()
            .find(|g| bool::from(g.token.as_slice().ct_eq(token)))
            .ok_or(Error::Denied)?;
        Ok(Lease {
            id: peer_id(&g.token),
            revoked: g.revoke.subscribe(),
        })
    }
    pub fn peers(&self) -> Result<Vec<(PeerId, String)>, Error> {
        Ok(self
            .state
            .lock()
            .map_err(|_| Error::Storage)?
            .grants
            .iter()
            .map(|g| (peer_id(&g.token), g.name.clone()))
            .collect())
    }
    pub fn revoke(&self, id: PeerId) -> Result<(), Error> {
        let mut s = self.state.lock().map_err(|_| Error::Storage)?;
        let at = s
            .grants
            .iter()
            .position(|g| peer_id(&g.token) == id)
            .ok_or(Error::Denied)?;
        let grant = s.grants.remove(at);
        if let Err(e) = self.persist(&s.grants) {
            s.grants.insert(at, grant);
            return Err(e);
        }
        let _ = grant.revoke.send(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PrivateFile, Arc<Store>) {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let file = PrivateFile::new(dir.path().join("pairs"), 16384).unwrap();
        let store = Store::open(file.clone()).unwrap();
        (dir, file, store)
    }
    fn challenge(store: &Store, lifetime: Duration) -> PairCode {
        store
            .challenge(
                Endpoint {
                    host: "pc.local".into(),
                    port: 3244,
                },
                crate::config::encode_pin(b"fixture certificate"),
                lifetime,
            )
            .unwrap()
    }
    #[test]
    fn pairing_is_persisted_idempotent_and_revocable() {
        let (_dir, file, s) = fixture();
        let code = challenge(&s, Duration::from_secs(30));
        let serialized = code.encode().unwrap();
        let parsed = PairCode::decode(&serialized).unwrap();
        assert_eq!(parsed.endpoint, code.endpoint);
        let claim = token(&code.claim).unwrap();
        assert!(s.redeem(&[0; 32], [1; 16], "Tablet").is_err());
        let key = s.redeem(&claim, [1; 16], "Tablet").unwrap();
        assert_eq!(*key, *s.redeem(&claim, [1; 16], "Tablet").unwrap());
        assert!(s.redeem(&claim, [2; 16], "Tablet").is_err());
        assert!(s.redeem(&claim, [1; 16], "Other").is_err());
        assert_eq!(s.peers().unwrap().len(), 1);
        let lease = s.authenticate(&key).unwrap();
        let restored = Store::open(file).unwrap();
        assert_eq!(restored.authenticate(&key).unwrap().id, lease.id);
        s.revoke(lease.id).unwrap();
        assert!(*lease.revoked.borrow());
        assert!(s.authenticate(&key).is_err());
        assert!(PairCode::decode("usbmost:2:malformed").is_err());
        assert!(PairCode::decode(&"x".repeat(2049)).is_err());
    }
    #[test]
    fn expired_and_superseded_claims_never_pair() {
        let (_dir, _file, s) = fixture();
        let c = challenge(&s, Duration::from_millis(1));
        let old = token(&c.claim).unwrap();
        std::thread::sleep(Duration::from_millis(3));
        assert!(matches!(
            s.redeem(&old, [1; 16], "Tablet"),
            Err(Error::Expired)
        ));
        let _new = challenge(&s, Duration::from_secs(30));
        assert!(s.redeem(&old, [1; 16], "Tablet").is_err());
    }
    #[test]
    fn no_success_if_durable_storage_rejects_the_grant() {
        let (dir, file, s) = fixture();
        let code = challenge(&s, Duration::from_secs(30));
        let claim = token(&code.claim).unwrap();
        std::fs::create_dir(dir.path().join("pairs")).unwrap();
        assert!(matches!(
            s.redeem(&claim, [1; 16], "Tablet"),
            Err(Error::Storage)
        ));
        assert!(s.peers().unwrap().is_empty());
        std::fs::remove_dir(dir.path().join("pairs")).unwrap();
        s.redeem(&claim, [1; 16], "Tablet").unwrap();
        assert!(Store::open(file).is_ok());
    }
}
