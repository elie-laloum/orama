//! SQLite persistence for captured round-trips.
//!
//! A single background task owns the `rusqlite::Connection`. The request path
//! never touches SQLite directly: it builds a [`CallRecord`] and hands it off
//! over an unbounded channel, so persistence can never block or slow the relay.

use std::path::Path;

use serde_json::Value;
use tokio::sync::mpsc;

/// Header names whose values must be redacted before they are written to disk.
const REDACT_HEADERS: &[&str] = &["authorization", "x-api-key", "proxy-authorization"];

/// Placeholder written in place of a redacted header value.
pub const REDACTED: &str = "<redacted>";

/// One captured request→response exchange. Mirrors the POC schema; streaming
/// columns (`timestamp_first_chunk`, `response_raw_sse`, `response_reconstructed`)
/// are populated by later tickets and default to `None` here.
#[derive(Debug, Clone, Default)]
pub struct CallRecord {
    pub timestamp_start: String,
    pub timestamp_first_chunk: Option<String>,
    pub timestamp_end: Option<String>,
    pub method: String,
    pub url: String,
    pub request_headers: Value,
    pub request_body: Option<Value>,
    pub response_status: Option<i64>,
    pub response_headers: Option<Value>,
    pub response_raw_sse: Option<String>,
    pub response_reconstructed: Option<Value>,
    pub error: Option<String>,
}

/// A row read back out of the database (includes the assigned id).
#[derive(Debug, Clone)]
pub struct StoredCall {
    pub id: i64,
    pub record: CallRecord,
}

/// Redact sensitive header values in a headers JSON object (case-insensitive).
///
/// Returns a new value; the input is not mutated.
pub fn redact_headers(headers: &Value) -> Value {
    let mut out = headers.clone();
    if let Value::Object(map) = &mut out {
        for (name, value) in map.iter_mut() {
            if REDACT_HEADERS
                .iter()
                .any(|h| h.eq_ignore_ascii_case(name))
            {
                *value = Value::String(REDACTED.to_string());
            }
        }
    }
    out
}

/// Handle used by the request path to enqueue records for persistence.
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::UnboundedSender<CallRecord>,
}

impl StoreHandle {
    /// Enqueue a finished record. Best-effort: a closed channel is logged to
    /// stderr and dropped, never propagated to the client.
    pub fn record(&self, record: CallRecord) {
        if let Err(err) = self.tx.send(record) {
            eprintln!("tracer: failed to enqueue call record: {err}");
        }
    }
}

/// Open (or create) the SQLite database, apply the schema, and spawn the
/// background writer task. Returns a cloneable handle for the request path.
pub fn spawn_writer(path: impl AsRef<Path>) -> anyhow::Result<StoreHandle> {
    let conn = rusqlite::Connection::open(path)?;
    apply_schema(&conn)?;

    let (tx, mut rx) = mpsc::unbounded_channel::<CallRecord>();

    // The writer task owns the connection for its entire lifetime.
    tokio::task::spawn_blocking(move || {
        while let Some(record) = rx.blocking_recv() {
            if let Err(err) = insert(&conn, &record) {
                eprintln!("tracer: failed to persist call record: {err}");
            }
        }
    });

    Ok(StoreHandle { tx })
}

/// Apply the POC schema. Idempotent (`IF NOT EXISTS`).
pub fn apply_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS calls (
            id                       INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_start          TEXT NOT NULL,
            timestamp_first_chunk    TEXT,
            timestamp_end            TEXT,
            method                   TEXT NOT NULL,
            url                      TEXT NOT NULL,
            request_headers          TEXT NOT NULL,
            request_body             TEXT,
            response_status          INTEGER,
            response_headers         TEXT,
            response_raw_sse         TEXT,
            response_reconstructed   TEXT,
            error                    TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_calls_timestamp_start ON calls(timestamp_start);
        "#,
    )
}

fn value_to_text(v: &Option<Value>) -> Option<String> {
    v.as_ref().map(|v| v.to_string())
}

/// Insert one record. Auth headers are redacted here as a defensive final gate
/// even if the caller already redacted.
pub fn insert(conn: &rusqlite::Connection, record: &CallRecord) -> rusqlite::Result<i64> {
    let request_headers = redact_headers(&record.request_headers).to_string();
    let response_headers = record
        .response_headers
        .as_ref()
        .map(|h| redact_headers(h).to_string());

    conn.execute(
        r#"
        INSERT INTO calls (
            timestamp_start, timestamp_first_chunk, timestamp_end,
            method, url, request_headers, request_body,
            response_status, response_headers, response_raw_sse,
            response_reconstructed, error
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
        "#,
        rusqlite::params![
            record.timestamp_start,
            record.timestamp_first_chunk,
            record.timestamp_end,
            record.method,
            record.url,
            request_headers,
            value_to_text(&record.request_body),
            record.response_status,
            response_headers,
            record.response_raw_sse,
            value_to_text(&record.response_reconstructed),
            record.error,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn text_to_value(s: Option<String>) -> Option<Value> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

/// Read all stored calls, most recent first. Used by tests and the read API.
pub fn list_calls(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<StoredCall>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, timestamp_start, timestamp_first_chunk, timestamp_end,
               method, url, request_headers, request_body,
               response_status, response_headers, response_raw_sse,
               response_reconstructed, error
        FROM calls
        ORDER BY id DESC
        "#,
    )?;
    let rows = stmt.query_map([], row_to_stored)?;
    rows.collect()
}

/// Read a single stored call by id.
pub fn get_call(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<StoredCall>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT id, timestamp_start, timestamp_first_chunk, timestamp_end,
               method, url, request_headers, request_body,
               response_status, response_headers, response_raw_sse,
               response_reconstructed, error
        FROM calls
        WHERE id = ?1
        "#,
    )?;
    let mut rows = stmt.query_map([id], row_to_stored)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

fn row_to_stored(row: &rusqlite::Row) -> rusqlite::Result<StoredCall> {
    let request_headers: String = row.get(6)?;
    Ok(StoredCall {
        id: row.get(0)?,
        record: CallRecord {
            timestamp_start: row.get(1)?,
            timestamp_first_chunk: row.get(2)?,
            timestamp_end: row.get(3)?,
            method: row.get(4)?,
            url: row.get(5)?,
            request_headers: serde_json::from_str(&request_headers)
                .unwrap_or(Value::Null),
            request_body: text_to_value(row.get(7)?),
            response_status: row.get(8)?,
            response_headers: text_to_value(row.get(9)?),
            response_raw_sse: row.get(10)?,
            response_reconstructed: text_to_value(row.get(11)?),
            error: row.get(12)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_auth_headers_case_insensitively() {
        let headers = json!({
            "Authorization": "Bearer secret",
            "x-api-key": "sk-abc",
            "content-type": "application/json",
        });
        let red = redact_headers(&headers);
        assert_eq!(red["Authorization"], REDACTED);
        assert_eq!(red["x-api-key"], REDACTED);
        assert_eq!(red["content-type"], "application/json");
    }

    #[test]
    fn insert_and_read_back_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();

        let record = CallRecord {
            timestamp_start: "2026-07-25T00:00:00Z".into(),
            timestamp_end: Some("2026-07-25T00:00:01Z".into()),
            method: "POST".into(),
            url: "/v1/messages".into(),
            request_headers: json!({"authorization": "Bearer secret"}),
            request_body: Some(json!({"model": "claude"})),
            response_status: Some(200),
            response_headers: Some(json!({"content-type": "application/json"})),
            ..Default::default()
        };
        let id = insert(&conn, &record).unwrap();
        assert!(id > 0);

        let got = get_call(&conn, id).unwrap().unwrap();
        assert_eq!(got.record.method, "POST");
        assert_eq!(got.record.response_status, Some(200));
        // Auth was redacted on write.
        assert_eq!(got.record.request_headers["authorization"], REDACTED);
        // Non-auth data stored verbatim.
        assert_eq!(got.record.request_body.unwrap()["model"], "claude");
    }

    #[test]
    fn list_is_most_recent_first() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        for i in 0..3 {
            let mut r = CallRecord {
                timestamp_start: format!("t{i}"),
                method: "GET".into(),
                url: format!("/{i}"),
                request_headers: json!({}),
                ..Default::default()
            };
            r.url = format!("/{i}");
            insert(&conn, &r).unwrap();
        }
        let all = list_calls(&conn).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].record.url, "/2");
        assert_eq!(all[2].record.url, "/0");
    }
}
