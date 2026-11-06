//! Interoperability fixture, invoked only by tools/run-ssh-e2e.py on loopback.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use usb_most_connection::{
    config::{Endpoint, Profile, encode_pin},
    ssh::{Credential, Error, LocalBridge, SshClient, TrustRequest},
};
use zeroize::Zeroizing;

fn credential() -> Credential {
    Credential::Key {
        private: Zeroizing::new(
            std::fs::read_to_string(std::env::var("USB_MOST_FIXTURE_KEY").unwrap()).unwrap(),
        ),
        passphrase: None,
    }
}
fn profile(port: u16, pin: Option<String>) -> Profile {
    Profile::Ssh {
        endpoint: Endpoint {
            host: std::env::var("USB_MOST_FIXTURE_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port,
        },
        user: std::env::var("USB_MOST_FIXTURE_USER").unwrap(),
        host_key: pin,
        reverse_port: 0,
        prepare: false,
    }
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let port: u16 = std::env::var("USB_MOST_FIXTURE_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let denied: u16 = std::env::var("USB_MOST_FIXTURE_DENIED_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let public =
        russh::keys::load_public_key(std::env::var("USB_MOST_FIXTURE_HOST_PUB").unwrap()).unwrap();
    let expected = encode_pin(&public.to_bytes().unwrap());
    let trust_count = Arc::new(AtomicUsize::new(0));
    let count = trust_count.clone();
    let expected_actor = expected.clone();
    let (approvals, mut questions) = mpsc::channel::<TrustRequest>(2);
    let trust = tokio::spawn(async move {
        while let Some(q) = questions.recv().await {
            count.fetch_add(1, Ordering::AcqRel);
            let _ = q.reply.send(q.pin == expected_actor);
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let proof = [97u8; 32];
    let count = Arc::new(AtomicUsize::new(0));
    let accepted = count.clone();
    let echo = tokio::spawn(async move {
        loop {
            let (mut s, _) = listener.accept().await.unwrap();
            let accepted = accepted.clone();
            tokio::spawn(async move {
                let mut prefix = [0; 40];
                s.read_exact(&mut prefix).await.unwrap();
                assert_eq!(&prefix[..8], b"UMLOCAL1");
                assert_eq!(&prefix[8..], proof);
                accepted.fetch_add(1, Ordering::AcqRel);
                s.write_all(&[1]).await.unwrap();
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let bridge = || LocalBridge::new(local, proof).unwrap();
    if let Ok(rounds) = std::env::var("USB_MOST_FIXTURE_STRESS_ROUNDS") {
        let rounds: usize = rounds.parse().unwrap();
        assert!((1..=100).contains(&rounds));
        let mut failed = Vec::new();
        for round in 1..=rounds {
            let attempt = async {
                let client = SshClient::connect(
                    &profile(port, Some(expected.clone())),
                    credential(),
                    bridge(),
                    approvals.clone(),
                )
                .await?;
                let reply = client
                    .exec(
                        "printf USB_MOST_KERNEL_TCP_OK",
                        Zeroizing::new(vec![]),
                        Duration::from_secs(30),
                    )
                    .await?;
                assert_eq!(reply.exit, 0);
                assert_eq!(reply.stdout.as_slice(), b"USB_MOST_KERNEL_TCP_OK");
                Ok::<_, Error>(())
            }
            .await;
            match attempt {
                Ok(()) => println!("PASS isolated kernel TCP SSH {round}/{rounds}"),
                Err(error) => {
                    eprintln!("FAIL isolated kernel TCP SSH {round}: {error:?}");
                    failed.push(round);
                }
            }
        }
        std::fs::write(std::path::PathBuf::from(std::env::var("USB_MOST_FIXTURE_KEY_DIRECTORY").unwrap())
            .join("stress-result.json"), serde_json::to_vec_pretty(&serde_json::json!({
                "rounds":rounds,"failed":failed,"scope":"isolated Linux network namespaces/kernel TCP; no QEMU NAT or USB"
            })).unwrap()).unwrap();
        assert!(failed.is_empty(), "kernel TCP SSH stress failed");
        trust.abort();
        echo.abort();
        return;
    }
    let session = SshClient::connect(
        &profile(port, None),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert_eq!(session.host_pin, expected);
    assert_eq!(trust_count.load(Ordering::Acquire), 1);
    println!("PASS OpenSSH unknown host is explicitly approved before public-key auth");
    let out = session
        .exec(
            "printf 'hello'; printf 'metadata' >&2",
            Zeroizing::new(vec![]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(out.exit, 0);
    assert_eq!(out.stdout.as_slice(), b"hello");
    assert_eq!(out.stderr.as_slice(), b"metadata");
    let out = session
        .exec(
            "cat",
            Zeroizing::new(b"stdin contains quotes ' \" ; $(id) but is data".to_vec()),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(
        out.stdout.as_slice(),
        b"stdin contains quotes ' \" ; $(id) but is data"
    );
    let out = session
        .exec("exit 7", Zeroizing::new(vec![]), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(out.exit, 7);
    println!(
        "PASS OpenSSH exec captures separate bounded stdout/stderr, stdin is data, exit status retained"
    );
    let detected = usb_most_connection::bootstrap::probe(&session)
        .await
        .unwrap();
    assert_eq!(detected.os, usb_most_connection::provision::Os::Linux);
    println!(
        "PASS embedded nonce-bound Linux preflight over actual OpenSSH without target changes"
    );
    assert!(matches!(
        session
            .exec(
                "printf 'not executed'",
                Zeroizing::new(vec![]),
                Duration::ZERO
            )
            .await,
        Err(Error::InvalidConfig)
    ));
    let reverse = session.forward(0).await.unwrap();
    assert!(reverse >= 1024);
    assert_eq!(session.forward(0).await, Err(Error::Forwarding));
    let before = count.load(Ordering::Acquire);
    let mut denied_local = TcpStream::connect(("127.0.0.1", reverse)).await.unwrap();
    denied_local.write_all(&[0u8; 40]).await.unwrap();
    let mut byte = [0];
    let result = tokio::time::timeout(Duration::from_secs(3), denied_local.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    assert_eq!(count.load(Ordering::Acquire), before);
    let mut stream = gateway(reverse, &session.gateway_proof()).await;
    let bytes: Vec<u8> = (0..65556).map(|n| n as u8).collect();
    stream.write_all(&bytes).await.unwrap();
    let mut received = vec![0; bytes.len()];
    tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, bytes);
    println!(
        "PASS OpenSSH reverse forwarding and authenticated local bridge preserve all65556 bytes"
    );
    drop(stream);
    drop(session);

    let session = SshClient::connect(
        &profile(port, Some(expected.clone())),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert_eq!(trust_count.load(Ordering::Acquire), 1);
    let reverse = session.forward(0).await.unwrap();
    let before = count.load(Ordering::Acquire);
    let mut held = Vec::new();
    for _ in 0..8 {
        let mut s = gateway(reverse, &session.gateway_proof()).await;
        s.write_all(b"x").await.unwrap();
        let mut b = [0];
        s.read_exact(&mut b).await.unwrap();
        assert_eq!(b, [b'x']);
        held.push(s);
    }
    assert_eq!(count.load(Ordering::Acquire) - before, 8);
    let mut ninth = TcpStream::connect(("127.0.0.1", reverse)).await.unwrap();
    let mut one = [0];
    let result = tokio::time::timeout(Duration::from_secs(3), ninth.read(&mut one))
        .await
        .unwrap();
    assert!(matches!(result, Ok(0) | Err(_)));
    println!(
        "PASS OpenSSH persisted host trust, eight concurrent channels, excess stream rejected"
    );
    drop(session);
    for mut stream in held {
        let result = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut one))
            .await
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
    }
    println!("PASS dropping SSH session wakes active channel readers and releases bridge sockets");

    let wrong = profile(port, Some(encode_pin(b"wrong host key")));
    let result = SshClient::connect(
        &wrong,
        Credential::Password(Zeroizing::new("fixture-password-not-used".into())),
        bridge(),
        approvals.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::HostKey)));
    let result = SshClient::connect(
        &profile(port, Some(expected.clone())),
        Credential::Password(Zeroizing::new("fixture-wrong-password".into())),
        bridge(),
        approvals.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::Authentication)));
    let result = SshClient::connect(
        &profile(port, Some(expected.clone())),
        Credential::Key {
            private: Zeroizing::new("invalid-key".into()),
            passphrase: None,
        },
        bridge(),
        approvals.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::InvalidKey)));
    println!(
        "PASS changed host key, unsupported authentication and malformed private key fail closed"
    );

    for name in [
        "client-rsa",
        "client-rsa-pem",
        "client-rsa-pkcs8",
        "client-rsa-encrypted",
        "client-encrypted",
    ] {
        let path =
            std::path::PathBuf::from(std::env::var("USB_MOST_FIXTURE_KEY_DIRECTORY").unwrap())
                .join(name);
        let private = Zeroizing::new(std::fs::read_to_string(path).unwrap());
        let passphrase = if name.ends_with("-encrypted") {
            Some(Zeroizing::new("fixture-passphrase".into()))
        } else {
            None
        };
        if passphrase.is_some() {
            for wrong in [
                None,
                Some(Zeroizing::new("wrong-fixture-passphrase".into())),
            ] {
                let result = SshClient::connect(
                    &profile(port, Some(expected.clone())),
                    Credential::Key {
                        private: private.clone(),
                        passphrase: wrong,
                    },
                    bridge(),
                    approvals.clone(),
                )
                .await;
                assert!(
                    matches!(result, Err(Error::InvalidKey)),
                    "wrong passphrase for {name}"
                );
            }
        }
        let s = SshClient::connect(
            &profile(port, Some(expected.clone())),
            Credential::Key {
                private,
                passphrase,
            },
            bridge(),
            approvals.clone(),
        )
        .await
        .unwrap_or_else(|error| panic!("valid key format {name}: {error:?}"));
        assert_eq!(
            s.exec(
                "printf 'key-ok'",
                Zeroizing::new(vec![]),
                Duration::from_secs(5)
            )
            .await
            .unwrap()
            .stdout
            .as_slice(),
            b"key-ok"
        );
    }
    // Arbitrary encrypted PEM KDF costs remain outside the supported envelope.
    // A valid file and correct password must not bypass that explicit policy.
    let rejected_path =
        std::path::PathBuf::from(std::env::var("USB_MOST_FIXTURE_KEY_DIRECTORY").unwrap())
            .join("client-rsa-pkcs8-encrypted");
    let result = SshClient::connect(
        &profile(port, Some(expected.clone())),
        Credential::Key {
            private: Zeroizing::new(std::fs::read_to_string(rejected_path).unwrap()),
            passphrase: Some(Zeroizing::new("fixture-passphrase".into())),
        },
        bridge(),
        approvals.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::InvalidKey)));
    println!(
        "PASS RSA/SHA2 OpenSSH/PKCS1/PKCS8; encrypted OpenSSH RSA/Ed25519; missing/wrong passphrases and encrypted PKCS8 rejected"
    );
    let session = SshClient::connect(
        &profile(denied, Some(expected.clone())),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert_eq!(session.forward(0).await, Err(Error::Forwarding));
    drop(session);
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let session = SshClient::connect(
        &profile(port, Some(expected.clone())),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert_eq!(session.forward(occupied_port).await, Err(Error::Forwarding));
    drop(session);
    assert!(
        TcpStream::connect(("127.0.0.1", occupied_port))
            .await
            .is_ok()
    );
    println!("PASS disabled forwarding and occupied remote port never replace a foreign listener");

    let session = SshClient::connect(
        &profile(port, Some(expected.clone())),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        session
            .exec(
                "head -c 17000 /dev/zero",
                Zeroizing::new(vec![]),
                Duration::from_secs(5)
            )
            .await,
        Err(Error::OutputLimit)
    ));
    drop(session);
    let session = SshClient::connect(
        &profile(port, Some(expected)),
        credential(),
        bridge(),
        approvals.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        session
            .exec(
                "sleep 5",
                Zeroizing::new(vec![]),
                Duration::from_millis(100)
            )
            .await,
        Err(Error::Timeout)
    ));
    drop(session);
    println!("PASS remote output quota and command deadline abort the uncertain connection");
    echo.abort();
    trust.abort();
    println!("PASS embedded Rust SSH / actual OpenSSH interoperability");
}

async fn gateway(port: u16, proof: &[u8; 32]) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    usb_most_connection::gateway::client(&mut socket, proof)
        .await
        .unwrap();
    let mut ready = [0];
    tokio::time::timeout(Duration::from_secs(5), socket.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, [1]);
    socket
}
