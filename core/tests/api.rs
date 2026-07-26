//! Integration test: the read-only API lists captured calls (most recent first,
//! with at-a-glance basics) and the /ui page is served. Read-only is enforced
//! by only exposing GET routes.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::Router;
use orama_core::{
    server::router,
    store::{insert, CallRecord},
    Config,
};
use serde_json::json;
use tokio::net::TcpListener;

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

fn seed(db: &std::path::Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    orama_core::store::apply_schema(&conn).unwrap();
    // Two calls; the second is a streaming error call.
    insert(
        &conn,
        &CallRecord {
            timestamp_start: "2026-07-25T00:00:00Z".into(),
            method: "POST".into(),
            url: "/v1/messages".into(),
            request_headers: json!({"authorization": "Bearer secret"}),
            request_body: Some(json!({"model": "claude-a"})),
            response_status: Some(200),
            ..Default::default()
        },
    )
    .unwrap();
    insert(
        &conn,
        &CallRecord {
            timestamp_start: "2026-07-25T00:01:00Z".into(),
            method: "POST".into(),
            url: "/v1/messages".into(),
            request_headers: json!({}),
            request_body: Some(json!({"model": "claude-b"})),
            response_status: Some(500),
            response_raw_sse: Some("event: x\n".into()),
            error: Some("boom".into()),
            ..Default::default()
        },
    )
    .unwrap();
}

#[tokio::test]
async fn api_lists_calls_most_recent_first_and_serves_ui() {
    let db = std::env::temp_dir().join(format!("orama-api-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);
    seed(&db);

    let cfg = Config::default().with_db_path(&db);
    let addr = spawn(router(cfg, None)).await;

    // List endpoint.
    let list_text = reqwest::get(format!("http://{addr}/api/calls"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&list_text).unwrap();
    let calls = list["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);

    // Most recent first (higher id / later timestamp).
    assert_eq!(calls[0]["model"], "claude-b");
    assert_eq!(calls[0]["status"], 500);
    assert_eq!(calls[0]["is_stream"], true);
    assert_eq!(calls[0]["has_error"], true);
    assert_eq!(calls[1]["model"], "claude-a");
    assert_eq!(calls[1]["is_stream"], false);
    assert_eq!(calls[1]["has_error"], false);
    // At-a-glance basics present.
    assert!(calls[0]["timestamp_start"].is_string());
    assert_eq!(calls[0]["method"], "POST");

    // UI page served as HTML.
    let ui = reqwest::get(format!("http://{addr}/ui")).await.unwrap();
    let ct = ui
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/html"));
    let html = ui.text().await.unwrap();
    assert!(html.to_lowercase().contains("orama"));
    assert!(html.contains("/api/calls"));

    // Read-only: POST to the API is not allowed.
    let post = reqwest::Client::new()
        .post(format!("http://{addr}/api/calls"))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), 405, "API is read-only (GET only)");

    let _ = std::fs::remove_file(&db);
}
