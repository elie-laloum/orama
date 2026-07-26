//! Field extraction from a raw capture.
//!
//! Everything here reads the stored JSON directly. It is deliberately total:
//! a missing or unexpected field yields `None` rather than an error, because a
//! provider adding or renaming a field must never cost us the whole row.

use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

/// Case-insensitive lookup in a stored headers object.
pub fn header<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    headers
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_str())
}

/// Milliseconds between two RFC3339 instants, when both are present and parse.
pub fn millis_between(start: &str, end: Option<&str>) -> Option<i64> {
    let start = OffsetDateTime::parse(start, &Rfc3339).ok()?;
    let end = OffsetDateTime::parse(end?, &Rfc3339).ok()?;
    i64::try_from((end - start).whole_milliseconds()).ok()
}

/// Content fingerprint. Stable across runs and machines, unlike `DefaultHasher`,
/// so a rebuild of the derived tables reproduces identical values.
pub fn fingerprint(parts: &[&str]) -> String {
    fingerprint_n(parts, 32)
}

/// Fingerprint truncated to `hex_len` hex characters. Trace ids use 32 and span
/// ids 16, matching W3C trace-context widths so they read as familiar ids.
pub fn fingerprint_n(parts: &[&str], hex_len: usize) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        // Length-prefix-free separator: a byte that cannot appear in UTF-8 text.
        hasher.update(&[0xff]);
    }
    hasher.finalize().to_hex()[..hex_len].to_string()
}

/// Parse a W3C `traceresponse` header: `00-<32 hex trace>-<16 hex span>-<flags>`.
///
/// This is the upstream's own trace context — real provider-assigned ids, not
/// something we derived, so it is stored as evidence alongside our own.
pub fn trace_context(value: &str) -> Option<(String, String)> {
    let mut parts = value.trim().split('-');
    let _version = parts.next()?;
    let trace = parts.next()?;
    let span = parts.next()?;
    let hex = |s: &str, len: usize| {
        (s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
    };
    Some((hex(trace, 32)?, hex(span, 16)?))
}

/// Identity carried in `metadata.user_id`.
///
/// The value is a JSON-encoded **string**, not an object, so a plain JSON
/// pointer through it can never match. Decoding it is what makes the session-id
/// fallback work at all and is the only source of `account_uuid` / `device_id`.
pub fn identity(body: Option<&Value>) -> (Option<String>, Option<String>, Option<String>) {
    let Some(raw) = body
        .and_then(|body| body.pointer("/metadata/user_id"))
        .and_then(Value::as_str)
    else {
        return (None, None, None);
    };
    let Ok(decoded) = serde_json::from_str::<Value>(raw) else {
        return (None, None, None);
    };
    let field = |name: &str| decoded.get(name).and_then(Value::as_str).map(str::to_owned);
    (
        field("session_id"),
        field("account_uuid"),
        field("device_id"),
    )
}

/// The harness agent variant, from the billing header Claude Code injects as the
/// first system segment: `x-anthropic-billing-header: cc_version=2.1.220.85f;`.
///
/// The suffix differs per harness agent (main loop, subagent, classifier,
/// summarizer), which makes it a provider-supplied corroboration of the agent
/// classification rather than a guess.
pub fn billing_variant(system_first: &str) -> Option<String> {
    let rest = system_first.split("cc_version=").nth(1)?;
    let value = rest.split(';').next()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Value following `marker` on the same line, trimmed. Used for the environment
/// block Claude Code injects into the system prompt.
pub fn line_value<'a>(haystack: &'a str, marker: &str) -> Option<&'a str> {
    let rest = haystack.split(marker).nth(1)?;
    let line = rest.lines().next()?.trim();
    (!line.is_empty()).then_some(line)
}

/// Token usage, including the fields the previous parser ignored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageFields {
    pub input: Option<i64>,
    pub output: Option<i64>,
    pub cache_creation: Option<i64>,
    pub cache_read: Option<i64>,
    pub cache_5m: Option<i64>,
    pub cache_1h: Option<i64>,
    pub thinking: Option<i64>,
    pub service_tier: Option<String>,
    pub ttl_source: Option<String>,
}

/// Read `usage` off an assembled response message.
///
/// The 5m/1h split matters because the two TTLs are priced differently; when the
/// response omits it, the caller can still recover it from the request's
/// declared `cache_control.ttl`.
pub fn usage(message: &Value) -> UsageFields {
    let Some(usage) = message.get("usage") else {
        return UsageFields::default();
    };
    let int = |name: &str| usage.get(name).and_then(Value::as_i64);
    let nested = |path: &str, name: &str| {
        usage
            .get(path)
            .and_then(|value| value.get(name))
            .and_then(Value::as_i64)
    };
    let cache_5m = nested("cache_creation", "ephemeral_5m_input_tokens");
    let cache_1h = nested("cache_creation", "ephemeral_1h_input_tokens");
    UsageFields {
        input: int("input_tokens"),
        output: int("output_tokens"),
        cache_creation: int("cache_creation_input_tokens"),
        cache_read: int("cache_read_input_tokens"),
        cache_5m,
        cache_1h,
        thinking: nested("output_tokens_details", "thinking_tokens"),
        service_tier: usage
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ttl_source: (cache_5m.is_some() || cache_1h.is_some()).then(|| "response".to_owned()),
    }
}

/// The cache TTL the request asked for, from any `cache_control.ttl` marker in
/// the system segments or message content blocks.
///
/// This is the fallback that keeps cache accounting honest for calls whose
/// response was never captured.
pub fn requested_cache_ttl(body: Option<&Value>) -> Option<String> {
    fn scan(value: &Value, found: &mut Option<String>) {
        match value {
            Value::Object(map) => {
                if let Some(ttl) = map
                    .get("cache_control")
                    .and_then(|cc| cc.get("ttl"))
                    .and_then(Value::as_str)
                {
                    // A longer TTL dominates: it is the more expensive write.
                    if found.as_deref() != Some("1h") {
                        *found = Some(ttl.to_owned());
                    }
                }
                for nested in map.values() {
                    scan(nested, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| scan(item, found)),
            _ => {}
        }
    }
    let mut found = None;
    for key in ["system", "messages"] {
        if let Some(section) = body.and_then(|body| body.get(key)) {
            scan(section, &mut found);
        }
    }
    found
}

/// Split an MCP tool name (`mcp__<server>__<tool>`) into its server component.
pub fn mcp_server(name: &str) -> Option<String> {
    let rest = name.strip_prefix("mcp__")?;
    let server = rest.split("__").next()?;
    (!server.is_empty()).then(|| server.to_owned())
}

/// Truncate to `max` characters on a char boundary, marking elision.
pub fn excerpt(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trace_context_accepts_w3c_and_rejects_malformed() {
        let (trace, span) =
            trace_context("00-4f601cc5782981dd2c0fa88fc28b142c-f7cb4b424805cfa4-01").unwrap();
        assert_eq!(trace, "4f601cc5782981dd2c0fa88fc28b142c");
        assert_eq!(span, "f7cb4b424805cfa4");
        assert!(trace_context("garbage").is_none());
        assert!(trace_context("00-short-f7cb4b424805cfa4-01").is_none());
    }

    #[test]
    fn identity_decodes_the_json_encoded_user_id_string() {
        // The provider sends this as a *string*, which is why a plain JSON
        // pointer through it never matched.
        let body = json!({"metadata": {"user_id": "{\"device_id\":\"d1\",\"account_uuid\":\"a1\",\"session_id\":\"s1\"}"}});
        let (session, account, device) = identity(Some(&body));
        assert_eq!(session.as_deref(), Some("s1"));
        assert_eq!(account.as_deref(), Some("a1"));
        assert_eq!(device.as_deref(), Some("d1"));
    }

    #[test]
    fn identity_is_absent_rather_than_wrong_when_shape_differs() {
        let body = json!({"metadata": {"user_id": {"session_id": "s1"}}});
        assert_eq!(identity(Some(&body)), (None, None, None));
    }

    #[test]
    fn billing_variant_is_parsed_from_the_first_system_segment() {
        let text = "x-anthropic-billing-header: cc_version=2.1.220.85f; cc_entrypoint=cli;";
        assert_eq!(billing_variant(text).as_deref(), Some("2.1.220.85f"));
        assert!(billing_variant("no header here").is_none());
    }

    #[test]
    fn usage_reads_the_cache_ttl_split_and_thinking_tokens() {
        let message = json!({"usage": {
            "input_tokens": 10, "output_tokens": 161,
            "cache_creation_input_tokens": 10338, "cache_read_input_tokens": 86515,
            "cache_creation": {"ephemeral_1h_input_tokens": 10338, "ephemeral_5m_input_tokens": 0},
            "output_tokens_details": {"thinking_tokens": 133},
            "service_tier": "standard"
        }});
        let usage = usage(&message);
        assert_eq!(usage.cache_1h, Some(10338));
        assert_eq!(usage.cache_5m, Some(0));
        assert_eq!(usage.thinking, Some(133));
        assert_eq!(usage.service_tier.as_deref(), Some("standard"));
        assert_eq!(usage.ttl_source.as_deref(), Some("response"));
    }

    #[test]
    fn requested_cache_ttl_finds_markers_and_prefers_the_longer_one() {
        let body = json!({
            "system": [{"text": "s", "cache_control": {"type": "ephemeral", "ttl": "5m"}}],
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "t", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ]}]
        });
        assert_eq!(requested_cache_ttl(Some(&body)).as_deref(), Some("1h"));
        assert!(requested_cache_ttl(Some(&json!({"messages": []}))).is_none());
    }

    #[test]
    fn mcp_server_is_extracted_from_the_naming_convention() {
        assert_eq!(
            mcp_server("mcp__plugin_posthog_posthog__exec").as_deref(),
            Some("plugin_posthog_posthog")
        );
        assert!(mcp_server("Bash").is_none());
    }

    #[test]
    fn line_value_reads_the_injected_environment_block() {
        let system = "Here is useful information:\n - Primary working directory: /home/e/p\nCurrent branch: master\n";
        assert_eq!(line_value(system, "working directory: "), Some("/home/e/p"));
        assert_eq!(line_value(system, "Current branch: "), Some("master"));
        assert!(line_value(system, "Nonexistent: ").is_none());
    }

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        assert_eq!(fingerprint(&["a", "b"]), fingerprint(&["a", "b"]));
        assert_ne!(fingerprint(&["a", "b"]), fingerprint(&["b", "a"]));
        // Separator prevents concatenation collisions.
        assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
    }

    #[test]
    fn excerpt_truncates_on_char_boundaries() {
        assert_eq!(excerpt("héllo", 10), "héllo");
        assert_eq!(excerpt("héllo", 3), "hél…");
    }
}
