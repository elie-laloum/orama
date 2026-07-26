//! The v2 API, served from the derived tables.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::Router;
use orama_core::{
    derive::write::backfill,
    server::router,
    store::{apply_schema, insert, CallRecord},
    Config,
};
use serde_json::{json, Value};
use tokio::net::TcpListener;

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn call(seq: i64, session: &str, role_tools: usize, error: bool) -> CallRecord {
    let tools: Vec<Value> = (0..role_tools)
        .map(|i| json!({"name": format!("tool{i}")}))
        .collect();
    CallRecord {
        timestamp_start: format!("2026-07-26T10:{:02}:00Z", seq),
        timestamp_end: Some(format!("2026-07-26T10:{:02}:02Z", seq)),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({"x-app": "cli", "x-claude-code-session-id": session}),
        request_body: Some(json!({
            "model": "claude-opus-5",
            "max_tokens": 4096,
            "system": [{"text": "x-anthropic-billing-header: cc_version=2.1.220.85f; cc_entrypoint=cli;"},
                       {"text": "You are Claude Code."}],
            "tools": tools,
            "messages": [{"role": "user", "content": format!("task {seq}")}],
        })),
        response_status: Some(if error { 500 } else { 200 }),
        response_body: Some(json!({
            "type": "message", "role": "assistant", "model": "claude-opus-5",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 100, "output_tokens": 20,
                      "cache_read_input_tokens": 5000}
        })),
        ..Default::default()
    }
}

async fn get(addr: SocketAddr, path: &str) -> (u16, Value) {
    let response = reqwest::get(format!("http://{addr}{path}")).await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

#[tokio::test]
async fn v2_serves_every_surface_from_the_derived_tables() {
    let db = std::env::temp_dir().join(format!("orama-v2-{}.sqlite", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }

    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        apply_schema(&conn).unwrap();
        for seq in 1..=3 {
            insert(&conn, &call(seq, "session-a", 5, false)).unwrap();
        }
        insert(&conn, &call(4, "session-a", 0, true)).unwrap();
        backfill(&conn).unwrap();
    }

    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;

    // Meta reports coverage so a partial total is never shown as complete.
    let (status, meta) = get(addr, "/api/v2/meta").await;
    assert_eq!(status, 200);
    assert_eq!(meta["generations"], 4);
    assert!(meta["parser_version"].is_string());
    assert!(meta["usage_coverage"].as_f64().unwrap() > 0.0);

    let (_, overview) = get(addr, "/api/v2/overview").await;
    assert_eq!(overview["totals"]["generations"], 4);
    assert_eq!(overview["totals"]["errors"], 1);
    // Caching is the dominant cost term, so the saving must be reported.
    assert!(overview["totals"]["cache_savings_usd"].as_f64().unwrap() > 0.0);

    let (_, page) = get(addr, "/api/v2/generations?limit=2").await;
    assert_eq!(page["generations"].as_array().unwrap().len(), 2);

    // A filter must actually filter — an ignored one would misreport the data.
    let (_, errors_only) = get(addr, "/api/v2/generations?is_error=1&limit=50").await;
    assert_eq!(errors_only["generations"].as_array().unwrap().len(), 1);
    let (_, none) = get(addr, "/api/v2/generations?model=nope&limit=50").await;
    assert!(none["generations"].as_array().unwrap().is_empty());

    // An unknown filter is rejected rather than silently dropped.
    let (status, body) = get(addr, "/api/v2/generations?bogus=1").await;
    assert_eq!(status, 400);
    assert!(body["error"].as_str().unwrap().contains("bogus"));

    // Detail routes resolve by span id and by call id.
    let span = page["generations"][0]["span_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, detail) = get(addr, &format!("/api/v2/generations/{span}")).await;
    assert_eq!(status, 200);
    assert!(detail["generation"]["call_id"].is_number());
    assert!(detail["tool_calls"].is_array());

    // Raw stays a separate route: it is evidence, not the default view.
    let (status, raw) = get(addr, &format!("/api/v2/generations/{span}/raw")).await;
    assert_eq!(status, 200);
    assert!(
        raw["request_body"].is_object(),
        "raw JSON is decoded, not escaped"
    );

    let (_, sessions) = get(addr, "/api/v2/sessions").await;
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    let (status, session) = get(addr, "/api/v2/sessions/session-a").await;
    assert_eq!(status, 200);
    assert_eq!(session["timeline"].as_array().unwrap().len(), 4);
    assert!(!session["agents"].as_array().unwrap().is_empty());

    let (_, traces) = get(addr, "/api/v2/traces").await;
    assert!(!traces["traces"].as_array().unwrap().is_empty());

    let (_, alerts) = get(addr, "/api/v2/alerts?severity=error").await;
    for alert in alerts["alerts"].as_array().unwrap() {
        assert_eq!(alert["severity"], "error");
        // Every alert explains itself; that is the point of the catalogue.
        assert!(!alert["recommendation"].as_str().unwrap().is_empty());
    }

    let (_, cost) = get(addr, "/api/v2/cost?group_by=model").await;
    let bucket = &cost["buckets"][0];
    assert_eq!(bucket["bucket"], "claude-opus-5");
    // Coverage travels with the total so a partial one is never mistaken.
    assert!(bucket["priced_share"].as_f64().unwrap() > 0.0);

    let (status, _) = get(addr, "/api/v2/cost?group_by=nonsense").await;
    assert_eq!(status, 400);

    let (_, tools) = get(addr, "/api/v2/tools").await;
    assert!(tools["tools"].is_array());

    let (status, missing) = get(addr, "/api/v2/traces/does-not-exist").await;
    assert_eq!(status, 404);
    assert_eq!(missing["error"], "not found");

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
}
