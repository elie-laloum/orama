//! Row shapes for the derived layer.
//!
//! These mirror the `generations` / `tool_calls` tables one-to-one. They hold
//! fingerprints, counters and short excerpts — never copies of message content,
//! which stays addressable in `calls`.

use serde::Serialize;

/// One derived API round-trip: the unit that carries tokens, cost and outcome.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GenerationRow {
    pub call_id: i64,

    // Correlation. Trace and agent columns stay `None` until the trace model
    // lands; they are declared now so the schema does not churn again.
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub upstream_trace_id: Option<String>,
    pub upstream_span_id: Option<String>,
    pub request_id: Option<String>,
    pub account_uuid: Option<String>,
    pub device_id: Option<String>,
    pub org_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub agent_role: Option<String>,
    pub billing_variant: Option<String>,

    // Environment
    pub provider: String,
    pub framework: Option<String>,
    pub client_version: Option<String>,
    pub git_branch: Option<String>,
    pub project_name: Option<String>,
    pub cwd: Option<String>,

    // Model and request parameters
    pub model: Option<String>,
    pub model_resolved: Option<String>,
    pub service_tier: Option<String>,
    pub is_stream: bool,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub thinking_mode: Option<String>,
    pub thinking_budget: Option<i64>,
    pub stop_sequences: Option<String>,
    pub context_management: Option<String>,
    pub compaction_requested: bool,

    // Tokens
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_5m_tokens: Option<i64>,
    pub cache_creation_1h_tokens: Option<i64>,
    /// How the 5m/1h split was determined: `response`, `request` or `assumed`.
    pub cache_ttl_source: Option<String>,
    pub thinking_tokens: Option<i64>,

    // Cost. All `None` for a model absent from the pricing table — never zero,
    // which would read as "this call was free".
    pub cost_input_usd: Option<f64>,
    pub cost_output_usd: Option<f64>,
    pub cost_cache_write_usd: Option<f64>,
    pub cost_cache_read_usd: Option<f64>,
    pub cost_total_usd: Option<f64>,
    /// What the call would have cost with no caching, so the UI can show what
    /// the cache actually saved.
    pub cost_uncached_equiv_usd: Option<f64>,
    pub pricing_model_id: Option<String>,
    pub pricing_version: Option<String>,

    // Timing
    pub started_at: String,
    pub first_token_at: Option<String>,
    pub ended_at: Option<String>,
    pub ttft_ms: Option<i64>,
    pub latency_ms: Option<i64>,

    // Outcome
    pub http_status: Option<i64>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub is_error: bool,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub retry_count: Option<i64>,
    pub should_retry: Option<bool>,
    pub ratelimit_status: Option<String>,
    pub ratelimit_5h_utilization: Option<f64>,
    pub ratelimit_7d_utilization: Option<f64>,
    pub ratelimit_reset_at: Option<String>,
    pub overage_status: Option<String>,

    // Shape
    pub system_hash: Option<String>,
    pub system_chars: Option<i64>,
    pub system_segments_count: Option<i64>,
    pub system_cache_points: Option<i64>,
    pub tools_hash: Option<String>,
    pub tools_declared_count: Option<i64>,
    pub messages_count: Option<i64>,
    pub context_chars: Option<i64>,
    pub history_prefix_hash: Option<String>,
    /// The last history turn is a genuine user message rather than a
    /// tool-result continuation — i.e. this call opens a new user turn.
    pub new_user_turn: bool,
    /// Identity of the conversation root, used to spot a branch or a restart.
    pub first_turn_hash: Option<String>,
    pub tool_call_count: i64,
    pub tools_called: Option<String>,
    pub user_prompt: Option<String>,
    pub block_counts: Option<String>,
}

/// One tool invocation observed inside a generation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolCallRow {
    pub call_id: i64,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub seq: i64,
    pub tool_use_id: Option<String>,
    pub name: String,
    /// MCP server name, parsed from the `mcp__<server>__<tool>` convention.
    pub server: Option<String>,
    pub is_mcp: bool,
    pub was_declared: bool,
    pub input_chars: Option<i64>,
    pub input_excerpt: Option<String>,
    pub result_chars: Option<i64>,
    pub result_excerpt: Option<String>,
    pub is_error: Option<bool>,
    /// `ok`, `error` or `pending` (a call whose result has not been seen yet).
    pub status: String,
    pub emitted_at: Option<String>,
    pub observed_at: Option<String>,
    pub duration_ms: Option<i64>,
}

/// Everything derived from one raw capture, written as a single unit.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Derived {
    pub generation: GenerationRow,
    pub tool_calls: Vec<ToolCallRow>,
}
