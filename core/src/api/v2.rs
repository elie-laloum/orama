//! The v2 read API, served from the derived tables.
//!
//! v1 re-parsed every stored body on every request — the alerts endpoint was
//! O(sessions × calls) full normalizations. Everything here is indexed SQL, so
//! a page costs one query regardless of how much history exists.
//!
//! Rows serialize by column name rather than through hand-written DTOs. The
//! table *is* the contract: a column added by a migration appears in the API
//! without a second definition to keep in sync, and none can silently drift.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::types::ValueRef;
use serde_json::{json, Map, Value};

use super::ReadStore;
use crate::{derive::PARSER_VERSION, detect::SignalPolicy, pricing::PRICING_VERSION};

/// Largest page any endpoint will return, whatever the caller asks for.
const MAX_LIMIT: i64 = 200;
const DEFAULT_LIMIT: i64 = 50;

/// Columns holding JSON. Stored as text, so they are decoded on the way out
/// rather than being handed to the client as an escaped string.
const JSON_COLUMNS: &[&str] = &[
    "tools_called",
    "block_counts",
    "stop_sequences",
    "context_management",
    "models",
    "observed",
];

pub fn routes() -> Router<ReadStore> {
    Router::new()
        .route("/api/v2/meta", get(meta))
        .route("/api/v2/overview", get(overview))
        .route("/api/v2/generations", get(generations))
        .route("/api/v2/generations/:span", get(generation))
        .route("/api/v2/generations/:span/raw", get(generation_raw))
        .route("/api/v2/traces", get(traces))
        .route("/api/v2/traces/:trace", get(trace))
        .route("/api/v2/sessions", get(sessions))
        .route("/api/v2/sessions/:session", get(session))
        .route("/api/v2/alerts", get(alerts))
        .route("/api/v2/errors", get(errors))
        .route("/api/v2/tools", get(tools))
        .route("/api/v2/tools/:name", get(tool))
        .route("/api/v2/cost", get(cost))
        .route("/api/v2/policy", get(policy))
        .route("/api/v2/events", get(super::events::stream))
}

// ── row serialization ────────────────────────────────────────────────────

/// Turn a row into an object keyed by column name.
fn row_to_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let statement = row.as_ref();
    let mut object = Map::new();
    for index in 0..statement.column_count() {
        let name = statement.column_name(index)?;
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => json!(value),
            ValueRef::Real(value) => json!(value),
            ValueRef::Text(bytes) => {
                let text = String::from_utf8_lossy(bytes).into_owned();
                if JSON_COLUMNS.contains(&name) {
                    serde_json::from_str(&text).unwrap_or(Value::String(text))
                } else {
                    Value::String(text)
                }
            }
            ValueRef::Blob(_) => Value::Null,
        };
        object.insert(name.to_owned(), value);
    }
    Ok(Value::Object(object))
}

fn query(
    store: &ReadStore,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Value>, rusqlite::Error> {
    let conn = store.open()?;
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(params, |row| row_to_json(row))?;
    rows.collect()
}

fn respond(rows: Result<Vec<Value>, rusqlite::Error>, key: &str) -> Response {
    match rows {
        Ok(rows) => Json(json!({ key: rows })).into_response(),
        Err(err) => db_error(err),
    }
}

fn db_error(err: rusqlite::Error) -> Response {
    eprintln!("orama: v2 query failed: {err}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
}

// ── filters ──────────────────────────────────────────────────────────────

/// Query parameters a caller may filter on, and the SQL each maps to.
///
/// An allowlist rather than string interpolation: an unknown parameter is a
/// 400, because a filter that is silently ignored is a lie about the data.
const FILTERS: &[(&str, &str)] = &[
    ("session_id", "session_id = ?"),
    ("trace_id", "trace_id = ?"),
    ("agent_id", "agent_id = ?"),
    ("agent_role", "agent_role = ?"),
    ("model", "model = ?"),
    ("provider", "provider = ?"),
    ("framework", "framework = ?"),
    ("stop_reason", "stop_reason = ?"),
    ("is_error", "is_error = ?"),
    ("from", "started_at >= ?"),
    ("to", "started_at <= ?"),
    ("min_cost_usd", "cost_total_usd >= ?"),
    ("min_latency_ms", "latency_ms >= ?"),
    ("min_total_tokens", "total_tokens >= ?"),
];

/// Parameters that control paging rather than filtering.
const CONTROLS: &[&str] = &["cursor", "limit", "sort", "order", "group_by", "window"];

struct Filters {
    clauses: Vec<String>,
    values: Vec<Box<dyn rusqlite::ToSql>>,
}

impl Filters {
    /// Build a WHERE fragment, rejecting anything not on the allowlist.
    fn parse(params: &HashMap<String, String>, allowed: &[(&str, &str)]) -> Result<Self, String> {
        let mut clauses = Vec::new();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        for (key, value) in params {
            if CONTROLS.contains(&key.as_str()) {
                continue;
            }
            let Some((_, clause)) = allowed.iter().find(|(name, _)| name == key) else {
                let known: Vec<&str> = allowed.iter().map(|(name, _)| *name).collect();
                return Err(format!(
                    "unknown filter `{key}`; accepted: {}",
                    known.join(", ")
                ));
            };
            clauses.push((*clause).to_owned());
            values.push(Box::new(value.clone()));
        }
        Ok(Self { clauses, values })
    }

    fn where_sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.clauses.join(" AND "))
        }
    }

    fn as_params(&self) -> Vec<&dyn rusqlite::ToSql> {
        self.values.iter().map(|value| value.as_ref()).collect()
    }
}

fn limit_of(params: &HashMap<String, String>) -> i64 {
    params
        .get("limit")
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

/// Keyset cursor: the `started_at` of the last row on the previous page.
///
/// Keyset rather than offset so a page stays stable while new captures arrive —
/// with offset, a row inserted mid-scroll shifts everything and the reader
/// silently skips or repeats one.
fn cursor_of(params: &HashMap<String, String>) -> String {
    params.get("cursor").cloned().unwrap_or_default()
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

// ── endpoints ────────────────────────────────────────────────────────────

/// What this database contains and which versions produced it.
async fn meta(State(store): State<ReadStore>) -> Response {
    let sql = r#"
        SELECT (SELECT COUNT(*) FROM calls)                                    AS calls,
               (SELECT COUNT(*) FROM generations)                              AS generations,
               (SELECT COUNT(*) FROM tool_calls)                               AS tool_calls,
               (SELECT COUNT(*) FROM sessions)                                 AS sessions,
               (SELECT COUNT(DISTINCT trace_id) FROM generations
                 WHERE trace_id IS NOT NULL)                                   AS traces,
               (SELECT COUNT(*) FROM alerts)                                   AS alerts,
               (SELECT COUNT(*) FROM derive_failures)                          AS derive_failures,
               -- Coverage, so the UI can say what it does not know rather than
               -- presenting a partial total as complete.
               (SELECT CAST(SUM(input_tokens IS NOT NULL) AS REAL)
                       / NULLIF(COUNT(*), 0) FROM generations)                 AS usage_coverage,
               (SELECT CAST(SUM(cost_total_usd IS NOT NULL) AS REAL)
                       / NULLIF(COUNT(*), 0) FROM generations)                 AS cost_coverage
    "#;
    match query(&store, sql, &[]) {
        Ok(mut rows) if !rows.is_empty() => {
            let mut counts = rows.remove(0);
            if let Some(object) = counts.as_object_mut() {
                object.insert("parser_version".into(), json!(PARSER_VERSION));
                object.insert("pricing_version".into(), json!(PRICING_VERSION));
                object.insert(
                    "policy_version".into(),
                    json!(SignalPolicy::default().version),
                );
            }
            Json(counts).into_response()
        }
        Ok(_) => not_found(),
        Err(err) => db_error(err),
    }
}

/// Dashboard aggregates in one pass.
async fn overview(State(store): State<ReadStore>) -> Response {
    let totals = query(
        &store,
        r#"
        SELECT COUNT(*)                                AS generations,
               COUNT(DISTINCT session_id)              AS sessions,
               COUNT(DISTINCT trace_id)                AS traces,
               SUM(is_error)                           AS errors,
               SUM(input_tokens)                       AS input_tokens,
               SUM(output_tokens)                      AS output_tokens,
               SUM(cache_read_tokens)                  AS cache_read_tokens,
               SUM(cache_creation_tokens)              AS cache_creation_tokens,
               SUM(thinking_tokens)                    AS thinking_tokens,
               SUM(cost_total_usd)                     AS cost_total_usd,
               MAX(0, SUM(COALESCE(cost_uncached_equiv_usd, 0))
                      - SUM(COALESCE(cost_total_usd, 0))) AS cache_savings_usd,
               SUM(tool_call_count)                    AS tool_calls,
               MIN(started_at)                         AS first_seen,
               MAX(started_at)                         AS last_seen
          FROM generations
        "#,
        &[],
    );
    let by_severity = query(
        &store,
        "SELECT severity, COUNT(*) AS count FROM alerts GROUP BY severity",
        &[],
    );
    let by_model = query(
        &store,
        r#"
        SELECT model, COUNT(*) AS generations, SUM(cost_total_usd) AS cost_total_usd,
               SUM(total_tokens) AS total_tokens
          FROM generations WHERE model IS NOT NULL
         GROUP BY model ORDER BY COALESCE(cost_total_usd, 0) DESC
        "#,
        &[],
    );
    let recent = query(
        &store,
        r#"
        SELECT session_id, title, generation_count, tool_call_count, error_count,
               cost_total_usd, primary_model, started_at, ended_at
          FROM sessions ORDER BY started_at DESC LIMIT 10
        "#,
        &[],
    );

    match (totals, by_severity, by_model, recent) {
        (Ok(mut totals), Ok(severity), Ok(models), Ok(recent)) => Json(json!({
            "totals": totals.pop().unwrap_or(Value::Null),
            "alerts_by_severity": severity,
            "by_model": models,
            "recent_sessions": recent,
        }))
        .into_response(),
        (Err(err), ..) | (_, Err(err), ..) | (_, _, Err(err), _) | (_, _, _, Err(err)) => {
            db_error(err)
        }
    }
}

/// The flat, filterable generations table.
async fn generations(
    State(store): State<ReadStore>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let filters = match Filters::parse(&params, FILTERS) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };
    let sql = format!(
        r#"
        SELECT call_id, span_id, trace_id, session_id, parent_span_id, depth,
               provider, framework, agent_name, agent_role, model, model_resolved,
               started_at, ended_at, ttft_ms, latency_ms,
               input_tokens, output_tokens, total_tokens, cache_read_tokens,
               cache_creation_tokens, thinking_tokens,
               cost_total_usd, cost_cache_read_usd, cost_cache_write_usd,
               http_status, stop_reason, is_error, error_kind,
               tool_call_count, tools_called, user_prompt
          FROM generations
         WHERE (? = '' OR started_at < ?){filters}
         ORDER BY started_at DESC, call_id DESC
         LIMIT ?
        "#,
        filters = filters.where_sql()
    );
    let cursor = cursor_of(&params);
    let limit = limit_of(&params);
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&cursor, &cursor];
    bound.extend(filters.as_params());
    bound.push(&limit);
    respond(query(&store, &sql, &bound), "generations")
}

/// One generation, with its tool calls and alerts.
async fn generation(State(store): State<ReadStore>, Path(span): Path<String>) -> Response {
    let rows = query(
        &store,
        "SELECT * FROM generations WHERE span_id = ?1 OR call_id = ?1",
        &[&span],
    );
    let generation = match rows {
        Ok(mut rows) if !rows.is_empty() => rows.remove(0),
        Ok(_) => return not_found(),
        Err(err) => return db_error(err),
    };
    let call_id = generation.get("call_id").cloned().unwrap_or(Value::Null);
    let tools = query(
        &store,
        "SELECT * FROM tool_calls WHERE call_id = ?1 ORDER BY seq",
        &[&call_id.as_i64()],
    );
    let alerts = query(
        &store,
        "SELECT * FROM alerts WHERE call_id = ?1 ORDER BY severity",
        &[&call_id.as_i64()],
    );
    match (tools, alerts) {
        (Ok(tools), Ok(alerts)) => Json(json!({
            "generation": generation,
            "tool_calls": tools,
            "alerts": alerts,
        }))
        .into_response(),
        (Err(err), _) | (_, Err(err)) => db_error(err),
    }
}

/// The verbatim capture behind a generation. Deliberately a separate route:
/// raw is evidence, not the default view.
async fn generation_raw(State(store): State<ReadStore>, Path(span): Path<String>) -> Response {
    let sql = r#"
        SELECT c.id, c.timestamp_start, c.timestamp_first_chunk, c.timestamp_end,
               c.method, c.url, c.request_headers, c.request_body,
               c.response_status, c.response_headers, c.response_body,
               c.response_raw_sse, c.response_reconstructed, c.error
          FROM calls c JOIN generations g ON g.call_id = c.id
         WHERE g.span_id = ?1 OR g.call_id = ?1
    "#;
    match query(&store, sql, &[&span]) {
        Ok(mut rows) if !rows.is_empty() => {
            // Bodies and headers are stored as JSON text; decode so the client
            // gets structure rather than an escaped blob.
            let mut row = rows.remove(0);
            if let Some(object) = row.as_object_mut() {
                for key in [
                    "request_headers",
                    "request_body",
                    "response_headers",
                    "response_body",
                    "response_reconstructed",
                ] {
                    if let Some(Value::String(text)) = object.get(key) {
                        if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                            object.insert(key.to_owned(), parsed);
                        }
                    }
                }
            }
            Json(row).into_response()
        }
        Ok(_) => not_found(),
        Err(err) => db_error(err),
    }
}

/// Traces — one user turn each, rolled up from their generations.
async fn traces(
    State(store): State<ReadStore>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let filters = match Filters::parse(&params, FILTERS) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };
    let sql = format!(
        r#"
        SELECT trace_id, session_id,
               MIN(started_at) AS started_at,
               MAX(COALESCE(ended_at, started_at)) AS ended_at,
               COUNT(*) AS generation_count,
               SUM(tool_call_count) AS tool_call_count,
               COUNT(DISTINCT agent_id) AS agent_count,
               SUM(is_error) AS error_count,
               SUM(total_tokens) AS total_tokens,
               SUM(cost_total_usd) AS cost_total_usd,
               MAX(depth) AS max_depth,
               (SELECT user_prompt FROM generations inner_g
                 WHERE inner_g.trace_id = g.trace_id AND inner_g.user_prompt IS NOT NULL
                 ORDER BY inner_g.started_at LIMIT 1) AS user_prompt
          FROM generations g
         WHERE trace_id IS NOT NULL{filters}
         GROUP BY trace_id
        HAVING (? = '' OR MIN(started_at) < ?)
         ORDER BY started_at DESC
         LIMIT ?
        "#,
        filters = filters.where_sql()
    );
    let cursor = cursor_of(&params);
    let limit = limit_of(&params);
    // Filter values bind before the cursor here: the filters sit in WHERE,
    // which precedes the HAVING clause textually.
    let mut bound: Vec<&dyn rusqlite::ToSql> = filters.as_params();
    bound.push(&cursor);
    bound.push(&cursor);
    bound.push(&limit);
    respond(query(&store, &sql, &bound), "traces")
}

/// One trace as a tree: its generations, and each one's tool spans.
async fn trace(State(store): State<ReadStore>, Path(trace): Path<String>) -> Response {
    let spans = query(
        &store,
        r#"
        SELECT call_id, span_id, parent_span_id, depth, agent_name, agent_role,
               model, started_at, ended_at, ttft_ms, latency_ms,
               input_tokens, output_tokens, total_tokens, cost_total_usd,
               http_status, stop_reason, is_error, error_kind,
               tool_call_count, tools_called, user_prompt
          FROM generations WHERE trace_id = ?1 ORDER BY started_at, call_id
        "#,
        &[&trace],
    );
    let tools = query(
        &store,
        r#"
        SELECT id, call_id, seq, tool_use_id, name, server, is_mcp, was_declared,
               input_chars, input_excerpt, result_chars, result_excerpt,
               is_error, status, emitted_at, duration_ms
          FROM tool_calls WHERE trace_id = ?1 ORDER BY call_id, seq
        "#,
        &[&trace],
    );
    let alerts = query(
        &store,
        "SELECT * FROM alerts WHERE trace_id = ?1",
        &[&trace],
    );
    match (spans, tools, alerts) {
        (Ok(spans), Ok(tools), Ok(alerts)) if !spans.is_empty() => Json(json!({
            "trace_id": trace,
            "generations": spans,
            "tool_calls": tools,
            "alerts": alerts,
        }))
        .into_response(),
        (Ok(_), Ok(_), Ok(_)) => not_found(),
        (Err(err), ..) | (_, Err(err), _) | (_, _, Err(err)) => db_error(err),
    }
}

async fn sessions(
    State(store): State<ReadStore>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = limit_of(&params);
    let cursor = cursor_of(&params);
    respond(
        query(
            &store,
            r#"
            SELECT * FROM sessions
             WHERE (? = '' OR started_at < ?)
             ORDER BY started_at DESC LIMIT ?
            "#,
            &[&cursor, &cursor, &limit],
        ),
        "sessions",
    )
}

/// One session: its rollup, its traces, its agents, and its context trend.
async fn session(State(store): State<ReadStore>, Path(id): Path<String>) -> Response {
    let summary = query(
        &store,
        "SELECT * FROM sessions WHERE session_id = ?1",
        &[&id],
    );
    let agents = query(
        &store,
        r#"
        SELECT agent_id, agent_name, agent_role, billing_variant, model,
               COUNT(*) AS generation_count, SUM(cost_total_usd) AS cost_total_usd,
               SUM(total_tokens) AS total_tokens
          FROM generations WHERE session_id = ?1 AND agent_id IS NOT NULL
         GROUP BY agent_id ORDER BY COALESCE(cost_total_usd, 0) DESC
        "#,
        &[&id],
    );
    let timeline = query(
        &store,
        r#"
        SELECT call_id, span_id, trace_id, agent_name, agent_role, model,
               started_at, ttft_ms, latency_ms,
               input_tokens, cache_read_tokens, cache_creation_tokens,
               COALESCE(input_tokens, 0) + COALESCE(cache_read_tokens, 0)
                 + COALESCE(cache_creation_tokens, 0) AS context_tokens,
               cost_total_usd, is_error, stop_reason
          FROM generations WHERE session_id = ?1 ORDER BY started_at, call_id
        "#,
        &[&id],
    );
    let alerts = query(
        &store,
        "SELECT * FROM alerts WHERE session_id = ?1 ORDER BY occurred_at DESC",
        &[&id],
    );
    match (summary, agents, timeline, alerts) {
        (Ok(mut summary), Ok(agents), Ok(timeline), Ok(alerts)) if !summary.is_empty() => {
            Json(json!({
                "session": summary.remove(0),
                "agents": agents,
                "timeline": timeline,
                "alerts": alerts,
            }))
            .into_response()
        }
        (Ok(_), Ok(_), Ok(_), Ok(_)) => not_found(),
        (Err(err), ..) | (_, Err(err), ..) | (_, _, Err(err), _) | (_, _, _, Err(err)) => {
            db_error(err)
        }
    }
}

/// Alert filters are their own allowlist — the columns differ from generations.
const ALERT_FILTERS: &[(&str, &str)] = &[
    ("severity", "severity = ?"),
    ("category", "category = ?"),
    ("rule_id", "rule_id = ?"),
    ("confidence", "confidence = ?"),
    ("session_id", "session_id = ?"),
    ("trace_id", "trace_id = ?"),
    ("call_id", "call_id = ?"),
    ("scope_kind", "scope_kind = ?"),
];

async fn alerts(
    State(store): State<ReadStore>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let filters = match Filters::parse(&params, ALERT_FILTERS) {
        Ok(filters) => filters,
        Err(message) => return bad_request(message),
    };
    let sql = format!(
        r#"
        SELECT * FROM alerts
         WHERE (? = '' OR occurred_at < ?){filters}
         ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1
                                WHEN 'warning' THEN 2 ELSE 3 END,
                  occurred_at DESC
         LIMIT ?
        "#,
        filters = filters.where_sql()
    );
    let cursor = cursor_of(&params);
    let limit = limit_of(&params);
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&cursor, &cursor];
    bound.extend(filters.as_params());
    bound.push(&limit);

    let facets = query(
        &store,
        r#"
        SELECT severity, category, rule_id, COUNT(*) AS count
          FROM alerts GROUP BY severity, category, rule_id
        "#,
        &[],
    );
    match (query(&store, &sql, &bound), facets) {
        (Ok(rows), Ok(facets)) => Json(json!({ "alerts": rows, "facets": facets })).into_response(),
        (Err(err), _) | (_, Err(err)) => db_error(err),
    }
}

/// The errors surface: alerts grouped by rule, worst first.
async fn errors(State(store): State<ReadStore>) -> Response {
    respond(
        query(
            &store,
            r#"
            SELECT rule_id, category, severity, confidence, title,
                   MIN(explanation) AS explanation, MIN(impact) AS impact,
                   MIN(recommendation) AS recommendation,
                   COUNT(*) AS occurrences,
                   COUNT(DISTINCT session_id) AS sessions,
                   MIN(occurred_at) AS first_seen, MAX(occurred_at) AS last_seen,
                   MIN(summary) AS sample_summary
              FROM alerts
             WHERE severity IN ('critical', 'error', 'warning')
             GROUP BY rule_id
             ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'error' THEN 1 ELSE 2 END,
                      occurrences DESC
            "#,
            &[],
        ),
        "groups",
    )
}

/// Per-tool analytics.
async fn tools(State(store): State<ReadStore>) -> Response {
    let rows = query(
        &store,
        r#"
        SELECT name, server, MAX(is_mcp) AS is_mcp,
               COUNT(*) AS calls,
               SUM(COALESCE(is_error, 0)) AS errors,
               CAST(SUM(COALESCE(is_error, 0)) AS REAL) / COUNT(*) AS error_rate,
               SUM(status = 'pending') AS pending,
               SUM(was_declared = 0) AS undeclared,
               CAST(AVG(result_chars) AS INT) AS avg_result_chars,
               MAX(result_chars) AS max_result_chars,
               COUNT(DISTINCT session_id) AS sessions,
               MAX(emitted_at) AS last_used
          FROM tool_calls GROUP BY name, server ORDER BY calls DESC
        "#,
        &[],
    );
    respond(rows, "tools")
}

async fn tool(
    State(store): State<ReadStore>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let limit = limit_of(&params);
    respond(
        query(
            &store,
            r#"
            SELECT t.*, g.span_id, g.agent_name, g.model
              FROM tool_calls t LEFT JOIN generations g ON g.call_id = t.call_id
             WHERE t.name = ?1 ORDER BY t.emitted_at DESC LIMIT ?2
            "#,
            &[&name, &limit],
        ),
        "calls",
    )
}

/// Cost, grouped along whichever axis the caller asks for.
async fn cost(
    State(store): State<ReadStore>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // An allowlist, not interpolation: `group_by` names a column.
    let column = match params
        .get("group_by")
        .map(String::as_str)
        .unwrap_or("model")
    {
        "model" => "model",
        "session" => "session_id",
        "agent" => "agent_name",
        "provider" => "provider",
        "day" => "substr(started_at, 1, 10)",
        other => {
            return bad_request(format!(
                "unknown group_by `{other}`; accepted: model, session, agent, provider, day"
            ))
        }
    };
    let sql = format!(
        r#"
        SELECT {column} AS bucket,
               COUNT(*) AS generations,
               SUM(input_tokens) AS input_tokens,
               SUM(output_tokens) AS output_tokens,
               SUM(cache_read_tokens) AS cache_read_tokens,
               SUM(cache_creation_tokens) AS cache_creation_tokens,
               SUM(cost_input_usd) AS cost_input_usd,
               SUM(cost_output_usd) AS cost_output_usd,
               SUM(cost_cache_write_usd) AS cost_cache_write_usd,
               SUM(cost_cache_read_usd) AS cost_cache_read_usd,
               SUM(cost_total_usd) AS cost_total_usd,
               MAX(0, SUM(COALESCE(cost_uncached_equiv_usd, 0))
                      - SUM(COALESCE(cost_total_usd, 0))) AS cache_savings_usd,
               -- How much of this bucket is actually priced, so a partial total
               -- is never presented as a complete one.
               CAST(SUM(cost_total_usd IS NOT NULL) AS REAL) / COUNT(*) AS priced_share
          FROM generations
         WHERE {column} IS NOT NULL
         GROUP BY bucket
         ORDER BY COALESCE(cost_total_usd, 0) DESC
        "#
    );
    respond(query(&store, &sql, &[]), "buckets")
}

/// The thresholds every detector compares against.
async fn policy() -> Json<SignalPolicy> {
    Json(SignalPolicy::default())
}
