//! Persisting and rebuilding the derived layer.
//!
//! Every write is delete-then-insert scoped to one `call_id`, so deriving the
//! same capture twice is a no-op rather than a duplicate. That makes backfill,
//! live derivation and a full rebuild the same code path.

use rusqlite::{named_params, Connection, Result};

use super::{derive_one, Derived, PARSER_VERSION};
use crate::store::{list_calls, StoredCall};
use crate::util::now_rfc3339;

/// Key under which the parser version that produced the derived tables is kept.
const META_PARSER_VERSION: &str = "parser_version";

/// What a backfill pass did, for reporting to the operator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub derived: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Write the derived rows for one capture, replacing any previous derivation.
pub fn write_derived(conn: &Connection, derived: &Derived) -> Result<()> {
    let row = &derived.generation;
    // Cascades to tool_calls, so a re-derive never leaves orphans behind.
    conn.execute("DELETE FROM generations WHERE call_id = ?1", [row.call_id])?;
    conn.execute(
        r#"
        INSERT INTO generations (
            call_id, parser_version, session_id, trace_id, span_id, parent_span_id,
            upstream_trace_id, upstream_span_id, request_id, account_uuid, device_id, org_id,
            agent_id, agent_name, agent_role, billing_variant,
            provider, framework, client_version, git_branch, project_name, cwd,
            model, model_resolved, service_tier, is_stream, max_tokens, temperature,
            thinking_mode, thinking_budget, stop_sequences, context_management, compaction_requested,
            input_tokens, output_tokens, total_tokens, cache_creation_tokens, cache_read_tokens,
            cache_creation_5m_tokens, cache_creation_1h_tokens, cache_ttl_source, thinking_tokens,
            started_at, first_token_at, ended_at, ttft_ms, latency_ms,
            http_status, stop_reason, stop_sequence, is_error, error_kind, error_message,
            retry_count, should_retry, ratelimit_status, ratelimit_5h_utilization,
            ratelimit_7d_utilization, ratelimit_reset_at, overage_status,
            system_hash, system_chars, system_segments_count, system_cache_points,
            tools_hash, tools_declared_count, messages_count, context_chars, history_prefix_hash,
            tool_call_count, tools_called, user_prompt, block_counts,
            new_user_turn, first_turn_hash
        ) VALUES (
            :call_id, :parser_version, :session_id, :trace_id, :span_id, :parent_span_id,
            :upstream_trace_id, :upstream_span_id, :request_id, :account_uuid, :device_id, :org_id,
            :agent_id, :agent_name, :agent_role, :billing_variant,
            :provider, :framework, :client_version, :git_branch, :project_name, :cwd,
            :model, :model_resolved, :service_tier, :is_stream, :max_tokens, :temperature,
            :thinking_mode, :thinking_budget, :stop_sequences, :context_management, :compaction_requested,
            :input_tokens, :output_tokens, :total_tokens, :cache_creation_tokens, :cache_read_tokens,
            :cache_creation_5m_tokens, :cache_creation_1h_tokens, :cache_ttl_source, :thinking_tokens,
            :started_at, :first_token_at, :ended_at, :ttft_ms, :latency_ms,
            :http_status, :stop_reason, :stop_sequence, :is_error, :error_kind, :error_message,
            :retry_count, :should_retry, :ratelimit_status, :ratelimit_5h_utilization,
            :ratelimit_7d_utilization, :ratelimit_reset_at, :overage_status,
            :system_hash, :system_chars, :system_segments_count, :system_cache_points,
            :tools_hash, :tools_declared_count, :messages_count, :context_chars, :history_prefix_hash,
            :tool_call_count, :tools_called, :user_prompt, :block_counts,
            :new_user_turn, :first_turn_hash
        )
        "#,
        named_params! {
            ":call_id": row.call_id,
            ":parser_version": PARSER_VERSION,
            ":session_id": row.session_id,
            ":trace_id": row.trace_id,
            ":span_id": row.span_id,
            ":parent_span_id": row.parent_span_id,
            ":upstream_trace_id": row.upstream_trace_id,
            ":upstream_span_id": row.upstream_span_id,
            ":request_id": row.request_id,
            ":account_uuid": row.account_uuid,
            ":device_id": row.device_id,
            ":org_id": row.org_id,
            ":agent_id": row.agent_id,
            ":agent_name": row.agent_name,
            ":agent_role": row.agent_role,
            ":billing_variant": row.billing_variant,
            ":provider": row.provider,
            ":framework": row.framework,
            ":client_version": row.client_version,
            ":git_branch": row.git_branch,
            ":project_name": row.project_name,
            ":cwd": row.cwd,
            ":model": row.model,
            ":model_resolved": row.model_resolved,
            ":service_tier": row.service_tier,
            ":is_stream": row.is_stream,
            ":max_tokens": row.max_tokens,
            ":temperature": row.temperature,
            ":thinking_mode": row.thinking_mode,
            ":thinking_budget": row.thinking_budget,
            ":stop_sequences": row.stop_sequences,
            ":context_management": row.context_management,
            ":compaction_requested": row.compaction_requested,
            ":input_tokens": row.input_tokens,
            ":output_tokens": row.output_tokens,
            ":total_tokens": row.total_tokens,
            ":cache_creation_tokens": row.cache_creation_tokens,
            ":cache_read_tokens": row.cache_read_tokens,
            ":cache_creation_5m_tokens": row.cache_creation_5m_tokens,
            ":cache_creation_1h_tokens": row.cache_creation_1h_tokens,
            ":cache_ttl_source": row.cache_ttl_source,
            ":thinking_tokens": row.thinking_tokens,
            ":started_at": row.started_at,
            ":first_token_at": row.first_token_at,
            ":ended_at": row.ended_at,
            ":ttft_ms": row.ttft_ms,
            ":latency_ms": row.latency_ms,
            ":http_status": row.http_status,
            ":stop_reason": row.stop_reason,
            ":stop_sequence": row.stop_sequence,
            ":is_error": row.is_error,
            ":error_kind": row.error_kind,
            ":error_message": row.error_message,
            ":retry_count": row.retry_count,
            ":should_retry": row.should_retry,
            ":ratelimit_status": row.ratelimit_status,
            ":ratelimit_5h_utilization": row.ratelimit_5h_utilization,
            ":ratelimit_7d_utilization": row.ratelimit_7d_utilization,
            ":ratelimit_reset_at": row.ratelimit_reset_at,
            ":overage_status": row.overage_status,
            ":system_hash": row.system_hash,
            ":system_chars": row.system_chars,
            ":system_segments_count": row.system_segments_count,
            ":system_cache_points": row.system_cache_points,
            ":tools_hash": row.tools_hash,
            ":tools_declared_count": row.tools_declared_count,
            ":messages_count": row.messages_count,
            ":context_chars": row.context_chars,
            ":history_prefix_hash": row.history_prefix_hash,
            ":tool_call_count": row.tool_call_count,
            ":tools_called": row.tools_called,
            ":user_prompt": row.user_prompt,
            ":block_counts": row.block_counts,
            ":new_user_turn": row.new_user_turn,
            ":first_turn_hash": row.first_turn_hash,
        },
    )?;
    let generation_id = conn.last_insert_rowid();

    for tool in &derived.tool_calls {
        conn.execute(
            r#"
            INSERT INTO tool_calls (
                generation_id, call_id, session_id, trace_id, seq, tool_use_id, name, server,
                is_mcp, was_declared, input_chars, input_excerpt, result_chars, result_excerpt,
                is_error, status, emitted_at, observed_at, duration_ms, parser_version
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
            )
            "#,
            rusqlite::params![
                generation_id,
                tool.call_id,
                tool.session_id,
                tool.trace_id,
                tool.seq,
                tool.tool_use_id,
                tool.name,
                tool.server,
                tool.is_mcp,
                tool.was_declared,
                tool.input_chars,
                tool.input_excerpt,
                tool.result_chars,
                tool.result_excerpt,
                tool.is_error,
                tool.status,
                tool.emitted_at,
                tool.observed_at,
                tool.duration_ms,
                PARSER_VERSION,
            ],
        )?;
    }

    // A previous failure for this call is resolved once derivation succeeds.
    conn.execute(
        "DELETE FROM derive_failures WHERE call_id = ?1",
        [row.call_id],
    )?;
    Ok(())
}

/// Record that a capture could not be derived. The raw row is already durable;
/// this makes our own failure visible in the product instead of only on stderr.
pub fn record_failure(conn: &Connection, call_id: i64, stage: &str, error: &str, panicked: bool) {
    let result = conn.execute(
        r#"
        INSERT INTO derive_failures (call_id, parser_version, stage, error, panicked, at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(call_id) DO UPDATE SET
            parser_version = excluded.parser_version, stage = excluded.stage,
            error = excluded.error, panicked = excluded.panicked, at = excluded.at
        "#,
        rusqlite::params![
            call_id,
            PARSER_VERSION,
            stage,
            error,
            panicked,
            now_rfc3339()
        ],
    );
    if let Err(err) = result {
        eprintln!("tracer: could not record derive failure for call {call_id}: {err}");
    }
}

/// Derive one capture, catching a parser panic so it can never lose the row.
///
/// Returns the session the capture belongs to, so the caller can refresh just
/// that session's rollup.
pub fn derive_and_write(conn: &Connection, call: &StoredCall) -> Option<String> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| derive_one(call)));
    match outcome {
        Ok(derived) => {
            let session = derived.generation.session_id.clone();
            match write_derived(conn, &derived) {
                Ok(()) => session,
                Err(err) => {
                    record_failure(conn, call.id, "write", &err.to_string(), false);
                    None
                }
            }
        }
        Err(_) => {
            record_failure(conn, call.id, "derive", "parser panicked", true);
            None
        }
    }
}

/// Derive one freshly captured call, then re-assemble and roll up its session.
///
/// Trace membership depends on the calls around it, so the whole session is
/// re-assembled — scoped to one session, this stays cheap as history grows.
pub fn derive_live(conn: &Connection, call: &StoredCall) {
    let Some(session) = derive_and_write(conn, call) else {
        return;
    };
    if let Err(err) = super::trace::assemble_session(conn, &session) {
        eprintln!("tracer: failed to assemble session {session}: {err}");
    }
    if let Err(err) = rollup_sessions(conn, Some(&session)) {
        eprintln!("tracer: failed to refresh session rollup: {err}");
    }
}

/// Drop every derived row. `calls` is untouched.
pub fn clear_derived(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM tool_calls; DELETE FROM generations; DELETE FROM sessions; DELETE FROM derive_failures;",
    )
}

/// Derive every capture that has no current-version derived row.
///
/// Idempotent and safe to run at any time; a rebuild is just this after
/// [`clear_derived`].
pub fn backfill(conn: &Connection) -> Result<BackfillReport> {
    let pending: Vec<i64> = {
        let mut stmt = conn.prepare(
            r#"
            SELECT c.id FROM calls c
            LEFT JOIN generations g ON g.call_id = c.id AND g.parser_version = ?1
            WHERE g.id IS NULL
            ORDER BY c.id
            "#,
        )?;
        let rows = stmt.query_map([PARSER_VERSION], |row| row.get(0))?;
        rows.collect::<Result<_>>()?
    };

    let mut report = BackfillReport::default();
    if pending.is_empty() {
        return Ok(report);
    }

    // Reading the full call set once is cheaper than a per-id round trip, and
    // backfill runs against a bounded local database.
    let calls = list_calls(conn)?;
    let wanted: std::collections::HashSet<i64> = pending.into_iter().collect();
    for call in calls.iter().filter(|call| wanted.contains(&call.id)) {
        let before = failure_count(conn);
        derive_and_write(conn, call);
        if failure_count(conn) > before {
            report.failed += 1;
        } else {
            report.derived += 1;
        }
    }
    super::trace::assemble_all(conn)?;
    rollup_sessions(conn, None)?;
    set_meta(conn, META_PARSER_VERSION, PARSER_VERSION)?;
    Ok(report)
}

/// Open a capture database, migrate it, and bring the derived layer up to date.
///
/// This is the entry point for the `tracer derive` command: it owns the
/// connection so callers never need a SQLite dependency of their own.
pub fn run_backfill(
    db_path: impl AsRef<std::path::Path>,
    rebuild: bool,
) -> anyhow::Result<BackfillReport> {
    let conn = Connection::open(db_path)?;
    crate::store::apply_schema(&conn)?;
    if rebuild {
        clear_derived(&conn)?;
    }
    Ok(backfill(&conn)?)
}

/// Rebuild the derived layer when it was produced by a different parser.
pub fn rebuild_if_stale(conn: &Connection) -> Result<BackfillReport> {
    let stored = get_meta(conn, META_PARSER_VERSION)?;
    if stored.as_deref() != Some(PARSER_VERSION) && stored.is_some() {
        clear_derived(conn)?;
    }
    backfill(conn)
}

/// Recompute session rollups from the derived generations.
///
/// `only` scopes the work to a single session, which is what the live write
/// path uses — recomputing every session on every captured call would make
/// steady-state cost grow with history rather than stay flat.
pub fn rollup_sessions(conn: &Connection, only: Option<&str>) -> Result<()> {
    match only {
        Some(session) => conn.execute("DELETE FROM sessions WHERE session_id = ?1", [session])?,
        None => conn.execute("DELETE FROM sessions", [])?,
    };
    conn.execute(
        r#"
        INSERT INTO sessions (
            session_id, title, project_name, git_branch, cwd, account_uuid, org_id,
            framework, client_version, primary_model, models,
            started_at, ended_at, duration_ms,
            generation_count, tool_call_count, error_count,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, thinking_tokens,
            cost_total_usd, peak_context_tokens, usage_coverage, parser_version
        )
        SELECT
            session_id,
            -- Title comes from the first tool-bearing call: sidechains and quota
            -- probes declare no tools, and their prompts ("quota", a summarize
            -- instruction) are not what the human asked for.
            COALESCE(
              (SELECT user_prompt FROM generations inner_g
                WHERE inner_g.session_id = g.session_id AND inner_g.user_prompt IS NOT NULL
                  AND inner_g.tools_declared_count > 0
                ORDER BY inner_g.started_at LIMIT 1),
              (SELECT user_prompt FROM generations inner_g
                WHERE inner_g.session_id = g.session_id AND inner_g.user_prompt IS NOT NULL
                ORDER BY inner_g.started_at LIMIT 1)
            ),
            MAX(project_name), MAX(git_branch), MAX(cwd), MAX(account_uuid), MAX(org_id),
            MAX(framework), MAX(client_version),
            (SELECT model FROM generations inner_g
              WHERE inner_g.session_id = g.session_id AND inner_g.model IS NOT NULL
              GROUP BY model ORDER BY COUNT(*) DESC LIMIT 1),
            (SELECT json_group_array(DISTINCT model) FROM generations inner_g
              WHERE inner_g.session_id = g.session_id AND inner_g.model IS NOT NULL),
            MIN(started_at), MAX(COALESCE(ended_at, started_at)),
            NULL,
            COUNT(*), SUM(tool_call_count), SUM(is_error),
            SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens),
            SUM(cache_creation_tokens), SUM(thinking_tokens),
            SUM(cost_total_usd), MAX(input_tokens),
            CAST(SUM(input_tokens IS NOT NULL) AS REAL) / COUNT(*),
            ?1
        FROM generations g
        WHERE session_id IS NOT NULL AND (?2 IS NULL OR session_id = ?2)
        GROUP BY session_id
        "#,
        rusqlite::params![PARSER_VERSION, only],
    )?;
    Ok(())
}

fn failure_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM derive_failures", [], |row| row.get(0))
        .unwrap_or(0)
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}
