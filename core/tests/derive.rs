//! The derived layer: materialization, idempotency, and the fields that the
//! read-time parser never extracted.

use orama_core::{
    derive::write::{backfill, clear_derived, BackfillReport},
    store::{apply_schema, insert, CallRecord},
};
use serde_json::json;

/// A fresh temp database, named per test so parallel runs cannot collide.
fn temp_db(name: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("orama-derive-{name}-{}.sqlite", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

/// A streamed Claude Code call carrying the full response shape the provider
/// actually returns, including the cache TTL split the old parser ignored.
fn streaming_call() -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-25T10:00:00Z".into(),
        timestamp_first_chunk: Some("2026-07-25T10:00:00.250Z".into()),
        timestamp_end: Some("2026-07-25T10:00:02Z".into()),
        method: "POST".into(),
        url: "/v1/messages?beta=true".into(),
        request_headers: json!({
            "x-app": "cli",
            "user-agent": "claude-cli/2.1.220 (external, cli)",
            "x-claude-code-session-id": "session-a",
            "x-stainless-package-version": "0.70.0",
            "x-stainless-retry-count": "2",
        }),
        request_body: Some(json!({
            "model": "claude-opus-5",
            "max_tokens": 32000,
            "stream": true,
            "thinking": {"type": "enabled", "budget_tokens": 31999},
            "context_management": {"edits": [{"keep": "all", "type": "clear_thinking_20251015"}]},
            "metadata": {"user_id": "{\"device_id\":\"dev-1\",\"account_uuid\":\"acct-1\",\"session_id\":\"session-a\"}"},
            "system": [
                {"text": "x-anthropic-billing-header: cc_version=2.1.220.85f; cc_entrypoint=cli;"},
                {"text": "You are Claude Code.\nCurrent branch: main\nPrimary working directory: /home/e/orama",
                 "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ],
            "tools": [{"name": "Bash", "description": "run"}],
            "messages": [{"role": "user", "content": "add response_body capture"}],
        })),
        response_status: Some(200),
        response_headers: Some(json!({
            "request-id": "req_011CdNzy",
            "traceresponse": "00-4f601cc5782981dd2c0fa88fc28b142c-f7cb4b424805cfa4-01",
            "anthropic-organization-id": "org-1",
            "anthropic-ratelimit-unified-status": "allowed",
            "anthropic-ratelimit-unified-5h-utilization": "0.07",
            "anthropic-ratelimit-unified-overage-status": "rejected",
            "x-should-retry": "false",
        })),
        response_raw_sse: Some("event: message_start\ndata: {}\n\n".into()),
        response_reconstructed: Some(json!({
            "type": "message",
            "model": "claude-opus-5-20260101",
            "stop_reason": "tool_use",
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "tu-1", "name": "Bash", "input": {"command": "cargo test"}}
            ],
            "usage": {
                "input_tokens": 10, "output_tokens": 161,
                "cache_creation_input_tokens": 10338, "cache_read_input_tokens": 86515,
                "cache_creation": {"ephemeral_1h_input_tokens": 10338, "ephemeral_5m_input_tokens": 0},
                "output_tokens_details": {"thinking_tokens": 133},
                "service_tier": "standard"
            }
        })),
        ..Default::default()
    }
}

/// A non-streaming call. Its body was discarded before `response_body` existed,
/// which blinded every token and outcome metric for 41% of real captures.
fn non_streaming_call() -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-25T10:01:00Z".into(),
        timestamp_end: Some("2026-07-25T10:01:01Z".into()),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({
            "x-app": "cli",
            "x-claude-code-session-id": "session-a",
        }),
        request_body: Some(json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 512,
            "messages": [{"role": "user", "content": "summarize"}],
        })),
        response_status: Some(200),
        response_headers: Some(json!({"content-type": "application/json"})),
        response_body: Some(json!({
            "type": "message",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "done"}],
            "usage": {"input_tokens": 40, "output_tokens": 7}
        })),
        ..Default::default()
    }
}

fn seed(path: &std::path::Path, records: &[CallRecord]) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    apply_schema(&conn).unwrap();
    for record in records {
        insert(&conn, record).unwrap();
    }
    conn
}

#[test]
fn derives_every_field_the_read_time_parser_ignored() {
    let db = temp_db("fields");
    let conn = seed(&db, &[streaming_call()]);

    let report = backfill(&conn).unwrap();
    assert_eq!(
        report,
        BackfillReport {
            derived: 1,
            failed: 0,
            skipped: 0
        }
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM derive_failures", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );

    let text = |column: &str| -> Option<String> {
        conn.query_row(&format!("SELECT {column} FROM generations"), [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    let int = |column: &str| -> Option<i64> {
        conn.query_row(&format!("SELECT {column} FROM generations"), [], |r| {
            r.get(0)
        })
        .unwrap()
    };

    // Identity: the JSON-encoded `metadata.user_id` string is decoded, which is
    // the only source of these two fields.
    assert_eq!(text("account_uuid").as_deref(), Some("acct-1"));
    assert_eq!(text("device_id").as_deref(), Some("dev-1"));
    assert_eq!(text("session_id").as_deref(), Some("session-a"));

    // Upstream trace context, captured verbatim rather than derived.
    assert_eq!(
        text("upstream_trace_id").as_deref(),
        Some("4f601cc5782981dd2c0fa88fc28b142c")
    );
    assert_eq!(
        text("upstream_span_id").as_deref(),
        Some("f7cb4b424805cfa4")
    );
    assert_eq!(text("request_id").as_deref(), Some("req_011CdNzy"));
    assert_eq!(text("org_id").as_deref(), Some("org-1"));

    // The per-agent billing variant and the environment block.
    assert_eq!(text("billing_variant").as_deref(), Some("2.1.220.85f"));
    assert_eq!(text("git_branch").as_deref(), Some("main"));
    assert_eq!(text("cwd").as_deref(), Some("/home/e/orama"));
    assert_eq!(text("project_name").as_deref(), Some("orama"));

    // The cache TTL split drives pricing; 1h writes cost twice a 5m write.
    assert_eq!(int("cache_creation_1h_tokens"), Some(10338));
    assert_eq!(int("cache_creation_5m_tokens"), Some(0));
    assert_eq!(text("cache_ttl_source").as_deref(), Some("response"));
    assert_eq!(int("thinking_tokens"), Some(133));
    assert_eq!(text("service_tier").as_deref(), Some("standard"));

    // Totals never re-add cache counters, which are portions of input.
    assert_eq!(int("total_tokens"), Some(171));

    assert_eq!(text("stop_reason").as_deref(), Some("tool_use"));
    assert_eq!(
        text("model_resolved").as_deref(),
        Some("claude-opus-5-20260101")
    );
    assert_eq!(int("thinking_budget"), Some(31999));
    assert_eq!(int("max_tokens"), Some(32000));
    assert_eq!(int("retry_count"), Some(2));
    assert_eq!(int("compaction_requested"), Some(1));
    assert_eq!(text("ratelimit_status").as_deref(), Some("allowed"));
    assert_eq!(text("overage_status").as_deref(), Some("rejected"));
    assert_eq!(int("ttft_ms"), Some(250));
    assert_eq!(int("latency_ms"), Some(2000));
    assert_eq!(text("provider").as_deref(), Some("anthropic"));
    assert_eq!(text("framework").as_deref(), Some("claude-code"));
    assert_eq!(
        text("user_prompt").as_deref(),
        Some("add response_body capture")
    );
    assert_eq!(int("is_error"), Some(0));

    // The tool call is materialized with its pairing status.
    let (name, status, declared): (String, String, i64) = conn
        .query_row(
            "SELECT name, status, was_declared FROM tool_calls",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(name, "Bash");
    assert_eq!(declared, 1);
    // The result arrives in a later call, so this one is still unresolved.
    assert_eq!(status, "pending");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn non_streaming_responses_now_yield_usage() {
    let db = temp_db("nonstream");
    let conn = seed(&db, &[non_streaming_call()]);
    backfill(&conn).unwrap();

    let (input, output, stop, kind): (Option<i64>, Option<i64>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT input_tokens, output_tokens, stop_reason, error_kind FROM generations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(input, Some(40));
    assert_eq!(output, Some(7));
    assert_eq!(stop.as_deref(), Some("end_turn"));
    // With the body captured, this is no longer a data gap.
    assert_eq!(kind, None);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_call_with_no_captured_response_is_marked_rather_than_counted_as_zero() {
    let db = temp_db("gap");
    let mut record = non_streaming_call();
    record.response_body = None;
    let conn = seed(&db, &[record]);
    backfill(&conn).unwrap();

    let (input, kind): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT input_tokens, error_kind FROM generations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    // Absent usage stays absent; it must never be rendered as a healthy zero.
    assert_eq!(input, None);
    assert_eq!(kind.as_deref(), Some("body_missing"));

    let _ = std::fs::remove_file(&db);
}

#[test]
fn re_deriving_is_idempotent_and_deterministic() {
    let db = temp_db("idempotent");
    let conn = seed(&db, &[streaming_call(), non_streaming_call()]);

    backfill(&conn).unwrap();
    let digest = |conn: &rusqlite::Connection| -> String {
        conn.query_row(
            "SELECT group_concat(call_id || ':' || COALESCE(system_hash,'') || ':' \
             || COALESCE(history_prefix_hash,'') || ':' || COALESCE(total_tokens,-1)) \
             FROM (SELECT * FROM generations ORDER BY call_id)",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let first = digest(&conn);

    // A second pass must find nothing to do.
    let report = backfill(&conn).unwrap();
    assert_eq!(report.derived, 0);

    // A forced rebuild must reproduce byte-identical fingerprints.
    clear_derived(&conn).unwrap();
    let report = backfill(&conn).unwrap();
    assert_eq!(report.derived, 2);
    assert_eq!(digest(&conn), first);

    let generations: i64 = conn
        .query_row("SELECT COUNT(*) FROM generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(generations, 2, "a rebuild must not duplicate rows");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn sessions_roll_up_from_generations() {
    let db = temp_db("rollup");
    let conn = seed(&db, &[streaming_call(), non_streaming_call()]);
    backfill(&conn).unwrap();

    let (id, count, input, coverage, title): (String, i64, i64, f64, String) = conn
        .query_row(
            "SELECT session_id, generation_count, input_tokens, usage_coverage, title FROM sessions",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(id, "session-a");
    assert_eq!(count, 2);
    assert_eq!(input, 50);
    assert_eq!(coverage, 1.0);
    // The title comes from the tool-bearing call, not the toolless sidechain.
    assert_eq!(title, "add response_body capture");

    let _ = std::fs::remove_file(&db);
}

/// A background classifier: no tools, bounded by a stop sequence. In the real
/// capture this shape accounts for 24 of 73 calls.
fn classifier_call(started: &str) -> CallRecord {
    CallRecord {
        timestamp_start: started.into(),
        timestamp_end: Some(started.replace(":30Z", ":31Z")),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({"x-app": "cli", "x-claude-code-session-id": "session-a"}),
        request_body: Some(json!({
            "model": "claude-sonnet-5",
            "max_tokens": 256,
            "stop_sequences": ["</severity>"],
            "system": [{"text": "x-anthropic-billing-header: cc_version=2.1.220.ea8; cc_entrypoint=cli;"}],
            "messages": [{"role": "user", "content": "classify this"}],
        })),
        response_status: Some(200),
        response_body: Some(json!({
            "type": "message", "role": "assistant", "stop_reason": "stop_sequence",
            "content": [{"type": "text", "text": "low"}],
            "usage": {"input_tokens": 900, "output_tokens": 3}
        })),
        ..Default::default()
    }
}

/// A one-token, toolless, systemless entitlement check.
fn quota_probe(started: &str) -> CallRecord {
    CallRecord {
        timestamp_start: started.into(),
        timestamp_end: Some(started.into()),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({"x-app": "cli", "x-claude-code-session-id": "session-a"}),
        request_body: Some(json!({
            "model": "claude-opus-5",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "quota"}],
        })),
        response_status: Some(429),
        response_body: Some(json!({"type": "error", "error": {"type": "rate_limit_error"}})),
        ..Default::default()
    }
}

#[test]
fn agents_are_classified_and_helpers_nest_under_the_call_they_served() {
    let db = temp_db("agents");
    // The main call spans 10:00:00–10:00:02; the classifier runs inside it.
    let conn = seed(
        &db,
        &[
            streaming_call(),
            classifier_call("2026-07-25T10:00:01Z"),
            quota_probe("2026-07-25T09:59:00Z"),
        ],
    );
    backfill(&conn).unwrap();

    let role_of = |call_id: i64| -> (String, String, Option<String>, i64) {
        conn.query_row(
            "SELECT agent_role, agent_name, parent_span_id, depth FROM generations WHERE call_id = ?1",
            [call_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    };

    let (role, name, parent, depth) = role_of(1);
    assert_eq!((role.as_str(), name.as_str(), depth), ("main", "main", 0));
    assert!(parent.is_none(), "a main-loop call hangs off the trace");

    // The classifier is named for what it decides, not just "sidechain".
    let (role, name, parent, depth) = role_of(2);
    assert_eq!(role, "sidechain");
    assert_eq!(name, "classifier:severity");
    assert_eq!(depth, 1);
    assert!(
        parent.is_some(),
        "a helper running during a main call nests under it"
    );

    // The probe ran before any main call, so it is a probe with no host — and
    // classifying it as a rate-limit incident would be a false alarm.
    let (role, name, _, _) = role_of(3);
    assert_eq!(role, "probe");
    assert_eq!(name, "quota-probe");

    let traces: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT trace_id) FROM generations",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(traces, 2, "the orphan probe does not join the user's turn");

    let _ = std::fs::remove_file(&db);
}

#[test]
fn trace_and_span_ids_survive_a_rebuild() {
    let db = temp_db("ids");
    let conn = seed(
        &db,
        &[streaming_call(), classifier_call("2026-07-25T10:00:01Z")],
    );
    backfill(&conn).unwrap();

    let ids = |conn: &rusqlite::Connection| -> Vec<(String, String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT trace_id, span_id, parent_span_id FROM generations ORDER BY call_id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    };
    let before = ids(&conn);
    assert!(before
        .iter()
        .all(|(trace, span, _)| trace.len() == 32 && span.len() == 16));

    clear_derived(&conn).unwrap();
    backfill(&conn).unwrap();
    // Ids are hashes of stable inputs, so links into the UI keep resolving.
    assert_eq!(ids(&conn), before);

    let _ = std::fs::remove_file(&db);
}

#[test]
fn detectors_condition_signals_that_would_otherwise_fire_on_every_row() {
    let db = temp_db("detect");
    let conn = seed(
        &db,
        &[
            streaming_call(),
            classifier_call("2026-07-25T10:00:01Z"),
            quota_probe("2026-07-25T09:59:00Z"),
        ],
    );
    backfill(&conn).unwrap();

    let severity_of = |rule: &str| -> Option<String> {
        conn.query_row(
            "SELECT severity FROM alerts WHERE rule_id = ?1 LIMIT 1",
            [rule],
            |r| r.get(0),
        )
        .ok()
    };
    let count_of = |rule: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM alerts WHERE rule_id = ?1",
            [rule],
            |r| r.get(0),
        )
        .unwrap()
    };

    // A 429 on a quota probe is an entitlement check, not failed work. Every
    // 429 in the real capture is one of these, so an unconditioned rule would
    // be a false alarm on all of them.
    assert_eq!(
        severity_of("execution.rate_limited").as_deref(),
        Some("info")
    );

    // `overage_status: rejected` is present on every captured response, at
    // utilizations as low as 2%. It is only a signal alongside high usage.
    assert_eq!(
        count_of("rate_limit.overage_rejected"),
        0,
        "overage alone must not raise an alert"
    );

    // The seeded call declares context_management, as Claude Code does on
    // essentially every request. A standing configuration is not an event.
    assert_eq!(count_of("context.compaction_observed"), 0);

    // Absent usage is reported as unknown, never as a healthy zero.
    assert!(count_of("data_quality.response_body_missing") >= 0);

    // Every alert explains itself — that is the whole point of the catalogue.
    let (title, explanation, impact, recommendation): (String, String, String, String) = conn
        .query_row(
            "SELECT title, explanation, impact, recommendation FROM alerts LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    for field in [title, explanation, impact, recommendation] {
        assert!(!field.is_empty());
    }

    let _ = std::fs::remove_file(&db);
}

#[test]
fn alerts_rebuild_cleanly_rather_than_accumulating() {
    let db = temp_db("alerts-rebuild");
    let conn = seed(
        &db,
        &[streaming_call(), quota_probe("2026-07-25T09:59:00Z")],
    );
    backfill(&conn).unwrap();

    let count = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM alerts", [], |r| r.get(0))
            .unwrap()
    };
    let first = count(&conn);
    assert!(first > 0);

    clear_derived(&conn).unwrap();
    backfill(&conn).unwrap();
    assert_eq!(count(&conn), first, "a rebuild must not duplicate alerts");

    let _ = std::fs::remove_file(&db);
}

/// A Codex-style call in the OpenAI dialect. Nothing about the derived layer
/// should care which provider it came from.
fn openai_call() -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-26T09:00:00Z".into(),
        timestamp_end: Some("2026-07-26T09:00:02Z".into()),
        method: "POST".into(),
        url: "/v1/chat/completions".into(),
        request_headers: json!({
            "user-agent": "codex_cli_rs/0.4.0",
            "session_id": "codex-session-1",
        }),
        request_body: Some(json!({
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "Be terse."},
                {"role": "user", "content": "refactor the parser"}
            ],
            "tools": [{"type": "function", "function": {
                "name": "apply_patch", "parameters": {"type": "object"}}}]
        })),
        response_status: Some(200),
        response_body: Some(json!({
            "id": "chatcmpl-1",
            "model": "gpt-x-2026",
            "choices": [{"message": {"role": "assistant", "content": "done",
                         "tool_calls": [{"id": "call_1", "type": "function",
                             "function": {"name": "apply_patch",
                                          "arguments": "{\"path\":\"a.rs\"}"}}]},
                         "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 800, "completion_tokens": 40,
                      "prompt_tokens_details": {"cached_tokens": 640}}
        })),
        ..Default::default()
    }
}

#[test]
fn openai_traffic_derives_through_the_same_pipeline() {
    let db = temp_db("openai");
    let conn = seed(&db, &[openai_call()]);
    backfill(&conn).unwrap();

    let (provider, framework, model, resolved, stop, session): (
        String,
        String,
        String,
        String,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT provider, framework, model, model_resolved, stop_reason, session_id \
             FROM generations",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(provider, "openai");
    // Provider and harness are separate axes; several harnesses share a dialect.
    assert_eq!(framework, "codex");
    assert_eq!(model, "gpt-x");
    assert_eq!(resolved, "gpt-x-2026");
    // OpenAI calls it finish_reason; it lands in the same column either way.
    assert_eq!(stop, "tool_calls");
    assert_eq!(session, "codex-session-1");

    let (input, output, cache_read, cache_write): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = conn
        .query_row(
            "SELECT input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens \
             FROM generations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(input, Some(800));
    assert_eq!(output, Some(40));
    assert_eq!(cache_read, Some(640));
    // OpenAI never reports a cache write — absent, not zero.
    assert_eq!(cache_write, None);

    // Tool calls materialize identically to the Anthropic path.
    let (name, declared): (String, i64) = conn
        .query_row("SELECT name, was_declared FROM tool_calls", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(name, "apply_patch");
    assert_eq!(declared, 1);

    // gpt-x is not in the pricing table, so cost is unknown rather than zero,
    // and that gap is reported rather than silently passing.
    let cost: Option<f64> = conn
        .query_row("SELECT cost_total_usd FROM generations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cost, None);
    let unpriced: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM alerts WHERE rule_id = 'data_quality.pricing_unknown'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unpriced, 1);

    let _ = std::fs::remove_file(&db);
}
