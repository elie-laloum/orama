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

/// A call whose harness configuration is spelled out: a system prompt, a tool
/// set with real descriptions and schemas, and an inline system turn.
fn harness_call(seq: i64, session: &str, prompt: &str, tool_description: &str) -> CallRecord {
    let mut record = call(seq, session, 0, false);
    record.request_body = Some(json!({
        "model": "claude-opus-5",
        "system": [
            {"text": "x-anthropic-billing-header: cc_version=2.1.220.85f; cc_entrypoint=cli;"},
            {"text": prompt, "cache_control": {"type": "ephemeral", "ttl": "1h"}},
        ],
        "tools": [
            {"name": "Bash", "description": tool_description,
             "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}}},
            {"name": "mcp__posthog__exec", "description": "run a query",
             "input_schema": {"type": "object"}},
        ],
        "messages": [
            {"role": "user", "content": "do the thing"},
            // Claude Code injects these mid-thread; they are neither the user
            // nor the model, and they are frequently the largest single item.
            {"role": "system", "content": "skill instructions, injected"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "u1", "name": "Bash", "input": {"command": "ls"}}]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "u1", "content": "a.txt"}]},
        ],
    }));
    record
}

#[tokio::test]
async fn harness_surface_reports_what_the_client_declared() {
    let db = std::env::temp_dir().join(format!("orama-harness-{}.sqlite", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }

    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        apply_schema(&conn).unwrap();
        // Three calls sharing one configuration, then one whose tool description
        // alone differs — a rewritten description is a different harness.
        for seq in 1..=3 {
            insert(
                &conn,
                &harness_call(seq, "s-a", "You are Claude Code.", "run a command"),
            )
            .unwrap();
        }
        insert(
            &conn,
            &harness_call(4, "s-a", "You are Claude Code.", "run a shell command"),
        )
        .unwrap();
        backfill(&conn).unwrap();
    }

    let addr = spawn(router(Config::default().with_db_path(&db), None)).await;

    let (status, harness) = get(addr, "/api/v2/harness").await;
    assert_eq!(status, 200);

    // The system prompt is identical across all four calls, so it is stored once
    // however many calls re-sent it. That dedup is the whole reason the content
    // is affordable to keep verbatim.
    let prompts = harness["system_prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["generations"], 4);
    // The opening skips the billing header, which identifies nothing.
    assert!(prompts[0]["opening"]
        .as_str()
        .unwrap()
        .starts_with("You are Claude Code."));

    // Two tool sets: same names, one rewritten description.
    let sets = harness["tool_sets"].as_array().unwrap();
    assert_eq!(
        sets.len(),
        2,
        "a changed description is a different tool set"
    );
    assert!(sets
        .iter()
        .all(|set| set["tool_count"] == 2 && set["mcp_count"] == 1));

    // Where the characters go, corpus-wide.
    let budget = &harness["budget"];
    assert!(budget["tools_chars"].as_i64().unwrap() > 0);
    assert!(budget["system_chars"].as_i64().unwrap() > 0);

    // A declared tool that was never called is the point of this list.
    let declared = harness["declared_tools"].as_array().unwrap();
    let unused = declared
        .iter()
        .find(|tool| tool["name"] == "mcp__posthog__exec")
        .unwrap();
    assert_eq!(unused["calls"], 0);
    assert_eq!(unused["is_mcp"], 1);
    assert!(unused["chars"].as_i64().unwrap() > 0);

    // The prompt reads back verbatim, segment by segment, with its cache points.
    let hash = prompts[0]["system_hash"].as_str().unwrap().to_owned();
    let (status, detail) = get(addr, &format!("/api/v2/harness/system/{hash}")).await;
    assert_eq!(status, 200);
    let segments = detail["system"]["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1]["text"], "You are Claude Code.");
    assert_eq!(segments[1]["cache_control"], true);
    assert_eq!(detail["usage"]["generations"], 4);

    // The tool set reads back with schemas decoded, heaviest first.
    let set_hash = sets[0]["tools_hash"].as_str().unwrap().to_owned();
    let (status, set) = get(addr, &format!("/api/v2/harness/tools/{set_hash}")).await;
    assert_eq!(status, 200);
    let tools = set["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools[0]["chars"].as_i64().unwrap() >= tools[1]["chars"].as_i64().unwrap());
    assert!(tools
        .iter()
        .any(|tool| tool["input_schema"]["type"] == "object"));

    let (status, _) = get(addr, "/api/v2/harness/system/nope").await;
    assert_eq!(status, 404);
    let (status, _) = get(addr, "/api/v2/harness/tools/nope").await;
    assert_eq!(status, 404);

    // Per-call composition: the three sections are disjoint and sum to the whole.
    let (_, page) = get(addr, "/api/v2/generations?limit=1").await;
    let span = page["generations"][0]["span_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, context) = get(addr, &format!("/api/v2/generations/{span}/context")).await;
    assert_eq!(status, 200);
    let composition = &context["composition"];
    let parts: i64 = ["system_chars", "tools_chars", "history_chars"]
        .iter()
        .map(|key| composition[*key].as_i64().unwrap())
        .sum();
    assert_eq!(parts, composition["total_chars"].as_i64().unwrap());
    assert!(composition["tools_chars"].as_i64().unwrap() > 0);

    // The injected system turn is reported as a system turn, not as "other" —
    // otherwise the largest non-tool item in the context is invisible.
    let turns = context["thread"]["turns"].as_array().unwrap();
    assert!(
        turns.iter().any(|turn| turn["role"] == "system"),
        "an inline system turn must keep its role: {turns:?}"
    );
    // Blocks carry their kind and their weight, so the shape reads before the text.
    let kinds = context["thread"]["by_kind"].as_array().unwrap();
    assert!(kinds.iter().any(|kind| kind["kind"] == "tool_use"));
    assert!(kinds.iter().all(|kind| kind["chars"].as_i64().unwrap() > 0));
    assert!(context["tools"].as_array().unwrap().len() == 2);

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
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
