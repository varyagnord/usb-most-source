//! Bound imported key work before invoking a cryptographic parser. Arbitrary
//! encrypted PEM/PPK KDF parameters are deliberately not accepted.
use base64::Engine;
use zeroize::Zeroizing;
struct Ssh<'a>(&'a [u8]);
impl<'a> Ssh<'a> {
    fn u32(&mut self) -> Result<u32, ()> {
        if self.0.len() < 4 {
            return Err(());
        }
        let n = u32::from_be_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(n)
    }
    fn string(&mut self) -> Result<&'a [u8], ()> {
        let n = self.u32()? as usize;
        if n > self.0.len() {
            return Err(());
        }
        let (s, r) = self.0.split_at(n);
        self.0 = r;
        Ok(s)
    }
}
struct Der<'a>(&'a [u8]);
impl<'a> Der<'a> {
    fn node(&mut self, tag: u8) -> Result<&'a [u8], ()> {
        if self.0.len() < 2 || self.0[0] != tag {
            return Err(());
        }
        let n = self.0[1];
        self.0 = &self.0[2..];
        let n = if n < 128 {
            usize::from(n)
        } else {
            let count = usize::from(n & 127);
            if count == 0 || count > 3 || self.0.len() < count || self.0[0] == 0 {
                return Err(());
            }
            let n = self.0[..count]
                .iter()
                .fold(0usize, |n, b| n * 256 + usize::from(*b));
            self.0 = &self.0[count..];
            if n < 128 {
                return Err(());
            }
            n
        };
        if n > self.0.len() {
            return Err(());
        }
        let (bytes, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(bytes)
    }
}
fn rsa_modulus(bytes: &[u8]) -> Result<(), ()> {
    if bytes.is_empty() || bytes.len() > 1025 {
        return Err(());
    }
    let bytes = bytes.strip_prefix(&[0]).unwrap_or(bytes);
    if !(256..=1024).contains(&bytes.len()) || bytes[0] == 0 {
        return Err(());
    }
    Ok(())
}
fn rsa_der(bytes: &[u8]) -> Result<(), ()> {
    let mut outer = Der(bytes);
    let mut inner = Der(outer.node(0x30)?);
    if !outer.0.is_empty() {
        return Err(());
    }
    if inner.node(2)? != [0] {
        return Err(());
    }
    rsa_modulus(inner.node(2)?)
}
pub fn validate(text: &str) -> Result<(), ()> {
    if text.len() > 65536 {
        return Err(());
    }
    let text = text.trim_start_matches('\u{feff}').trim();
    let (kind, body) = if let Some(b) = text.strip_prefix("-----BEGIN OPENSSH PRIVATE KEY-----") {
        (
            0,
            b.strip_suffix("-----END OPENSSH PRIVATE KEY-----")
                .ok_or(())?,
        )
    } else if let Some(b) = text.strip_prefix("-----BEGIN RSA PRIVATE KEY-----") {
        (
            1,
            b.strip_suffix("-----END RSA PRIVATE KEY-----").ok_or(())?,
        )
    } else if let Some(b) = text.strip_prefix("-----BEGIN PRIVATE KEY-----") {
        (2, b.strip_suffix("-----END PRIVATE KEY-----").ok_or(())?)
    } else if let Some(b) = text.strip_prefix("-----BEGIN EC PRIVATE KEY-----") {
        (3, b.strip_suffix("-----END EC PRIVATE KEY-----").ok_or(())?)
    } else {
        return Err(());
    };
    let compact = Zeroizing::new(
        body.bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect::<Vec<_>>(),
    );
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(compact.as_slice())
            .map_err(|_| ())?,
    );
    if kind == 1 {
        return rsa_der(&bytes);
    }
    if kind == 3 {
        return if bytes.len() <= 1024 { Ok(()) } else { Err(()) };
    }
    if kind == 2 {
        let mut outer = Der(&bytes);
        let mut body = Der(outer.node(0x30)?);
        if !outer.0.is_empty() {
            return Err(());
        }
        let version = body.node(2)?;
        if version != [0] && version != [1] {
            return Err(());
        }
        let mut algorithm = Der(body.node(0x30)?);
        let oid = algorithm.node(6)?;
        let key = body.node(4)?;
        return match oid {
            b"\x2a\x86\x48\x86\xf7\x0d\x01\x01\x01" => rsa_der(key),
            b"\x2b\x65\x70" if bytes.len() <= 256 => Ok(()),
            b"\x2a\x86\x48\xce\x3d\x02\x01" if bytes.len() <= 1024 => Ok(()),
            _ => Err(()),
        };
    }
    let mut ssh = Ssh(bytes.strip_prefix(b"openssh-key-v1\0").ok_or(())?);
    let cipher = ssh.string()?;
    let kdf = ssh.string()?;
    let options = ssh.string()?;
    if kdf == b"bcrypt" {
        if ![
            b"aes256-ctr".as_slice(),
            b"aes256-cbc",
            b"aes128-ctr",
            b"aes192-ctr",
        ]
        .contains(&cipher)
        {
            return Err(());
        }
        let mut cost = Ssh(options);
        if cost.string()?.len() > 64 {
            return Err(());
        }
        let rounds = cost.u32()?;
        if !(1..=128).contains(&rounds) || !cost.0.is_empty() {
            return Err(());
        }
    } else if kdf != b"none" || cipher != b"none" || !options.is_empty() {
        return Err(());
    }
    if ssh.u32()? != 1 {
        return Err(());
    }
    let mut public = Ssh(ssh.string()?);
    let name = public.string()?;
    match name {
        b"ssh-rsa" => {
            if public.string()?.len() > 8 {
                return Err(());
            }
            rsa_modulus(public.string()?)?;
        }
        b"ssh-ed25519" => {
            if public.string()?.len() != 32 {
                return Err(());
            }
        }
        b"ecdsa-sha2-nistp256" | b"ecdsa-sha2-nistp384" | b"ecdsa-sha2-nistp521" => {
            if public.string()?.len() > 16 || public.string()?.len() > 133 {
                return Err(());
            }
        }
        _ => return Err(()),
    }
    if !public.0.is_empty() || ssh.string()?.len() > 49152 || !ssh.0.is_empty() {
        return Err(());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unbounded_kdf_before_decryption() {
        fn string(bytes: &[u8], out: &mut Vec<u8>) {
            out.extend((bytes.len() as u32).to_be_bytes());
            out.extend(bytes);
        }
        for rounds in [0u32, 129, u32::MAX] {
            let mut b = b"openssh-key-v1\0".to_vec();
            string(b"aes256-ctr", &mut b);
            string(b"bcrypt", &mut b);
            let mut opts = Vec::new();
            string(&[1; 16], &mut opts);
            opts.extend(rounds.to_be_bytes());
            string(&opts, &mut b);
            let pem = format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----",
                base64::engine::general_purpose::STANDARD.encode(b)
            );
            assert!(validate(&pem).is_err());
        }
        for kind in ["ENCRYPTED PRIVATE KEY", "PuTTY-User-Key-File-3"] {
            assert!(
                validate(&format!(
                    "-----BEGIN {kind}-----\nAAAA\n-----END {kind}-----"
                ))
                .is_err()
            );
        }
    }
}
