//! Short-lived, owner-authorized local setup surface. No remotely reachable web
//! administration, cloud service, cookies, external assets, or executable input.
use super::{Config, admin, invalid, request_pair, setup, state_root};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use qrcodegen::{QrCode, QrCodeEcc};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
mod linux_launcher;
mod listing;
const HTML: &str = include_str!("local_ui/index.html");
const SCRIPT: &str = include_str!("local_ui/setup.js");
const STYLE: &str = include_str!("local_ui/style.css");
struct Context {
    root: PathBuf,
    authority: String,
    origin: String,
    token: Zeroizing<String>,
    expires: Instant,
    operation: Mutex<()>,
    listing: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Status {},
    Download {},
    Install {},
    Pair { host: String },
    Revoke { id: [u8; 16] },
}
#[derive(Serialize)]
struct Snapshot {
    running: bool,
    direct: bool,
    stack: bool,
    connected: bool,
    active: usize,
    addresses: Vec<String>,
    peers: Vec<([u8; 16], String)>,
}
fn addresses() -> io::Result<Vec<String>> {
    let mut values = if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|item| {
            let ip = item.ip();
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                return None;
            }
            let host = ip.to_string();
            usb_most_connection::config::Endpoint {
                host: host.clone(),
                port: 3241,
            }
            .validate()
            .ok()
            .map(|_| host)
        })
        .collect::<Vec<_>>();
    values.sort_by_key(|v| (v.contains(':'), v.clone()));
    values.dedup();
    values.truncate(64);
    Ok(values)
}
fn snapshot(root: &std::path::Path) -> io::Result<Snapshot> {
    let (running, direct, stack, connected, active) =
        match admin::request(root, &admin::Request::Health) {
            Ok(admin::Response::Health { ref status }) => (
                true,
                status.peer.is_none(),
                status.stack,
                status.connected,
                status.active,
            ),
            Err(_) => (false, false, false, false, 0),
            _ => return Err(invalid()),
        };
    let peers = if direct {
        match admin::request(root, &admin::Request::Peers)? {
            admin::Response::Peers { ref peers } => peers.clone(),
            _ => return Err(invalid()),
        }
    } else {
        Vec::new()
    };
    Ok(Snapshot {
        running,
        direct,
        stack,
        connected,
        active,
        addresses: addresses()?,
        peers,
    })
}
fn qr_svg(code: &str) -> io::Result<String> {
    let qr = QrCode::encode_text(code, QrCodeEcc::Medium).map_err(|_| invalid())?;
    let edge = qr.size() + 8;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {edge} {edge}\" shape-rendering=\"crispEdges\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><path fill=\"black\" d=\""
    );
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                use std::fmt::Write;
                write!(svg, "M{},{}h1v1h-1z", x + 4, y + 4).map_err(|_| invalid())?;
            }
        }
    }
    svg.push_str("\"/></svg>");
    Ok(svg)
}
fn authorized(context: &Context, headers: &HeaderMap) -> bool {
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if Instant::now() >= context.expires
        || value("host") != Some(context.authority.as_str())
        || value("origin") != Some(context.origin.as_str())
    {
        return false;
    }
    let Some(token) = value("x-usbmost-session") else {
        return false;
    };
    token.as_bytes().ct_eq(context.token.as_bytes()).into()
}
async fn secure(State(context): State<Arc<Context>>, request: Request, next: Next) -> Response {
    let mut response = if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(context.authority.as_str())
        || request.uri().path() == "/api" && !authorized(&context, request.headers())
    {
        StatusCode::FORBIDDEN.into_response()
    } else {
        next.run(request).await
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("cross-origin-resource-policy", "same-origin"),
    ] {
        response.headers_mut().insert(name, value.parse().unwrap());
    }
    response
}
async fn api(State(context): State<Arc<Context>>, Json(action): Json<Action>) -> Response {
    let Ok(_guard) = context.operation.try_lock() else {
        return StatusCode::CONFLICT.into_response();
    };
    let result: io::Result<serde_json::Value> = async {
        match action {
            Action::Download {} => Ok(match &context.listing {
                Some(url) => serde_json::json!({"url":url,"svg":qr_svg(url)?}),
                None => serde_json::json!({"url":null,"svg":null}),
            }),
            Action::Status {} => {
                let root = context.root.clone();
                let status = tokio::task::spawn_blocking(move || snapshot(&root)).await.map_err(|_| invalid())??;
                serde_json::to_value(status).map_err(|_| invalid())
            },
            Action::Install {} => {
                // A local owner still cannot take over an active foreign session:
                // the deployment engine performs its normal pause/health rollback.
                tokio::task::spawn_blocking(|| setup::install(&Config::Direct { port: 3241 })).await.map_err(|_| invalid())??;
                Ok(serde_json::json!({"ok": true}))
            },
            Action::Pair { host } => {
                let code = request_pair(&context.root, host).await?;
                Ok(serde_json::json!({"code": code.as_str(), "svg": qr_svg(&code)?, "expires_seconds": 300}))
            },
            Action::Revoke { id } => {
                let root = context.root.clone();
                let reply = tokio::task::spawn_blocking(move || admin::request(&root, &admin::Request::Revoke { id })).await.map_err(|_| invalid())??;
                if !matches!(reply, admin::Response::Revoked) { return Err(io::ErrorKind::PermissionDenied.into()); }
                Ok(serde_json::json!({"ok": true}))
            },
        }
    }.await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let message = if error.raw_os_error() == Some(3010) {
                "Для завершения установки перезагрузите компьютер."
            } else {
                match error.kind() {
                    io::ErrorKind::PermissionDenied => {
                        "Нужны права администратора или доступ уже отозван."
                    }
                    io::ErrorKind::AddrInUse | io::ErrorKind::WouldBlock => {
                        "Компьютер занят другим подключением. Остановите его и повторите."
                    }
                    io::ErrorKind::NotFound | io::ErrorKind::Unsupported => {
                        "USB-стек недоступен. Проверьте поддержку системы и необходимость перезагрузки."
                    }
                    _ => "Действие не завершено. Проверьте состояние приёмника и повторите.",
                }
            };
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": message})),
            )
                .into_response()
        }
    }
}
fn router(context: Arc<Context>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(HTML) }))
        .route(
            "/setup.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    SCRIPT,
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE) }),
        )
        .route("/api", post(api))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(context.clone(), secure))
        .with_state(context)
}
pub async fn launch(elevated: bool) -> io::Result<()> {
    #[cfg(windows)]
    if let Err(error) = usb_most_platform::windows::service::install::Manager::open() {
        if !elevated && error.kind() == io::ErrorKind::PermissionDenied {
            return tokio::task::spawn_blocking(
                usb_most_platform::windows::shell_ui::elevate_setup,
            )
            .await
            .map_err(|_| invalid())?;
        }
        return Err(error);
    }
    #[cfg(target_os = "linux")]
    if rustix::process::geteuid().as_raw() != 0 {
        if elevated {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        return linux_launcher::launch().await;
    }
    serve().await
}
pub async fn serve() -> io::Result<()> {
    #[cfg(unix)]
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    #[cfg(windows)]
    let _rights = usb_most_platform::windows::service::install::Manager::open()?;
    let listener = super::network::loopback_listener()?;
    let authority = listener.local_addr()?.to_string();
    let token = Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(usb_most_connection::security::random::<32>().map_err(|_| invalid())?),
    );
    let origin = format!("http://{authority}");
    #[cfg(windows)]
    let _handoff = {
        let nonce = super::digest_hex(
            &usb_most_connection::security::random::<32>().map_err(|_| invalid())?,
        )[..32]
            .to_owned();
        // Both components were generated locally and contain no HTML delimiters.
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\"><meta http-equiv=\"refresh\" content=\"0;url={origin}/#{}\"><title>USB Мост</title><a href=\"{origin}/#{}\">Открыть USB Мост</a>",
            token.as_str(),
            token.as_str()
        );
        tokio::task::spawn_blocking(move || {
            let page =
                usb_most_platform::windows::shell_ui::create_handoff(&nonce, html.as_bytes())?;
            usb_most_platform::windows::shell_ui::open_from_desktop(page.path())?;
            Ok::<_, io::Error>(page)
        })
        .await
        .map_err(|_| invalid())?
        .ok()
    };
    // Capability is only displayed to the local owner, never an argument, HTTP
    // path, query string or remotely visible receiver announcement.
    println!(
        "USB Мост · откройте в браузере на этом компьютере:\n{origin}/#{}",
        token.as_str()
    );
    let context = Arc::new(Context {
        root: state_root()?,
        authority,
        origin,
        token,
        expires: Instant::now() + Duration::from_secs(1800),
        operation: Mutex::new(()),
        listing: listing::parse(include_str!("local_ui/rustore.json")).map_err(|_| invalid())?,
    });
    axum::serve(listener, router(context)).with_graceful_shutdown(async {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = tokio::time::sleep(Duration::from_secs(1800)) => {} }
    }).await
}
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_authority_requires_token_exact_origin_and_host_and_deadline() {
        let mut c = Context {
            root: PathBuf::new(),
            authority: "127.0.0.1:1234".into(),
            origin: "http://127.0.0.1:1234".into(),
            token: Zeroizing::new("T".repeat(43)),
            expires: Instant::now() + Duration::from_secs(60),
            operation: Mutex::new(()),
            listing: None,
        };
        let mut h = HeaderMap::new();
        h.insert("host", c.authority.parse().unwrap());
        h.insert("origin", c.origin.parse().unwrap());
        h.insert("x-usbmost-session", c.token.parse().unwrap());
        assert!(authorized(&c, &h));
        for (name, value) in [
            ("origin", "http://evil.example"),
            ("origin", "null"),
            ("host", "evil.example:1234"),
            ("x-usbmost-session", "T"),
            ("x-usbmost-session", ""),
        ] {
            let mut bad = h.clone();
            bad.insert(name, value.parse().unwrap());
            assert!(!authorized(&c, &bad));
        }
        for name in ["host", "origin", "x-usbmost-session"] {
            let mut bad = h.clone();
            bad.remove(name);
            assert!(!authorized(&c, &bad));
        }
        c.expires = Instant::now();
        assert!(!authorized(&c, &h));
    }
}
