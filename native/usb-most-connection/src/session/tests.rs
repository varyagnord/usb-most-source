use super::*;
use crate::{config::Endpoint, ssh::generate_app_key};
use russh::server;
use std::sync::atomic::{AtomicUsize, Ordering};
struct Fixture {
    attempts: Arc<AtomicUsize>,
}
impl server::Handler for Fixture {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> Result<server::Auth, Self::Error> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        Ok(server::Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }
}
async fn event(session: &Session, kind: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            while let Some(json) = session.poll() {
                let value: serde_json::Value = serde_json::from_str(&json).unwrap();
                if value["event"] == kind {
                    return value;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn stale_trust_denial_and_cancellation_never_send_credentials() {
    for cancel in [false, true] {
        let key = generate_app_key().unwrap();
        let key = russh::keys::decode_secret_key(&key.private, None).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            if let Ok(session) =
                server::run_stream(config, socket, Fixture { attempts: count }).await
            {
                let _ = session.await;
            }
        });
        let profile = Profile::Ssh {
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port,
            },
            user: "fixture".into(),
            host_key: None,
            reverse_port: 0,
            prepare: false,
        };
        let secret = Secret::parse(br#"{"password":"synthetic-fixture"}"#).unwrap();
        let session = Session::start(
            profile,
            secret,
            LocalBridge::new("127.0.0.1:1".parse().unwrap(), [1; 32]).unwrap(),
            PathBuf::from("/unused-fixture"),
        );
        let question = event(&session, "trust").await;
        let pin = question["pin"].as_str().unwrap();
        assert!(!session.approve("old-question", true));
        if cancel {
            session.stop();
            event(&session, "closed").await;
        } else {
            assert!(session.approve(pin, false));
            assert_eq!(event(&session, "error").await["code"], "host_key_rejected");
        }
        assert_eq!(attempts.load(Ordering::Acquire), 0);
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
        assert!(!session.approve(pin, true));
        session.stop();
    }
}
#[test]
fn credentials_and_artifacts_are_strict_bounded_and_hash_checked() {
    for bytes in [
        br#"{"token":"a","unexpected":true}"#.as_slice(),
        br#"{"password":true}"#,
        br#"{"sudo_password":"not-a-line\ncommand"}"#,
    ] {
        assert!(Secret::parse(bytes).is_err());
    }
    assert!(Secret::parse(&vec![b' '; 131073]).is_err());
    let root = tempfile::tempdir().unwrap();
    let binary = b"synthetic-binary";
    let name = "usb-most-linux-x86_64";
    let hash = ring::digest::digest(&ring::digest::SHA256, binary)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut manifest = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"artifacts":[{"name":name,"sha256":hash,"bytes":binary.len()}]});
    std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(root.path().join(name), binary).unwrap();
    assert_eq!(
        artifact(root.path().to_path_buf(), name)
            .unwrap()
            .0
            .as_slice(),
        binary
    );
    for version in ["0.0.0-old", "999.0.0-future", ""] {
        manifest["version"] = serde_json::json!(version);
        std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
        assert!(matches!(
            artifact(root.path().to_path_buf(), name),
            Err("invalid_receiver")
        ));
    }
    manifest["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(root.path().join(name), b"tampered-binary").unwrap();
    assert!(artifact(root.path().to_path_buf(), name).is_err());
    assert!(artifact(root.path().to_path_buf(), "usb-most-windows-x86_64.exe").is_err());
}

#[tokio::test]
async fn durable_ack_is_required_before_the_connection_can_advance() {
    let (stop, _rx) = watch::channel(false);
    let session = Arc::new(Session {
        state: Mutex::new(State {
            events: VecDeque::new(),
            pending: None,
            save: None,
            next_save: 1,
            ended: false,
            exposed: false,
        }),
        stop,
    });
    for success in [true, false] {
        let owner = session.clone();
        let task = tokio::spawn(async move {
            owner
                .persist(serde_json::json!({"event":"paired","token":"synthetic-grant"}))
                .await
        });
        let request = event(&session, "paired").await;
        let operation = request["save_id"].as_u64().unwrap();
        assert!(!task.is_finished());
        assert!(!session.saved(operation + 1, true));
        assert!(session.saved(operation, success));
        assert!(!session.saved(operation, true));
        assert_eq!(task.await.unwrap().is_ok(), success);
    }
    session.stop();
    assert!(!session.saved(1, true));
}

struct ReconnectFixture {
    attempts: Arc<AtomicUsize>,
}
impl server::Handler for ReconnectFixture {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _user: &str,
        _password: &str,
    ) -> Result<server::Auth, Self::Error> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        Ok(server::Auth::Accept)
    }
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        assert_eq!(address, "127.0.0.1");
        assert_eq!(*port, 0);
        *port = 32401;
        Ok(true)
    }
}
#[tokio::test]
async fn reconnect_waits_for_storage_withdrawal_and_retains_the_approved_host_pin() {
    let key = generate_app_key().unwrap();
    let key = russh::keys::decode_secret_key(&key.private, None).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = Arc::new(AtomicUsize::new(0));
    let attempts = count.clone();
    let (tx, mut handles) = mpsc::channel(2);
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            let s = server::run_stream(
                config.clone(),
                socket,
                ReconnectFixture {
                    attempts: attempts.clone(),
                },
            )
            .await
            .unwrap();
            tx.send(s.handle()).await.unwrap();
            tokio::spawn(async move {
                let _ = s.await;
            });
        }
    });
    let session = Session::start(
        Profile::Ssh {
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port,
            },
            user: "fixture".into(),
            host_key: None,
            reverse_port: 0,
            prepare: false,
        },
        Secret::parse(br#"{"password":"synthetic-only"}"#).unwrap(),
        LocalBridge::new("127.0.0.1:1".parse().unwrap(), [1; 32]).unwrap(),
        PathBuf::from("/unused-fixture"),
    );
    let question = event(&session, "trust").await;
    let pin = question["pin"].as_str().unwrap();
    assert!(session.approve(pin, true));
    let saved = event(&session, "trusted").await;
    assert!(session.saved(saved["save_id"].as_u64().unwrap(), true));
    assert_eq!(event(&session, "state").await["code"], "connected");
    handles
        .recv()
        .await
        .unwrap()
        .disconnect(
            russh::Disconnect::ByApplication,
            "synthetic transport loss".into(),
            "".into(),
        )
        .await
        .unwrap();
    let barrier = event(&session, "disconnected").await;
    tokio::time::sleep(Duration::from_millis(4500)).await;
    assert_eq!(
        count.load(Ordering::Acquire),
        1,
        "no retry before storage grants have been withdrawn"
    );
    assert!(session.saved(barrier["save_id"].as_u64().unwrap(), true));
    let second = event(&session, "trusted").await;
    assert_eq!(second["pin"], pin);
    assert!(
        !session.approve(pin, true),
        "no second first-use trust prompt"
    );
    assert!(session.saved(second["save_id"].as_u64().unwrap(), true));
    assert_eq!(event(&session, "state").await["code"], "connected");
    assert_eq!(count.load(Ordering::Acquire), 2);
    session.stop();
    event(&session, "closed").await;
    let _ = handles
        .recv()
        .await
        .unwrap()
        .disconnect(
            russh::Disconnect::ByApplication,
            "fixture done".into(),
            "".into(),
        )
        .await;
    server.await.unwrap();
}

#[tokio::test]
async fn initial_unreachable_target_does_not_withdraw_unexposed_storage_grants() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let profile = Profile::Ssh {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port,
        },
        user: "fixture".into(),
        host_key: Some(crate::config::encode_pin(b"fixed fixture key")),
        reverse_port: 0,
        prepare: false,
    };
    let session = Session::start(
        profile,
        Secret::parse(br#"{"password":"synthetic-only"}"#).unwrap(),
        LocalBridge::new("127.0.0.1:1".parse().unwrap(), [1; 32]).unwrap(),
        PathBuf::from("/unused-fixture"),
    );
    assert_eq!(event(&session, "state").await["code"], "connecting");
    assert_eq!(event(&session, "state").await["code"], "reconnecting");
    assert!(
        session.state.lock().unwrap().save.is_none(),
        "no storage barrier before any remote USB access was possible"
    );
    session.stop();
    event(&session, "closed").await;
}

#[tokio::test]
async fn an_async_actor_panic_reports_failure_and_retires_pending_state() {
    let (stop, rx) = watch::channel(false);
    let session = Arc::new(Session {
        state: Mutex::new(State {
            events: VecDeque::new(),
            pending: None,
            save: None,
            next_save: 1,
            ended: false,
            exposed: false,
        }),
        stop,
    });
    let work = RUNTIME.spawn(async {
        panic!("synthetic connection actor panic");
        #[allow(unreachable_code)]
        Ok(())
    });
    session.monitor(rx, work);
    assert_eq!(event(&session, "error").await["code"], "internal_error");
    assert!(session.ended());
    assert!(!session.saved(1, true));
    assert!(!session.approve("stale", true));
    session.stop();
}
