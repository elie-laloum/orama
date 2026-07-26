//! The derived layer: materialized, queryable projections of raw captures.
//!
//! `calls` is append-only truth. Everything produced here is a pure function of
//! a raw row and [`PARSER_VERSION`], so the derived tables can be dropped and
//! rebuilt at any time. That property is what lets the API answer with indexed
//! SQL instead of re-parsing every stored body on every request.

pub mod extract;
pub mod model;
pub mod trace;
pub mod write;

use serde_json::Value;

use crate::parse::{
    model::Provider,
    model::{BlockKind, NormalizedCall, Origin, Role},
    parse_call,
};
use crate::store::StoredCall;

pub use model::{Derived, GenerationRow, ToolCallRow};

/// Bumping this invalidates every derived row and triggers a rebuild. Raw
/// captures are never touched by the rebuild.
pub const PARSER_VERSION: &str = "2026-07-26.1";

/// Longest excerpt kept for a tool input or result. Full content stays in
/// `calls`; derived rows exist to be scanned, not to duplicate 260 KB bodies.
const EXCERPT_CHARS: usize = 400;

/// Longest user-prompt excerpt stored on a generation.
const PROMPT_CHARS: usize = 512;

/// Build the derived rows for one raw capture.
pub fn derive_one(call: &StoredCall) -> Derived {
    let normalized = parse_call(call);
    let record = &call.record;
    let body = record.request_body.as_ref();
    let headers = &record.request_headers;
    let response_headers = record.response_headers.as_ref();

    let mut generation = GenerationRow {
        call_id: call.id,
        started_at: record.timestamp_start.clone(),
        first_token_at: record.timestamp_first_chunk.clone(),
        ended_at: record.timestamp_end.clone(),
        ttft_ms: extract::millis_between(
            &record.timestamp_start,
            record.timestamp_first_chunk.as_deref(),
        ),
        latency_ms: extract::millis_between(
            &record.timestamp_start,
            record.timestamp_end.as_deref(),
        ),
        http_status: record.response_status,
        model: normalized.model.clone(),
        is_stream: record.response_raw_sse.is_some(),
        ..Default::default()
    };

    // Provider and framework are orthogonal axes; the normalized enum still
    // conflates them, so map it here rather than leaking that into the schema.
    match normalized.provider {
        Provider::ClaudeCode => {
            generation.provider = "anthropic".to_owned();
            generation.framework = Some("claude-code".to_owned());
        }
        Provider::Unknown => generation.provider = "unknown".to_owned(),
    }

    identity(&mut generation, &normalized, headers, body);
    request_params(&mut generation, body);
    system_shape(&mut generation, &normalized);
    thread_shape(&mut generation, &normalized);
    response_fields(
        &mut generation,
        record
            .response_reconstructed
            .as_ref()
            .or(record.response_body.as_ref()),
    );
    cache_ttl_fallback(&mut generation, body);
    outcome(&mut generation, &normalized, headers, response_headers);
    cost(&mut generation);

    let tool_calls = tool_calls(&normalized, &generation);
    generation.tool_call_count = tool_calls.len() as i64;
    generation.tools_called = (!tool_calls.is_empty()).then(|| {
        let mut names: Vec<&str> = tool_calls.iter().map(|tool| tool.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        Value::from(names).to_string()
    });

    Derived {
        generation,
        tool_calls,
    }
}

/// Session, account and device identity, plus upstream correlation ids.
fn identity(
    row: &mut GenerationRow,
    normalized: &NormalizedCall,
    headers: &Value,
    body: Option<&Value>,
) {
    let (metadata_session, account_uuid, device_id) = extract::identity(body);
    row.session_id = normalized.session_key.clone().or(metadata_session);
    row.account_uuid = account_uuid;
    row.device_id = device_id;
    row.client_version = extract::header(headers, "x-stainless-package-version").map(str::to_owned);
}

/// Sampling and budget parameters declared on the request.
fn request_params(row: &mut GenerationRow, body: Option<&Value>) {
    let Some(body) = body else { return };
    row.max_tokens = body.get("max_tokens").and_then(Value::as_i64);
    row.temperature = body.get("temperature").and_then(Value::as_f64);
    row.thinking_mode = body
        .pointer("/thinking/type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    row.thinking_budget = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_i64);
    row.stop_sequences = body
        .get("stop_sequences")
        .filter(|value| !value.as_array().is_some_and(|items| items.is_empty()))
        .map(Value::to_string);
    if let Some(management) = body.get("context_management") {
        // The client telling us it dropped context is exact evidence, unlike a
        // size-drop heuristic inferred after the fact.
        row.compaction_requested = management
            .get("edits")
            .and_then(Value::as_array)
            .is_some_and(|edits| !edits.is_empty());
        row.context_management = Some(management.to_string());
    }
}

/// System-prompt fingerprint plus the environment Claude Code injects into it.
fn system_shape(row: &mut GenerationRow, normalized: &NormalizedCall) {
    let segments = &normalized.system;
    if segments.is_empty() {
        return;
    }
    row.billing_variant = extract::billing_variant(&segments[0].text);

    // The billing header carries a per-agent variant suffix that changes between
    // calls, so it is excluded from the fingerprint used for drift detection.
    let texts: Vec<&str> = segments
        .iter()
        .filter(|segment| extract::billing_variant(&segment.text).is_none())
        .map(|segment| segment.text.as_str())
        .collect();
    row.system_hash = (!texts.is_empty()).then(|| extract::fingerprint(&texts));
    row.system_chars = Some(
        segments
            .iter()
            .map(|segment| segment.approx_size.chars as i64)
            .sum(),
    );
    row.system_segments_count = Some(segments.len() as i64);
    row.system_cache_points = Some(
        segments
            .iter()
            .filter(|segment| segment.cache_control)
            .count() as i64,
    );

    let joined: String = texts.join("\n");
    row.git_branch = extract::line_value(&joined, "Current branch: ").map(str::to_owned);
    row.cwd = extract::line_value(&joined, "working directory: ").map(str::to_owned);
    row.project_name = row
        .cwd
        .as_deref()
        .and_then(|path| path.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_owned);

    let names: Vec<&str> = normalized
        .declared_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    row.tools_declared_count = Some(names.len() as i64);
    if !names.is_empty() {
        let mut sorted = names;
        sorted.sort_unstable();
        row.tools_hash = Some(extract::fingerprint(&sorted));
    }
}

/// Conversation size and the prefix fingerprint used for chain detection.
fn thread_shape(row: &mut GenerationRow, normalized: &NormalizedCall) {
    let history: Vec<&_> = normalized
        .thread
        .iter()
        .filter(|turn| matches!(turn.origin, Origin::History))
        .collect();
    row.messages_count = Some(history.len() as i64);
    row.context_chars = Some(
        normalized
            .thread
            .iter()
            .flat_map(|turn| &turn.blocks)
            .map(|block| block.approx_size.chars as i64)
            .sum(),
    );

    // A call opens a new user turn when its last history entry is something the
    // human wrote. Continuation calls inside a turn end with a tool-result turn
    // instead, which is what makes this the trace boundary signal.
    row.new_user_turn = history.last().is_some_and(|turn| {
        matches!(turn.role, Role::User)
            && turn
                .blocks
                .iter()
                .any(|block| matches!(block.kind, BlockKind::Text))
    });

    // A rolling fingerprint over history turns. Comparing two of these is a
    // string compare, replacing a debug-format of the entire conversation.
    let mut rolling = String::new();
    for turn in &history {
        let blocks: Vec<String> = turn
            .blocks
            .iter()
            .map(|block| {
                format!(
                    "{:?}:{}",
                    block.kind,
                    block.content.as_deref().unwrap_or_default()
                )
            })
            .collect();
        let refs: Vec<&str> = blocks.iter().map(String::as_str).collect();
        rolling =
            extract::fingerprint(&[&rolling, &format!("{:?}", turn.role), &refs.join("\u{1f}")]);
        // The hash after the first turn identifies the conversation root, so a
        // restart or a branch from scratch is visible without storing content.
        if row.first_turn_hash.is_none() {
            row.first_turn_hash = Some(rolling.clone());
        }
    }
    row.history_prefix_hash = (!history.is_empty()).then_some(rolling);

    let counts = &normalized.intra.block_counts;
    row.block_counts = serde_json::to_string(counts).ok();
    row.user_prompt = first_user_prompt(normalized);
}

/// The first genuinely human message, skipping harness-injected content.
fn first_user_prompt(normalized: &NormalizedCall) -> Option<String> {
    normalized
        .thread
        .iter()
        .filter(|turn| matches!(turn.role, Role::User))
        .flat_map(|turn| &turn.blocks)
        .filter(|block| matches!(block.kind, BlockKind::Text) && block.content_tag.is_none())
        .filter_map(|block| block.content.as_deref())
        .map(str::trim)
        .find(|text| !text.is_empty() && !is_injected(text))
        .map(|text| extract::excerpt(text, PROMPT_CHARS))
}

/// Harness-injected wrappers that look like user text but are not the user.
fn is_injected(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "<system-reminder",
        "<local-command-",
        "<environment_context",
        "<task-notification",
        "<command-",
    ];
    MARKERS.iter().any(|marker| text.starts_with(marker))
}

/// Usage, stop reason and the served model, from the assembled response.
fn response_fields(row: &mut GenerationRow, message: Option<&Value>) {
    let Some(message) = message else { return };
    if message.get("type").and_then(Value::as_str) == Some("error") {
        return;
    }
    let usage = extract::usage(message);
    row.input_tokens = usage.input;
    row.output_tokens = usage.output;
    // Cache counters describe portions of input and are never re-added here.
    row.total_tokens = usage.input.zip(usage.output).map(|(a, b)| a + b);
    row.cache_creation_tokens = usage.cache_creation;
    row.cache_read_tokens = usage.cache_read;
    row.cache_creation_5m_tokens = usage.cache_5m;
    row.cache_creation_1h_tokens = usage.cache_1h;
    row.thinking_tokens = usage.thinking;
    row.service_tier = usage.service_tier;
    row.cache_ttl_source = usage.ttl_source;
    row.model_resolved = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    row.stop_reason = message
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(str::to_owned);
    row.stop_sequence = message
        .get("stop_sequence")
        .and_then(Value::as_str)
        .map(str::to_owned);
}

/// When the response omitted the TTL split, attribute cache writes to the TTL
/// the request asked for rather than silently assuming the cheaper one.
fn cache_ttl_fallback(row: &mut GenerationRow, body: Option<&Value>) {
    if row.cache_ttl_source.is_some() {
        return;
    }
    let Some(written) = row.cache_creation_tokens.filter(|tokens| *tokens > 0) else {
        return;
    };
    match extract::requested_cache_ttl(body).as_deref() {
        Some("1h") => {
            row.cache_creation_1h_tokens = Some(written);
            row.cache_creation_5m_tokens = Some(0);
            row.cache_ttl_source = Some("request".to_owned());
        }
        Some(_) => {
            row.cache_creation_5m_tokens = Some(written);
            row.cache_creation_1h_tokens = Some(0);
            row.cache_ttl_source = Some("request".to_owned());
        }
        None => row.cache_ttl_source = Some("assumed".to_owned()),
    }
}

/// Failure classification and the rate-limit telemetry the provider returns.
fn outcome(
    row: &mut GenerationRow,
    normalized: &NormalizedCall,
    headers: &Value,
    response_headers: Option<&Value>,
) {
    row.error_message = normalized.error.clone();
    row.retry_count =
        extract::header(headers, "x-stainless-retry-count").and_then(|value| value.parse().ok());

    if let Some(response_headers) = response_headers {
        let text = |name: &str| extract::header(response_headers, name).map(str::to_owned);
        let ratio = |name: &str| {
            extract::header(response_headers, name).and_then(|value| value.parse::<f64>().ok())
        };
        row.request_id = text("request-id");
        row.org_id = text("anthropic-organization-id");
        row.should_retry =
            extract::header(response_headers, "x-should-retry").map(|value| value == "true");
        row.ratelimit_status = text("anthropic-ratelimit-unified-status");
        row.ratelimit_5h_utilization = ratio("anthropic-ratelimit-unified-5h-utilization");
        row.ratelimit_7d_utilization = ratio("anthropic-ratelimit-unified-7d-utilization");
        row.overage_status = text("anthropic-ratelimit-unified-overage-status");
        row.ratelimit_reset_at =
            extract::header(response_headers, "anthropic-ratelimit-unified-reset")
                .and_then(|value| value.parse::<i64>().ok())
                .and_then(|epoch| {
                    time::OffsetDateTime::from_unix_timestamp(epoch)
                        .ok()?
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok()
                });
        if let Some((trace, span)) =
            extract::header(response_headers, "traceresponse").and_then(extract::trace_context)
        {
            row.upstream_trace_id = Some(trace);
            row.upstream_span_id = Some(span);
        }
    }

    // Ordered most- to least-specific so the recorded kind is the actionable one.
    row.error_kind = if normalized.error.is_some() {
        Some("transport".to_owned())
    } else if row
        .http_status
        .is_some_and(|status| !(200..300).contains(&status))
    {
        Some("http".to_owned())
    } else if row.is_stream && row.ended_at.is_none() {
        Some("stream_incomplete".to_owned())
    } else if row.input_tokens.is_none() && row.http_status == Some(200) {
        // A successful call with no usage means the response was never captured.
        Some("body_missing".to_owned())
    } else {
        None
    };
    row.is_error = matches!(
        row.error_kind.as_deref(),
        Some("transport" | "http" | "stream_incomplete")
    );
}

/// Attribute cost, priced at the rates in effect when the call was captured.
///
/// The served model is preferred over the requested one: a fallback or an alias
/// resolution means the bill follows what actually ran.
fn cost(row: &mut GenerationRow) {
    let model = row.model_resolved.as_deref().or(row.model.as_deref());
    let tokens = crate::pricing::Tokens {
        input: row.input_tokens,
        output: row.output_tokens,
        cache_read: row.cache_read_tokens,
        cache_creation_5m: row.cache_creation_5m_tokens,
        cache_creation_1h: row.cache_creation_1h_tokens,
        cache_creation_total: row.cache_creation_tokens,
    };
    let Some(cost) = crate::pricing::price(model, &row.started_at, &tokens) else {
        return;
    };
    row.cost_input_usd = Some(cost.input_usd);
    row.cost_output_usd = Some(cost.output_usd);
    row.cost_cache_write_usd = Some(cost.cache_write_usd);
    row.cost_cache_read_usd = Some(cost.cache_read_usd);
    row.cost_total_usd = Some(cost.total_usd);
    row.cost_uncached_equiv_usd = Some(cost.uncached_equivalent_usd);
    row.pricing_model_id = Some(cost.model_id.to_owned());
    row.pricing_version = Some(crate::pricing::PRICING_VERSION.to_owned());
}

/// Tool invocations observed in this call, paired with their results.
fn tool_calls(normalized: &NormalizedCall, generation: &GenerationRow) -> Vec<ToolCallRow> {
    let declared: std::collections::HashSet<&str> = normalized
        .declared_tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();

    normalized
        .intra
        .tool_calls
        .iter()
        .enumerate()
        .map(|(seq, tool)| {
            let result = tool.result.as_ref();
            let is_error = result.and_then(|block| block.is_error);
            let status = match (result, is_error) {
                (Some(_), Some(true)) => "error",
                (Some(_), _) => "ok",
                (None, _) => "pending",
            };
            let input_text = tool.input.as_ref().map(Value::to_string);
            ToolCallRow {
                call_id: generation.call_id,
                session_id: generation.session_id.clone(),
                trace_id: generation.trace_id.clone(),
                seq: seq as i64,
                tool_use_id: tool.tool_use_id.clone(),
                server: extract::mcp_server(&tool.name),
                is_mcp: tool.name.starts_with("mcp__"),
                was_declared: declared.contains(tool.name.as_str()),
                input_chars: input_text
                    .as_deref()
                    .map(|text| text.chars().count() as i64),
                input_excerpt: input_text
                    .as_deref()
                    .map(|text| extract::excerpt(text, EXCERPT_CHARS)),
                result_chars: result.map(|block| block.approx_size.chars as i64),
                result_excerpt: result
                    .and_then(|block| block.content.as_deref())
                    .map(|text| extract::excerpt(text, EXCERPT_CHARS)),
                is_error,
                status: status.to_owned(),
                emitted_at: generation
                    .ended_at
                    .clone()
                    .or(Some(generation.started_at.clone())),
                observed_at: None,
                duration_ms: None,
                name: tool.name.clone(),
            }
        })
        .collect()
}
