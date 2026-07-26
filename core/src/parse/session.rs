//! Session assembly and inter-call signals, derived on demand.

use std::collections::BTreeMap;

use serde::Serialize;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::{model::*, parse_call};
use crate::store::StoredCall;

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub key: String,
    /// First meaningful user-authored text, excluding system/reminder content.
    pub title: Option<String>,
    pub calls_count: usize,
    pub model: Option<String>,
    pub start: String,
    pub end: Option<String>,
    pub has_error: bool,
    pub has_compaction: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineCall {
    pub id: i64,
    pub model: Option<String>,
    pub status: Option<i64>,
    pub start: String,
    pub first_chunk: Option<String>,
    pub end: Option<String>,
    pub ttft_ms: Option<i128>,
    pub latency_ms: Option<i128>,
    pub context_input_tokens: Option<u64>,
    pub context_approx_chars: usize,
    pub chain_break: bool,
    pub compaction: bool,
    pub system_drift: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSignals {
    pub context_growth: Vec<ContextPoint>,
    pub compaction_call_ids: Vec<i64>,
    pub system_drift_call_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextPoint {
    pub call_id: i64,
    pub input_tokens: Option<u64>,
    pub approx_chars: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub key: String,
    pub calls: Vec<TimelineCall>,
    pub signals: SessionSignals,
}

pub fn sessions(calls: &[StoredCall]) -> Vec<SessionSummary> {
    let mut summaries: Vec<_> = grouped(calls)
        .into_iter()
        .map(|(key, calls)| summarize(key, calls))
        .collect();
    // The dashboard is operational: newest activity belongs at the top, not
    // the lexical ordering of an opaque session key.
    summaries.sort_by(|left, right| right.end.cmp(&left.end).then(right.start.cmp(&left.start)));
    summaries
}

pub fn session(calls: &[StoredCall], key: &str) -> Option<SessionDetail> {
    grouped(calls)
        .remove(key)
        .map(|calls| detail(key.to_string(), calls))
}

fn grouped(calls: &[StoredCall]) -> BTreeMap<String, Vec<NormalizedCall>> {
    let mut groups: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for call in calls {
        let normalized = parse_call(call);
        // Unknown/missing session identifiers are deliberately isolated. This
        // avoids inventing potentially incorrect cross-call relationships.
        let key = normalized
            .session_key
            .clone()
            .unwrap_or_else(|| format!("unscoped-call-{}", normalized.id));
        groups.entry(key).or_default().push(normalized);
    }
    for calls in groups.values_mut() {
        calls.sort_by(|left, right| {
            left.timestamps
                .start
                .cmp(&right.timestamps.start)
                .then(left.id.cmp(&right.id))
        });
    }
    groups
}

fn summarize(key: String, calls: Vec<NormalizedCall>) -> SessionSummary {
    let title = conversation_title(&calls);
    let detail = detail(key.clone(), calls);
    let first = detail.calls.first().expect("session is nonempty");
    let last = detail.calls.last().expect("session is nonempty");
    SessionSummary {
        title,
        key,
        calls_count: detail.calls.len(),
        model: first.model.clone(),
        start: first.start.clone(),
        end: last.end.clone().or_else(|| Some(last.start.clone())),
        has_error: detail.calls.iter().any(|call| call.error.is_some()),
        has_compaction: !detail.signals.compaction_call_ids.is_empty(),
    }
}

fn conversation_title(calls: &[NormalizedCall]) -> Option<String> {
    calls
        .iter()
        .flat_map(|call| &call.thread)
        .filter(|turn| matches!(turn.role, Role::User))
        .flat_map(|turn| &turn.blocks)
        .filter(|block| matches!(block.kind, BlockKind::Text) && block.content_tag.is_none())
        .filter_map(|block| block.content.as_deref())
        .map(str::trim)
        .find(|text| !text.is_empty() && !is_injected_context(text))
        .map(short_title)
}

fn is_injected_context(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with("<system-reminder")
        || lower.starts_with("<local-command-")
        || lower.starts_with("<local-command-caveat")
        || lower.starts_with("<environment_context")
        || lower.starts_with("<task-notification")
}

fn short_title(text: &str) -> String {
    const MAX_CHARS: usize = 100;
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let title: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

fn detail(key: String, calls: Vec<NormalizedCall>) -> SessionDetail {
    let mut timeline = Vec::with_capacity(calls.len());
    let mut growth = Vec::with_capacity(calls.len());
    let mut compactions = Vec::new();
    let mut drifts = Vec::new();
    let mut previous: Option<&NormalizedCall> = None;
    for call in &calls {
        let approx_chars = thread_chars(call);
        let chain_break = previous.is_some_and(|previous| !is_history_prefix(previous, call));
        let compaction = previous.is_some_and(|previous| {
            significant_drop(previous.usage.input, call.usage.input)
                || significant_drop_usize(thread_chars(previous), approx_chars)
        });
        let system_drift = previous
            .is_some_and(|previous| system_fingerprint(previous) != system_fingerprint(call));
        if compaction {
            compactions.push(call.id);
        }
        if system_drift {
            drifts.push(call.id);
        }
        growth.push(ContextPoint {
            call_id: call.id,
            input_tokens: call.usage.input,
            approx_chars,
        });
        timeline.push(TimelineCall {
            id: call.id,
            model: call.model.clone(),
            status: call.response_status,
            start: call.timestamps.start.clone(),
            first_chunk: call.timestamps.first_chunk.clone(),
            end: call.timestamps.end.clone(),
            ttft_ms: duration_ms(
                &call.timestamps.start,
                call.timestamps.first_chunk.as_deref(),
            ),
            latency_ms: duration_ms(&call.timestamps.start, call.timestamps.end.as_deref()),
            context_input_tokens: call.usage.input,
            context_approx_chars: approx_chars,
            chain_break,
            compaction,
            system_drift,
            error: call.error.clone(),
        });
        previous = Some(call);
    }
    SessionDetail {
        key,
        calls: timeline,
        signals: SessionSignals {
            context_growth: growth,
            compaction_call_ids: compactions,
            system_drift_call_ids: drifts,
        },
    }
}

fn thread_chars(call: &NormalizedCall) -> usize {
    call.thread
        .iter()
        .flat_map(|turn| &turn.blocks)
        .map(|block| block.approx_size.chars)
        .sum()
}
fn system_fingerprint(call: &NormalizedCall) -> String {
    call.system
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("\u{1f}")
}
fn is_history_prefix(previous: &NormalizedCall, current: &NormalizedCall) -> bool {
    let old: Vec<_> = previous
        .thread
        .iter()
        .filter(|turn| matches!(turn.origin, Origin::History))
        .collect();
    let new: Vec<_> = current
        .thread
        .iter()
        .filter(|turn| matches!(turn.origin, Origin::History))
        .collect();
    old.len() <= new.len()
        && old
            .iter()
            .zip(new)
            .all(|(left, right)| format!("{:?}", left) == format!("{:?}", right))
}
fn significant_drop(before: Option<u64>, after: Option<u64>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if before > 0 && after.saturating_mul(100) < before.saturating_mul(70))
}
fn significant_drop_usize(before: usize, after: usize) -> bool {
    before > 0 && after.saturating_mul(100) < before.saturating_mul(70)
}
fn duration_ms(start: &str, end: Option<&str>) -> Option<i128> {
    let start = OffsetDateTime::parse(start, &Rfc3339).ok()?;
    let end = OffsetDateTime::parse(end?, &Rfc3339).ok()?;
    Some((end - start).whole_milliseconds())
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::store::CallRecord;

    fn call(
        id: i64,
        time: &str,
        input_tokens: u64,
        system: &str,
        messages: Vec<Value>,
    ) -> StoredCall {
        StoredCall {
            id,
            record: CallRecord {
                timestamp_start: time.to_string(),
                timestamp_end: Some("2026-07-25T00:00:02Z".to_string()),
                request_headers: json!({"x-app":"cli", "x-claude-code-session-id":"run-1"}),
                request_body: Some(json!({"model":"claude", "system":system, "messages":messages})),
                response_reconstructed: Some(
                    json!({"role":"assistant", "content":"ok", "usage":{"input_tokens":input_tokens,"output_tokens":1}}),
                ),
                ..Default::default()
            },
        }
    }

    #[test]
    fn groups_native_session_and_flags_compaction_and_drift() {
        let old_messages = vec![
            json!({"role":"user", "content":"one"}),
            json!({"role":"assistant", "content":"two"}),
        ];
        let calls = vec![
            call(
                1,
                "2026-07-25T00:00:00Z",
                100,
                "system A",
                old_messages.clone(),
            ),
            call(
                2,
                "2026-07-25T00:00:01Z",
                20,
                "system B",
                vec![json!({"role":"user", "content":"new"})],
            ),
        ];
        let summaries = sessions(&calls);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].title.as_deref(), Some("one"));
        assert!(summaries[0].has_compaction);
        let detail = session(&calls, "run-1").unwrap();
        assert_eq!(detail.calls.len(), 2);
        assert!(detail.calls[1].compaction);
        assert!(detail.calls[1].chain_break);
        assert!(detail.calls[1].system_drift);
        assert_eq!(detail.calls[1].latency_ms, Some(1000));
    }

    #[test]
    fn session_title_skips_injected_system_context_and_sessions_are_recent_first() {
        let old = call(
            1,
            "2026-07-25T00:00:00Z",
            1,
            "",
            vec![json!({"role":"user","content":"<system-reminder>ignore</system-reminder>"})],
        );
        let new = StoredCall {
            id: 2,
            record: CallRecord {
                timestamp_start: "2026-07-25T01:00:00Z".into(),
                timestamp_end: Some("2026-07-25T01:00:01Z".into()),
                request_headers: json!({"x-app":"cli", "x-claude-code-session-id":"run-2"}),
                request_body: Some(
                    json!({"model":"claude", "messages":[{"role":"user","content":"Please add a conversation title."}]}),
                ),
                response_reconstructed: Some(
                    json!({"role":"assistant","content":"ok","usage":{"input_tokens":1,"output_tokens":1}}),
                ),
                ..Default::default()
            },
        };
        let summaries = sessions(&[old, new]);
        assert_eq!(summaries[0].key, "run-2");
        assert_eq!(
            summaries[0].title.as_deref(),
            Some("Please add a conversation title.")
        );
        assert_eq!(summaries[1].title, None);
    }
}
