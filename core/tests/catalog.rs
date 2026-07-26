//! Pricing against the model catalogue: what happens offline, what happens when
//! rates move, and what must never happen to a call whose usage we never saw.

use orama_core::{
    derive::write::{backfill, reprice},
    store::{apply_schema, insert, CallRecord},
};
use serde_json::json;

fn temp_db(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "orama-catalog-{name}-{}.sqlite",
        std::process::id()
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    path
}

fn seed(path: &std::path::Path, records: &[CallRecord]) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    apply_schema(&conn).unwrap();
    for record in records {
        insert(&conn, record).unwrap();
    }
    conn
}

/// An Anthropic call with the usage shape the provider actually reports.
fn anthropic_call(model: &str) -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-26T10:00:00Z".into(),
        timestamp_end: Some("2026-07-26T10:00:02Z".into()),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({
            "user-agent": "claude-cli/2.0.0",
            "x-claude-code-session-id": "session-a",
        }),
        request_body: Some(json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}],
        })),
        response_status: Some(200),
        response_body: Some(json!({
            "type": "message", "role": "assistant", "model": model,
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12, "output_tokens": 40,
                "cache_read_input_tokens": 90_000,
                "cache_creation_input_tokens": 5_000
            }
        })),
        ..Default::default()
    }
}

/// A Codex call on a model that used to have no rates at all.
fn codex_call() -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-26T11:00:00Z".into(),
        timestamp_end: Some("2026-07-26T11:00:03Z".into()),
        method: "POST".into(),
        url: "/v1/chat/completions".into(),
        request_headers: json!({ "user-agent": "codex_cli_rs/0.145.0" }),
        request_body: Some(json!({
            "model": "gpt-5.6-luna",
            "messages": [{"role": "user", "content": "refactor"}],
        })),
        response_status: Some(200),
        response_body: Some(json!({
            "id": "chatcmpl-1", "model": "gpt-5.6-luna",
            "choices": [{"message": {"role": "assistant", "content": "ok"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10_149, "completion_tokens": 11,
                      "prompt_tokens_details": {"cached_tokens": 0}}
        })),
        ..Default::default()
    }
}

/// A call whose response was never captured: no usage counters at all.
fn call_with_no_usage() -> CallRecord {
    CallRecord {
        timestamp_start: "2026-07-26T12:00:00Z".into(),
        method: "POST".into(),
        url: "/v1/messages".into(),
        request_headers: json!({
            "user-agent": "claude-cli/2.0.0",
            "x-claude-code-session-id": "session-a",
        }),
        request_body: Some(json!({
            "model": "claude-opus-5",
            "messages": [{"role": "user", "content": "hello"}],
        })),
        response_status: Some(200),
        response_body: None,
        ..Default::default()
    }
}

fn cost_of(conn: &rusqlite::Connection, model: &str) -> Option<f64> {
    conn.query_row(
        "SELECT cost_total_usd FROM generations WHERE model = ?1",
        [model],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn a_fresh_database_prices_from_the_bundled_snapshot_with_no_network() {
    // The offline guarantee: a clone with no connectivity and no cached
    // catalogue still prices, because a snapshot ships in the binary.
    std::env::set_var("ORAMA_CATALOG_REFRESH", "0");
    let db = temp_db("offline");
    let conn = seed(&db, &[anthropic_call("claude-opus-5")]);
    backfill(&conn).unwrap();

    let cost = cost_of(&conn, "claude-opus-5").expect("priced from the bundled snapshot");
    assert!(cost > 0.0);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_codex_model_is_priced() {
    // The gap this change exists to close: these models were absent from the
    // hand-maintained table, so every Codex generation derived unpriced.
    let db = temp_db("codex");
    let conn = seed(&db, &[codex_call()]);
    backfill(&conn).unwrap();

    let cost = cost_of(&conn, "gpt-5.6-luna").expect("Codex models are priced");
    assert!(cost > 0.0);

    // And the signal that reported the gap stops firing.
    let unpriced: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM alerts WHERE rule_id = 'data_quality.pricing_unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unpriced, 0);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn a_call_with_no_usage_is_never_priced_at_zero() {
    // The invariant most at risk from a pricing rewrite. Five generations in the
    // real capture look like this: the response was lost, so nothing is known
    // about what they cost. Reporting $0.00 would say they were free.
    let db = temp_db("nousage");
    let conn = seed(&db, &[call_with_no_usage()]);
    backfill(&conn).unwrap();

    let cost: Option<f64> = conn
        .query_row("SELECT cost_total_usd FROM generations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(cost, None);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn re_pricing_updates_generations_and_sessions_together() {
    // Session totals are a sum of generation costs, so a re-price that skipped
    // them would leave the two disagreeing with no way to tell which was right.
    let db = temp_db("reprice");
    let conn = seed(&db, &[anthropic_call("claude-opus-5")]);
    backfill(&conn).unwrap();

    let generation = cost_of(&conn, "claude-opus-5").unwrap();
    let session: f64 = conn
        .query_row("SELECT cost_total_usd FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert!((generation - session).abs() < 1e-12);

    // Re-pricing against the same catalogue must be a no-op in value, not a
    // drift: prices are a pure function of the snapshot and the counters.
    let updated = reprice(&conn).unwrap();
    assert_eq!(updated, 1);
    assert_eq!(cost_of(&conn, "claude-opus-5").unwrap(), generation);
    let after: f64 = conn
        .query_row("SELECT cost_total_usd FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert!((after - session).abs() < 1e-12);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn re_pricing_does_not_re_read_the_captured_bodies() {
    // The whole argument for a targeted re-price over a full rebuild: cost is
    // computable from columns already on `generations`. Corrupting the raw body
    // after derivation proves nothing re-parses it — if this ever starts
    // failing, re-pricing has quietly become as expensive as rebuilding.
    let db = temp_db("nobody");
    let conn = seed(&db, &[anthropic_call("claude-opus-5")]);
    backfill(&conn).unwrap();
    let before = cost_of(&conn, "claude-opus-5").unwrap();

    conn.execute("UPDATE calls SET request_body = 'not json at all'", [])
        .unwrap();

    assert_eq!(reprice(&conn).unwrap(), 1);
    assert_eq!(cost_of(&conn, "claude-opus-5").unwrap(), before);
    let _ = std::fs::remove_file(&db);
}

#[test]
fn every_generation_records_which_rates_priced_it() {
    // Including the ones that could not be priced. "Unpriced" and "never
    // considered" have to be distinguishable, or a later refresh has no way to
    // find the rows that were unknown then and are known now.
    let db = temp_db("stamp");
    let conn = seed(
        &db,
        &[anthropic_call("claude-opus-5"), call_with_no_usage()],
    );
    backfill(&conn).unwrap();

    let unstamped: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM generations WHERE pricing_version IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unstamped, 0);

    // The version names the snapshot, so a row can be traced to its rates.
    let version: String = conn
        .query_row(
            "SELECT pricing_version FROM generations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(version.contains("+cat:"), "version is {version}");
    assert!(!version.contains("+cat:none"), "no catalogue was loaded");
    let _ = std::fs::remove_file(&db);
}

#[test]
fn the_catalogue_survives_a_second_schema_apply() {
    // Every migration step has to tolerate re-running against a database that
    // already satisfies it.
    let db = temp_db("schema");
    let conn = seed(&db, &[]);
    apply_schema(&conn).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_catalog'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 1);
    let _ = std::fs::remove_file(&db);
}
