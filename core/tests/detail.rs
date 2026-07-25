//! Integration test (ticket 07): the call detail endpoint returns the full
//! captured exchange — system prompt, messages, tools, reconstructed response,
//! errors — and never exposes the real auth secret.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::Router;
use serde_json::json;
use tokio::net::TcpListener;
use tracer_core::{
    server::router,
    store::{insert, CallRecord, REDACTED},
    Config,
};

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

async fn get_json(url: String) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::get(url).await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    let value = serde_json::from_str(&text).unwrap_or(json!(null));
    (status, value)
}

#[tokio::test]
async fn detail_returns_full_exchange_and_redacts_auth() {
    let db = std::env::temp_dir().join(format!("tracer-detail-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);

    // Seed one streaming call with system/messages/tools + reconstruction.
    let id = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        tracer_core::store::apply_schema(&conn).unwrap();
        insert(
            &conn,
            &CallRecord {
                timestamp_start: "2026-07-25T00:00:00Z".into(),
                timestamp_first_chunk: Some("2026-07-25T00:00:00.1Z".into()),
                timestamp_end: Some("2026-07-25T00:00:01Z".into()),
                method: "POST".into(),
                url: "/v1/messages".into(),
                // The writer redacts auth on insert; pass the real secret to
                // prove it never survives to the API.
                request_headers: json!({
                    "authorization": "Bearer sk-REAL-SECRET-should-never-appear",
                    "content-type": "application/json"
                }),
                request_body: Some(json!({
                    "model": "claude-x",
                    "system": "You are a careful assistant.",
                    "messages": [{"role": "user", "content": "hi"}],
                    "tools": [{"name": "get_weather", "description": "weather"}]
                })),
                response_status: Some(200),
                response_raw_sse: Some("event: message_stop\ndata: {}\n\n".into()),
                response_reconstructed: Some(json!({
                    "id": "msg_1",
                    "content": [{"type": "text", "text": "hello"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 3, "output_tokens": 5}
                })),
                ..Default::default()
            },
        )
        .unwrap()
    };

    let cfg = Config::default().with_db_path(&db);
    let addr = spawn(router(cfg, None)).await;

    // Detail by id.
    let (status, c) = get_json(format!("http://{addr}/api/calls/{id}")).await;
    assert_eq!(status, 200);

    // System / messages / tools from the request body.
    let body = &c["request_body"];
    assert_eq!(body["system"], "You are a careful assistant.");
    assert_eq!(body["messages"][0]["content"], "hi");
    assert_eq!(body["tools"][0]["name"], "get_weather");

    // Reconstructed response (streaming) + status + stream flag.
    assert_eq!(c["is_stream"], true);
    assert_eq!(c["response_reconstructed"]["content"][0]["text"], "hello");
    assert_eq!(c["response_reconstructed"]["stop_reason"], "end_turn");
    assert_eq!(c["response_status"], 200);

    // Auth is shown redacted; the real secret never appears anywhere.
    assert_eq!(c["request_headers"]["authorization"], REDACTED);
    let whole = c.to_string();
    assert!(
        !whole.contains("sk-REAL-SECRET"),
        "real secret must never be exposed by the detail API"
    );

    // Unknown id -> 404.
    let (missing, _) = get_json(format!("http://{addr}/api/calls/999999")).await;
    assert_eq!(missing, 404);

    let _ = std::fs::remove_file(&db);
}
