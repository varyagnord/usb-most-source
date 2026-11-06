use super::*;
use crate::{pairing::PairCode, security::Identity, vault::PrivateFile};
use base64::Engine;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use usb_most_protocol::agent::Connector;

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    endpoint: Endpoint,
    pin: String,
    gateway: SocketAddr,
    gateway_proof: [u8; 32],
    server: tokio::task::JoinHandle<Result<(), Error>>,
    stop: watch::Sender<bool>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store =
            Store::open(PrivateFile::new(dir.path().join("pairs"), 16384).unwrap()).unwrap();
        let identity = Identity::generate().unwrap();
        let pin = identity.pin();
        let network = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: network.local_addr().unwrap().port(),
        };
        let address = gateway.local_addr().unwrap();
        let gateway_proof = [31; 32];
        let (stop, rx) = watch::channel(false);
        let server = tokio::spawn(run_server(
            network,
            gateway,
            identity.server_config().unwrap(),
            store.clone(),
            gateway_proof,
            rx,
        ));
        Self {
            _dir: dir,
            store,
            endpoint,
            pin,
            gateway: address,
            gateway_proof,
            server,
            stop,
        }
    }
    fn code(&self) -> PairCode {
        self.store
            .challenge(
                self.endpoint.clone(),
                self.pin.clone(),
                Duration::from_secs(30),
            )
            .unwrap()
    }
    async fn paired(&self) -> Zeroizing<[u8; 32]> {
        pair(&self.code(), [1; 16], "fixture tablet").await.unwrap()
    }
    fn connector(&self) -> GatewayConnector {
        GatewayConnector::new(self.gateway, self.gateway_proof).unwrap()
    }
    async fn close(mut self) {
        self.stop.send_replace(true);
        timeout(Duration::from_secs(3), &mut self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.server.abort();
    }
}
struct Echo {
    address: SocketAddr,
    proof: [u8; 32],
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Echo {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let proof = [44; 32];
        let count = Arc::new(AtomicUsize::new(0));
        let accepted = count.clone();
        let task = tokio::spawn(async move {
            let mut jobs = JoinSet::new();
            loop {
                tokio::select! {
                    result=listener.accept()=>{
                        let(mut socket,_)=result.unwrap();let accepted=accepted.clone();jobs.spawn(async move {
                            let mut header=[0;40];if socket.read_exact(&mut header).await.is_err(){return;}
                            assert!(&header[..8]==b"UMLOCAL1" && header[8..]==proof);
                            accepted.fetch_add(1,Ordering::AcqRel);socket.write_all(&[1]).await.unwrap();let(mut reader,mut writer)=socket.split();let _=tokio::io::copy(&mut reader,&mut writer).await;
                        });
                    },
                    _=jobs.join_next(),if !jobs.is_empty()=>{},
                }
            }
        });
        Self {
            address,
            proof,
            count,
            task,
        }
    }
    fn bridge(&self) -> LocalBridge {
        LocalBridge::new(self.address, self.proof).unwrap()
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn client(
    f: &Fixture,
    token: Zeroizing<[u8; 32]>,
    bridge: LocalBridge,
) -> (
    watch::Sender<bool>,
    tokio::task::JoinHandle<Result<(), Error>>,
) {
    let (stop, rx) = watch::channel(false);
    let (tx, ready) = oneshot::channel();
    let task = tokio::spawn(run_client(
        f.endpoint.clone(),
        f.pin.clone(),
        token,
        bridge,
        rx,
        tx,
    ));
    timeout(Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    (stop, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paired_reverse_tls_preserves_all_bytes_and_stops_active_streams() {
    let f = Fixture::new().await;
    let token = f.paired().await;
    let echo = Echo::new().await;
    let (stop, task) = client(&f, token, echo.bridge()).await;
    let connector = f.connector();
    tokio::task::spawn_blocking(move || {
        let mut socket = connector.connect().unwrap();
        let bytes: Vec<u8> = (0..65556).map(|n| n as u8).collect();
        socket.write_all(&bytes).unwrap();
        let mut result = vec![0; bytes.len()];
        socket.read_exact(&mut result).unwrap();
        assert_eq!(result, bytes);
    })
    .await
    .unwrap();
    let connector = f.connector();
    let mut held = tokio::task::spawn_blocking(move || connector.connect().unwrap())
        .await
        .unwrap();
    stop.send_replace(true);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::task::spawn_blocking(move || {
        held.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut one = [0];
        match held.read(&mut one) {
            Ok(0) => {}
            Err(e)
                if !matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            other => panic!(
                "active reader not closed promptly: {:?}",
                other.map_err(|e| e.kind())
            ),
        }
    })
    .await
    .unwrap();
    assert!(echo.count.load(Ordering::Acquire) >= 1);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eight_streams_are_isolated_and_the_ninth_is_refused_without_disruption() {
    let f = Fixture::new().await;
    let echo = Echo::new().await;
    let (stop, task) = client(&f, f.paired().await, echo.bridge()).await;
    let connector = f.connector();
    timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || {
            let mut streams = Vec::new();
            for n in 0..8u8 {
                let mut s = connector.connect().unwrap();
                s.write_all(&[n; 257]).unwrap();
                streams.push(s);
            }
            assert!(connector.connect().is_err(), "ninth stream must be bounded");
            for (n, mut s) in streams.into_iter().enumerate() {
                let mut data = [0; 257];
                s.read_exact(&mut data).unwrap();
                assert_eq!(data, [n as u8; 257]);
            }
        }),
    )
    .await
    .unwrap()
    .unwrap();
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_fragmented_tls_preserves_bytes_and_cancels_without_replay() {
    async fn delayed(
        mut read: tokio::net::tcp::OwnedReadHalf,
        mut write: tokio::net::tcp::OwnedWriteHalf,
    ) -> io::Result<()> {
        let mut bytes = [0; 16384];
        let mut turn = 0u64;
        loop {
            let n = read.read(&mut bytes).await?;
            if n == 0 {
                return write.shutdown().await;
            }
            // 500..600 ms each way, plus fragmented writes; actual encrypted
            // TCP bytes remain unchanged. This is latency, not fake packet loss.
            tokio::time::sleep(Duration::from_millis(500 + turn % 101)).await;
            for chunk in bytes[..n].chunks(997) {
                write.write_all(chunk).await?;
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            turn += 17;
        }
    }
    let mut f = Fixture::new().await;
    let original = (f.endpoint.host.clone(), f.endpoint.port);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.endpoint.port = listener.local_addr().unwrap().port();
    let proxy = tokio::spawn(async move {
        let mut jobs = JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=>{
                    let (incoming,_) = accepted.unwrap();
                    let target=original.clone();
                    jobs.spawn(async move {
                        let outgoing=TcpStream::connect(target).await.unwrap();
                        incoming.set_nodelay(true).unwrap(); outgoing.set_nodelay(true).unwrap();
                        let (a,b)=incoming.into_split(); let (c,d)=outgoing.into_split();
                        let _=tokio::try_join!(delayed(a,d),delayed(c,b));
                    });
                },
                _=jobs.join_next(),if !jobs.is_empty()=>{},
            }
        }
    });
    struct Abort(tokio::task::JoinHandle<()>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _proxy = Abort(proxy);
    let echo = Echo::new().await;
    let token = f.paired().await;
    let (stop, task) = client(&f, token, echo.bridge()).await;
    let connector = f.connector();
    timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            let mut socket = connector.connect().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            let bytes: Vec<u8> = (0..65556).map(|n| (n * 17) as u8).collect();
            socket.write_all(&bytes).unwrap();
            let mut received = vec![0; bytes.len()];
            socket.read_exact(&mut received).unwrap();
            assert_eq!(received, bytes);
        }),
    )
    .await
    .unwrap()
    .unwrap();
    stop.send_replace(true);
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        echo.count.load(Ordering::Acquire),
        1,
        "no hidden replay/reconnect"
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_bad_claim_token_and_local_gateway_proof_before_usb_bridge() {
    let f = Fixture::new().await;
    let mut code = f.code();
    code.claim = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0; 32]);
    assert!(matches!(
        pair(&code, [1; 16], "fixture").await,
        Err(Error::Denied)
    ));
    let echo = Echo::new().await;
    let (_tx, rx) = watch::channel(false);
    let (ready, _) = oneshot::channel();
    assert!(matches!(
        run_client(
            f.endpoint.clone(),
            f.pin.clone(),
            Zeroizing::new([0; 32]),
            echo.bridge(),
            rx,
            ready
        )
        .await,
        Err(Error::Denied)
    ));
    let token = f.paired().await;
    let (stop, task) = client(&f, token, echo.bridge()).await;
    let connector = GatewayConnector::new(f.gateway, [0; 32]).unwrap();
    assert!(
        tokio::task::spawn_blocking(move || connector.connect().is_err())
            .await
            .unwrap()
    );
    assert_eq!(echo.count.load(Ordering::Acquire), 0);
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_control_is_busy_and_revoke_terminates_first() {
    let f = Fixture::new().await;
    let token = f.paired().await;
    let lease = f.store.authenticate(&token).unwrap();
    let echo = Echo::new().await;
    let (_stop, task) = client(&f, Zeroizing::new(*token), echo.bridge()).await;
    let (_stop, rx) = watch::channel(false);
    let (ready, _) = oneshot::channel();
    assert!(matches!(
        run_client(
            f.endpoint.clone(),
            f.pin.clone(),
            token,
            echo.bridge(),
            rx,
            ready
        )
        .await,
        Err(Error::Busy)
    ));
    f.store.revoke(lease.id).unwrap();
    assert!(
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_android_endpoint_rejects_pending_stream_promptly() {
    let f = Fixture::new().await;
    let token = f.paired().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (stop, task) = client(&f, token, LocalBridge::new(address, [44; 32]).unwrap()).await;
    let connector = f.connector();
    let result = tokio::task::spawn_blocking(move || connector.connect().is_err());
    assert!(
        timeout(Duration::from_secs(5), result)
            .await
            .unwrap()
            .unwrap()
    );
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    f.close().await;
}

// Keep the receiver's old control socket alive after closing only the Android
// side, as happens when a route disappears without a TCP FIN reaching the peer.
struct StrandedControl {
    endpoint: Endpoint,
    cut: Option<oneshot::Sender<()>>,
    release: Option<oneshot::Sender<()>>,
    attempts: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl StrandedControl {
    async fn new(target: Endpoint) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
        };
        let (cut, cut_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let task = tokio::spawn(async move {
            let mut first = Some((cut_rx, release_rx));
            let mut jobs = JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>{
                        let (mut client,_) = accepted.unwrap();
                        let mut server = TcpStream::connect((target.host.as_str(),target.port)).await.unwrap();
                        count.fetch_add(1, Ordering::AcqRel);
                        let controls = first.take();
                        jobs.spawn(async move {
                            if let Some((cut, release)) = controls {
                                tokio::select! {
                                    _=copy_bidirectional(&mut client,&mut server)=>{},
                                    _=cut=>{},
                                }
                                drop(client);
                                let _=release.await;
                            } else {
                                let _=copy_bidirectional(&mut client,&mut server).await;
                            }
                        });
                    },
                    _=jobs.join_next(),if !jobs.is_empty()=>{},
                }
            }
        });
        Self {
            endpoint,
            cut: Some(cut),
            release: Some(release),
            attempts,
            task,
        }
    }
}
impl Drop for StrandedControl {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct OwnedSession(Arc<crate::session::Session>);
impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.stop();
    }
}
fn direct_session(f: &Fixture, endpoint: Endpoint, token: &[u8; 32]) -> OwnedSession {
    let secret=serde_json::to_vec(&serde_json::json!({"token":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)})).unwrap();
    OwnedSession(crate::session::Session::start(
        crate::config::Profile::Direct {
            endpoint,
            server_pin: f.pin.clone(),
        },
        crate::session::Secret::parse(&secret).unwrap(),
        LocalBridge::new("127.0.0.1:1".parse().unwrap(), [1; 32]).unwrap(),
        "/unused-owned-tls-fixture".into(),
    ))
}
async fn next_session_event(session: &OwnedSession) -> serde_json::Value {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Some(raw) = session.0.poll() {
                return serde_json::from_str(&raw).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn session_code(session: &OwnedSession, kind: &str, code: &str) -> serde_json::Value {
    loop {
        let value = next_session_event(session).await;
        if value["event"] == kind && (code.is_empty() || value["code"] == code) {
            return value;
        }
        assert_eq!(value["event"], "state", "unexpected session event: {value}");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_session_waits_for_stranded_control_without_takeover_and_can_cancel() {
    for cancel in [false, true] {
        let f = Fixture::new().await;
        let token = f.paired().await;
        let mut proxy = StrandedControl::new(f.endpoint.clone()).await;
        let session = direct_session(&f, proxy.endpoint.clone(), &token);
        session_code(&session, "state", "connected").await;
        proxy.cut.take().unwrap().send(()).unwrap();
        let lost = session_code(&session, "disconnected", "").await;
        assert!(session.0.saved(lost["save_id"].as_u64().unwrap(), true));
        loop {
            let retry = session_code(&session, "state", "reconnecting").await;
            if retry["reason"] == "computer_busy" {
                break;
            }
        }
        assert_eq!(proxy.attempts.load(Ordering::Acquire), 2);
        // Receiver still owns the previous control; no takeover is attempted.
        if cancel {
            session.0.stop();
            session_code(&session, "closed", "").await;
            tokio::time::sleep(Duration::from_secs(6)).await;
            assert_eq!(
                proxy.attempts.load(Ordering::Acquire),
                2,
                "cancel stops the busy retry timer"
            );
        }
        proxy.release.take().unwrap().send(()).unwrap();
        if !cancel {
            session_code(&session, "state", "connected").await;
            assert_eq!(proxy.attempts.load(Ordering::Acquire), 3);
            session.0.stop();
            session_code(&session, "closed", "").await;
        }
        f.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_session_initial_busy_is_terminal_and_preserves_incumbent() {
    let f = Fixture::new().await;
    let token = f.paired().await;
    let echo = Echo::new().await;
    let (stop, task) = client(&f, Zeroizing::new(*token), echo.bridge()).await;
    let session = direct_session(&f, f.endpoint.clone(), &token);
    session_code(&session, "error", "computer_busy").await;
    assert!(session.0.ended());
    assert!(
        !task.is_finished(),
        "busy newcomer cannot stop an existing control"
    );
    stop.send_replace(true);
    task.await.unwrap().unwrap();
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_session_busy_recovery_expires_without_extending_its_deadline() {
    let f = Fixture::new().await;
    let token = f.paired().await;
    let identity = Identity::generate().unwrap();
    let pin = identity.pin();
    let config = identity.server_config().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
    };
    let (cut, cut_rx) = oneshot::channel();
    let count = Arc::new(AtomicUsize::new(0));
    let attempts = count.clone();
    let store = f.store.clone();
    let server = tokio::spawn(async move {
        let mut first = Some(cut_rx);
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = security::accept(socket, config.clone()).await.unwrap();
            let request = header(&mut stream).await.unwrap();
            assert_eq!(request.op, CONTROL);
            assert!(store.authenticate(&request.token).is_ok());
            attempts.fetch_add(1, Ordering::AcqRel);
            if let Some(cut) = first.take() {
                acknowledge(&mut stream).await.unwrap();
                let _ = cut.await;
            } else {
                stream.write_all(&[2]).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        }
    });
    let secret=serde_json::to_vec(&serde_json::json!({"token":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token.as_slice())})).unwrap();
    let session = OwnedSession(crate::session::Session::start(
        crate::config::Profile::Direct {
            endpoint,
            server_pin: pin,
        },
        crate::session::Secret::parse(&secret).unwrap(),
        LocalBridge::new("127.0.0.1:1".parse().unwrap(), [1; 32]).unwrap(),
        "/unused-owned-tls-fixture".into(),
    ));
    session_code(&session, "state", "connected").await;
    cut.send(()).unwrap();
    let lost = session_code(&session, "disconnected", "").await;
    assert!(session.0.saved(lost["save_id"].as_u64().unwrap(), true));
    let started = std::time::Instant::now();
    timeout(Duration::from_secs(70), async {
        loop {
            if let Some(raw) = session.0.poll() {
                let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                if value["event"] == "error" {
                    assert_eq!(value["code"], "computer_busy");
                    break;
                }
                assert_eq!(
                    value["event"], "state",
                    "no second USB exposure for a refused CONTROL"
                );
                assert_ne!(value["code"], "connected");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(59));
    assert!(
        count.load(Ordering::Acquire) <= 8,
        "bounded backoff, not a busy loop"
    );
    assert!(session.0.ended());
    server.abort();
    let _ = server.await;
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_session_changed_identity_is_terminal_without_trust_prompt_or_retry() {
    let mut f = Fixture::new().await;
    let token = f.paired().await;
    // The saved identity belongs to a different computer. The actual server
    // keeps its original certificate and never receives an application token.
    f.pin = Identity::generate().unwrap().pin();
    let session = direct_session(&f, f.endpoint.clone(), &token);
    session_code(&session, "error", "host_key_rejected").await;
    assert!(session.0.ended());
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        session.0.poll().is_none(),
        "identity failure cannot become a network retry"
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_identity_failure_does_not_consume_the_pairing_claim() {
    let f = Fixture::new().await;
    let original = f.code();
    let mut changed = PairCode::decode(&original.encode().unwrap()).unwrap();
    changed.server_pin = Identity::generate().unwrap().pin();
    assert!(matches!(
        pair(&changed, [6; 16], "owned-fixture").await,
        Err(Error::IdentityRejected)
    ));
    let token = pair(&original, [6; 16], "owned-fixture").await.unwrap();
    assert!(f.store.authenticate(&token).is_ok());
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_data_stream_identity_ends_control_before_local_usb_is_opened() {
    let f = Fixture::new().await;
    let impostor = Fixture::new().await;
    let token = f.paired().await;
    let echo = Echo::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
    };
    let (real, wrong) = (f.endpoint.clone(), impostor.endpoint.clone());
    let proxy = tokio::spawn(async move {
        let mut first = true;
        let mut jobs = JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=>{
                    let (mut client,_) = accepted.unwrap();
                    let target=if first { &real } else { &wrong };
                    first=false;
                    let mut server=TcpStream::connect((target.host.as_str(),target.port)).await.unwrap();
                    jobs.spawn(async move{let _=copy_bidirectional(&mut client,&mut server).await;});
                },
                _=jobs.join_next(),if !jobs.is_empty()=>{},
            }
        }
    });
    let secret=serde_json::to_vec(&serde_json::json!({"token":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token.as_slice())})).unwrap();
    let session = OwnedSession(crate::session::Session::start(
        crate::config::Profile::Direct {
            endpoint,
            server_pin: f.pin.clone(),
        },
        crate::session::Secret::parse(&secret).unwrap(),
        echo.bridge(),
        "/unused-owned-tls-fixture".into(),
    ));
    session_code(&session, "state", "connected").await;
    let connector = f.connector();
    let pending = tokio::task::spawn_blocking(move || connector.connect());
    session_code(&session, "error", "host_key_rejected").await;
    assert!(session.0.ended());
    assert!(
        timeout(Duration::from_secs(5), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        echo.count.load(Ordering::Acquire),
        0,
        "rejected data identity cannot open local USB"
    );
    proxy.abort();
    let _ = proxy.await;
    impostor.close().await;
    f.close().await;
}
