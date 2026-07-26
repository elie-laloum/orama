//! Detector thresholds.
//!
//! Every number a detector compares against lives here. Nothing downstream is
//! allowed a literal threshold of its own — a duplicated constant is a rule that
//! silently disagrees with the policy it claims to implement.

use serde::Serialize;

/// Bumping this rebuilds the alert table without re-deriving generations.
pub const POLICY_VERSION: &str = "2026-07-26.1";

#[derive(Debug, Clone, Serialize)]
pub struct SignalPolicy {
    pub version: String,

    /// Time to first token above this is worth surfacing.
    pub slow_ttft_ms: i64,
    /// End-to-end latency above this is worth surfacing.
    pub slow_latency_ms: i64,

    /// Below this share of context served from cache, cache is underperforming.
    pub low_cache_reuse_rate: f64,
    /// Cache reuse is only meaningful once the context is this large.
    pub cache_analysis_min_input_tokens: i64,

    /// A single content block this large dominates the context.
    pub large_block_chars: i64,
    /// A tool result this large is worth flagging on its own.
    pub large_tool_result_chars: i64,

    /// Context growing by more than this ratio between calls is unusual…
    pub context_growth_ratio: f64,
    /// …but only once the absolute increase is material.
    pub context_growth_min_tokens: i64,
    /// A drop of at least this share of context indicates compaction.
    pub compaction_drop_ratio: f64,
    /// Context trends are only meaningful above this size. Below it a ratio is
    /// arithmetic on noise — two tokens falling to one is a 50% drop and means
    /// nothing.
    pub context_trend_min_tokens: i64,

    /// A generation costing more than this is worth a look.
    pub expensive_call_usd: f64,
    /// A session costing more than this is worth a look.
    pub session_budget_usd: f64,
    /// Non-main agents consuming more than this share of session cost.
    pub sidechain_cost_share: f64,

    /// Rate-limit utilization above this is approaching the ceiling.
    pub high_utilization: f64,
    /// Utilization above this, paired with an overage rejection, is an incident.
    pub critical_utilization: f64,

    /// Thinking within this fraction of its budget has effectively exhausted it.
    pub thinking_budget_exhausted_ratio: f64,
}

impl Default for SignalPolicy {
    fn default() -> Self {
        Self {
            version: POLICY_VERSION.to_owned(),
            slow_ttft_ms: 3_000,
            slow_latency_ms: 60_000,
            low_cache_reuse_rate: 0.20,
            cache_analysis_min_input_tokens: 1_000,
            large_block_chars: 10_000,
            large_tool_result_chars: 20_000,
            context_growth_ratio: 1.40,
            context_growth_min_tokens: 20_000,
            compaction_drop_ratio: 0.30,
            context_trend_min_tokens: 1_000,
            expensive_call_usd: 0.50,
            session_budget_usd: 5.00,
            sidechain_cost_share: 0.30,
            high_utilization: 0.80,
            critical_utilization: 0.95,
            thinking_budget_exhausted_ratio: 0.95,
        }
    }
}
