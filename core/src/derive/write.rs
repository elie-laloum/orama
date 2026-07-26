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

/// Key under which the rates the cost columns were computed at are kept.
///
/// Distinct from the parser version because it moves for a different reason and
/// costs far less to fix — see [`rebuild_if_stale`].
const META_PRICING_VERSION: &str = "pricing_version";

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
            cost_input_usd, cost_output_usd, cost_cache_write_usd, cost_cache_read_usd,
            cost_total_usd, cost_uncached_equiv_usd, pricing_model_id, pricing_version,
            started_at, first_token_at, ended_at, ttft_ms, latency_ms,
            http_status, stop_reason, stop_sequence, is_error, error_kind, error_message,
            retry_count, should_retry, ratelimit_status, ratelimit_5h_utilization,
            ratelimit_7d_utilization, ratelimit_reset_at, overage_status,
            system_hash, system_chars, system_segments_count, system_cache_points,
            tools_hash, tools_declared_count, tools_chars,
            messages_count, context_chars, history_prefix_hash,
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
            :cost_input_usd, :cost_output_usd, :cost_cache_write_usd, :cost_cache_read_usd,
            :cost_total_usd, :cost_uncached_equiv_usd, :pricing_model_id, :pricing_version,
            :started_at, :first_token_at, :ended_at, :ttft_ms, :latency_ms,
            :http_status, :stop_reason, :stop_sequence, :is_error, :error_kind, :error_message,
            :retry_count, :should_retry, :ratelimit_status, :ratelimit_5h_utilization,
            :ratelimit_7d_utilization, :ratelimit_reset_at, :overage_status,
            :system_hash, :system_chars, :system_segments_count, :system_cache_points,
            :tools_hash, :tools_declared_count, :tools_chars,
            :messages_count, :context_chars, :history_prefix_hash,
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
            ":cost_input_usd": row.cost_input_usd,
            ":cost_output_usd": row.cost_output_usd,
            ":cost_cache_write_usd": row.cost_cache_write_usd,
            ":cost_cache_read_usd": row.cost_cache_read_usd,
            ":cost_total_usd": row.cost_total_usd,
            ":cost_uncached_equiv_usd": row.cost_uncached_equiv_usd,
            ":pricing_model_id": row.pricing_model_id,
            ":pricing_version": row.pricing_version,
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
            ":tools_chars": row.tools_chars,
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

    write_harness(conn, derived)?;

    // A previous failure for this call is resolved once derivation succeeds.
    conn.execute(
        "DELETE FROM derive_failures WHERE call_id = ?1",
        [row.call_id],
    )?;
    Ok(())
}

/// Store the system prompt and tool set this call declared.
///
/// `INSERT OR IGNORE` rather than upsert: the primary key is a fingerprint of
/// the content, so a row that already exists is by construction byte-identical
/// and re-writing it would be pure work. These rows are shared across calls and
/// so are never deleted per call — unlike everything else in `write_derived`.
fn write_harness(conn: &Connection, derived: &Derived) -> Result<()> {
    if let Some(prompt) = &derived.system_prompt {
        conn.execute(
            r#"
            INSERT OR IGNORE INTO system_prompts (
                system_hash, segments, total_chars, segment_count, cache_points, parser_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            rusqlite::params![
                prompt.system_hash,
                serde_json::to_string(&prompt.segments).unwrap_or_else(|_| "[]".to_owned()),
                prompt.total_chars,
                prompt.segment_count,
                prompt.cache_points,
                PARSER_VERSION,
            ],
        )?;
    }

    let Some(set) = &derived.tool_set else {
        return Ok(());
    };
    // The set row and its members go in together. Elsewhere a partial write is
    // repaired by the next re-derive, which deletes by `call_id` first; here the
    // presence of the set row is what suppresses re-writing the members, so a
    // half-written set would stay half-written for good.
    let tx = conn.unchecked_transaction()?;
    let inserted = tx.execute(
        r#"
        INSERT OR IGNORE INTO tool_sets (
            tools_hash, tool_count, total_chars, mcp_count, parser_version
        ) VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        rusqlite::params![
            set.tools_hash,
            set.tool_count,
            set.total_chars,
            set.mcp_count,
            PARSER_VERSION,
        ],
    )?;
    // The member rows are only written alongside a freshly inserted set. If the
    // set was already present its members are already there, and re-inserting
    // 121 declarations per call is exactly the cost this table exists to avoid.
    if inserted == 0 {
        return Ok(());
    }
    for tool in &set.tools {
        tx.execute(
            r#"
            INSERT OR IGNORE INTO tool_schemas (
                tools_hash, seq, name, server, is_mcp, description, input_schema, chars
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            rusqlite::params![
                set.tools_hash,
                tool.seq,
                tool.name,
                tool.server,
                tool.is_mcp,
                tool.description,
                tool.input_schema,
                tool.chars,
            ],
        )?;
    }
    tx.commit()
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
        eprintln!("orama: could not record derive failure for call {call_id}: {err}");
    }
}

/// Derive one capture, catching a parser panic so it can never lose the row.
///
/// Returns the session the capture belongs to, so the caller can refresh just
/// that session's rollup.
pub fn derive_and_write(conn: &Connection, call: &StoredCall) -> Option<String> {
    // Not every captured round trip is a generation. Skipping is not a failure
    // and records nothing — the raw call stays exactly where it was.
    if !super::is_inference_call(call) {
        return None;
    }
    // Every path that derives arrives here — live capture, backfill, rebuild,
    // and the `derive` command — so this is where rates are guaranteed to be in
    // force. `derive_one` is pure and takes no connection, and without this a
    // caller that never opened a server would silently derive everything
    // unpriced. After the first call it is one atomic read.
    crate::catalog::ensure_loaded(conn);

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
    // The raw row is already committed by the time this runs.
    crate::api::events::publish("capture.started", call.id, None, None);
    let Some(session) = derive_and_write(conn, call) else {
        crate::api::events::publish("derive.failed", call.id, None, None);
        return;
    };
    if let Err(err) = super::trace::assemble_session(conn, &session) {
        eprintln!("orama: failed to assemble session {session}: {err}");
    }
    if let Err(err) = rollup_sessions(conn, Some(&session)) {
        eprintln!("orama: failed to refresh session rollup: {err}");
    }
    if let Err(err) = crate::detect::evaluate(conn, &crate::detect::SignalPolicy::default()) {
        eprintln!("orama: failed to evaluate detectors: {err}");
    }
    // Emitted last, so a client that reacts to it finds every derived table
    // already consistent.
    let trace = conn
        .query_row(
            "SELECT trace_id FROM generations WHERE call_id = ?1",
            [call.id],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap_or(None);
    crate::api::events::publish("generation.derived", call.id, Some(session), trace);
}

/// Drop every derived row. `calls` is untouched.
pub fn clear_derived(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM alerts; DELETE FROM tool_calls; DELETE FROM generations; DELETE FROM sessions; \
         DELETE FROM tool_schemas; DELETE FROM tool_sets; DELETE FROM system_prompts; \
         DELETE FROM derive_failures;",
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
        if !super::is_inference_call(call) {
            report.skipped += 1;
            continue;
        }
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
    // Alerts are a pure function of the derived rows and the policy, so they
    // are recomputed wholesale once everything else is in place.
    crate::detect::evaluate(conn, &crate::detect::SignalPolicy::default())?;
    set_meta(conn, META_PARSER_VERSION, PARSER_VERSION)?;
    set_meta(
        conn,
        META_PRICING_VERSION,
        &crate::pricing::pricing_version(),
    )?;
    Ok(report)
}

/// Re-price every derived generation at the rates now in force.
///
/// Cost is the one derived field that can go stale without any capture changing:
/// rates live in a catalogue that refreshes on its own. Everything pricing needs
/// — provider, model, capture time, every token counter — is already a column on
/// `generations`, so this is a read and an update. It deliberately does not
/// re-parse the raw bodies: request bodies are the bulk of the database, and a
/// price change is no reason to read them again.
///
/// Returns how many rows changed hands, priced or unpriced.
pub fn reprice(conn: &Connection) -> Result<usize> {
    crate::catalog::ensure_loaded(conn);

    struct Row {
        id: i64,
        provider: String,
        model: Option<String>,
        started_at: String,
        tokens: crate::pricing::Tokens,
    }

    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, provider, COALESCE(model_resolved, model), started_at,
                   input_tokens, output_tokens, cache_read_tokens,
                   cache_creation_5m_tokens, cache_creation_1h_tokens, cache_creation_tokens
              FROM generations
            "#,
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok(Row {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                tokens: crate::pricing::Tokens {
                    input: row.get(4)?,
                    output: row.get(5)?,
                    cache_read: row.get(6)?,
                    cache_creation_5m: row.get(7)?,
                    cache_creation_1h: row.get(8)?,
                    cache_creation_total: row.get(9)?,
                },
            })
        })?;
        mapped.collect::<Result<_>>()?
    };

    let version = crate::pricing::pricing_version();
    let mut updated = 0usize;
    {
        let mut stmt = conn.prepare(
            r#"
            UPDATE generations
               SET cost_input_usd = ?2, cost_output_usd = ?3, cost_cache_write_usd = ?4,
                   cost_cache_read_usd = ?5, cost_total_usd = ?6,
                   cost_uncached_equiv_usd = ?7, pricing_model_id = ?8, pricing_version = ?9
             WHERE id = ?1
            "#,
        )?;
        for row in &rows {
            let cost = crate::pricing::price(
                &row.provider,
                row.model.as_deref(),
                &row.started_at,
                &row.tokens,
            );
            // An unpriceable call is written back as unpriced. Leaving a stale
            // figure in place would be worse than reporting nothing: it would
            // be a number nobody can reproduce.
            match cost {
                Some(cost) => stmt.execute(rusqlite::params![
                    row.id,
                    cost.input_usd,
                    cost.output_usd,
                    cost.cache_write_usd,
                    cost.cache_read_usd,
                    cost.total_usd,
                    cost.uncached_equivalent_usd,
                    cost.model_id,
                    version,
                ])?,
                None => stmt.execute(rusqlite::params![
                    row.id,
                    None::<f64>,
                    None::<f64>,
                    None::<f64>,
                    None::<f64>,
                    None::<f64>,
                    None::<f64>,
                    None::<String>,
                    version,
                ])?,
            };
            updated += 1;
        }
    }

    // Session totals sum generation costs, and both the cost rules and
    // `data_quality.pricing_unknown` read them, so neither can be left behind.
    rollup_sessions(conn, None)?;
    crate::detect::evaluate(conn, &crate::detect::SignalPolicy::default())?;
    set_meta(conn, META_PRICING_VERSION, &version)?;
    Ok(updated)
}

/// Open a capture database, migrate it, and bring the derived layer up to date.
///
/// This is the entry point for the `orama derive` command: it owns the
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

/// Bring the derived layer up to the running parser and the current rates.
///
/// Two different kinds of staleness, handled differently on purpose. A parser
/// change means the derived rows themselves are wrong, so they are discarded and
/// rebuilt from the captures. A price change means only the cost columns are
/// wrong, and re-parsing every request body to fix a multiplication would be
/// wasteful — the catalogue refreshes on its own schedule, so this happens far
/// more often than a parser bump.
pub fn rebuild_if_stale(conn: &Connection) -> Result<BackfillReport> {
    let stored = get_meta(conn, META_PARSER_VERSION)?;
    let parser_moved = stored.as_deref() != Some(PARSER_VERSION) && stored.is_some();
    if parser_moved {
        clear_derived(conn)?;
    }
    let report = backfill(conn)?;

    // A rebuild just priced everything from scratch, so there is nothing to
    // correct; only check the rates when the rows survived.
    if !parser_moved {
        let priced_at = get_meta(conn, META_PRICING_VERSION)?;
        let current = crate::pricing::pricing_version();
        if priced_at.as_deref() != Some(current.as_str()) && priced_at.is_some() {
            let updated = reprice(conn)?;
            if updated > 0 {
                eprintln!("orama: re-priced {updated} generation(s) at {current}");
            }
        }
    }
    Ok(report)
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
            cost_total_usd, cache_savings_usd, peak_context_tokens, usage_coverage, parser_version
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
            SUM(cost_total_usd),
            -- What caching saved: never negative, since a write-heavy session
            -- can cost more than its uncached equivalent.
            MAX(0, SUM(COALESCE(cost_uncached_equiv_usd, 0)) - SUM(COALESCE(cost_total_usd, 0))),
            MAX(input_tokens),
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
