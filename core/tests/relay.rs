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
use orama_core::{server::router, Config};
use tokio::net::TcpListener;

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
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
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
async fn each_dialect_reaches_its_own_upstream_through_one_listener() {
    // Two distinct upstreams, so "went to the right one" is observable rather
    // than inferred. Before per-dialect routing a Codex request was forwarded
    // to the Anthropic upstream, which no amount of parsing could recover from.
    let anthropic = Captured::default();
    let openai = Captured::default();
    let anthropic_addr = spawn(
        Router::new()
            .fallback(any(mock_upstream_handler))
            .with_state(anthropic.clone()),
    )
    .await;
    let openai_addr = spawn(
        Router::new()
            .fallback(any(mock_upstream_handler))
            .with_state(openai.clone()),
    )
    .await;

    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        format!("http://{anthropic_addr}"),
    )
    .with_openai_upstream(format!("http://{openai_addr}"));
    let proxy_addr = spawn(router(cfg, None)).await;

    let client = reqwest::Client::new();
    client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("anthropic-version", "2023-06-01")
        .body(r#"{"model":"claude-opus-5"}"#)
        .send()
        .await
        .unwrap();
    client
        .post(format!("http://{proxy_addr}/v1/responses"))
        .header("user-agent", "codex_cli_rs/1.0")
        .body(r#"{"model":"gpt-5"}"#)
        .send()
        .await
        .unwrap();

    let to_anthropic = anthropic.inner.lock().unwrap().clone().unwrap();
    let to_openai = openai.inner.lock().unwrap().clone().unwrap();
    assert_eq!(to_anthropic.path_and_query, "/v1/messages");
    assert_eq!(to_anthropic.body, br#"{"model":"claude-opus-5"}"#);
    assert_eq!(to_openai.path_and_query, "/v1/responses");
    assert_eq!(to_openai.body, br#"{"model":"gpt-5"}"#);
}

#[tokio::test]
async fn stopping_the_proxy_refuses_traffic_and_starting_resumes_it() {
    let cap = Captured::default();
    let upstream_addr = spawn(
        Router::new()
            .fallback(any(mock_upstream_handler))
            .with_state(cap.clone()),
    )
    .await;

    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        format!("http://{upstream_addr}"),
    );
    let proxy_addr = spawn(router(cfg, None)).await;
    let client = reqwest::Client::new();

    let call = |path: &'static str| {
        let client = client.clone();
        async move {
            client
                .post(format!("http://{proxy_addr}{path}"))
                .body(r#"{"model":"claude"}"#)
                .send()
                .await
                .unwrap()
        }
    };

    // A fresh proxy relays: stopping is a decision, never a default.
    assert_eq!(call("/v1/messages").await.status(), StatusCode::OK);

    client
        .post(format!("http://{proxy_addr}/api/v2/proxy/stop"))
        .send()
        .await
        .unwrap();
    *cap.inner.lock().unwrap() = None;

    let refused = call("/v1/messages?after=stop").await;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.headers().get("x-orama-state").unwrap(), "stopped");
    // Refused, not merely unreported: upstream never saw the request.
    assert!(cap.inner.lock().unwrap().is_none());

    // The dashboard is served by the same listener, so it has to survive the
    // stop — otherwise there is no way back.
    let body = client
        .get(format!("http://{proxy_addr}/api/v2/settings"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let settings: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(settings["proxy"]["running"], serde_json::json!(false));

    client
        .post(format!("http://{proxy_addr}/api/v2/proxy/start"))
        .send()
        .await
        .unwrap();

    assert_eq!(
        call("/v1/messages?after=start").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        cap.inner.lock().unwrap().clone().unwrap().path_and_query,
        "/v1/messages?after=start"
    );
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
