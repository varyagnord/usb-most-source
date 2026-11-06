//! Synthetic QR fixtures shared by real JVM/ART JNI tests. No production token.
use base64::Engine;
use qrcodegen::{QrCode, QrCodeEcc};
use std::{fs, path::PathBuf};
use usb_most_connection::{config::Endpoint, pairing::PairCode};
fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("output directory under build/"),
    );
    fs::create_dir_all(&out).unwrap();
    let out = out.canonicalize().unwrap();
    assert!(out.starts_with(root.join("build")));
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let code = PairCode {
        endpoint: Endpoint {
            host: "192.0.2.42".into(),
            port: 3241,
        },
        server_pin: b64.encode([9; 32]),
        claim: b64.encode([7; 32]),
    }
    .encode()
    .unwrap();
    let qr = QrCode::encode_text(&code, QrCodeEcc::Medium).unwrap();
    let size = (qr.size() as usize + 8) * 4;
    let mut image = format!("P5\n{size} {size}\n255\n").into_bytes();
    for y in 0..size {
        for x in 0..size {
            image.push(if qr.get_module(x as i32 / 4 - 4, y as i32 / 4 - 4) {
                0
            } else {
                255
            });
        }
    }
    fs::write(out.join("pairing.pgm"), image).unwrap();
    fs::write(out.join("pairing.txt"), code.as_bytes()).unwrap();
    println!("Synthetic QR fixture: {size}x{size}");
}
