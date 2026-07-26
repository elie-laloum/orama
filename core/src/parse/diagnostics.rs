//! Explainable, read-time diagnostics for normalized calls.
//!
//! The capture database remains the source of truth. This module only derives
//! serializable observations from normalized data and the versioned policy.

use std::collections::HashSet;

use serde::Serialize;

use super::{
    model::*,
    session::{SessionDetail, TimelineCall},
};

#[derive(Debug, Clone, Serialize)]
pub struct SignalPolicy {
    pub version: String,
    pub context_growth_ratio: f64,
    pub context_growth_min_tokens: u64,
    pub compaction_drop_ratio: f64,
    pub slow_ttft_ms: i128,
    pub slow_latency_ms: i128,
    pub low_cache_reuse_rate: f64,
    pub cache_analysis_min_input_tokens: u64,
    pub large_block_chars: usize,
}

impl Default for SignalPolicy {
    fn default() -> Self {
        Self {
            version: "2026-07-25.1".to_owned(),
            context_growth_ratio: 1.40,
            context_growth_min_tokens: 20_000,
            compaction_drop_ratio: 0.30,
            slow_ttft_ms: 3_000,
            slow_latency_ms: 10_000,
            low_cache_reuse_rate: 0.20,
            cache_analysis_min_input_tokens: 1_000,
            large_block_chars: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertCategory {
    Execution,
    Context,
    Performance,
    Cache,
    DataQuality,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Inferred,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Exactness {
    Exact,
    Approximate,
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedValue {
    pub label: String,
    pub value: Option<String>,
    pub unit: Option<String>,
    pub exactness: Exactness,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_ref: Option<BlockRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockRef {
    pub turn: usize,
    pub block: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: String,
    pub category: AlertCategory,
    pub severity: Severity,
    pub rule_id: String,
    pub title: String,
    pub summary: String,
    pub observed: Vec<ObservedValue>,
    pub explanation: String,
    pub impact: String,
    pub recommendation: String,
    pub sources: Vec<AlertSource>,
    pub occurred_at: String,
    pub policy_version: String,
    pub confidence: Confidence,
}

/// Totals deliberately exclude cache counters: they describe portions of input.
///
/// Anthropic reports `input_tokens` separately from cache read/create volumes.
/// Therefore cache reuse uses the full observed context input as its
/// denominator, not `input_tokens` alone; otherwise a small uncached suffix can
/// yield nonsensical rates above 100%.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TokenMetrics {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_reuse_rate: Option<f64>,
}

pub fn token_metrics(usage: &Usage) -> TokenMetrics {
    TokenMetrics {
        input_tokens: usage.input,
        output_tokens: usage.output,
        total_tokens: usage
            .input
            .zip(usage.output)
            .map(|(input, output)| input + output),
        cache_read_input_tokens: usage.cache_read,
        cache_creation_input_tokens: usage.cache_creation,
        cache_reuse_rate: cache_reuse_rate(usage),
    }
}

/// Cache-read share of all API-reported context input. Cache creation counts in
/// the denominator because it is input processed on this request, but never in
/// the numerator because it was not reused.
pub fn cache_reuse_rate(usage: &Usage) -> Option<f64> {
    let read = usage.cache_read?;
    let total_context_input = usage
        .input?
        .checked_add(read)?
        .checked_add(usage.cache_creation.unwrap_or(0))?;
    (total_context_input > 0).then_some(read as f64 / total_context_input as f64)
}

pub fn call_alerts(call: &NormalizedCall, policy: &SignalPolicy) -> Vec<Alert> {
    let mut alerts = Vec::new();
    let source = |metric_ref: &str| AlertSource {
        session_key: call.session_key.clone(),
        call_id: Some(call.id),
        block_ref: None,
        metric_ref: Some(metric_ref.to_owned()),
    };
    let add = |alerts: &mut Vec<Alert>,
               rule: &str,
               category,
               severity,
               title: &str,
               summary: String,
               observed,
               explanation: &str,
               impact: &str,
               recommendation: &str,
               confidence,
               source: AlertSource| {
        alerts.push(Alert {
            id: format!("{rule}:{}", call.id),
            category,
            severity,
            rule_id: rule.to_owned(),
            title: title.to_owned(),
            summary,
            observed,
            explanation: explanation.to_owned(),
            impact: impact.to_owned(),
            recommendation: recommendation.to_owned(),
            sources: vec![source],
            occurred_at: call
                .timestamps
                .end
                .clone()
                .unwrap_or_else(|| call.timestamps.start.clone()),
            policy_version: policy.version.clone(),
            confidence,
        });
    };

    if call
        .response_status
        .is_some_and(|status| !(200..300).contains(&status))
    {
        let status = call.response_status.unwrap();
        add(
            &mut alerts,
            "execution.http_status",
            AlertCategory::Execution,
            Severity::Error,
            "HTTP response failed",
            format!("The call returned HTTP {status}."),
            vec![number("HTTP status", status)],
            "The proxy captured a non-success HTTP response.",
            "The requested model operation may not have completed.",
            "Inspect the raw response and retry only after resolving the upstream error.",
            Confidence::Exact,
            source("response_status"),
        );
    }
    if let Some(error) = &call.error {
        add(
            &mut alerts,
            "execution.transport_error",
            AlertCategory::Execution,
            Severity::Error,
            "Captured transport error",
            "The proxy recorded a transport or relay error.".to_owned(),
            vec![text("Error", error)],
            "This error was persisted by the proxy while handling the call.",
            "The response may be incomplete or unavailable.",
            "Inspect the raw capture and the proxy logs.",
            Confidence::Exact,
            source("error"),
        );
    }
    if call.timestamps.end.is_none() {
        add(
            &mut alerts,
            "execution.incomplete_response",
            AlertCategory::Execution,
            Severity::Warning,
            "Response completion is unavailable",
            "No response-end timestamp was captured.".to_owned(),
            vec![unavailable("Response end")],
            "The capture may be incomplete or the stream may have been interrupted.",
            "Latency and completion cannot be confirmed.",
            "Inspect the raw SSE capture and transport error, if any.",
            Confidence::Unavailable,
            source("timestamp_end"),
        );
    }
    if call.usage.input.is_none() || call.usage.output.is_none() {
        add(
            &mut alerts,
            "data_quality.usage_missing",
            AlertCategory::DataQuality,
            Severity::Warning,
            "Token usage is unavailable",
            "Input and output tokens cannot both be calculated.".to_owned(),
            vec![unavailable("Total tokens")],
            "The reconstructed provider response did not include complete usage fields.",
            "Totals and cache ratios must not be interpreted as zero.",
            "Inspect the reconstructed response or raw stream.",
            Confidence::Unavailable,
            source("usage"),
        );
    }
    if call.timestamps.start.is_empty() {
        add(
            &mut alerts,
            "data_quality.timestamp_missing",
            AlertCategory::DataQuality,
            Severity::Warning,
            "Start timestamp is unavailable",
            "The call has no usable start timestamp.".to_owned(),
            vec![unavailable("Start time")],
            "The stored capture is missing its required timing reference.",
            "Chronology and performance analysis are limited.",
            "Inspect the source capture.",
            Confidence::Unavailable,
            source("timestamp_start"),
        );
    }
    let metrics = token_metrics(&call.usage);
    if call
        .usage
        .input
        .is_some_and(|input| input >= policy.cache_analysis_min_input_tokens)
    {
        match metrics.cache_reuse_rate {
            Some(rate) if rate < policy.low_cache_reuse_rate => add(&mut alerts, "cache.low_reuse", AlertCategory::Cache, Severity::Info, "Low cache reuse",
                format!("Only {:.0}% of input tokens were served from cache.", rate * 100.0), vec![percent("Cache reuse", rate)],
                "The API-reported cache-read tokens are low relative to total context input, including cache volumes.", "Large prompts may receive less cache benefit.",
                "Inspect cache-control points and repeated prompt segments.", Confidence::Exact, source("cache_reuse_rate")),
            None => add(&mut alerts, "cache.reuse_unavailable", AlertCategory::Cache, Severity::Info, "Cache reuse is unavailable",
                "Cache analysis was requested for a substantial input, but required counters are missing.".to_owned(), vec![unavailable("Cache reuse")],
                "Input or cache-read counters were not available from the API response.", "Cache health cannot be inferred as zero or healthy.",
                "Inspect usage in the reconstructed response.", Confidence::Unavailable, source("cache_reuse_rate")),
            _ => {}
        }
    }
    for (turn, block, item) in call.thread.iter().enumerate().flat_map(|(turn, item)| {
        item.blocks
            .iter()
            .enumerate()
            .map(move |(block, item)| (turn, block, item))
    }) {
        let block_source = AlertSource {
            session_key: call.session_key.clone(),
            call_id: Some(call.id),
            block_ref: Some(BlockRef { turn, block }),
            metric_ref: Some("approx_size.chars".to_owned()),
        };
        if item.approx_size.chars >= policy.large_block_chars {
            add(
                &mut alerts,
                "context.large_block",
                AlertCategory::Context,
                Severity::Warning,
                "Large content block",
                format!(
                    "A block contains approximately {} characters.",
                    item.approx_size.chars
                ),
                vec![approx("Block size", item.approx_size.chars, "chars")],
                "The normalized block size exceeded the centrally configured threshold.",
                "Large blocks can grow context and increase latency.",
                "Open the block and consider truncating or summarizing it.",
                Confidence::Inferred,
                block_source.clone(),
            );
        }
        if matches!(item.kind, BlockKind::ToolResult) && item.is_error == Some(true) {
            add(
                &mut alerts,
                "execution.tool_error",
                AlertCategory::Execution,
                Severity::Error,
                "Tool result reported an error",
                "A tool result is explicitly marked as an error.".to_owned(),
                vec![text(
                    "Tool use id",
                    item.tool_use_id.as_deref().unwrap_or("missing"),
                )],
                "The provider marked this tool result with is_error=true.",
                "The assistant may have acted on an unsuccessful tool operation.",
                "Inspect the tool result and its associated tool call.",
                Confidence::Exact,
                block_source.clone(),
            );
        }
    }
    let tool_uses: HashSet<&str> = call
        .thread
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter(|block| matches!(block.kind, BlockKind::ToolUse))
        .filter_map(|block| block.tool_use_id.as_deref())
        .collect();
    let tool_results: HashSet<&str> = call
        .thread
        .iter()
        .flat_map(|turn| &turn.blocks)
        .filter(|block| matches!(block.kind, BlockKind::ToolResult))
        .filter_map(|block| block.tool_use_id.as_deref())
        .collect();
    for id in tool_uses.difference(&tool_results) {
        add(
            &mut alerts,
            "execution.tool_result_missing",
            AlertCategory::Execution,
            Severity::Warning,
            "Tool use has no result",
            format!("Tool use `{id}` has no associated result."),
            vec![text("Tool use id", id)],
            "No normalized tool_result block references this tool use id.",
            "The tool interaction cannot be verified as complete.",
            "Inspect subsequent calls and the raw capture.",
            Confidence::Exact,
            source("tool_pairing"),
        );
    }
    for id in tool_results.difference(&tool_uses) {
        add(
            &mut alerts,
            "data_quality.tool_use_missing",
            AlertCategory::DataQuality,
            Severity::Warning,
            "Tool result has no call",
            format!("Tool result `{id}` has no associated tool use."),
            vec![text("Tool use id", id)],
            "No normalized tool_use block references this result id.",
            "Tool causality is incomplete.",
            "Inspect the conversation and raw capture.",
            Confidence::Exact,
            source("tool_pairing"),
        );
    }
    alerts
}

/// Derive session-scoped context and performance diagnostics from the timeline.
pub fn session_alerts(session: &SessionDetail, policy: &SignalPolicy) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for call in &session.calls {
        alerts.extend(timeline_alerts(call, &session.key, policy));
        if call.compaction {
            alerts.push(session_alert(
                "context.compaction", AlertCategory::Context, Severity::Warning,
                "Probable context compaction", "Context size dropped sharply relative to the prior call.",
                vec![approx("Current context", call.context_approx_chars, "chars")],
                "The session policy detected a substantial input-token or reconstructed-message drop.",
                "Earlier conversation context may no longer influence this call.",
                "Compare this call with its predecessor and inspect the conversation source.", call, &session.key, policy, Confidence::Inferred, "context_compaction"));
        }
        if call.system_drift {
            alerts.push(session_alert(
                "context.system_prompt_drift",
                AlertCategory::Context,
                Severity::Info,
                "System prompt changed",
                "The system prompt differs from the previous call.",
                vec![approx(
                    "Current system context",
                    call.context_approx_chars,
                    "chars",
                )],
                "A normalized system-prompt fingerprint changed between consecutive calls.",
                "Behavior and cache reuse may differ between calls.",
                "Inspect the system-prompt segments in both calls.",
                call,
                &session.key,
                policy,
                Confidence::Inferred,
                "system_prompt",
            ));
        }
    }
    for pair in session.signals.context_growth.windows(2) {
        let (Some(before), Some(after)) = (pair[0].input_tokens, pair[1].input_tokens) else {
            continue;
        };
        let increase = after.saturating_sub(before);
        let ratio = after as f64 / before.max(1) as f64;
        if ratio > policy.context_growth_ratio && increase >= policy.context_growth_min_tokens {
            if let Some(call) = session.calls.iter().find(|call| call.id == pair[1].call_id) {
                alerts.push(session_alert(
                    "context.excessive_growth", AlertCategory::Context, Severity::Warning,
                    "Context grew unusually quickly", &format!("Input grew by {increase} tokens ({ratio:.2}×)."),
                    vec![number("Increase", increase), percent("Growth ratio", ratio)],
                    "Exact API input-token counts exceeded both the relative and absolute policy thresholds.",
                    "Growing context can increase cost and response latency.",
                    "Inspect large conversation or tool-result blocks before this call.", call, &session.key, policy, Confidence::Exact, "input_tokens"));
            }
        }
    }
    alerts
}

fn session_alert(
    rule_id: &str,
    category: AlertCategory,
    severity: Severity,
    title: &str,
    summary: &str,
    observed: Vec<ObservedValue>,
    explanation: &str,
    impact: &str,
    recommendation: &str,
    call: &TimelineCall,
    session_key: &str,
    policy: &SignalPolicy,
    confidence: Confidence,
    metric_ref: &str,
) -> Alert {
    Alert {
        id: format!("{rule_id}:{}", call.id),
        category,
        severity,
        rule_id: rule_id.to_owned(),
        title: title.to_owned(),
        summary: summary.to_owned(),
        observed,
        explanation: explanation.to_owned(),
        impact: impact.to_owned(),
        recommendation: recommendation.to_owned(),
        sources: vec![AlertSource {
            session_key: Some(session_key.to_owned()),
            call_id: Some(call.id),
            block_ref: None,
            metric_ref: Some(metric_ref.to_owned()),
        }],
        occurred_at: call.end.clone().unwrap_or_else(|| call.start.clone()),
        policy_version: policy.version.clone(),
        confidence,
    }
}

pub fn timeline_alerts(
    call: &TimelineCall,
    session_key: &str,
    policy: &SignalPolicy,
) -> Vec<Alert> {
    let mut alerts = Vec::new();
    for (rule_id, title, value, threshold, metric) in [
        (
            "performance.slow_ttft",
            "Time to first token is high",
            call.ttft_ms,
            policy.slow_ttft_ms,
            "ttft_ms",
        ),
        (
            "performance.slow_latency",
            "Total latency is high",
            call.latency_ms,
            policy.slow_latency_ms,
            "latency_ms",
        ),
    ] {
        if let Some(value) = value.filter(|value| *value > threshold) {
            alerts.push(Alert {
                id: format!("{rule_id}:{}", call.id),
                category: AlertCategory::Performance,
                severity: Severity::Warning,
                rule_id: rule_id.to_owned(),
                title: title.to_owned(),
                summary: format!(
                    "{} ms exceeds the {} ms policy threshold.",
                    value, threshold
                ),
                observed: vec![number("Observed", value), number("Threshold", threshold)],
                explanation: "Measured timestamps exceed the configured absolute threshold."
                    .to_owned(),
                impact: "Interactive responsiveness may be degraded.".to_owned(),
                recommendation: "Inspect context size, model behavior, and upstream latency."
                    .to_owned(),
                sources: vec![AlertSource {
                    session_key: Some(session_key.to_owned()),
                    call_id: Some(call.id),
                    block_ref: None,
                    metric_ref: Some(metric.to_owned()),
                }],
                occurred_at: call.end.clone().unwrap_or_else(|| call.start.clone()),
                policy_version: policy.version.clone(),
                confidence: Confidence::Exact,
            });
        }
    }
    alerts
}

fn number(label: &str, value: impl ToString) -> ObservedValue {
    ObservedValue {
        label: label.to_owned(),
        value: Some(value.to_string()),
        unit: None,
        exactness: Exactness::Exact,
    }
}
fn percent(label: &str, value: f64) -> ObservedValue {
    ObservedValue {
        label: label.to_owned(),
        value: Some(format!("{:.4}", value)),
        unit: Some("ratio".to_owned()),
        exactness: Exactness::Exact,
    }
}
fn approx(label: &str, value: usize, unit: &str) -> ObservedValue {
    ObservedValue {
        label: label.to_owned(),
        value: Some(value.to_string()),
        unit: Some(unit.to_owned()),
        exactness: Exactness::Approximate,
    }
}
fn text(label: &str, value: &str) -> ObservedValue {
    ObservedValue {
        label: label.to_owned(),
        value: Some(value.to_owned()),
        unit: None,
        exactness: Exactness::Exact,
    }
}
fn unavailable(label: &str) -> ObservedValue {
    ObservedValue {
        label: label.to_owned(),
        value: None,
        unit: None,
        exactness: Exactness::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_total_does_not_double_count_cache_fields() {
        let metrics = token_metrics(&Usage {
            input: Some(100),
            output: Some(25),
            cache_read: Some(80),
            cache_creation: Some(10),
        });
        assert_eq!(metrics.total_tokens, Some(125));
        assert_eq!(metrics.cache_reuse_rate, Some(80.0 / 190.0));
    }

    #[test]
    fn cache_reuse_never_exceeds_one_when_api_input_excludes_cached_tokens() {
        let metrics = token_metrics(&Usage {
            input: Some(10),
            output: Some(1),
            cache_read: Some(86_515),
            cache_creation: Some(10_338),
        });
        assert!(metrics.cache_reuse_rate.unwrap() < 1.0);
        assert!((metrics.cache_reuse_rate.unwrap() - 86_515.0 / 96_863.0).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_values_remain_unavailable() {
        let metrics = token_metrics(&Usage {
            input: Some(100),
            output: None,
            cache_read: None,
            cache_creation: None,
        });
        assert_eq!(metrics.total_tokens, None);
        assert_eq!(metrics.cache_reuse_rate, None);
    }
}
