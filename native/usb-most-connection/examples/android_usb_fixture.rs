//! Synthetic USB endpoint for an explicitly owned emulator integration test.
#[path = "receiver_lab/usb.rs"]
mod usb;
fn main() {
    use base64::Engine;
    use std::{fs, io::Write, path::PathBuf, time::Duration};
    assert_eq!(
        std::env::var("USB_MOST_OWNED_EMULATOR_LAB").as_deref(),
        Ok("1")
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let output = PathBuf::from(std::env::args().nth(1).unwrap())
        .canonicalize()
        .unwrap();
    assert!(output.starts_with(root.join("build/e2e")));
    let fixture = usb::Fixture::start(true);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("bridge.json"))
        .unwrap();
    file.write_all(serde_json::to_string(&serde_json::json!({"port":fixture.address.port(),"proof":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(fixture.proof)})).unwrap().as_bytes()).unwrap();
    file.sync_all().unwrap();
    for _ in 0..6000 {
        if output.join("stop-fixture").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    fs::write(
        output.join("usb-result.json"),
        serde_json::to_vec(&fixture.stats()).unwrap(),
    )
    .unwrap();
}
