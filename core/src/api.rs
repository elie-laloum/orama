//! Read-only JSON API + static HTML UI over the capture database.
//!
//! Strictly read-only: every handler opens a short-lived connection and only
//! issues SELECTs. Nothing here mutates stored data.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};

use crate::store::{get_call, list_calls, StoredCall};

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

    /// Open a fresh read-only connection. SQLite multiplexes readers freely, so
    /// a per-request connection keeps the API isolated from the writer task.
    fn open(&self) -> rusqlite::Result<rusqlite::Connection> {
        use rusqlite::OpenFlags;
        rusqlite::Connection::open_with_flags(
            self.db_path.as_ref(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
    }
}

/// Mount the read-only API and UI onto a router.
pub fn routes(store: ReadStore) -> Router {
    Router::new()
        .route("/ui", get(ui_index))
        .route("/api/calls", get(list_handler))
        .route("/api/calls/:id", get(detail_handler))
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
        "response_raw_sse": r.response_raw_sse,
        "response_reconstructed": r.response_reconstructed,
        "is_stream": r.response_raw_sse.is_some(),
        "error": r.error,
    })
}

/// GET /ui — the single-page static UI (served inline; touches no files).
async fn ui_index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(UI_HTML),
    )
}

/// The UI is a single self-contained HTML document with inline CSS/JS that
/// talks to the read-only JSON API.
const UI_HTML: &str = include_str!("ui.html");
