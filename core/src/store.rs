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
    /// Body of a non-streaming response, captured verbatim. `None` for streams,
    /// where [`Self::response_raw_sse`] holds the payload instead.
    pub response_body: Option<Value>,
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
            if REDACT_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(name)) {
                *value = Value::String(REDACTED.to_string());
            }
        }
    }
    out
}

/// Work for the single writer task.
///
/// Re-pricing goes through the same channel as capture rather than opening a
/// second connection: SQLite gets exactly one writer, and a catalogue refresh
/// queues behind in-flight captures instead of racing them.
enum WriterMsg {
    Record(Box<CallRecord>),
    Reprice,
}

/// Handle used by the request path to enqueue records for persistence.
#[derive(Clone)]
pub struct StoreHandle {
    tx: mpsc::UnboundedSender<WriterMsg>,
}

impl StoreHandle {
    /// Enqueue a finished record. Best-effort: a closed channel is logged to
    /// stderr and dropped, never propagated to the client.
    pub fn record(&self, record: CallRecord) {
        self.send(WriterMsg::Record(Box::new(record)), "call record");
    }

    /// Ask the writer to re-price the derived generations.
    ///
    /// Called when a catalogue refresh changes the rates under rows that were
    /// already priced.
    pub fn reprice(&self) {
        self.send(WriterMsg::Reprice, "reprice request");
    }

    fn send(&self, message: WriterMsg, what: &str) {
        if let Err(err) = self.tx.send(message) {
            eprintln!("orama: failed to enqueue {what}: {err}");
        }
    }
}

/// Open (or create) the SQLite database, apply the schema, and spawn the
/// background writer task. Returns a cloneable handle for the request path.
pub fn spawn_writer(path: impl AsRef<Path>) -> anyhow::Result<StoreHandle> {
    let conn = rusqlite::Connection::open(path)?;
    apply_schema(&conn)?;

    // Rates have to be in force before anything is derived or re-priced, and
    // this is offline: the cached snapshot if there is one, the embedded seed
    // otherwise. The network refresh comes later and separately, so a machine
    // with no connectivity still prices.
    crate::catalog::ensure_loaded(&conn);

    // Bring the derived layer up to the running parser before serving anything.
    // Without this an upgraded binary reads a database derived by an older
    // parser: the new columns and tables are simply empty, and the UI reports
    // "nothing captured" for data that is in fact sitting in `calls`. Derivation
    // is a pure function of the raw rows, so this is always safe to run.
    match crate::derive::write::rebuild_if_stale(&conn) {
        Ok(report) if report.derived > 0 || report.failed > 0 => {
            eprintln!(
                "orama: derived {} call(s), {} failed",
                report.derived, report.failed
            );
        }
        Ok(_) => {}
        // A backfill failure must not stop the proxy: relaying traffic is the
        // job that cannot be dropped, and analysis is best-effort.
        Err(err) => eprintln!("orama: could not bring the derived layer up to date: {err}"),
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<WriterMsg>();

    // The writer task owns the connection for its entire lifetime.
    tokio::task::spawn_blocking(move || {
        while let Some(message) = rx.blocking_recv() {
            match message {
                WriterMsg::Record(record) => match insert(&conn, &record) {
                    Ok(id) => {
                        // The raw row is already durable, so derivation runs second
                        // and its failures are recorded rather than propagated.
                        // Deriving from the in-memory record matches deriving from
                        // the stored row: redaction only touches auth headers, which
                        // the derived layer never reads.
                        let stored = StoredCall {
                            id,
                            record: *record,
                        };
                        crate::derive::write::derive_live(&conn, &stored);
                    }
                    Err(err) => eprintln!("orama: failed to persist call record: {err}"),
                },
                WriterMsg::Reprice => match crate::derive::write::reprice(&conn) {
                    Ok(updated) => eprintln!("orama: re-priced {updated} generation(s)"),
                    Err(err) => eprintln!("orama: could not re-price generations: {err}"),
                },
            }
        }
    });

    Ok(StoreHandle { tx })
}

/// Ordered schema migrations. Applying step `i` leaves the database at
/// `user_version = i + 1`, so the constant below is the whole schema history.
///
/// Databases created before migrations existed sit at `user_version = 0` with
/// the v1 tables already present, so every step must be safe to re-run against
/// a database that already satisfies it.
const MIGRATIONS: &[Migration] = &[
    // v1 — POC schema: one raw capture table.
    Migration::Sql(
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
    ),
    // v2 — capture non-streaming response bodies, which were previously
    // forwarded to the client and dropped.
    Migration::AddColumn {
        table: "calls",
        column: "response_body",
        ddl: "ALTER TABLE calls ADD COLUMN response_body TEXT",
    },
    // v3 — materialized derived tables.
    //
    // `calls` stays the append-only source of truth. Everything below is a pure
    // function of `calls` and the parser version, so it can be dropped and
    // rebuilt at any time without data loss. Content is never copied here —
    // request bodies are ~96% of the database because each call re-sends the
    // whole conversation, so only fingerprints, counters and short excerpts land
    // in derived rows.
    Migration::Sql(DERIVED_SCHEMA_V3),
    // v4 — signals the trace model needs, computed per call but only meaningful
    // when read in session order.
    Migration::AddColumn {
        table: "generations",
        column: "new_user_turn",
        ddl: "ALTER TABLE generations ADD COLUMN new_user_turn INTEGER NOT NULL DEFAULT 0",
    },
    Migration::AddColumn {
        table: "generations",
        column: "first_turn_hash",
        ddl: "ALTER TABLE generations ADD COLUMN first_turn_hash TEXT",
    },
    Migration::AddColumn {
        table: "generations",
        column: "depth",
        ddl: "ALTER TABLE generations ADD COLUMN depth INTEGER NOT NULL DEFAULT 0",
    },
    // v5 — persisted, queryable alerts.
    Migration::Sql(ALERTS_SCHEMA_V5),
    // v6 — the harness itself: what the client declares, as opposed to what the
    // conversation says. Content-addressed, so the 320 KB tool block a session
    // re-sends on every call is stored once.
    Migration::Sql(HARNESS_SCHEMA_V6),
    Migration::AddColumn {
        table: "generations",
        column: "tools_chars",
        ddl: "ALTER TABLE generations ADD COLUMN tools_chars INTEGER",
    },
    // v7 — the cached model catalogue.
    Migration::Sql(CATALOG_SCHEMA_V7),
];

/// The model catalogue, cached from models.dev.
///
/// The odd one out in this schema: it is neither a raw capture nor a function of
/// one, but a copy of an external document. Exactly one row is kept — the
/// snapshot in force — because nothing derives from a superseded catalogue, and
/// keeping old payloads would only invite pricing a call against a version
/// nobody chose. `digest` is what pricing stamps onto the rows it prices, so any
/// generation can be traced back to the rates that produced it.
const CATALOG_SCHEMA_V7: &str = r#"
CREATE TABLE IF NOT EXISTS model_catalog (
    digest      TEXT PRIMARY KEY,
    etag        TEXT,
    -- When this payload was downloaded.
    fetched_at  TEXT NOT NULL,
    -- When it was last confirmed current; a 304 moves this and not fetched_at.
    checked_at  TEXT NOT NULL,
    -- gzipped models.dev api.json.
    payload     BLOB NOT NULL
);
"#;

/// Alert storage. Derived like everything else, so a policy change rebuilds it.
///
/// `dedup_key` makes re-derivation idempotent and lets a recurring condition
/// increment an occurrence count instead of producing a row per capture.
const ALERTS_SCHEMA_V5: &str = r#"
CREATE TABLE alerts (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    dedup_key      TEXT NOT NULL UNIQUE,
    rule_id        TEXT NOT NULL,
    category       TEXT NOT NULL,
    severity       TEXT NOT NULL,
    confidence     TEXT NOT NULL,
    scope_kind     TEXT NOT NULL,
    scope_id       TEXT NOT NULL,
    call_id        INTEGER REFERENCES calls(id) ON DELETE CASCADE,
    span_id        TEXT,
    trace_id       TEXT,
    session_id     TEXT,
    title          TEXT NOT NULL,
    summary        TEXT NOT NULL,
    explanation    TEXT NOT NULL,
    impact         TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    observed       TEXT NOT NULL,
    metric_value   REAL,
    threshold      REAL,
    metric_unit    TEXT,
    occurred_at    TEXT NOT NULL,
    occurrences    INTEGER NOT NULL DEFAULT 1,
    policy_version TEXT NOT NULL,
    parser_version TEXT NOT NULL
);
CREATE INDEX idx_alerts_occurred ON alerts(occurred_at DESC);
CREATE INDEX idx_alerts_severity ON alerts(severity, occurred_at DESC);
CREATE INDEX idx_alerts_category ON alerts(category, occurred_at DESC);
CREATE INDEX idx_alerts_rule     ON alerts(rule_id, occurred_at DESC);
CREATE INDEX idx_alerts_session  ON alerts(session_id, occurred_at DESC);
CREATE INDEX idx_alerts_call     ON alerts(call_id);
"#;

/// The harness configuration a request declares: system prompt and tool set.
///
/// Keyed by content fingerprint rather than by call. A session re-sends an
/// identical system prompt and tool block on every turn, so storing them per
/// call would multiply the database by the number of turns to hold the same
/// bytes — the exact reason the derived layer keeps only fingerprints elsewhere.
/// Here the fingerprint *is* the key, which makes the content affordable to keep
/// verbatim: one row per distinct harness configuration ever observed.
///
/// Rows are shared between calls, so they are never deleted per-call on
/// re-derivation. A configuration no longer referenced by any generation is
/// harmless; a full rebuild clears the tables outright.
const HARNESS_SCHEMA_V6: &str = r#"
CREATE TABLE system_prompts (
    system_hash    TEXT PRIMARY KEY,
    -- JSON array of {text, chars, cache_control}, in the order sent.
    segments       TEXT NOT NULL,
    total_chars    INTEGER NOT NULL,
    segment_count  INTEGER NOT NULL,
    cache_points   INTEGER NOT NULL,
    parser_version TEXT NOT NULL
);

CREATE TABLE tool_sets (
    tools_hash     TEXT PRIMARY KEY,
    tool_count     INTEGER NOT NULL,
    total_chars    INTEGER NOT NULL,
    mcp_count      INTEGER NOT NULL,
    parser_version TEXT NOT NULL
);

CREATE TABLE tool_schemas (
    tools_hash   TEXT NOT NULL REFERENCES tool_sets(tools_hash) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    name         TEXT NOT NULL,
    server       TEXT,
    is_mcp       INTEGER NOT NULL,
    description  TEXT,
    input_schema TEXT,
    -- What this one declaration costs in the request, description plus schema.
    chars        INTEGER NOT NULL,
    PRIMARY KEY (tools_hash, name)
);
CREATE INDEX idx_tool_schemas_name  ON tool_schemas(name);
CREATE INDEX idx_tool_schemas_chars ON tool_schemas(chars DESC);
"#;

/// Derived-layer DDL. Kept separate for readability; applied as migration v3.
const DERIVED_SCHEMA_V3: &str = r#"
CREATE TABLE generations (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    call_id                  INTEGER NOT NULL UNIQUE REFERENCES calls(id) ON DELETE CASCADE,
    parser_version           TEXT NOT NULL,

    -- correlation. trace/span/agent columns are filled by the trace model.
    session_id               TEXT,
    trace_id                 TEXT,
    span_id                  TEXT,
    parent_span_id           TEXT,
    upstream_trace_id        TEXT,
    upstream_span_id         TEXT,
    request_id               TEXT,
    account_uuid             TEXT,
    device_id                TEXT,
    org_id                   TEXT,
    agent_id                 TEXT,
    agent_name               TEXT,
    agent_role               TEXT,
    billing_variant          TEXT,

    -- environment
    provider                 TEXT NOT NULL,
    framework                TEXT,
    client_version           TEXT,
    git_branch               TEXT,
    project_name             TEXT,
    cwd                      TEXT,

    -- model and request parameters
    model                    TEXT,
    model_resolved           TEXT,
    service_tier             TEXT,
    is_stream                INTEGER NOT NULL DEFAULT 0,
    max_tokens               INTEGER,
    temperature              REAL,
    thinking_mode            TEXT,
    thinking_budget          INTEGER,
    stop_sequences           TEXT,
    context_management       TEXT,
    compaction_requested     INTEGER NOT NULL DEFAULT 0,

    -- tokens
    input_tokens             INTEGER,
    output_tokens            INTEGER,
    total_tokens             INTEGER,
    cache_creation_tokens    INTEGER,
    cache_read_tokens        INTEGER,
    cache_creation_5m_tokens INTEGER,
    cache_creation_1h_tokens INTEGER,
    cache_ttl_source         TEXT,
    thinking_tokens          INTEGER,

    -- cost, filled once the pricing table lands
    cost_input_usd           REAL,
    cost_output_usd          REAL,
    cost_cache_write_usd     REAL,
    cost_cache_read_usd      REAL,
    cost_total_usd           REAL,
    cost_uncached_equiv_usd  REAL,
    pricing_model_id         TEXT,
    pricing_version          TEXT,

    -- timing
    started_at               TEXT NOT NULL,
    first_token_at           TEXT,
    ended_at                 TEXT,
    ttft_ms                  INTEGER,
    latency_ms               INTEGER,

    -- outcome
    http_status              INTEGER,
    stop_reason              TEXT,
    stop_sequence            TEXT,
    is_error                 INTEGER NOT NULL DEFAULT 0,
    error_kind               TEXT,
    error_message            TEXT,
    retry_count              INTEGER,
    should_retry             INTEGER,
    ratelimit_status         TEXT,
    ratelimit_5h_utilization REAL,
    ratelimit_7d_utilization REAL,
    ratelimit_reset_at       TEXT,
    overage_status           TEXT,

    -- shape: fingerprints and counters only, never content copies
    system_hash              TEXT,
    system_chars             INTEGER,
    system_segments_count    INTEGER,
    system_cache_points      INTEGER,
    tools_hash               TEXT,
    tools_declared_count     INTEGER,
    messages_count           INTEGER,
    context_chars            INTEGER,
    history_prefix_hash      TEXT,
    tool_call_count          INTEGER NOT NULL DEFAULT 0,
    tools_called             TEXT,
    user_prompt              TEXT,
    block_counts             TEXT
);
CREATE INDEX idx_gen_started ON generations(started_at DESC);
CREATE INDEX idx_gen_session ON generations(session_id, started_at);
CREATE INDEX idx_gen_trace   ON generations(trace_id, started_at);
CREATE INDEX idx_gen_parent  ON generations(parent_span_id);
CREATE INDEX idx_gen_model   ON generations(model, started_at DESC);
CREATE INDEX idx_gen_agent   ON generations(agent_id, started_at DESC);
CREATE INDEX idx_gen_errors  ON generations(started_at DESC) WHERE is_error = 1;
CREATE INDEX idx_gen_cost    ON generations(cost_total_usd DESC);
CREATE INDEX idx_gen_syshash ON generations(session_id, system_hash);

CREATE TABLE tool_calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    generation_id       INTEGER NOT NULL REFERENCES generations(id) ON DELETE CASCADE,
    call_id             INTEGER NOT NULL REFERENCES calls(id) ON DELETE CASCADE,
    session_id          TEXT,
    trace_id            TEXT,
    seq                 INTEGER NOT NULL,
    tool_use_id         TEXT,
    name                TEXT NOT NULL,
    server              TEXT,
    is_mcp              INTEGER NOT NULL DEFAULT 0,
    was_declared        INTEGER NOT NULL DEFAULT 1,
    input_chars         INTEGER,
    input_excerpt       TEXT,
    result_chars        INTEGER,
    result_excerpt      TEXT,
    is_error            INTEGER,
    status              TEXT NOT NULL,
    emitted_at          TEXT,
    observed_at         TEXT,
    duration_ms         INTEGER,
    parser_version      TEXT NOT NULL
);
CREATE INDEX idx_tool_gen     ON tool_calls(generation_id, seq);
CREATE INDEX idx_tool_name    ON tool_calls(name, emitted_at DESC);
CREATE INDEX idx_tool_session ON tool_calls(session_id, name);
CREATE INDEX idx_tool_bad     ON tool_calls(emitted_at DESC) WHERE status <> 'ok';

CREATE TABLE sessions (
    session_id               TEXT PRIMARY KEY,
    title                    TEXT,
    project_name             TEXT,
    git_branch               TEXT,
    cwd                      TEXT,
    account_uuid             TEXT,
    org_id                   TEXT,
    framework                TEXT,
    client_version           TEXT,
    primary_model            TEXT,
    models                   TEXT,
    started_at               TEXT NOT NULL,
    ended_at                 TEXT,
    duration_ms              INTEGER,
    generation_count         INTEGER NOT NULL DEFAULT 0,
    tool_call_count          INTEGER NOT NULL DEFAULT 0,
    error_count              INTEGER NOT NULL DEFAULT 0,
    input_tokens             INTEGER,
    output_tokens            INTEGER,
    cache_read_tokens        INTEGER,
    cache_creation_tokens    INTEGER,
    thinking_tokens          INTEGER,
    cost_total_usd           REAL,
    cache_savings_usd        REAL,
    peak_context_tokens      INTEGER,
    usage_coverage           REAL,
    parser_version           TEXT NOT NULL
);
CREATE INDEX idx_sessions_started ON sessions(started_at DESC);
CREATE INDEX idx_sessions_cost    ON sessions(cost_total_usd DESC);

-- Derivation bookkeeping. A failure here must never cost us the raw capture,
-- so it is recorded and surfaced rather than logged and forgotten.
CREATE TABLE derive_failures (
    call_id        INTEGER PRIMARY KEY REFERENCES calls(id) ON DELETE CASCADE,
    parser_version TEXT NOT NULL,
    stage          TEXT NOT NULL,
    error          TEXT NOT NULL,
    panicked       INTEGER NOT NULL DEFAULT 0,
    at             TEXT NOT NULL
);

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

/// One schema step. `AddColumn` is separate because `ALTER TABLE ... ADD COLUMN`
/// has no `IF NOT EXISTS` form and must be probed before it runs.
enum Migration {
    Sql(&'static str),
    AddColumn {
        table: &'static str,
        column: &'static str,
        ddl: &'static str,
    },
}

/// Bring the database up to the latest schema version. Idempotent.
pub fn apply_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    apply_pragmas(conn)?;

    let current: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let version = index as u32 + 1;
        if version <= current {
            continue;
        }
        match migration {
            Migration::Sql(sql) => conn.execute_batch(sql)?,
            Migration::AddColumn { table, column, ddl } => {
                if !column_exists(conn, table, column)? {
                    conn.execute_batch(ddl)?;
                }
            }
        }
        // `user_version` takes a literal, not a bound parameter.
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    }
    Ok(())
}

/// Connection settings applied to every connection, reader or writer.
///
/// WAL lets the read API query concurrently with the writer task instead of
/// racing it for the rollback journal, which previously surfaced as `SQLITE_BUSY`.
pub fn apply_pragmas(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    // `journal_mode` returns the resulting mode, so it needs a row-returning call.
    let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
}

/// Is `column` already present on `table`?
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
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
            response_status, response_headers, response_body, response_raw_sse,
            response_reconstructed, error
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
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
            value_to_text(&record.response_body),
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
               response_status, response_headers, response_body, response_raw_sse,
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
               response_status, response_headers, response_body, response_raw_sse,
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
            request_headers: serde_json::from_str(&request_headers).unwrap_or(Value::Null),
            request_body: text_to_value(row.get(7)?),
            response_status: row.get(8)?,
            response_headers: text_to_value(row.get(9)?),
            response_body: text_to_value(row.get(10)?),
            response_raw_sse: row.get(11)?,
            response_reconstructed: text_to_value(row.get(12)?),
            error: row.get(13)?,
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
    fn schema_is_versioned_and_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as u32);

        // Re-running must be a no-op rather than an error.
        apply_schema(&conn).unwrap();
        assert!(column_exists(&conn, "calls", "response_body").unwrap());
    }

    #[test]
    fn legacy_database_without_user_version_migrates_in_place() {
        // Reproduce a pre-migrations database: v1 tables present, version 0.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE calls (
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
            INSERT INTO calls (timestamp_start, method, url, request_headers)
            VALUES ('t0', 'POST', '/v1/messages', '{}');
            "#,
        )
        .unwrap();

        apply_schema(&conn).unwrap();

        // The pre-existing row survives and gains the new column as NULL.
        let calls = list_calls(&conn).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].record.url, "/v1/messages");
        assert!(calls[0].record.response_body.is_none());
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
