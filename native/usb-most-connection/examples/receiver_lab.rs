//! Isolated VM E2E harness, never a receiver installer for the developer's host.
//! The target is restricted to the loopback QEMU fixture prepared by linux-vm-lab.py.
#[path = "receiver_lab/usb.rs"]
mod usb;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
use usb_most_connection::{
    bridge::LocalBridge,
    config::{Endpoint, Profile},
    session::{Secret, Session},
};
use zeroize::Zeroizing;
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert!(
        (3..=4).contains(&args.len()),
        "receiver_lab LAB ARTIFACTS CONNECTED_SECONDS [HANDSHAKE_COUNT]"
    );
    let lab = fs::canonicalize(&args[0]).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(lab.starts_with(root.join("build/vm-lab")));
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(lab.join("connection.json")).unwrap()).unwrap();
    assert_eq!(metadata["ssh_host"], "127.0.0.1");
    assert_eq!(metadata["ssh_user"], "fixture");
    let pid = fs::read_to_string(lab.join("pid"))
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let cmd = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    assert!(
        cmd.windows(b"qemu-system-x86_64".len())
            .any(|v| v == b"qemu-system-x86_64")
    );
    let disk = lab
        .join("guest.qcow2")
        .to_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    assert!(cmd.windows(disk.len()).any(|v| v == disk));
    let port = u16::try_from(metadata["ssh_port"].as_u64().unwrap()).unwrap();
    let direct = args.get(3).is_some_and(|mode| mode == "direct");
    if let Some(rounds) = args.get(3).filter(|_| !direct) {
        handshake_stress(&lab, port, rounds.parse().unwrap());
        return;
    }
    let seconds = args[2].parse::<u64>().unwrap();
    assert!((1..=86_400).contains(&seconds));
    let fixture = usb::Fixture::start(true);
    let mut profile = Profile::Ssh {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port,
        },
        user: "fixture".into(),
        host_key: None,
        reverse_port: 0,
        prepare: true,
    };
    let private = Zeroizing::new(fs::read_to_string(lab.join("client")).unwrap());
    let json=Zeroizing::new(serde_json::to_vec(&serde_json::json!({"private_key":private.as_str(),"client_id":"0123456789abcdef0123456789abcdef"})).unwrap());
    let mut secret = Secret::parse(&json).unwrap();
    if direct {
        use base64::Engine;
        let code = Zeroizing::new(fs::read_to_string(lab.join("direct-pair-code")).unwrap());
        let parsed = usb_most_connection::pairing::PairCode::decode(code.trim()).unwrap();
        assert_eq!(parsed.endpoint.host, "127.0.0.1", "owned QEMU forward only");
        profile = Profile::Direct {
            endpoint: parsed.endpoint.clone(),
            server_pin: parsed.server_pin.clone(),
        };
        let stored = lab.join("direct-token");
        let value = if stored.exists() {
            serde_json::json!({"token": fs::read_to_string(stored).unwrap()})
        } else {
            serde_json::json!({"pair_code": code.trim(), "pair_nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([19u8;16])})
        };
        secret = Secret::parse(&Zeroizing::new(serde_json::to_vec(&value).unwrap())).unwrap();
    }
    let session = Session::start(
        profile,
        secret,
        LocalBridge::new(fixture.address, fixture.proof).unwrap(),
        fs::canonicalize(&args[1]).unwrap(),
    );
    let started = Instant::now();
    let mut connected = None;
    let result = loop {
        if let Some(event) = session.poll() {
            let value: serde_json::Value = serde_json::from_str(&event).unwrap();
            match value["event"].as_str().unwrap() {
                "trust" => {
                    assert!(session.approve(value["pin"].as_str().unwrap(), true));
                    println!("PASS isolated fixture host key approved");
                }
                "trusted" => {
                    let mut saved = fs::OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(lab.join("trusted-pin.json"))
                        .unwrap();
                    saved.write_all(event.as_bytes()).unwrap();
                    saved.sync_all().unwrap();
                    assert!(session.saved(value["save_id"].as_u64().unwrap(), true));
                    println!("PASS trust saved before preparation");
                }
                "paired" if direct => {
                    let mut options = fs::OpenOptions::new();
                    options.create(true).truncate(true).write(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    let mut saved = options.open(lab.join("direct-token")).unwrap();
                    saved
                        .write_all(value["token"].as_str().unwrap().as_bytes())
                        .unwrap();
                    saved.sync_all().unwrap();
                    assert!(session.saved(value["save_id"].as_u64().unwrap(), true));
                    println!("PASS pairing saved before USB exposure");
                }
                "state" => {
                    let code = value["code"].as_str().unwrap();
                    println!("STATE {code}");
                    if code == "connected" {
                        connected = Some(Instant::now());
                    }
                }
                "error" => break Err(format!("session error {}", value["code"])),
                "closed" => break Err("session closed early".to_owned()),
                _ => panic!("unexpected fixture event"),
            }
        }
        if connected.is_some_and(|at| at.elapsed() >= Duration::from_secs(seconds)) {
            break Ok(());
        }
        // Startup has a separate deadline. Long-lived stability runs must not
        // be cut off by the former unconditional four-minute fixture timeout.
        if connected.is_none() && started.elapsed() > Duration::from_secs(240) {
            break Err("fixture timeout".to_owned());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    session.stop();
    let stats = fixture.stats();
    fs::write(
        lab.join("usb-result.json"),
        serde_json::to_vec_pretty(&stats).unwrap(),
    )
    .unwrap();
    println!("USB fixture counters {stats}");
    result.unwrap();
    assert!(
        stats["imports"].as_u64().unwrap() > 0,
        "actual VHCI import required"
    );
    assert!(
        stats["controls"].as_u64().unwrap() > 0,
        "actual kernel enumeration required"
    );
    assert!(
        stats["reports"].as_u64().unwrap() > 0,
        "actual interrupt requests required"
    );
}

/// Investigate an observed slow-link handshake/MAC failure independently of
/// USB, provisioning and reconnection. Each round owns a fresh SSH transport.
fn handshake_stress(lab: &std::path::Path, port: u16, rounds: usize) {
    use usb_most_connection::ssh::{Credential, SshClient, TrustRequest};
    assert!((1..=100).contains(&rounds));
    // Test executable only. Whitelist protocol metadata; never print auth
    // messages, private keys, packet contents or arbitrary library records.
    struct MetadataLog;
    impl log::Log for MetadataLog {
        fn enabled(&self, record: &log::Metadata<'_>) -> bool {
            record.target() == "russh::client" || record.target() == "russh::client::kex"
        }
        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let message = record.args().to_string();
            if [
                "negotiated algorithms:",
                "< msg type",
                "kex ",
                "disconnected",
                "ssh id",
                "drop ",
            ]
            .iter()
            .any(|prefix| message.starts_with(prefix))
            {
                eprintln!("SSH_METADATA {message}");
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: MetadataLog = MetadataLog;
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(lab.join("trusted-pin.json")).unwrap()).unwrap();
    let profile = Profile::Ssh {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port,
        },
        user: "fixture".into(),
        host_key: Some(saved["pin"].as_str().unwrap().into()),
        reverse_port: 0,
        prepare: false,
    };
    let key = Zeroizing::new(fs::read_to_string(lab.join("client")).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (approval, mut questions) = tokio::sync::mpsc::channel::<TrustRequest>(1);
        let deny = tokio::spawn(async move {
            while let Some(q) = questions.recv().await { let _ = q.reply.send(false); }
        });
        let mut failed = Vec::new();
        for index in 0..rounds {
            eprintln!("BEGIN independent SSH {}", index + 1);
            let result = async {
                // No forwarded channel is opened and no USB or root action is
                // involved. The bridge endpoint is deliberately unreachable.
                let client = SshClient::connect(&profile,
                    Credential::Key { private: Zeroizing::new(key.to_string()), passphrase: None },
                    LocalBridge::new("127.0.0.1:1".parse().unwrap(), [91;32]).unwrap(),
                    approval.clone()).await?;
                let output = client.exec("printf USB_MOST_SSH_STRESS_OK", Zeroizing::new(vec![]),
                    Duration::from_secs(30)).await?;
                assert_eq!(output.exit, 0);
                assert_eq!(output.stdout.as_slice(), b"USB_MOST_SSH_STRESS_OK");
                Ok::<_, usb_most_connection::ssh::Error>(())
            }.await;
            match result {
                Ok(()) => println!("PASS independent SSH handshake/exec {}/{rounds}", index + 1),
                Err(error) => {
                    eprintln!("FAIL independent SSH handshake/exec {}: {error:?}", index + 1);
                    failed.push(index + 1);
                }
            }
        }
        deny.abort();
        fs::write(lab.join("ssh-stress-result.json"), serde_json::to_vec_pretty(
            &serde_json::json!({"rounds":rounds,"failed":failed,"scope":"owned QEMU SSH; no USB or provisioning"})).unwrap()).unwrap();
        assert!(failed.is_empty(), "some independent SSH rounds failed");
    });
}
