//! Integration test: a completed round-trip through the proxy is persisted as a
//! single SQLite row with auth redacted, without blocking the request path.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::{http::StatusCode, response::IntoResponse, routing::any, Router};
use tokio::net::TcpListener;
use tracer_core::{
    server::router,
    store::{list_calls, StoreHandle, REDACTED},
    Config,
};

async fn mock_upstream() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        r#"{"id":"msg_x","content":[{"type":"text","text":"pong"}]}"#,
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
async fn roundtrip_is_persisted_with_redacted_auth() {
    // Mock upstream.
    let upstream_addr = spawn(Router::new().fallback(any(mock_upstream))).await;

    // Temp DB.
    let dir = std::env::temp_dir();
    let db = dir.join(format!("tracer-test-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);

    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        format!("http://{upstream_addr}"),
    )
    .with_db_path(&db);

    let store: StoreHandle = tracer_core::store::spawn_writer(&db).unwrap();
    let proxy_addr = spawn(router(cfg.clone(), Some(store))).await;

    // Fire a request through the proxy.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("authorization", "Bearer super-secret")
        .header("content-type", "application/json")
        .body(r#"{"model":"claude","system":"be nice"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.text().await.unwrap().contains("pong"));

    // Give the async writer a moment to drain.
    let mut found = None;
    for _ in 0..50 {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let calls = list_calls(&conn).unwrap();
        if !calls.is_empty() {
            found = Some(calls);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let calls = found.expect("a row should have been written");
    assert_eq!(calls.len(), 1, "exactly one row per round-trip");
    let call = &calls[0].record;

    assert_eq!(call.method, "POST");
    assert_eq!(call.url, "/v1/messages");
    assert_eq!(call.response_status, Some(200));
    assert!(call.timestamp_end.is_some());

    // Auth redacted, other data verbatim.
    assert_eq!(call.request_headers["authorization"], REDACTED);
    assert_eq!(call.request_body.as_ref().unwrap()["system"], "be nice");

    // Non-streaming responses carry their body in `response_body`; it used to be
    // forwarded to the client and dropped, leaving the exchange half-captured.
    let body = call
        .response_body
        .as_ref()
        .expect("a non-streaming response body must be captured");
    assert_eq!(body["id"], "msg_x");
    assert_eq!(body["content"][0]["text"], "pong");
    assert!(
        call.response_raw_sse.is_none(),
        "a non-streaming response has no SSE capture"
    );

    // The writer derives immediately after the raw insert commits, so the
    // analytics layer is queryable without a separate backfill pass.
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (derived, failures): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM generations), (SELECT COUNT(*) FROM derive_failures)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(derived, 1, "the live write path must derive as it captures");
    assert_eq!(failures, 0);

    let _ = std::fs::remove_file(&db);
}
