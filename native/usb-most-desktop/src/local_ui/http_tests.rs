use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use usb_most_connection::{
    config::Endpoint,
    pairing::{PairCode, Store},
    vault::PrivateFile,
};

async fn call(context: &Context, path: &str, body: &str, trusted: bool) -> (u16, String) {
    let mut socket = tokio::net::TcpStream::connect(&context.authority)
        .await
        .unwrap();
    let authentication = if trusted {
        format!(
            "Origin: {}\r\nX-USBMost-Session: {}\r\n",
            context.origin,
            context.token.as_str()
        )
    } else {
        "Origin: http://untrusted.example\r\n".into()
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {}\r\n{authentication}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        context.authority,
        body.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.take(200000).read_to_end(&mut bytes),
    )
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let (_, body) = text.split_once("\r\n\r\n").unwrap();
    assert!(
        text.to_ascii_lowercase()
            .contains("cache-control: no-store")
    );
    (status, body.to_owned())
}
async fn exercise(listing: Option<String>) {
    // All files belong to this empty local fixture. No driver, service installer
    // or physical device is ever invoked by this test.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("private-state");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
    }
    #[cfg(windows)]
    let _private_directory = usb_most_platform::windows::create_private_directory(&root).unwrap();
    let root = root.as_path();
    PrivateFile::new(root.join("receiver.json"), 4096)
        .unwrap()
        .replace(br#"{"transport":"direct","port":3241}"#)
        .unwrap();
    let store = Store::open(PrivateFile::new(root.join("pairs.json"), 16384).unwrap()).unwrap();
    let pin = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9; 32]);
    let status = admin::initial(None, "fixture".into());
    status.lock().unwrap().snapshot.stack = true; // Synthetic health; no driver loaded.
    let (stop, rx) = tokio::sync::watch::channel(false);
    let owner = admin::start(
        root,
        status,
        Some(admin::Pairing {
            store: store.clone(),
            pin,
            port: 3241,
        }),
        rx,
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    let context = Arc::new(Context {
        root: root.to_owned(),
        origin: format!("http://{authority}"),
        authority,
        token: Zeroizing::new("Q".repeat(43)),
        expires: Instant::now() + Duration::from_secs(180),
        operation: Mutex::new(()),
        listing: listing.clone(),
    });
    let app = router(context.clone());
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert_eq!(
        call(&context, "/api", r#"{"op":"status"}"#, false).await.0,
        403
    );
    assert_eq!(
        call(&context, "/api", r#"{"op":"download"}"#, false)
            .await
            .0,
        403
    );
    assert_eq!(
        call(
            &context,
            "/api",
            r#"{"op":"download","url":"https://evil.example"}"#,
            true
        )
        .await
        .0,
        422
    );
    assert_eq!(call(&context, "/api", &"x".repeat(4097), true).await.0, 413);
    assert_eq!(
        call(
            &context,
            "/api",
            r#"{"op":"status","command":"anything"}"#,
            true
        )
        .await
        .0,
        422
    );
    assert_eq!(
        call(
            &context,
            "/api",
            r#"{"op":"pair","host":"https://evil.example"}"#,
            true
        )
        .await
        .0,
        400
    );
    let (download_status, download_body) =
        call(&context, "/api", r#"{"op":"download"}"#, true).await;
    assert_eq!(download_status, 200);
    let download: serde_json::Value = serde_json::from_str(&download_body).unwrap();
    assert_eq!(download["url"], serde_json::json!(listing));
    assert_eq!(download["svg"].is_string(), listing.is_some());
    let (code, body) = call(
        &context,
        "/api",
        r#"{"op":"pair","host":"192.0.2.42"}"#,
        true,
    )
    .await;
    assert_eq!(code, 200);
    let result: serde_json::Value = serde_json::from_str(&body).unwrap();
    let pair = PairCode::decode(result["code"].as_str().unwrap()).unwrap();
    assert_eq!(
        pair.endpoint,
        Endpoint {
            host: "192.0.2.42".into(),
            port: 3241
        }
    );
    assert!(result["svg"].as_str().unwrap().starts_with("<svg"));
    assert_eq!(result["expires_seconds"], 300);
    let token = usb_most_connection::pairing::token(&pair.claim).unwrap();
    let grant = store
        .redeem(&token, [3; 16], "<script>fixture</script>")
        .unwrap();
    let lease = store.authenticate(&grant).unwrap();
    let (code, body) = call(&context, "/api", r#"{"op":"status"}"#, true).await;
    assert_eq!(code, 200);
    let snapshot: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snapshot["peers"].as_array().unwrap().len(), 1);
    if let Ok(python) = std::env::var("USB_MOST_BROWSER_PYTHON") {
        use std::io::Write;
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/receiver-ui-browser-e2e.py");
        let payload = serde_json::json!({"listing":listing, "scope":"temporary-owner-ipc-no-usb", "url": format!("{}/#{}", context.origin, context.token.as_str())}).to_string();
        tokio::task::spawn_blocking(move || {
            let mut child = std::process::Command::new(python)
                .arg(script)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(payload.as_bytes())
                .unwrap();
            assert!(child.wait().unwrap().success(), "actual browser E2E failed");
        })
        .await
        .unwrap();
    }
    let body = serde_json::json!({"op":"revoke", "id":lease.id}).to_string();
    assert_eq!(call(&context, "/api", &body, true).await.0, 200);
    assert!(*lease.revoked.borrow());
    assert!(store.authenticate(&grant).is_err());
    assert_eq!(call(&context, "/api", &body, true).await.0, 400);
    http.abort();
    let _ = http.await;
    stop.send_replace(true);
    owner.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_http_pair_redeem_list_revoke_and_cross_origin_rejection() {
    exercise(None).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_store_link_is_public_only_and_does_not_replace_pairing() {
    exercise(Some(
        "https://www.rustore.ru/catalog/app/ru.example.usbmost.fixture".into(),
    ))
    .await;
}
