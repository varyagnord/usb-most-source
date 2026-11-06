//! Safe decoder for the pinned usbip-win2 0.9.8.1 GET_IMPORTED_DEVICES ABI.
//! A port alone is not ownership: match its one-use alias and private proxy hop.
use std::io;
pub(crate) const RECORD: usize = 1132;
pub(crate) const HEADER: usize = 4;
pub(crate) const STRUCT_SIZE: usize = HEADER + RECORD;
pub(crate) const CAPACITY: usize = HEADER + 255 * RECORD;

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn cstring(bytes: &[u8]) -> io::Result<&[u8]> {
    let end = bytes.iter().position(|b| *b == 0).ok_or_else(invalid)?;
    Ok(&bytes[..end])
}
pub(crate) fn validate(port: i32, local_port: u16, alias: &[u8; 32]) -> io::Result<()> {
    if !(1..=255).contains(&port)
        || local_port == 0
        || alias[30..] != [0, 0]
        || !alias[..30].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(())
}
pub(crate) fn matches(
    bytes: &[u8],
    port: i32,
    local_port: u16,
    alias: &[u8; 32],
) -> io::Result<bool> {
    validate(port, local_port, alias)?;
    if !(HEADER..=CAPACITY).contains(&bytes.len())
        || !(bytes.len() - HEADER).is_multiple_of(RECORD)
        || u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize != STRUCT_SIZE
    {
        return Err(invalid());
    }
    let service = local_port.to_string();
    let mut seen = [false; 256];
    let mut matched = false;
    for record in bytes[HEADER..].as_chunks::<RECORD>().0 {
        let number = i32::from_le_bytes(record[..4].try_into().unwrap());
        if !(1..=255).contains(&number) || std::mem::replace(&mut seen[number as usize], true) {
            return Err(invalid());
        }
        let busid = cstring(&record[8..40])?;
        let proxy_port = cstring(&record[40..72])?;
        let host = cstring(&record[72..1097])?;
        if number == port {
            matched =
                busid == &alias[..30] && proxy_port == service.as_bytes() && host == b"127.0.0.1";
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ALIAS: &[u8; 32] = b"0123456789abcdef0123456789abcd\0\0";
    fn fixture() -> Vec<u8> {
        let mut bytes = vec![0; STRUCT_SIZE];
        bytes[..4].copy_from_slice(&(STRUCT_SIZE as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(&7i32.to_le_bytes());
        bytes[12..44].copy_from_slice(ALIAS);
        bytes[44..49].copy_from_slice(b"49152");
        bytes[76..85].copy_from_slice(b"127.0.0.1");
        bytes
    }
    #[test]
    fn port_alias_and_private_proxy_must_all_match() {
        let b = fixture();
        assert!(matches(&b, 7, 49152, ALIAS).unwrap());
        assert!(!matches(&b, 8, 49152, ALIAS).unwrap());
        assert!(!matches(&b, 7, 49153, ALIAS).unwrap());
        let mut alias = *ALIAS;
        alias[0] = b'f';
        assert!(!matches(&b, 7, 49152, &alias).unwrap());
        let mut other = b.clone();
        other[76] = b'9';
        assert!(!matches(&other, 7, 49152, ALIAS).unwrap());
        assert!(!matches(&b[..4], 7, 49152, ALIAS).unwrap());
    }
    #[test]
    fn bounded_framing_rejects_ambiguous_and_malformed_driver_results() {
        let b = fixture();
        for size in 0..b.len() {
            if size != HEADER {
                assert!(matches(&b[..size], 7, 49152, ALIAS).is_err());
            }
        }
        let mut duplicate = b.clone();
        duplicate.extend_from_slice(&b[4..]);
        assert!(matches(&duplicate, 7, 49152, ALIAS).is_err());
        let mut bad = b.clone();
        bad[4..8].copy_from_slice(&256i32.to_le_bytes());
        assert!(matches(&bad, 7, 49152, ALIAS).is_err());
        for range in [12..44, 44..76, 76..1101] {
            let mut bad = b.clone();
            bad[range].fill(b'x');
            assert!(matches(&bad, 7, 49152, ALIAS).is_err());
        }
        let mut huge = vec![0; CAPACITY + RECORD];
        huge[..4].copy_from_slice(&b[..4]);
        assert!(matches(&huge, 7, 49152, ALIAS).is_err());
        let mut old_abi = b.clone();
        old_abi[..4].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches(&old_abi, 7, 49152, ALIAS).is_err());
        for port in [-2, -1, 0, 256] {
            assert!(matches(&b, port, 49152, ALIAS).is_err());
        }
    }
}
