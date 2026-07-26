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
use crate::{
    derive::PARSER_VERSION,
    detect::SignalPolicy,
    parse::{
        model::{BlockKind, NormalizedCall},
        parse_call,
    },
    pricing::pricing_version,
    store::get_call,
};

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
    "segments",
    "input_schema",
];

pub fn routes() -> Router<ReadStore> {
    Router::new()
        .route("/api/v2/meta", get(meta))
        .route("/api/v2/overview", get(overview))
        .route("/api/v2/generations", get(generations))
        .route("/api/v2/generations/:span", get(generation))
        .route("/api/v2/generations/:span/raw", get(generation_raw))
        .route("/api/v2/generations/:span/context", get(generation_context))
        .route("/api/v2/harness", get(harness))
        .route("/api/v2/harness/system/:hash", get(harness_system))
        .route("/api/v2/harness/tools/:hash", get(harness_tools))
        .route("/api/v2/traces", get(traces))
        .route("/api/v2/traces/:trace", get(trace))
        .route("/api/v2/sessions", get(sessions))
        .route("/api/v2/sessions/:session", get(session))
        .route("/api/v2/alerts", get(alerts))
        .route("/api/v2/errors", get(errors))
        .route("/api/v2/tools", get(tools))
        .route("/api/v2/tools/:name", get(tool))
        .route("/api/v2/cost", get(cost))
        .route("/api/v2/models", get(models))
        .route("/api/v2/policy", get(policy))
        .route("/api/v2/events", get(super::events::stream))
}

// ── model catalogue ──────────────────────────────────────────────────────

/// Provenance of the rates in force.
pub(crate) fn catalog_meta() -> Value {
    match crate::catalog::current() {
        Some(catalog) => json!({
            "digest": catalog.snapshot.digest,
            "source": catalog.snapshot.source.as_str(),
            "fetched_at": catalog.snapshot.fetched_at,
            "checked_at": catalog.snapshot.checked_at,
            "providers": catalog.provider_count(),
            "models": catalog.model_count(),
        }),
        // No catalogue means nothing is priced. Saying so beats an empty object
        // that reads like a catalogue with no models in it.
        None => Value::Null,
    }
}

/// Models this capture actually used, enriched from the catalogue.
///
/// The observed side is SQL; the catalogue side is the in-memory snapshot. It is
/// joined here rather than in the query because the catalogue is not a derived
/// table — it is a copy of an external document, and projecting 5,756 rows into
/// SQLite to serve a screen that lists four of them would be a second copy to
/// keep in sync for no gain.
///
/// Everything the catalogue does not know stays `null` while the observed
/// counts survive: a model absent upstream is unpriced, not unused.
async fn models(State(store): State<ReadStore>) -> Response {
    let sql = r#"
        SELECT provider,
               model,
               COALESCE(pricing_model_id, model)                       AS priced_as,
               COUNT(*)                                                AS generations,
               SUM(input_tokens)                                       AS input_tokens,
               SUM(output_tokens)                                      AS output_tokens,
               SUM(cache_read_tokens)                                  AS cache_read_tokens,
               SUM(cache_creation_tokens)                              AS cache_creation_tokens,
               SUM(cost_total_usd)                                     AS cost_total_usd,
               MAX(COALESCE(input_tokens, 0) + COALESCE(cache_read_tokens, 0)
                   + COALESCE(cache_creation_tokens, 0))               AS peak_context_tokens,
               MIN(started_at)                                         AS first_seen,
               MAX(started_at)                                         AS last_seen,
               CAST(SUM(cost_total_usd IS NOT NULL) AS REAL) / COUNT(*) AS priced_share
          FROM generations
         WHERE model IS NOT NULL
         GROUP BY provider, model
         ORDER BY COALESCE(cost_total_usd, 0) DESC
    "#;

    let mut rows = match query(&store, sql, &[]) {
        Ok(rows) => rows,
        Err(err) => return db_error(err),
    };

    let catalog = crate::catalog::current();
    for row in &mut rows {
        let Some(object) = row.as_object_mut() else {
            continue;
        };
        let provider = object
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // Prefer the id pricing actually resolved to, so the rates shown are the
        // rates that were charged rather than a second, independent lookup that
        // could disagree with them.
        let model = object
            .get("priced_as")
            .or_else(|| object.get("model"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        let record = catalog
            .as_ref()
            .and_then(|catalog| catalog.resolve(&provider, &model));
        let Some(record) = record else {
            object.insert("in_catalog".into(), json!(false));
            continue;
        };

        let rates = record.pricing.as_ref().map(|pricing| &pricing.base);
        object.insert("in_catalog".into(), json!(true));
        object.insert("name".into(), json!(record.name));
        object.insert("family".into(), json!(record.family));
        object.insert("description".into(), json!(record.description));
        object.insert("status".into(), json!(record.status));
        object.insert("release_date".into(), json!(record.release_date));
        object.insert("knowledge".into(), json!(record.knowledge));
        object.insert("context_limit".into(), json!(record.limits.context));
        object.insert("output_limit".into(), json!(record.limits.output));
        object.insert("rate_input".into(), json!(rates.map(|r| r.input)));
        object.insert("rate_output".into(), json!(rates.map(|r| r.output)));
        object.insert(
            "rate_cache_read".into(),
            json!(rates.and_then(|r| r.cache_read)),
        );
        object.insert(
            "rate_cache_write".into(),
            json!(rates.and_then(|r| r.cache_write)),
        );
        object.insert(
            "tiered".into(),
            json!(record
                .pricing
                .as_ref()
                .is_some_and(|pricing| !pricing.tiers.is_empty())),
        );
        object.insert("reasoning".into(), json!(record.capabilities.reasoning));
        object.insert("tool_call".into(), json!(record.capabilities.tool_call));
        object.insert(
            "structured_output".into(),
            json!(record.capabilities.structured_output),
        );
        object.insert("attachment".into(), json!(record.capabilities.attachment));
        object.insert(
            "input_modalities".into(),
            json!(record.capabilities.input_modalities),
        );
    }

    Json(json!({ "catalog": catalog_meta(), "models": rows })).into_response()
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
                object.insert("pricing_version".into(), json!(pricing_version()));
                object.insert(
                    "policy_version".into(),
                    json!(SignalPolicy::default().version),
                );
                // Where the prices came from, so any screen showing a cost can
                // say what it was priced against.
                object.insert("catalog".into(), catalog_meta());
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

/// Longest slice of any one block returned by the context view. The whole point
/// of the surface is to be readable, and a single tool result can be 260 KB;
/// `/raw` remains the verbatim escape hatch, and exact sizes are always exact.
const BLOCK_PREVIEW_CHARS: usize = 2_000;

/// What one call actually put in front of the model, laid out by section.
///
/// The three sections are disjoint and sum to the request: the system prompt,
/// the tool declarations, and the message thread. Only the thread is a
/// conversation; the other two are harness overhead re-sent on every turn, and
/// on a Claude Code main-loop call they dominate it.
async fn generation_context(State(store): State<ReadStore>, Path(span): Path<String>) -> Response {
    let shape = query(
        &store,
        r#"
        SELECT call_id, span_id, session_id, agent_name, agent_role, model,
               started_at, system_hash, system_chars, system_segments_count,
               system_cache_points, tools_hash, tools_chars, tools_declared_count,
               messages_count, context_chars, block_counts, compaction_requested,
               context_management, thinking_mode, thinking_budget, max_tokens,
               input_tokens, cache_read_tokens, cache_creation_tokens, output_tokens
          FROM generations WHERE span_id = ?1 OR call_id = ?1
        "#,
        &[&span],
    );
    let shape = match shape {
        Ok(mut rows) if !rows.is_empty() => rows.remove(0),
        Ok(_) => return not_found(),
        Err(err) => return db_error(err),
    };
    let Some(call_id) = shape.get("call_id").and_then(Value::as_i64) else {
        return not_found();
    };

    let system_hash = shape.get("system_hash").and_then(Value::as_str);
    let system = match system_hash {
        Some(hash) => match query(
            &store,
            "SELECT segments, total_chars, segment_count, cache_points FROM system_prompts WHERE system_hash = ?1",
            &[&hash],
        ) {
            Ok(mut rows) if !rows.is_empty() => rows.remove(0),
            Ok(_) => Value::Null,
            Err(err) => return db_error(err),
        },
        None => Value::Null,
    };

    let tools_hash = shape.get("tools_hash").and_then(Value::as_str);
    let tools = match tools_hash {
        Some(hash) => match query(
            &store,
            r#"
            SELECT name, server, is_mcp, chars,
                   substr(description, 1, 240) AS description_head
              FROM tool_schemas WHERE tools_hash = ?1 ORDER BY chars DESC
            "#,
            &[&hash],
        ) {
            Ok(rows) => Value::Array(rows),
            Err(err) => return db_error(err),
        },
        None => Value::Array(Vec::new()),
    };

    // The thread has no derived table — it is the one part of a request that is
    // different on every call, so materializing it would copy the whole corpus.
    // Parsing one capture on demand is what `/raw` already does.
    let thread = {
        let conn = match store.open() {
            Ok(conn) => conn,
            Err(err) => return db_error(err),
        };
        match get_call(&conn, call_id) {
            Ok(Some(call)) => thread_outline(&parse_call(&call)),
            Ok(None) => return not_found(),
            Err(err) => return db_error(err),
        }
    };

    let number = |key: &str| shape.get(key).and_then(Value::as_i64).unwrap_or_default();
    Json(json!({
        "shape": shape,
        "composition": {
            "system_chars": number("system_chars"),
            "tools_chars": number("tools_chars"),
            "history_chars": number("context_chars"),
            "total_chars": number("system_chars") + number("tools_chars") + number("context_chars"),
        },
        "system": system,
        "tools": tools,
        "thread": thread,
    }))
    .into_response()
}

/// The message thread as a list of blocks: what each one is, how big, and
/// enough of its text to recognize it.
fn thread_outline(normalized: &NormalizedCall) -> Value {
    let turns: Vec<Value> = normalized
        .thread
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let blocks: Vec<Value> = turn
                .blocks
                .iter()
                .map(|block| {
                    json!({
                        "kind": block.kind,
                        // The tag is what separates the human's words from the
                        // reminders and command output the harness injects into
                        // user turns — indistinguishable without it.
                        "content_tag": block.content_tag,
                        "chars": block.approx_size.chars,
                        "tool_name": block.tool_name,
                        "is_error": block.is_error,
                        "preview": block.content.as_deref().map(|text| {
                            crate::derive::extract::excerpt(text, BLOCK_PREVIEW_CHARS)
                        }).or_else(|| {
                            // A tool_use block carries its arguments rather than
                            // text; showing nothing would misreport it as empty.
                            block.input.as_ref().map(|input| {
                                crate::derive::extract::excerpt(
                                    &input.to_string(), BLOCK_PREVIEW_CHARS)
                            })
                        }),
                        "truncated": block.approx_size.chars > BLOCK_PREVIEW_CHARS,
                    })
                })
                .collect();
            json!({
                "index": index,
                "role": turn.role,
                "origin": turn.origin,
                "chars": turn.blocks.iter().map(|b| b.approx_size.chars).sum::<usize>(),
                "blocks": blocks,
            })
        })
        .collect();

    // A per-kind roll-up over the thread, so the shape of the history is legible
    // before reading any of it.
    let mut by_kind: HashMap<String, (usize, usize)> = HashMap::new();
    for block in normalized.thread.iter().flat_map(|turn| &turn.blocks) {
        let name = match block.kind {
            BlockKind::Text => "text",
            BlockKind::Thinking => "thinking",
            BlockKind::ToolUse => "tool_use",
            BlockKind::ToolResult => "tool_result",
            BlockKind::Image => "image",
            BlockKind::Other => "other",
        };
        let entry = by_kind.entry(name.to_owned()).or_default();
        entry.0 += 1;
        entry.1 += block.approx_size.chars;
    }
    let mut kinds: Vec<Value> = by_kind
        .into_iter()
        .map(|(kind, (count, chars))| json!({ "kind": kind, "count": count, "chars": chars }))
        .collect();
    kinds.sort_by_key(|kind| -(kind["chars"].as_i64().unwrap_or_default()));

    json!({ "turns": turns, "by_kind": kinds })
}

// ── harness ──────────────────────────────────────────────────────────────

/// Every distinct harness configuration observed, and what it costs.
///
/// The question this answers is "what is the client actually sending?", which no
/// other surface asks. A conversation is what the user and model said; the
/// harness is the system prompt and the tool block wrapped around it, re-sent in
/// full on every single call and — on a Claude Code main loop — several times
/// larger than the conversation it carries.
async fn harness(State(store): State<ReadStore>) -> Response {
    // Where the characters actually go, corpus-wide. `context_chars` counts the
    // message thread only, so the three are disjoint and sum to the request.
    let budget = query(
        &store,
        r#"
        SELECT SUM(system_chars)                       AS system_chars,
               SUM(tools_chars)                        AS tools_chars,
               SUM(context_chars)                      AS history_chars,
               COUNT(*)                                AS generations,
               SUM(tools_chars IS NOT NULL)            AS with_tools,
               MAX(tools_chars)                        AS max_tools_chars,
               MAX(context_chars)                      AS max_history_chars
          FROM generations
        "#,
        &[],
    );

    let prompts = query(
        &store,
        r#"
        SELECT p.system_hash, p.total_chars, p.segment_count, p.cache_points,
               COUNT(g.id)                             AS generations,
               COUNT(DISTINCT g.session_id)            AS sessions,
               MIN(g.started_at)                       AS first_seen,
               MAX(g.started_at)                       AS last_seen,
               (SELECT GROUP_CONCAT(DISTINCT agent_role) FROM generations r
                 WHERE r.system_hash = p.system_hash)  AS agent_roles,
               (SELECT span_id FROM generations r
                 WHERE r.system_hash = p.system_hash
                 ORDER BY r.started_at DESC LIMIT 1)   AS latest_span,
               -- The opening line is what makes one prompt recognizable from a
               -- list of fingerprints, which are otherwise indistinguishable.
               -- Claude Code's first segment is the billing header, identical in
               -- shape across every variant, so it identifies nothing: skip past
               -- it to the first line that is actually prompt text.
               substr(CASE
                 WHEN json_extract(p.segments, '$[0].text') LIKE 'x-anthropic-billing-header:%'
                 THEN json_extract(p.segments, '$[1].text')
                 ELSE json_extract(p.segments, '$[0].text')
               END, 1, 120) AS opening
          FROM system_prompts p
          LEFT JOIN generations g ON g.system_hash = p.system_hash
         GROUP BY p.system_hash
         ORDER BY generations DESC
        "#,
        &[],
    );

    let sets = query(
        &store,
        r#"
        SELECT s.tools_hash, s.tool_count, s.total_chars, s.mcp_count,
               COUNT(g.id)                             AS generations,
               COUNT(DISTINCT g.session_id)            AS sessions,
               MIN(g.started_at)                       AS first_seen,
               MAX(g.started_at)                       AS last_seen,
               (SELECT GROUP_CONCAT(DISTINCT agent_role) FROM generations r
                 WHERE r.tools_hash = s.tools_hash)    AS agent_roles
          FROM tool_sets s
          LEFT JOIN generations g ON g.tools_hash = s.tools_hash
         GROUP BY s.tools_hash
         ORDER BY generations DESC
        "#,
        &[],
    );

    // Every declared tool, ranked by what it costs against whether it earns it.
    // A tool declared in every request and never once called is pure context
    // spend, and this is the only place that comparison can be made.
    let declared = query(
        &store,
        r#"
        SELECT t.name,
               MAX(t.server)                           AS server,
               MAX(t.is_mcp)                           AS is_mcp,
               MAX(t.chars)                            AS chars,
               COUNT(DISTINCT t.tools_hash)            AS tool_sets,
               (SELECT COUNT(*) FROM tool_calls c WHERE c.name = t.name) AS calls,
               (SELECT MAX(emitted_at) FROM tool_calls c WHERE c.name = t.name) AS last_used
          FROM tool_schemas t
         GROUP BY t.name
         ORDER BY calls ASC, chars DESC
        "#,
        &[],
    );

    match (budget, prompts, sets, declared) {
        (Ok(mut budget), Ok(prompts), Ok(sets), Ok(declared)) => Json(json!({
            "budget": budget.pop().unwrap_or(Value::Null),
            "system_prompts": prompts,
            "tool_sets": sets,
            "declared_tools": declared,
        }))
        .into_response(),
        (Err(err), ..) | (_, Err(err), ..) | (_, _, Err(err), _) | (_, _, _, Err(err)) => {
            db_error(err)
        }
    }
}

/// One system prompt, verbatim, segment by segment.
async fn harness_system(State(store): State<ReadStore>, Path(hash): Path<String>) -> Response {
    let prompt = query(
        &store,
        "SELECT * FROM system_prompts WHERE system_hash = ?1",
        &[&hash],
    );
    let usage = query(
        &store,
        r#"
        SELECT COUNT(*) AS generations, COUNT(DISTINCT session_id) AS sessions,
               MIN(started_at) AS first_seen, MAX(started_at) AS last_seen,
               GROUP_CONCAT(DISTINCT agent_role) AS agent_roles,
               GROUP_CONCAT(DISTINCT model) AS models,
               GROUP_CONCAT(DISTINCT billing_variant) AS client_versions
          FROM generations WHERE system_hash = ?1
        "#,
        &[&hash],
    );
    match (prompt, usage) {
        (Ok(mut prompt), Ok(mut usage)) if !prompt.is_empty() => Json(json!({
            "system": prompt.remove(0),
            "usage": usage.pop().unwrap_or(Value::Null),
        }))
        .into_response(),
        (Ok(_), Ok(_)) => not_found(),
        (Err(err), _) | (_, Err(err)) => db_error(err),
    }
}

/// One tool set: every declaration in it, with its weight and its usage.
async fn harness_tools(State(store): State<ReadStore>, Path(hash): Path<String>) -> Response {
    let set = query(
        &store,
        "SELECT * FROM tool_sets WHERE tools_hash = ?1",
        &[&hash],
    );
    let tools = query(
        &store,
        r#"
        SELECT t.seq, t.name, t.server, t.is_mcp, t.description, t.input_schema, t.chars,
               (SELECT COUNT(*) FROM tool_calls c
                 JOIN generations g ON g.call_id = c.call_id
                WHERE c.name = t.name AND g.tools_hash = t.tools_hash) AS calls
          FROM tool_schemas t
         WHERE t.tools_hash = ?1
         ORDER BY t.chars DESC
        "#,
        &[&hash],
    );
    let usage = query(
        &store,
        r#"
        SELECT COUNT(*) AS generations, COUNT(DISTINCT session_id) AS sessions,
               MIN(started_at) AS first_seen, MAX(started_at) AS last_seen,
               GROUP_CONCAT(DISTINCT agent_role) AS agent_roles
          FROM generations WHERE tools_hash = ?1
        "#,
        &[&hash],
    );
    match (set, tools, usage) {
        (Ok(mut set), Ok(tools), Ok(mut usage)) if !set.is_empty() => Json(json!({
            "tool_set": set.remove(0),
            "tools": tools,
            "usage": usage.pop().unwrap_or(Value::Null),
        }))
        .into_response(),
        (Ok(_), Ok(_), Ok(_)) => not_found(),
        (Err(err), ..) | (_, Err(err), _) | (_, _, Err(err)) => db_error(err),
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
