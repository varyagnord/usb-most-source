//! Actual Windows service/TLS/pairing/kernel HID and token reconnect.
#[cfg(windows)]
#[path = "receiver_lab/usb.rs"]
mod usb;
#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() {
    use base64::Engine;
    use std::{
        io::Write,
        net::TcpListener,
        path::PathBuf,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use usb_most_connection::{
        bridge::LocalBridge,
        config::Profile,
        pairing::PairCode,
        session::{Secret, Session},
    };
    use zeroize::Zeroizing;
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
    );
    let receiver = PathBuf::from(std::env::args().nth(1).unwrap())
        .canonicalize()
        .unwrap();
    fn idle(receiver: &std::path::Path) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let output = Command::new(receiver).arg("--health").output().unwrap();
            assert!(output.status.success());
            let health: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            if health["status"]["active"] == 0 && health["status"]["connected"] == false {
                return;
            }
            assert!(
                Instant::now() < end,
                "owned previous session did not retire"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    idle(&receiver);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut installer = Command::new(&receiver)
        .arg("--install-stdin")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    installer
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{{\"transport\":\"direct\",\"port\":{port}}}").as_bytes())
        .unwrap();
    assert!(installer.wait().unwrap().success());
    let first_host = if TcpListener::bind("[::1]:0").is_ok() {
        "::1"
    } else {
        "127.0.0.1"
    };
    let output = Command::new(&receiver)
        .args(["--pair", first_host])
        .output()
        .unwrap();
    assert!(output.status.success());
    let code = Zeroizing::new(String::from_utf8(output.stdout).unwrap());
    let parsed = PairCode::decode(code.trim()).unwrap();
    let mut profile = Profile::Direct {
        endpoint: parsed.endpoint.clone(),
        server_pin: parsed.server_pin.clone(),
    };
    let mut secret=Zeroizing::new(serde_json::to_vec(&serde_json::json!({"pair_code":code.trim(),"pair_nonce":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([23u8;16])})).unwrap());
    let fixture = usb::Fixture::start(true);
    for round in 0..2 {
        if round == 1 {
            if let Profile::Direct { endpoint, .. } = &mut profile {
                endpoint.host = "127.0.0.1".into();
            }
        }
        let session = Session::start(
            profile.clone(),
            Secret::parse(&secret).unwrap(),
            LocalBridge::new(fixture.address, fixture.proof).unwrap(),
            receiver.parent().unwrap().to_path_buf(),
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut connected = None;
        loop {
            if let Some(event) = session.poll() {
                let event: serde_json::Value = serde_json::from_str(&event).unwrap();
                match event["event"].as_str().unwrap() {
                    "paired" => {
                        assert_eq!(round, 0, "second session must reuse the grant");
                        secret = Zeroizing::new(
                            serde_json::to_vec(&serde_json::json!({"token":event["token"]}))
                                .unwrap(),
                        );
                        assert!(session.saved(event["save_id"].as_u64().unwrap(), true));
                    }
                    "state" => {
                        let code = event["code"].as_str().unwrap();
                        assert_ne!(code, "reconnecting");
                        if code == "connected" {
                            assert!(connected.is_none());
                            connected = Some(Instant::now());
                        }
                    }
                    other => panic!("direct Windows fixture event {other}: {}", event["code"]),
                }
            }
            if connected.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(15)) {
                break;
            }
            assert!(Instant::now() < deadline, "direct Windows fixture timeout");
            std::thread::sleep(Duration::from_millis(25));
        }
        session.stop();
        idle(&receiver);
        let stats = fixture.stats();
        assert_eq!(stats["imports"], round + 1);
        assert!(stats["controls"].as_u64().unwrap() > 0 && stats["reports"].as_u64().unwrap() > 0);
    }
    println!("PASS Windows TLS first address {first_host}, reconnect IPv4");
    println!(
        "PASS Windows TLS: real SCM receiver, pairing persistence acknowledgement, kernel HID controls/interrupts, saved-token reconnect, exactly two imports"
    );
}
