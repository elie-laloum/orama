//! Read-only JSON API + static HTML UI over the capture database.
//!
//! Strictly read-only: every handler opens a short-lived connection and only
//! issues SELECTs. Nothing here mutates stored data.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use futures::stream;
use serde_json::{json, Value};

use crate::{
    parse::{
        diagnostics::{call_alerts, session_alerts, token_metrics, SignalPolicy},
        parse_call, session,
    },
    store::{get_call, list_calls, StoredCall},
};

/// Read-only handle to the capture DB for the API/UI.
#[derive(Clone)]
pub struct ReadStore {
    db_path: Arc<PathBuf>,
}

impl ReadStore {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: Arc::new(db_path.into()),
        }
    }

    /// Open a fresh connection for reading. SQLite multiplexes readers freely,
    /// so a per-request connection keeps the API isolated from the writer task.
    ///
    /// The file is opened read-write because a WAL database needs to map its
    /// shared-memory index, which a strictly read-only handle cannot create.
    /// `query_only` then enforces the read-only guarantee inside SQLite itself,
    /// so no handler can mutate stored data.
    fn open(&self) -> rusqlite::Result<rusqlite::Connection> {
        use rusqlite::OpenFlags;
        let conn = rusqlite::Connection::open_with_flags(
            self.db_path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA query_only = ON;")?;
        Ok(conn)
    }
}

/// Mount the read-only API and UI onto a router.
pub fn routes(store: ReadStore) -> Router {
    Router::new()
        .route("/ui", get(ui_index))
        .route("/ui/", get(ui_index))
        .route("/ui/main.js", get(ui_bundle))
        .route("/api/calls", get(list_handler))
        .route("/api/calls/:id", get(detail_handler))
        .route("/api/calls/:id/normalized", get(normalized_handler))
        .route("/api/calls/:id/diagnostics", get(call_diagnostics_handler))
        .route("/api/signal-policy", get(signal_policy_handler))
        .route("/api/sessions", get(sessions_handler))
        .route("/api/sessions/:key", get(session_handler))
        .route(
            "/api/sessions/:key/diagnostics",
            get(session_diagnostics_handler),
        )
        .route("/api/ui/dashboard", get(dashboard_handler))
        .route("/api/ui/events", get(events_handler))
        .route("/api/ui/alerts", get(alerts_handler))
        .with_state(store)
}

/// GET /api/calls — list all captured calls, most recent first, as summaries.
async fn list_handler(State(store): State<ReadStore>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match list_calls(&conn) {
        Ok(calls) => {
            let items: Vec<Value> = calls.iter().map(summary_json).collect();
            Json(json!({ "calls": items })).into_response()
        }
        Err(err) => db_error(err),
    }
}

/// GET /api/calls/:id/normalized — provider-neutral semantic view, derived at read time.
async fn normalized_handler(State(store): State<ReadStore>, Path(id): Path<i64>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match get_call(&conn, id) {
        Ok(Some(call)) => Json(parse_call(&call)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(err) => db_error(err),
    }
}

/// GET /api/calls/:id/diagnostics — derived UI diagnostics and token metrics.
async fn call_diagnostics_handler(State(store): State<ReadStore>, Path(id): Path<i64>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match get_call(&conn, id) {
        Ok(Some(call)) => {
            let normalized = parse_call(&call);
            let policy = SignalPolicy::default();
            Json(json!({
                "call_id": normalized.id,
                "token_metrics": token_metrics(&normalized.usage),
                "alerts": call_alerts(&normalized, &policy),
            }))
            .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(err) => db_error(err),
    }
}

/// GET /api/signal-policy — versioned, read-only detector thresholds.
async fn signal_policy_handler() -> Json<SignalPolicy> {
    Json(SignalPolicy::default())
}

/// GET /api/sessions — session summaries derived from all captured calls.
async fn sessions_handler(State(store): State<ReadStore>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match list_calls(&conn) {
        Ok(calls) => Json(json!({ "sessions": session::sessions(&calls) })).into_response(),
        Err(err) => db_error(err),
    }
}

/// GET /api/sessions/:key — chronological session timeline and inter-call signals.
async fn session_handler(State(store): State<ReadStore>, Path(key): Path<String>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match list_calls(&conn) {
        Ok(calls) => match session::session(&calls, &key) {
            Some(session) => Json(session).into_response(),
            None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        },
        Err(err) => db_error(err),
    }
}

/// GET /api/sessions/:key/diagnostics — session-scoped context and performance diagnostics.
async fn session_diagnostics_handler(
    State(store): State<ReadStore>,
    Path(key): Path<String>,
) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match list_calls(&conn) {
        Ok(calls) => match session::session(&calls, &key) {
            Some(detail) => {
                let policy = SignalPolicy::default();
                Json(json!({ "session_key": key, "alerts": session_alerts(&detail, &policy) }))
                    .into_response()
            }
            None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        },
        Err(err) => db_error(err),
    }
}

/// GET /api/ui/dashboard — compact read-only dashboard snapshot.
async fn dashboard_handler(State(store): State<ReadStore>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    let calls = match list_calls(&conn) {
        Ok(calls) => calls,
        Err(err) => return db_error(err),
    };
    let policy = SignalPolicy::default();
    let summaries: Vec<Value> = calls.iter().map(summary_json).collect();
    let mut alerts = Vec::new();
    let mut total_tokens = 0_u64;
    let mut token_count = 0_u64;
    let mut cache_rates = Vec::new();
    for stored in &calls {
        let call = parse_call(stored);
        let metrics = token_metrics(&call.usage);
        if let Some(total) = metrics.total_tokens {
            total_tokens += total;
            token_count += 1;
        }
        if let Some(rate) = metrics.cache_reuse_rate {
            cache_rates.push(rate);
        }
        alerts.extend(call_alerts(&call, &policy));
    }
    let session_summaries = session::sessions(&calls);
    let errors = alerts
        .iter()
        .filter(|alert| {
            matches!(
                alert.severity,
                crate::parse::diagnostics::Severity::Error
                    | crate::parse::diagnostics::Severity::Critical
            )
        })
        .count();
    let warnings = alerts
        .iter()
        .filter(|alert| matches!(alert.severity, crate::parse::diagnostics::Severity::Warning))
        .count();
    Json(json!({ "calls": summaries, "sessions": session_summaries, "alerts": alerts, "totals": {
        "calls": calls.len(), "sessions": session::sessions(&calls).len(), "errors": errors, "warnings": warnings,
        "total_tokens": (token_count > 0).then_some(total_tokens),
        "cache_reuse_rate": (!cache_rates.is_empty()).then(|| cache_rates.iter().sum::<f64>() / cache_rates.len() as f64),
    }})).into_response()
}

/// GET /api/ui/alerts — read-only derived alert inventory, filterable by query.
async fn alerts_handler(
    State(store): State<ReadStore>,
    axum::extract::Query(filters): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    let calls = match list_calls(&conn) {
        Ok(calls) => calls,
        Err(err) => return db_error(err),
    };
    let policy = SignalPolicy::default();
    let mut alerts = Vec::new();
    for stored in &calls {
        let normalized = parse_call(stored);
        alerts.extend(call_alerts(&normalized, &policy));
    }
    for summary in session::sessions(&calls) {
        if let Some(detail) = session::session(&calls, &summary.key) {
            alerts.extend(session_alerts(&detail, &policy));
        }
    }
    alerts.retain(|alert| {
        filters.get("severity").is_none_or(|value| {
            value
                == &serde_json::to_value(alert.severity)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
        }) && filters.get("category").is_none_or(|value| {
            value
                == &serde_json::to_value(alert.category)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
        }) && filters.get("call_id").is_none_or(|value| {
            alert
                .sources
                .iter()
                .any(|source| source.call_id.is_some_and(|id| id.to_string() == *value))
        }) && filters.get("session_key").is_none_or(|value| {
            alert
                .sources
                .iter()
                .any(|source| source.session_key.as_deref() == Some(value))
        })
    });
    let offset = filters
        .get("offset")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = filters
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50)
        .min(100);
    let total = alerts.len();
    Json(json!({ "alerts": alerts.into_iter().skip(offset).take(limit).collect::<Vec<_>>(), "total": total, "offset": offset, "limit": limit })).into_response()
}

/// GET /api/ui/events — read-only invalidation heartbeat for local clients.
async fn events_handler() -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let events = stream::unfold(0_u64, |id| async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let event = Event::default()
            .id(id.to_string())
            .event("dashboard.changed")
            .data("{}");
        Some((Ok(event), id + 1))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// GET /api/calls/:id — full detail for a single call.
async fn detail_handler(State(store): State<ReadStore>, Path(id): Path<i64>) -> Response {
    let conn = match store.open() {
        Ok(c) => c,
        Err(err) => return db_error(err),
    };
    match get_call(&conn, id) {
        Ok(Some(call)) => Json(detail_json(&call)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
        Err(err) => db_error(err),
    }
}

fn db_error(err: rusqlite::Error) -> Response {
    // Read errors are non-fatal for the process; report cleanly to the client.
    eprintln!("tracer: read API db error: {err}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
        .into_response()
}

/// Extract the model from a stored request body, if present.
fn model_of(call: &StoredCall) -> Option<String> {
    call.record
        .request_body
        .as_ref()
        .and_then(|b| b.get("model"))
        .and_then(|m| m.as_str())
        .map(String::from)
}

/// At-a-glance summary for the call list.
fn summary_json(call: &StoredCall) -> Value {
    let r = &call.record;
    json!({
        "id": call.id,
        "timestamp_start": r.timestamp_start,
        "timestamp_end": r.timestamp_end,
        "method": r.method,
        "url": r.url,
        "model": model_of(call),
        "status": r.response_status,
        "is_stream": r.response_raw_sse.is_some(),
        "has_error": r.error.is_some(),
    })
}

/// Full detail payload for one call.
pub fn detail_json(call: &StoredCall) -> Value {
    let r = &call.record;
    json!({
        "id": call.id,
        "timestamp_start": r.timestamp_start,
        "timestamp_first_chunk": r.timestamp_first_chunk,
        "timestamp_end": r.timestamp_end,
        "method": r.method,
        "url": r.url,
        "model": model_of(call),
        "request_headers": r.request_headers,
        "request_body": r.request_body,
        "response_status": r.response_status,
        "response_headers": r.response_headers,
        "response_body": r.response_body,
        "response_raw_sse": r.response_raw_sse,
        "response_reconstructed": r.response_reconstructed,
        "is_stream": r.response_raw_sse.is_some(),
        "error": r.error,
    })
}

/// GET /ui — the production React/Rspack dashboard bundle.
async fn ui_index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(UI_HTML),
    )
}

/// GET /ui/main.js — the compiled dashboard bundle.
async fn ui_bundle() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        UI_BUNDLE,
    )
}

// Staged by `core/build.rs`, which falls back to a placeholder when the frontend
// bundle has not been built. Compiling never requires a prior `npm run build`.
const UI_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/ui_index.html"));
const UI_BUNDLE: &str = include_str!(concat!(env!("OUT_DIR"), "/ui_main.js"));
