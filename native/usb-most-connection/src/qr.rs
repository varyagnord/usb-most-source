//! QR camera frames are untrusted input. Only a validated product pairing code
//! leaves this module; URLs, commands, oversized images and ambiguous frames do not.
use crate::pairing::PairCode;
use zeroize::Zeroizing;
pub const MAX_PIXELS: usize = 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Dimensions,
    Missing,
    Ambiguous,
}
pub fn decode(width: usize, height: usize, gray: &[u8]) -> Result<Zeroizing<String>, Error> {
    let pixels = width.checked_mul(height).ok_or(Error::Dimensions)?;
    if !(32..=1280).contains(&width)
        || !(32..=1280).contains(&height)
        || pixels > MAX_PIXELS
        || pixels != gray.len()
    {
        return Err(Error::Dimensions);
    }
    match decode_once(width, height, gray) {
        Err(Error::Missing) => {
            // Some finder-pattern orientations produce an unusable perspective
            // grid in quircs. One bounded quarter-turn retries detection without
            // accepting damaged ECC or weakening PairCode validation.
            let mut rotated = Zeroizing::new(vec![255; pixels]);
            for y in 0..height {
                for x in 0..width {
                    rotated[x * height + (height - y - 1)] = gray[y * width + x];
                }
            }
            decode_once(height, width, &rotated)
        }
        result => result,
    }
}
fn decode_once(width: usize, height: usize, gray: &[u8]) -> Result<Zeroizing<String>, Error> {
    let mut decoder = quircs::Quirc::default();
    let mut selected = None;
    // Cap extracted candidates even for a deliberately busy camera image.
    for (index, candidate) in decoder.identify(width, height, gray).take(17).enumerate() {
        if index == 16 {
            return Err(Error::Ambiguous);
        }
        let Ok(code) = candidate else {
            continue;
        };
        let Ok(data) = code.decode() else {
            continue;
        };
        let payload = Zeroizing::new(data.payload);
        if payload.len() > 2048 {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&payload) else {
            continue;
        };
        if PairCode::decode(text).is_err() {
            continue;
        }
        if selected.is_some() {
            return Err(Error::Ambiguous);
        }
        selected = Some(Zeroizing::new(text.to_owned()));
    }
    selected.ok_or(Error::Missing)
}
#[cfg(test)]
mod tests {
    use super::*;
    use qrcodegen::{QrCode, QrCodeEcc};
    fn code(host: &str) -> Zeroizing<String> {
        use base64::Engine;
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        PairCode {
            endpoint: crate::config::Endpoint {
                host: host.into(),
                port: 3241,
            },
            server_pin: b.encode([9; 32]),
            claim: b.encode([7; 32]),
        }
        .encode()
        .unwrap()
    }
    fn image(text: &str) -> (usize, Vec<u8>) {
        let qr = QrCode::encode_text(text, QrCodeEcc::Medium).unwrap();
        let size = (qr.size() as usize + 8) * 4;
        let mut pixels = vec![255; size * size];
        for y in 0..size {
            for x in 0..size {
                if qr.get_module(x as i32 / 4 - 4, y as i32 / 4 - 4) {
                    pixels[y * size + x] = 0;
                }
            }
        }
        (size, pixels)
    }
    #[test]
    fn qr_roundtrip_all_rotations_dns_and_ipv6() {
        for host in ["192.168.1.23", "computer.local", "2001:db8::123"] {
            let text = code(host);
            let (size, mut pixels) = image(&text);
            for rotation in 0..4 {
                assert_eq!(
                    decode(size, size, &pixels)
                        .unwrap_or_else(|e| panic!("{host}: rotation {rotation}: {e:?}"))
                        .as_str(),
                    text.as_str()
                );
                let mut next = vec![255; pixels.len()];
                for y in 0..size {
                    for x in 0..size {
                        next[x * size + (size - y - 1)] = pixels[y * size + x];
                    }
                }
                pixels = next;
            }
        }
    }
    #[test]
    fn frames_cannot_be_commands_overflows_or_ambiguous_pairings() {
        for (w, h, bytes) in [(0, 0, 0), (usize::MAX, 2, 0), (32, 32, 1), (1280, 1280, 0)] {
            assert_eq!(
                decode(w, h, &vec![0; bytes]).unwrap_err(),
                Error::Dimensions
            );
        }
        assert_eq!(decode(64, 64, &[255; 4096]).unwrap_err(), Error::Missing);
        for text in [
            "https://attacker.example",
            "https://www.rustore.ru/catalog/app/ru.example.usbmost.fixture",
            "usbmost:1:malformed",
            "ssh://user:pass@host",
        ] {
            let (size, pixels) = image(text);
            assert_eq!(decode(size, size, &pixels).unwrap_err(), Error::Missing);
        }
        let text = code("pc.local");
        let (size, pixels) = image(&text);
        let mut both = vec![255; size * size * 2];
        for y in 0..size {
            for x in 0..size {
                both[y * size * 2 + x] = pixels[y * size + x];
                both[y * size * 2 + size + x] = pixels[y * size + x];
            }
        }
        assert_eq!(decode(size * 2, size, &both).unwrap_err(), Error::Ambiguous);
    }
}
