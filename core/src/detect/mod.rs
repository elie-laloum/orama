//! Explainable detectors over the derived tables.
//!
//! Each rule is a query, not a pass over raw JSON. That is what the derived
//! layer bought: a detector reads indexed columns, so the whole catalogue runs
//! in one sweep instead of re-parsing every stored body per rule.
//!
//! Two principles decide what a rule may claim. **Absent is not healthy** — a
//! missing counter produces a data-quality alert, never a silent pass. And
//! **a condition that is true of every row is not a signal** — the rate-limit
//! and overage rules carry co-conditions for exactly that reason, because the
//! headers they read are set on every response the provider sends.

pub mod policy;

use rusqlite::{Connection, Result};

pub use policy::{SignalPolicy, POLICY_VERSION};

/// One detector. `sql` selects a fixed column set; the prose is attached from
/// these fields so every alert of a kind explains itself identically.
struct Rule {
    id: &'static str,
    category: &'static str,
    confidence: &'static str,
    title: &'static str,
    explanation: &'static str,
    impact: &'static str,
    recommendation: &'static str,
    /// Must select, in order: severity, scope_kind, scope_id, call_id, span_id,
    /// trace_id, session_id, summary, observed, metric_value, threshold,
    /// metric_unit, occurred_at.
    sql: &'static str,
}

/// The catalogue. Thresholds are written as `{placeholders}` and filled from
/// the policy at run time, so no rule carries a literal of its own.
const RULES: &[Rule] = &[
    // ── execution ────────────────────────────────────────────────────────
    Rule {
        id: "execution.http_error",
        category: "execution",
        confidence: "exact",
        title: "Request failed",
        explanation: "The provider returned a non-success HTTP status.",
        impact: "The model did not do the requested work.",
        recommendation: "Inspect the raw response body for the provider's error type.",
        sql: r#"
            SELECT 'error', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'HTTP ' || http_status || ' from the provider.',
                   json_object('http_status', http_status),
                   http_status, NULL, NULL, started_at
              FROM generations
             WHERE http_status >= 400 AND http_status <> 429
        "#,
    },
    Rule {
        id: "execution.rate_limited",
        category: "execution",
        confidence: "exact",
        title: "Rate limited",
        explanation: "The provider returned HTTP 429.",
        impact: "The call was rejected and its work did not happen.",
        recommendation: "Back off and retry; check utilization if this repeats.",
        // A quota probe is a deliberate one-token entitlement check, not work
        // that failed. Every 429 in the captured data is one of these, so an
        // unconditioned rule here would be a false alarm on all of them.
        sql: r#"
            SELECT CASE WHEN agent_role = 'probe' THEN 'info' ELSE 'error' END,
                   'call', span_id, call_id, span_id, trace_id, session_id,
                   CASE WHEN agent_role = 'probe'
                        THEN 'A quota probe was rate limited; this is an entitlement check, not failed work.'
                        ELSE 'The provider rejected this call with HTTP 429.' END,
                   json_object('http_status', 429, 'agent_role', agent_role),
                   429, NULL, NULL, started_at
              FROM generations
             WHERE http_status = 429
        "#,
    },
    Rule {
        id: "execution.transport_error",
        category: "execution",
        confidence: "exact",
        title: "Transport error",
        explanation: "The proxy recorded a transport failure while relaying.",
        impact: "The response is incomplete or absent.",
        recommendation: "Inspect the raw capture and the proxy log.",
        sql: r#"
            SELECT 'error', 'call', span_id, call_id, span_id, trace_id, session_id,
                   COALESCE(error_message, 'A transport error was recorded.'),
                   json_object('error', error_message),
                   NULL, NULL, NULL, started_at
              FROM generations WHERE error_kind = 'transport'
        "#,
    },
    Rule {
        id: "execution.stream_incomplete",
        category: "execution",
        confidence: "exact",
        title: "Stream ended early",
        explanation: "A streamed response has no end timestamp, so it never completed.",
        impact: "The assistant turn is truncated; downstream totals are partial.",
        recommendation: "Inspect the raw SSE capture for where it stopped.",
        sql: r#"
            SELECT 'error', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'The stream was cut off before it finished.',
                   json_object('is_stream', 1), NULL, NULL, NULL, started_at
              FROM generations WHERE error_kind = 'stream_incomplete'
        "#,
    },
    Rule {
        id: "execution.truncated_output",
        category: "execution",
        confidence: "exact",
        title: "Output hit the token cap",
        explanation: "Generation stopped because max_tokens was reached, not because the model finished.",
        impact: "The answer is cut off mid-thought and usually needs a retry.",
        recommendation: "Raise max_tokens, or lower effort so the model spends fewer tokens thinking.",
        sql: r#"
            SELECT 'error', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Generation stopped at the ' || COALESCE(max_tokens, 0) || '-token cap.',
                   json_object('max_tokens', max_tokens, 'output_tokens', output_tokens,
                               'thinking_tokens', thinking_tokens),
                   output_tokens, max_tokens, 'tokens', started_at
              FROM generations WHERE stop_reason = 'max_tokens'
        "#,
    },
    Rule {
        id: "execution.refusal",
        category: "execution",
        confidence: "exact",
        title: "Model declined",
        explanation: "Safety classifiers declined the request.",
        impact: "No answer was produced for this turn.",
        recommendation: "Check the refusal category; consider a fallback model.",
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'The request was declined.', json_object('stop_reason', stop_reason),
                   NULL, NULL, NULL, started_at
              FROM generations WHERE stop_reason = 'refusal'
        "#,
    },
    // ── tooling ──────────────────────────────────────────────────────────
    Rule {
        id: "tooling.tool_error",
        category: "tooling",
        confidence: "exact",
        title: "Tool returned an error",
        explanation: "The tool result is explicitly marked as an error.",
        impact: "The agent may have continued from a failed operation.",
        recommendation: "Open the tool call and read its input and result.",
        sql: r#"
            SELECT 'error', 'tool', t.span_id_key, t.call_id, NULL, t.trace_id, t.session_id,
                   'Tool `' || t.name || '` failed.',
                   json_object('tool', t.name, 'result', t.result_excerpt),
                   NULL, NULL, NULL, COALESCE(t.emitted_at, '')
              FROM (SELECT tc.*, tc.call_id || ':' || tc.seq AS span_id_key FROM tool_calls tc) t
             WHERE t.is_error = 1
        "#,
    },
    Rule {
        id: "tooling.undeclared_tool",
        category: "tooling",
        confidence: "exact",
        title: "Undeclared tool invoked",
        explanation: "A tool was called that the request never declared.",
        impact: "Tool availability and the model's belief about it have diverged.",
        recommendation: "Reconcile the declared tool set with what the harness exposes.",
        sql: r#"
            SELECT 'warning', 'tool', t.call_id || ':' || t.seq, t.call_id, NULL,
                   t.trace_id, t.session_id,
                   'Tool `' || t.name || '` was called but not declared.',
                   json_object('tool', t.name), NULL, NULL, NULL, COALESCE(t.emitted_at, '')
              FROM tool_calls t WHERE t.was_declared = 0
        "#,
    },
    Rule {
        id: "tooling.oversized_result",
        category: "tooling",
        confidence: "approximate",
        title: "Tool result is very large",
        explanation: "A single tool result exceeds the configured size threshold.",
        impact: "One result is consuming a large share of the context window.",
        recommendation: "Narrow the tool's output, or summarize before returning it.",
        sql: r#"
            SELECT 'warning', 'tool', t.call_id || ':' || t.seq, t.call_id, NULL,
                   t.trace_id, t.session_id,
                   'Tool `' || t.name || '` returned about ' || t.result_chars || ' characters.',
                   json_object('tool', t.name, 'result_chars', t.result_chars),
                   t.result_chars, {large_tool_result_chars}, 'chars', COALESCE(t.emitted_at, '')
              FROM tool_calls t WHERE t.result_chars >= {large_tool_result_chars}
        "#,
    },
    // ── performance ──────────────────────────────────────────────────────
    Rule {
        id: "performance.slow_ttft",
        category: "performance",
        confidence: "exact",
        title: "Slow first token",
        explanation: "Time to first token exceeded the policy threshold.",
        impact: "The session feels unresponsive before output starts.",
        recommendation: "Check context size and effort; a large prompt delays the first token.",
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'First token took ' || ttft_ms || ' ms.',
                   json_object('ttft_ms', ttft_ms, 'input_tokens', input_tokens),
                   ttft_ms, {slow_ttft_ms}, 'ms', started_at
              FROM generations WHERE ttft_ms > {slow_ttft_ms}
        "#,
    },
    Rule {
        id: "performance.slow_latency",
        category: "performance",
        confidence: "exact",
        title: "Slow call",
        explanation: "End-to-end latency exceeded the policy threshold.",
        impact: "The turn took a long time to complete.",
        recommendation: "Compare against effort and output size before treating it as a fault.",
        sql: r#"
            SELECT 'info', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'The call took ' || latency_ms || ' ms.',
                   json_object('latency_ms', latency_ms, 'output_tokens', output_tokens),
                   latency_ms, {slow_latency_ms}, 'ms', started_at
              FROM generations WHERE latency_ms > {slow_latency_ms}
        "#,
    },
    // ── cache ────────────────────────────────────────────────────────────
    Rule {
        id: "cache.low_reuse",
        category: "cache",
        confidence: "exact",
        title: "Low cache reuse",
        explanation: "Little of a substantial context was served from cache. The denominator is \
                      the whole context — input plus cache read plus cache write — not the uncached \
                      remainder, which would put reuse above 100%.",
        impact: "Context is being re-billed at full input rate instead of a tenth.",
        recommendation: "Look for a changing prefix: a timestamp or id early in the prompt invalidates everything after it.",
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Only ' || CAST(ROUND(100.0 * reuse) AS INT) || '% of context came from cache.',
                   json_object('cache_read_tokens', cache_read_tokens, 'input_tokens', input_tokens),
                   reuse, {low_cache_reuse_rate}, 'ratio', started_at
              FROM (
                SELECT *, CAST(COALESCE(cache_read_tokens,0) AS REAL)
                          / NULLIF(COALESCE(input_tokens,0) + COALESCE(cache_read_tokens,0)
                                   + COALESCE(cache_creation_tokens,0), 0) AS reuse
                  FROM generations
                 WHERE input_tokens IS NOT NULL
                   AND COALESCE(input_tokens,0) + COALESCE(cache_read_tokens,0)
                       >= {cache_analysis_min_input_tokens}
              )
             WHERE reuse IS NOT NULL AND reuse < {low_cache_reuse_rate}
        "#,
    },
    Rule {
        id: "cache.ttl_overpaid",
        category: "cache",
        confidence: "inferred",
        title: "Paid for a one-hour cache the session outlived",
        explanation: "Cache was written at the one-hour TTL, which costs twice the five-minute rate, \
                      but the whole session finished inside the hour.",
        impact: "The extra write premium bought retention the session never used.",
        recommendation: "Use the shorter TTL for sessions that finish quickly.",
        // Long sessions genuinely benefit from the 1h TTL, so writing at that
        // rate is not itself a finding — it is normal for a long-running agent.
        // The signal is a 1h write in a session that then ended within the hour.
        sql: r#"
            SELECT 'info', 'session', s.session_id, NULL, NULL, NULL, s.session_id,
                   'The session paid the 1h cache rate but finished in ' ||
                     CAST(s.minutes AS INT) || ' minutes.',
                   json_object('session_minutes', s.minutes,
                               'cache_1h_tokens', s.hour_tokens,
                               'cost_cache_write_usd', s.write_usd),
                   s.minutes, 60, 'minutes', s.started_at
              FROM (
                SELECT session_id, MIN(started_at) AS started_at,
                       (julianday(MAX(COALESCE(ended_at, started_at)))
                        - julianday(MIN(started_at))) * 1440.0 AS minutes,
                       SUM(COALESCE(cache_creation_1h_tokens, 0)) AS hour_tokens,
                       SUM(COALESCE(cost_cache_write_usd, 0)) AS write_usd
                  FROM generations WHERE session_id IS NOT NULL GROUP BY session_id
              ) s
             WHERE s.hour_tokens > 0 AND s.minutes < 60
        "#,
    },
    // ── context ──────────────────────────────────────────────────────────
    Rule {
        id: "context.compaction_observed",
        category: "context",
        confidence: "inferred",
        title: "Context dropped sharply",
        explanation: "Input tokens fell steeply from the same agent's previous call, which is what \
                      compaction looks like from the outside.",
        impact: "Earlier conversation no longer influences the model, and the cache prefix is gone.",
        recommendation: "Expected once a long session fills its window; investigate if it happens early.",
        // The client declares `context_management` on essentially every request
        // as a standing configuration, so its presence says nothing. A measured
        // drop is the event.
        sql: r#"
            SELECT 'info', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Context fell from ' || prev || ' to ' || ctx || ' tokens.',
                   json_object('previous_context_tokens', prev, 'context_tokens', ctx),
                   CAST(ctx AS REAL) / prev, {compaction_drop_ratio}, 'ratio', started_at
              FROM (
                SELECT g.*, ctx,
                       LAG(ctx) OVER (
                         PARTITION BY session_id, agent_id ORDER BY started_at, call_id
                       ) AS prev
                  FROM (
                    SELECT *, COALESCE(input_tokens, 0) + COALESCE(cache_read_tokens, 0)
                              + COALESCE(cache_creation_tokens, 0) AS ctx
                      FROM generations
                     WHERE session_id IS NOT NULL AND input_tokens IS NOT NULL
                  ) g
              )
             WHERE prev >= {context_trend_min_tokens}
               AND CAST(ctx AS REAL) / prev <= (1.0 - {compaction_drop_ratio})
        "#,
    },
    Rule {
        id: "context.excessive_growth",
        category: "context",
        confidence: "exact",
        title: "Context grew unusually fast",
        explanation: "Input tokens jumped well beyond the policy ratio from the same agent's previous call.",
        impact: "Cost and first-token latency both scale with context size.",
        recommendation: "Look for a large tool result or pasted block just before this call.",
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Context grew from ' || prev || ' to ' || ctx || ' tokens.',
                   json_object('previous_context_tokens', prev, 'context_tokens', ctx),
                   CAST(ctx AS REAL) / prev, {context_growth_ratio}, 'ratio', started_at
              FROM (
                SELECT g.*, ctx,
                       LAG(ctx) OVER (
                         PARTITION BY session_id, agent_id ORDER BY started_at, call_id
                       ) AS prev
                  FROM (
                    SELECT *, COALESCE(input_tokens, 0) + COALESCE(cache_read_tokens, 0)
                              + COALESCE(cache_creation_tokens, 0) AS ctx
                      FROM generations
                     WHERE session_id IS NOT NULL AND input_tokens IS NOT NULL
                  ) g
              )
             WHERE prev >= {context_trend_min_tokens}
               AND CAST(ctx AS REAL) / prev > {context_growth_ratio}
               AND ctx - prev >= {context_growth_min_tokens}
        "#,
    },
    Rule {
        id: "context.thinking_budget_exhausted",
        category: "context",
        confidence: "exact",
        title: "Thinking budget exhausted",
        explanation: "Thinking tokens reached the budget the request declared.",
        impact: "The model stopped reasoning because it ran out of room, not because it was done.",
        recommendation: "Raise the budget, or raise effort and let the model pace itself.",
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Thinking used ' || thinking_tokens || ' of a ' || thinking_budget || ' token budget.',
                   json_object('thinking_tokens', thinking_tokens, 'thinking_budget', thinking_budget),
                   thinking_tokens, thinking_budget, 'tokens', started_at
              FROM generations
             WHERE thinking_budget > 0 AND thinking_tokens IS NOT NULL
               AND CAST(thinking_tokens AS REAL) / thinking_budget >= {thinking_budget_exhausted_ratio}
        "#,
    },
    // ── rate limits ──────────────────────────────────────────────────────
    Rule {
        id: "rate_limit.utilization_high",
        category: "rate_limit",
        confidence: "exact",
        title: "Approaching the rate limit",
        explanation: "Reported utilization of a rate-limit window crossed the policy threshold.",
        impact: "Further calls risk being rejected until the window resets.",
        recommendation: "Slow down or wait for the reset time.",
        sql: r#"
            SELECT CASE WHEN MAX(COALESCE(ratelimit_5h_utilization,0),
                                 COALESCE(ratelimit_7d_utilization,0)) >= {critical_utilization}
                        THEN 'error' ELSE 'warning' END,
                   'call', span_id, call_id, span_id, trace_id, session_id,
                   'Rate-limit utilization reached ' ||
                     CAST(ROUND(100.0 * MAX(COALESCE(ratelimit_5h_utilization,0),
                                            COALESCE(ratelimit_7d_utilization,0))) AS INT) || '%.',
                   json_object('utilization_5h', ratelimit_5h_utilization,
                               'utilization_7d', ratelimit_7d_utilization,
                               'reset_at', ratelimit_reset_at),
                   MAX(COALESCE(ratelimit_5h_utilization,0), COALESCE(ratelimit_7d_utilization,0)),
                   {high_utilization}, 'ratio', started_at
              FROM generations
             WHERE MAX(COALESCE(ratelimit_5h_utilization,0),
                       COALESCE(ratelimit_7d_utilization,0)) >= {high_utilization}
        "#,
    },
    Rule {
        id: "rate_limit.overage_rejected",
        category: "rate_limit",
        confidence: "exact",
        title: "Overage rejected under load",
        explanation: "Overage was refused while utilization was already high.",
        impact: "There is no headroom left once the limit is reached.",
        recommendation: "Enable overage or reduce concurrency before the window fills.",
        // The overage header reads `rejected` on every captured response, at
        // utilizations as low as 2%. Alone it is noise on 100% of rows, so it
        // only counts as a signal when utilization is genuinely high.
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'Overage is rejected and utilization is already high.',
                   json_object('overage_status', overage_status,
                               'utilization_5h', ratelimit_5h_utilization),
                   ratelimit_5h_utilization, {high_utilization}, 'ratio', started_at
              FROM generations
             WHERE overage_status = 'rejected'
               AND COALESCE(ratelimit_5h_utilization, 0) >= {high_utilization}
        "#,
    },
    // ── cost ─────────────────────────────────────────────────────────────
    Rule {
        id: "cost.expensive_generation",
        category: "cost",
        confidence: "exact",
        title: "Expensive call",
        explanation: "One call cost more than the policy threshold.",
        impact: "A small number of calls can dominate a session's spend.",
        recommendation: "Check the cost breakdown: cache writes often outweigh input and output.",
        sql: r#"
            SELECT 'info', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'This call cost $' || ROUND(cost_total_usd, 4) || '.',
                   json_object('cost_total_usd', cost_total_usd,
                               'cost_cache_write_usd', cost_cache_write_usd,
                               'cost_cache_read_usd', cost_cache_read_usd),
                   cost_total_usd, {expensive_call_usd}, 'usd', started_at
              FROM generations WHERE cost_total_usd > {expensive_call_usd}
        "#,
    },
    Rule {
        id: "cost.session_budget",
        category: "cost",
        confidence: "exact",
        title: "Session over budget",
        explanation: "Total session cost exceeded the policy threshold.",
        impact: "This session is a material share of spend.",
        recommendation: "Break the cost down by agent to see which one is responsible.",
        sql: r#"
            SELECT 'warning', 'session', session_id, NULL, NULL, NULL, session_id,
                   'This session cost $' || ROUND(cost_total_usd, 4) || '.',
                   json_object('cost_total_usd', cost_total_usd,
                               'cache_savings_usd', cache_savings_usd,
                               'generation_count', generation_count),
                   cost_total_usd, {session_budget_usd}, 'usd', started_at
              FROM sessions WHERE cost_total_usd > {session_budget_usd}
        "#,
    },
    Rule {
        id: "cost.sidechain_overhead",
        category: "cost",
        confidence: "exact",
        title: "Background agents dominate cost",
        explanation: "Agents other than the main loop account for a large share of session spend.",
        impact: "Money is going to background work rather than the user's task.",
        recommendation: "Check which agent it is; a classifier re-sending a large system prompt is the usual cause.",
        sql: r#"
            SELECT 'warning', 'session', s.session_id, NULL, NULL, NULL, s.session_id,
                   CAST(ROUND(100.0 * s.side / s.total) AS INT) ||
                     '% of session cost went to non-main agents.',
                   json_object('sidechain_usd', s.side, 'total_usd', s.total),
                   s.side / s.total, {sidechain_cost_share}, 'ratio', s.started_at
              FROM (
                SELECT session_id, MIN(started_at) AS started_at,
                       SUM(CASE WHEN agent_role <> 'main' THEN COALESCE(cost_total_usd,0) ELSE 0 END) AS side,
                       SUM(COALESCE(cost_total_usd, 0)) AS total
                  FROM generations WHERE session_id IS NOT NULL GROUP BY session_id
              ) s
             WHERE s.total > 0 AND s.side / s.total >= {sidechain_cost_share}
        "#,
    },
    // ── data quality ─────────────────────────────────────────────────────
    Rule {
        id: "data_quality.response_body_missing",
        category: "data_quality",
        confidence: "unavailable",
        title: "Response was never captured",
        explanation: "A successful non-streaming call has no stored response body.",
        impact: "Tokens, cost and outcome are unknown for this call — not zero, unknown.",
        recommendation: "Captures taken before response-body recording landed cannot be recovered.",
        // Scoped to model calls: a health probe with an empty body is honestly
        // recorded as having no response, but it is not a gap worth reporting.
        sql: r#"
            SELECT 'warning', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'No response body was stored, so usage and cost are unknown.',
                   json_object('http_status', http_status, 'model', model),
                   NULL, NULL, NULL, started_at
              FROM generations
             WHERE error_kind = 'body_missing' AND model IS NOT NULL
        "#,
    },
    Rule {
        id: "data_quality.pricing_unknown",
        category: "data_quality",
        confidence: "unavailable",
        title: "Model is not priced",
        explanation: "This model is absent from the pricing table.",
        impact: "Its spend is missing from every cost total.",
        recommendation: "Add the model's rates to the pricing table.",
        sql: r#"
            SELECT 'info', 'call', span_id, call_id, span_id, trace_id, session_id,
                   'No rates are known for `' || model || '`.',
                   json_object('model', model), NULL, NULL, NULL, started_at
              FROM generations
             WHERE model IS NOT NULL AND cost_total_usd IS NULL AND input_tokens IS NOT NULL
        "#,
    },
    Rule {
        id: "data_quality.parser_failed",
        category: "data_quality",
        confidence: "exact",
        title: "Derivation failed",
        explanation: "The parser could not derive this capture. This is a defect in the tool, not the traffic.",
        impact: "The call is missing from every derived view.",
        recommendation: "Report the failure with the call id; the raw capture is intact.",
        sql: r#"
            SELECT 'critical', 'call', CAST(call_id AS TEXT), call_id, NULL, NULL, NULL,
                   'Deriving this capture failed at the ' || stage || ' stage.',
                   json_object('stage', stage, 'error', error, 'panicked', panicked),
                   NULL, NULL, NULL, at
              FROM derive_failures
        "#,
    },
];

/// Recompute every alert from the derived tables.
///
/// Wholesale rather than incremental: alerts are a pure function of the derived
/// rows and the policy, so rebuilding is both correct and cheap.
pub fn evaluate(conn: &Connection, policy: &SignalPolicy) -> Result<usize> {
    conn.execute("DELETE FROM alerts", [])?;
    let mut total = 0;
    for rule in RULES {
        total += run(conn, rule, policy)?;
    }
    Ok(total)
}

fn run(conn: &Connection, rule: &Rule, policy: &SignalPolicy) -> Result<usize> {
    // A CTE names the rule body's columns — SQLite has no Postgres-style
    // `AS alias(col, ...)` on a subquery. `scope_id` falls back to the call id
    // so a generation that never got a span (traffic outside any session) still
    // produces a usable, unique key rather than a null one.
    let sql = format!(
        r#"
        WITH r(severity, scope_kind, scope_id, call_id, span_id, trace_id, session_id,
               summary, observed, metric_value, threshold, metric_unit, occurred_at) AS (
            {body}
        )
        INSERT INTO alerts (
            dedup_key, rule_id, category, severity, confidence, scope_kind, scope_id,
            call_id, span_id, trace_id, session_id, title, summary, explanation,
            impact, recommendation, observed, metric_value, threshold, metric_unit,
            occurred_at, policy_version, parser_version
        )
        SELECT '{rule_id}:' || r.scope_kind || ':' || COALESCE(r.scope_id, 'call-' || r.call_id),
               '{rule_id}', '{category}', r.severity, '{confidence}',
               r.scope_kind, COALESCE(r.scope_id, 'call-' || r.call_id),
               r.call_id, r.span_id, r.trace_id, r.session_id,
               '{title}', r.summary, '{explanation}', '{impact}', '{recommendation}',
               r.observed, r.metric_value, r.threshold, r.metric_unit,
               r.occurred_at, '{policy_version}', '{parser_version}'
          FROM r
         WHERE true
        ON CONFLICT(dedup_key) DO UPDATE SET occurrences = occurrences + 1
        "#,
        rule_id = rule.id,
        category = rule.category,
        confidence = rule.confidence,
        title = escape(rule.title),
        explanation = escape(rule.explanation),
        impact = escape(rule.impact),
        recommendation = escape(rule.recommendation),
        policy_version = policy.version,
        parser_version = crate::derive::PARSER_VERSION,
        body = fill(rule.sql, policy),
    );
    conn.execute(&sql, [])
}

/// Substitute policy thresholds into a rule body. Values are numbers from a
/// trusted struct, never user input.
fn fill(sql: &str, policy: &SignalPolicy) -> String {
    sql.replace("{slow_ttft_ms}", &policy.slow_ttft_ms.to_string())
        .replace("{slow_latency_ms}", &policy.slow_latency_ms.to_string())
        .replace(
            "{low_cache_reuse_rate}",
            &policy.low_cache_reuse_rate.to_string(),
        )
        .replace(
            "{cache_analysis_min_input_tokens}",
            &policy.cache_analysis_min_input_tokens.to_string(),
        )
        .replace("{large_block_chars}", &policy.large_block_chars.to_string())
        .replace(
            "{large_tool_result_chars}",
            &policy.large_tool_result_chars.to_string(),
        )
        .replace(
            "{context_growth_ratio}",
            &policy.context_growth_ratio.to_string(),
        )
        .replace(
            "{context_growth_min_tokens}",
            &policy.context_growth_min_tokens.to_string(),
        )
        .replace(
            "{compaction_drop_ratio}",
            &policy.compaction_drop_ratio.to_string(),
        )
        .replace(
            "{context_trend_min_tokens}",
            &policy.context_trend_min_tokens.to_string(),
        )
        .replace(
            "{expensive_call_usd}",
            &policy.expensive_call_usd.to_string(),
        )
        .replace(
            "{session_budget_usd}",
            &policy.session_budget_usd.to_string(),
        )
        .replace(
            "{sidechain_cost_share}",
            &policy.sidechain_cost_share.to_string(),
        )
        .replace("{high_utilization}", &policy.high_utilization.to_string())
        .replace(
            "{critical_utilization}",
            &policy.critical_utilization.to_string(),
        )
        .replace(
            "{thinking_budget_exhausted_ratio}",
            &policy.thinking_budget_exhausted_ratio.to_string(),
        )
}

/// Quote a static string for inline use in SQL.
fn escape(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_threshold_placeholder_resolves() {
        let policy = SignalPolicy::default();
        for rule in RULES {
            let filled = fill(rule.sql, &policy);
            assert!(
                !filled.contains('{'),
                "rule {} has an unresolved threshold placeholder",
                rule.id
            );
        }
    }

    #[test]
    fn rule_ids_are_unique_and_namespaced() {
        let mut seen = std::collections::HashSet::new();
        for rule in RULES {
            assert!(seen.insert(rule.id), "duplicate rule id {}", rule.id);
            assert!(
                rule.id.contains('.'),
                "rule {} should be namespaced by category",
                rule.id
            );
        }
    }

    #[test]
    fn every_rule_explains_itself() {
        for rule in RULES {
            for (field, value) in [
                ("title", rule.title),
                ("explanation", rule.explanation),
                ("impact", rule.impact),
                ("recommendation", rule.recommendation),
            ] {
                assert!(!value.is_empty(), "rule {} has an empty {field}", rule.id);
            }
        }
    }
}
