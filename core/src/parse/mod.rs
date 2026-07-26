//! Provider detection and read-time normalization of stored calls.

mod claude_code;
pub mod diagnostics;
pub mod model;
pub mod session;
pub mod signals;

use serde_json::Value;

use crate::store::StoredCall;
use model::{IntraSignals, NormalizedCall, Provider, Timestamps, Usage};

/// A provider-specific semantic parser. Parsers never mutate stored raw data.
pub trait ProviderParser {
    fn parse(&self, call: &StoredCall) -> NormalizedCall;
}

/// Detect the provider from captured request headers.
pub fn detect(headers: &Value) -> Provider {
    let Some(headers) = headers.as_object() else {
        return Provider::Unknown;
    };
    let value = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
            .unwrap_or("")
    };
    if value("x-app").eq_ignore_ascii_case("cli")
        || value("user-agent")
            .to_ascii_lowercase()
            .contains("claude-cli")
    {
        Provider::ClaudeCode
    } else {
        Provider::Unknown
    }
}

/// Derive a normalized representation from a stored call.
pub fn parse_call(call: &StoredCall) -> NormalizedCall {
    match detect(&call.record.request_headers) {
        Provider::ClaudeCode => claude_code::ClaudeCodeParser.parse(call),
        Provider::Unknown => raw_fallback(call),
    }
}

fn raw_fallback(call: &StoredCall) -> NormalizedCall {
    let record = &call.record;
    NormalizedCall {
        id: call.id,
        provider: Provider::Unknown,
        model: record
            .request_body
            .as_ref()
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str)
            .map(String::from),
        session_key: None,
        timestamps: Timestamps {
            start: record.timestamp_start.clone(),
            first_chunk: record.timestamp_first_chunk.clone(),
            end: record.timestamp_end.clone(),
        },
        system: Vec::new(),
        declared_tools: Vec::new(),
        thread: Vec::new(),
        usage: Usage::default(),
        intra: IntraSignals::default(),
        response_status: record.response_status,
        error: record.error.clone(),
        raw_fallback: true,
    }
}
