//! Integration test: a request through the proxy is forwarded verbatim to a
//! mock upstream and the response is returned unchanged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
    routing::any,
    Router,
};
use tokio::net::TcpListener;
use tracer_core::{server::router, Config};

#[derive(Default, Clone)]
struct Captured {
    inner: Arc<Mutex<Option<CapturedReq>>>,
}

#[derive(Clone)]
struct CapturedReq {
    method: String,
    path_and_query: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn mock_upstream_handler(
    State(cap): State<Captured>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let authorization = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    *cap.inner.lock().unwrap() = Some(CapturedReq {
        method: method.to_string(),
        path_and_query: uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        authorization,
        body: body.to_vec(),
    });

    (
        StatusCode::OK,
        [("content-type", "application/json"), ("x-mock", "yes")],
        r#"{"id":"msg_1","content":[{"type":"text","text":"pong"}]}"#,
    )
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
    ))
    .await
    .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn relay_forwards_verbatim_and_returns_unchanged() {
    // 1. Mock upstream that records what it received.
    let cap = Captured::default();
    let upstream_app = Router::new()
        .fallback(any(mock_upstream_handler))
        .with_state(cap.clone());
    let upstream_addr = spawn(upstream_app).await;

    // 2. Proxy pointed at the mock upstream.
    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        format!("http://{upstream_addr}"),
    );
    let proxy_app = router(cfg, None);
    let proxy_addr = spawn(proxy_app).await;

    // 3. Client request through the proxy, with an auth token.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{proxy_addr}/v1/messages?beta=true"))
        .header("authorization", "Bearer secret-token-123")
        .header("content-type", "application/json")
        .body(r#"{"model":"claude","messages":[]}"#)
        .send()
        .await
        .unwrap();

    // Response returned unchanged.
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("x-mock").unwrap(), "yes");
    let text = resp.text().await.unwrap();
    assert!(text.contains("pong"));

    // Upstream saw the request verbatim, auth passed through untouched.
    let got = cap.inner.lock().unwrap().clone().unwrap();
    assert_eq!(got.method, "POST");
    assert_eq!(got.path_and_query, "/v1/messages?beta=true");
    assert_eq!(
        got.authorization.as_deref(),
        Some("Bearer secret-token-123")
    );
    assert_eq!(got.body, br#"{"model":"claude","messages":[]}"#);
}

#[tokio::test]
async fn relay_failure_returns_gateway_error_not_panic() {
    // Proxy pointed at a dead upstream port.
    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        "http://127.0.0.1:1", // unroutable
    );
    let proxy_addr = spawn(router(cfg, None)).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{proxy_addr}/v1/models"))
        .send()
        .await
        .unwrap();
    // Client gets a clean gateway error, the proxy did not crash.
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}
