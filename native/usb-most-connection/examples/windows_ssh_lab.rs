//! Embedded Session -> actual Windows OpenSSH -> SCM -> synthetic HID.
//! Explicit disposable VM only; no physical USB or user credentials.
#[cfg(windows)]
#[path = "receiver_lab/usb.rs"]
mod usb;
#[cfg(not(windows))]
fn main() {}
#[cfg(windows)]
fn main() {
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, Instant},
    };
    use usb_most_connection::{
        bridge::LocalBridge,
        config::{Endpoint, Profile},
        session::{Secret, Session},
    };
    assert!(
        std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true")
            || std::env::var("USB_MOST_WINDOWS_VM_LAB").is_ok_and(|v| v == "1")
    );
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 2);
    let lab = PathBuf::from(&args[0]).canonicalize().unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(lab.join("fixture.json")).unwrap()).unwrap();
    assert_eq!(config["scope"], "isolated-Windows-OpenSSH-only");
    let port = u16::try_from(config["port"].as_u64().unwrap()).unwrap();
    let user = config["user"].as_str().unwrap();
    assert_eq!(user, "usbmostsshfixture");
    let private = zeroize::Zeroizing::new(fs::read_to_string(lab.join("client")).unwrap());
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "private_key": private.as_str(), "client_id": "0123456789abcdef0123456789abcdef"
        }))
        .unwrap(),
    );
    let fixture = usb::Fixture::start(true);
    let profile = Profile::Ssh {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port,
        },
        user: user.into(),
        host_key: None,
        reverse_port: 0,
        prepare: true,
    };
    // Diagnose fixed read-only probes before mutation. This fixture contains
    // only synthetic accounts/keys; never enable these logs in the product.
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use usb_most_connection::{
            bootstrap,
            ssh::{Credential, SshClient},
        };
        let (approval, mut pending) =
            tokio::sync::mpsc::channel::<usb_most_connection::ssh::TrustRequest>(1);
        let trust = tokio::spawn(async move {
            while let Some(request) = pending.recv().await {
                let _ = request.reply.send(true);
            }
        });
        let client = SshClient::connect(
            &profile,
            Credential::Key {
                private: private.clone(),
                passphrase: None,
            },
            LocalBridge::new(fixture.address, fixture.proof).unwrap(),
            approval,
        )
        .await
        .unwrap();
        for (label, command) in [
            ("platform", "uname -s"),
            ("windows", bootstrap::windows_probe_command()),
        ] {
            let started = Instant::now();
            let reply = client
                .exec(
                    command,
                    zeroize::Zeroizing::new(b"12345678123456781234567812345678\n".to_vec()),
                    Duration::from_secs(30),
                )
                .await;
            match reply {
                Ok(reply) => println!(
                    "PROBE {label}: elapsed_ms={} exit={} stdout={} stderr={}",
                    started.elapsed().as_millis(),
                    reply.exit,
                    String::from_utf8_lossy(&reply.stdout),
                    String::from_utf8_lossy(&reply.stderr)
                ),
                Err(e) => panic!("PROBE {label}: {e:?}"),
            }
        }
        let probe = bootstrap::probe(&client)
            .await
            .expect("nonce-bound Windows SSH probe");
        assert_eq!(probe.os, usb_most_connection::provision::Os::Windows);
        assert!(probe.elevated);
        if args[1] != "--probe-only" {
            assert!(probe.usb_stack);
        }
        drop(client);
        trust.abort();
    });
    if args[1] == "--probe-only" {
        println!("PASS embedded Windows OpenSSH platform detection and nonce-bound probe");
        return;
    }
    let session = Session::start(
        profile,
        Secret::parse(&bytes).unwrap(),
        LocalBridge::new(fixture.address, fixture.proof).unwrap(),
        PathBuf::from(&args[1]).canonicalize().unwrap(),
    );
    let start = Instant::now();
    let mut connected = None;
    loop {
        if let Some(event) = session.poll() {
            let event: serde_json::Value = serde_json::from_str(&event).unwrap();
            match event["event"].as_str().unwrap() {
                "trust" => assert!(session.approve(event["pin"].as_str().unwrap(), true)),
                "trusted" => assert!(session.saved(event["save_id"].as_u64().unwrap(), true)),
                "state" => {
                    let code = event["code"].as_str().unwrap();
                    println!("STATE {code}");
                    if code == "connected" {
                        connected = Some(Instant::now());
                    }
                }
                "error" => panic!("Windows SSH fixture session error {}", event["code"]),
                "closed" => panic!("Windows SSH fixture closed early"),
                _ => panic!("unexpected fixture event"),
            }
        }
        if connected.is_some_and(|time: Instant| time.elapsed() >= Duration::from_secs(15)) {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(240),
            "Windows SSH fixture timeout"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    session.stop();
    let stats = fixture.stats();
    for field in ["imports", "controls", "reports"] {
        assert!(
            stats[field].as_u64().unwrap() > 0,
            "actual Windows kernel {field} required"
        );
    }
    assert_eq!(stats["imports"], 1, "stable session must not reimport");
    println!(
        "PASS Windows OpenSSH: embedded Session, host trust/save acknowledgement, reverse forwarding, real service preparation and synthetic HID enumeration/interrupts"
    );
}
