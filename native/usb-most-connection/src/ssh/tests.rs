use super::*;
use crate::config::Endpoint;
use russh::server;
use std::sync::atomic::AtomicUsize;

struct PasswordServer {
    attempts: Arc<AtomicUsize>,
}
impl server::Handler for PasswordServer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        if user == "fixture-user" && password == "fixture-secret" {
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_is_only_sent_after_trust_and_authentication_result_is_checked() {
    let key = generate_app_key().unwrap();
    crate::key_guard::validate(&key.private).unwrap();
    let private = russh::keys::decode_secret_key(&key.private, None).unwrap();
    let pin = encode_pin(&private.public_key().to_bytes().unwrap());
    let config = Arc::new(server::Config {
        keys: vec![private],
        auth_rejection_time: Duration::from_millis(5),
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let task = tokio::spawn(async move {
        let mut jobs = JoinSet::new();
        for _ in 0..3 {
            let (socket, _) = listener.accept().await.unwrap();
            let handler = PasswordServer {
                attempts: count.clone(),
            };
            let config = config.clone();
            jobs.spawn(async move {
                if let Ok(session) = server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
        while jobs.join_next().await.is_some() {}
    });
    let profile = |pin| Profile::Ssh {
        endpoint: Endpoint {
            host: "127.0.0.1".into(),
            port: address.port(),
        },
        user: "fixture-user".into(),
        host_key: Some(pin),
        reverse_port: 0,
        prepare: false,
    };
    let (approve, _) = mpsc::channel(1);
    let bridge = || LocalBridge::new("127.0.0.1:1".parse().unwrap(), [9; 32]).unwrap();
    let result = SshClient::connect(
        &profile(encode_pin(b"wrong host")),
        Credential::Password(Zeroizing::new("fixture-secret".into())),
        bridge(),
        approve.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::HostKey)));
    assert_eq!(attempts.load(Ordering::Acquire), 0);
    let result = SshClient::connect(
        &profile(pin.clone()),
        Credential::Password(Zeroizing::new("wrong".into())),
        bridge(),
        approve.clone(),
    )
    .await;
    assert!(matches!(result, Err(Error::Authentication)));
    assert_eq!(attempts.load(Ordering::Acquire), 1);
    let good = SshClient::connect(
        &profile(pin),
        Credential::Password(Zeroizing::new("fixture-secret".into())),
        bridge(),
        approve,
    )
    .await
    .unwrap();
    assert_eq!(attempts.load(Ordering::Acquire), 2);
    drop(good);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
}
