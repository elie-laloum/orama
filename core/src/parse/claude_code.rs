use serde_json::Value;

use crate::store::StoredCall;

use super::{model::*, signals, ProviderParser};

pub(super) struct ClaudeCodeParser;

impl ProviderParser for ClaudeCodeParser {
    fn parse(&self, call: &StoredCall) -> NormalizedCall {
        let record = &call.record;
        let body = record.request_body.as_ref();
        let system = body.map(system_segments).unwrap_or_default();
        let declared_tools = body.map(tool_declarations).unwrap_or_default();
        let mut thread: Vec<Turn> = body
            .and_then(|body| body.get("messages"))
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .map(|message| parse_turn(message, Origin::History))
                    .collect()
            })
            .unwrap_or_default();
        let response = response_message(record);
        if let Some(response) = response {
            thread.push(parse_turn(response, Origin::New));
        }
        let usage = response.map(usage_of).unwrap_or_default();
        let intra = signals::intra_signals(&thread, &declared_tools);
        NormalizedCall {
            id: call.id,
            provider: Provider::ClaudeCode,
            model: body
                .and_then(|body| body.get("model"))
                .and_then(Value::as_str)
                .map(String::from),
            session_key: session_key(&record.request_headers, body),
            timestamps: Timestamps {
                start: record.timestamp_start.clone(),
                first_chunk: record.timestamp_first_chunk.clone(),
                end: record.timestamp_end.clone(),
            },
            system,
            declared_tools,
            thread,
            usage,
            intra,
            response_status: record.response_status,
            error: record.error.clone(),
            raw_fallback: false,
        }
    }
}

/// The provider's assembled response message: reconstructed from the SSE tee for
/// streamed calls, captured verbatim for non-streaming ones. Both carry the same
/// Anthropic Message shape, so everything downstream parses identically.
///
/// Error payloads (`{"type":"error",…}`) are deliberately excluded: they are not
/// an assistant turn and must not enter the conversation thread. They stay
/// available through the raw capture.
fn response_message(record: &crate::store::CallRecord) -> Option<&Value> {
    let candidate = record
        .response_reconstructed
        .as_ref()
        .or(record.response_body.as_ref())?;
    match candidate.get("type").and_then(Value::as_str) {
        Some("message") | None => Some(candidate),
        Some(_) => None,
    }
}

fn session_key(headers: &Value, body: Option<&Value>) -> Option<String> {
    headers
        .as_object()
        .and_then(|headers| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("x-claude-code-session-id"))
                .and_then(|(_, value)| value.as_str())
                .map(String::from)
        })
        .or_else(|| {
            body.and_then(|body| body.pointer("/metadata/user_id/session_id"))
                .and_then(Value::as_str)
                .map(String::from)
        })
}

fn system_segments(body: &Value) -> Vec<SystemSegment> {
    let Some(system) = body.get("system") else {
        return Vec::new();
    };
    match system {
        Value::String(text) => vec![SystemSegment {
            text: text.clone(),
            approx_size: ApproxSize::of_text(text),
            cache_control: false,
        }],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let text = block.get("text").and_then(Value::as_str)?;
                Some(SystemSegment {
                    text: text.to_string(),
                    approx_size: ApproxSize::of_text(text),
                    cache_control: block.get("cache_control").is_some(),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_declarations(body: &Value) -> Vec<ToolDecl> {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            Some(ToolDecl {
                name: tool.get("name")?.as_str()?.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(String::from),
                input_schema: tool.get("input_schema").cloned(),
            })
        })
        .collect()
}

fn parse_turn(value: &Value, origin: Origin) -> Turn {
    let role = match value.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        _ => Role::Other,
    };
    let blocks = match value.get("content") {
        Some(Value::String(text)) => vec![text_block(text)],
        Some(Value::Array(blocks)) => blocks.iter().map(parse_block).collect(),
        Some(other) => vec![other_block(other)],
        None => Vec::new(),
    };
    Turn {
        role,
        origin,
        blocks,
    }
}

fn parse_block(value: &Value) -> Block {
    let typ = value.get("type").and_then(Value::as_str).unwrap_or("");
    match typ {
        "text" => text_block(value.get("text").and_then(Value::as_str).unwrap_or("")),
        "thinking" => Block {
            kind: BlockKind::Thinking,
            content_tag: None,
            content: value
                .get("thinking")
                .and_then(Value::as_str)
                .map(String::from),
            approx_size: ApproxSize::of_value(value),
            tool_name: None,
            input: None,
            tool_use_id: None,
            is_error: None,
        },
        "tool_use" => Block {
            kind: BlockKind::ToolUse,
            content_tag: None,
            content: None,
            approx_size: ApproxSize::of_value(value),
            tool_name: value.get("name").and_then(Value::as_str).map(String::from),
            input: value.get("input").cloned(),
            tool_use_id: value.get("id").and_then(Value::as_str).map(String::from),
            is_error: None,
        },
        "tool_result" => Block {
            kind: BlockKind::ToolResult,
            content_tag: None,
            content: content_text(value.get("content")),
            approx_size: ApproxSize::of_value(value),
            tool_name: None,
            input: None,
            tool_use_id: value
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(String::from),
            is_error: value.get("is_error").and_then(Value::as_bool),
        },
        "image" => Block {
            kind: BlockKind::Image,
            content_tag: None,
            content: None,
            approx_size: ApproxSize::of_value(value),
            tool_name: None,
            input: None,
            tool_use_id: None,
            is_error: None,
        },
        _ => other_block(value),
    }
}

fn text_block(text: &str) -> Block {
    let (content, content_tag) = tagged_text(text);
    Block {
        kind: BlockKind::Text,
        content_tag,
        approx_size: ApproxSize::of_text(&content),
        content: Some(content),
        tool_name: None,
        input: None,
        tool_use_id: None,
        is_error: None,
    }
}
fn other_block(value: &Value) -> Block {
    Block {
        kind: BlockKind::Other,
        content_tag: None,
        approx_size: ApproxSize::of_value(value),
        content: Some(value.to_string()),
        tool_name: None,
        input: None,
        tool_use_id: None,
        is_error: None,
    }
}
fn tagged_text(text: &str) -> (String, Option<ContentTag>) {
    const TAGS: [(&str, ContentTag); 6] = [
        ("system-reminder", ContentTag::SystemReminder),
        ("local-command-caveat", ContentTag::LocalCommandCaveat),
        ("command-name", ContentTag::CommandName),
        ("command-message", ContentTag::CommandMessage),
        ("command-args", ContentTag::CommandArgs),
        ("local-command-stdout", ContentTag::LocalCommandStdout),
    ];
    let trimmed = text.trim();
    for (name, tag) in TAGS {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        if let Some(inner) = trimmed
            .strip_prefix(&open)
            .and_then(|value| value.strip_suffix(&close))
        {
            return (inner.trim().to_owned(), Some(tag));
        }
        // Some injected blocks are opening tags followed by content but no
        // closing tag. Preserve their payload while still classifying them.
        if let Some(inner) = trimmed.strip_prefix(&open) {
            return (inner.trim().to_owned(), Some(tag));
        }
    }
    (text.to_owned(), None)
}

fn content_text(value: Option<&Value>) -> Option<String> {
    value.map(|content| match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    })
}
fn usage_of(response: &Value) -> Usage {
    let usage = response.get("usage");
    Usage {
        input: number(usage, "input_tokens"),
        output: number(usage, "output_tokens"),
        cache_creation: number(usage, "cache_creation_input_tokens"),
        cache_read: number(usage, "cache_read_input_tokens"),
    }
}
fn number(usage: Option<&Value>, field: &str) -> Option<u64> {
    usage
        .and_then(|usage| usage.get(field))
        .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CallRecord, StoredCall};
    use serde_json::json;

    #[test]
    fn normalizes_claude_code_thread_tools_and_usage() {
        let call = StoredCall {
            id: 7,
            record: CallRecord {
                timestamp_start: "start".into(),
                request_headers: json!({"x-app":"cli", "x-claude-code-session-id":"session-1"}),
                request_body: Some(
                    json!({"model":"claude", "system":[{"type":"text","text":"rules", "cache_control":{"type":"ephemeral"}}], "tools":[{"name":"read", "input_schema":{"type":"object"}}], "messages":[{"role":"user","content":"hello"},{"role":"assistant","content":[{"type":"tool_use","id":"use-1","name":"read","input":{"path":"x"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"use-1","content":"file"}]}]}),
                ),
                response_reconstructed: Some(
                    json!({"role":"assistant", "content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"done"}], "usage":{"input_tokens":10,"output_tokens":3,"cache_creation_input_tokens":2,"cache_read_input_tokens":4}}),
                ),
                ..Default::default()
            },
        };
        let normalized = ClaudeCodeParser.parse(&call);
        assert_eq!(normalized.session_key.as_deref(), Some("session-1"));
        assert!(normalized.system[0].cache_control);
        assert_eq!(normalized.thread.len(), 4);
        assert!(matches!(normalized.thread[3].origin, Origin::New));
        assert_eq!(normalized.usage.cache_read, Some(4));
        assert_eq!(
            normalized.intra.tool_calls[0]
                .result
                .as_ref()
                .unwrap()
                .content
                .as_deref(),
            Some("file")
        );
    }
}
