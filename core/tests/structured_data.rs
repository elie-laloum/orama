//! Read-only API coverage for normalized Claude Code calls and sessions.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::Router;
use serde_json::json;
use tokio::net::TcpListener;
use tracer_core::{
    server::router,
    store::{insert, CallRecord},
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

async fn get_json(url: String) -> serde_json::Value {
    let text = reqwest::get(url).await.unwrap().text().await.unwrap();
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
async fn derived_endpoints_normalize_and_group_claude_code_calls() {
    let db = std::env::temp_dir().join(format!("tracer-structured-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let id = {
        let conn = rusqlite::Connection::open(&db).unwrap();
        tracer_core::store::apply_schema(&conn).unwrap();
        insert(&conn, &CallRecord {
            timestamp_start: "2026-07-25T00:00:00Z".into(), timestamp_first_chunk: Some("2026-07-25T00:00:00.100Z".into()), timestamp_end: Some("2026-07-25T00:00:01Z".into()), method: "POST".into(), url: "/v1/messages".into(),
            request_headers: json!({"x-app":"cli", "x-claude-code-session-id":"session-a"}),
            request_body: Some(json!({"model":"claude-x", "system":[{"type":"text","text":"rules","cache_control":{"type":"ephemeral"}}], "tools":[{"name":"read","input_schema":{"type":"object"}}], "messages":[{"role":"user","content":"hi"}]})),
            response_status: Some(200), response_reconstructed: Some(json!({"role":"assistant","content":[{"type":"tool_use","id":"use-1","name":"read","input":{"path":"x"}}],"usage":{"input_tokens":11,"output_tokens":2,"cache_read_input_tokens":4,"cache_creation_input_tokens":3}})),
            ..Default::default()
        }).unwrap()
    };
    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;

    let normalized = get_json(format!("http://{addr}/api/calls/{id}/normalized")).await;
    assert_eq!(normalized["provider"], "claude_code");
    assert_eq!(normalized["session_key"], "session-a");
    assert_eq!(normalized["thread"].as_array().unwrap().len(), 2);
    assert_eq!(normalized["thread"][1]["origin"], "new");
    assert_eq!(normalized["usage"]["cache_read"], 4);
    assert_eq!(normalized["intra"]["tool_calls"][0]["name"], "read");

    let sessions = get_json(format!("http://{addr}/api/sessions")).await;
    assert_eq!(sessions["sessions"][0]["key"], "session-a");
    let session = get_json(format!("http://{addr}/api/sessions/session-a")).await;
    assert_eq!(session["calls"][0]["ttft_ms"], 100);
    assert_eq!(session["calls"][0]["latency_ms"], 1000);

    let policy = get_json(format!("http://{addr}/api/signal-policy")).await;
    assert_eq!(policy["version"], "2026-07-25.1");
    let diagnostics = get_json(format!("http://{addr}/api/calls/{id}/diagnostics")).await;
    assert_eq!(diagnostics["token_metrics"]["total_tokens"], 13);
    assert!(
        (diagnostics["token_metrics"]["cache_reuse_rate"]
            .as_f64()
            .unwrap()
            - 4.0 / 18.0)
            .abs()
            < f64::EPSILON
    );
    assert_eq!(
        diagnostics["alerts"][0]["rule_id"],
        "execution.tool_result_missing"
    );
    let session_diagnostics =
        get_json(format!("http://{addr}/api/sessions/session-a/diagnostics")).await;
    assert!(session_diagnostics["alerts"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_file(&db);
}

#[tokio::test]
async fn dashboard_uses_reconstructed_usage_for_exact_token_totals() {
    let db = std::env::temp_dir().join(format!("tracer-dashboard-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let conn = rusqlite::Connection::open(&db).unwrap();
    tracer_core::store::apply_schema(&conn).unwrap();
    insert(&conn, &CallRecord {
        timestamp_start: "2026-07-25T00:00:00Z".into(),
        request_headers: json!({"x-app":"cli", "x-claude-code-session-id":"session-a"}),
        request_body: Some(json!({"model":"claude-x", "messages":[]})),
        response_reconstructed: Some(json!({"role":"assistant", "content":"ok", "usage":{"input_tokens":100,"output_tokens":25,"cache_read_input_tokens":80}})),
        ..Default::default()
    }).unwrap();
    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;
    let dashboard = get_json(format!("http://{addr}/api/ui/dashboard")).await;
    assert_eq!(dashboard["totals"]["total_tokens"], 125);
    let _ = std::fs::remove_file(&db);
}
