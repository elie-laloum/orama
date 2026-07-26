//! Normalization for OpenAI-dialect traffic — Codex, opencode, and anything
//! else speaking Chat Completions or the Responses API.
//!
//! This exists to prove the provider abstraction with a second real dialect.
//! Everything it produces lands in the same [`NormalizedCall`] the Anthropic
//! parser produces, so detectors and the derived layer never learn which
//! provider a call came from.
//!
//! The two dialects differ enough to matter: Chat Completions puts the whole
//! conversation in `messages[]` with tool results as `role: "tool"` turns and
//! reports usage as `prompt_tokens`/`completion_tokens`; the Responses API uses
//! `input[]`/`output[]` items and reports `input_tokens`/`output_tokens`. Both
//! are folded into one shape here.

use serde_json::Value;

use crate::store::StoredCall;

use super::{model::*, signals, ProviderParser};

pub(super) struct OpenAiParser;

impl ProviderParser for OpenAiParser {
    fn parse(&self, call: &StoredCall) -> NormalizedCall {
        let record = &call.record;
        let body = record.request_body.as_ref();
        let Request {
            system,
            declared_tools,
            turns,
        } = body.map(split_request).unwrap_or_default();

        let mut thread: Vec<Turn> = turns.iter().map(request_turn).collect();

        let response = record
            .response_reconstructed
            .as_ref()
            .or(record.response_body.as_ref());
        if let Some(response) = response {
            if let Some(turn) = response_turn(response) {
                thread.push(turn);
            }
        }

        let intra = signals::intra_signals(&thread, &declared_tools);
        NormalizedCall {
            id: call.id,
            provider: Provider::OpenAi,
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
            usage: response.map(usage_of).unwrap_or_default(),
            intra,
            response_status: record.response_status,
            error: record.error.clone(),
            raw_fallback: false,
        }
    }
}

/// The session a call belongs to.
///
/// OpenAI has no session header of its own, so this leans on what the harnesses
/// send: Codex forwards a session id, and the Responses API threads state
/// through `previous_response_id`.
///
/// `session-id` leads because that is what Codex actually sends — the
/// underscore and `x-`-prefixed spellings below were guesses that no real
/// client uses, and while they were the only candidates every Codex call
/// derived with a null `session_id`. That left the capture invisible to every
/// surface keyed on sessions, which is most of them.
fn session_key(headers: &Value, body: Option<&Value>) -> Option<String> {
    let header = |name: &str| {
        headers
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
            .map(String::from)
    };
    header("session-id")
        // One Codex run carries the same value in both; `thread-id` survives if
        // the run identifier ever stops being sent.
        .or_else(|| header("thread-id"))
        .or_else(|| header("session_id"))
        .or_else(|| header("x-session-id"))
        .or_else(|| header("x-codex-session-id"))
        .or_else(|| {
            body.and_then(|body| body.get("conversation"))
                .and_then(Value::as_str)
                .map(String::from)
        })
}

/// A request split into what the harness declares and what the conversation
/// says. The three are disjoint by construction: a system segment is never also
/// a turn, so `system_chars` and `context_chars` cannot double-count it.
#[derive(Default)]
struct Request<'a> {
    system: Vec<SystemSegment>,
    declared_tools: Vec<ToolDecl>,
    turns: &'a [Value],
}

/// Separate the harness from the conversation.
///
/// Chat Completions puts instructions in `messages[]` and tools at the top
/// level. The Responses API has an `instructions` field — but Codex uses
/// neither: it packs its whole harness into the leading items of `input[]`, as
/// `developer` messages plus one `additional_tools` envelope. Read only as
/// turns, those 30k of prompt and 24k of tool schemas were reported as
/// conversation, and the harness looked empty.
///
/// The boundary is the *leading run*: the first item that is not
/// `system`/`developer` opens the conversation, and a system turn appearing
/// after that stays a turn. It is injected content rather than the harness, and
/// folding it in would give the prompt a different fingerprint on every call —
/// one Harness row per call instead of one per harness.
fn split_request(body: &Value) -> Request<'_> {
    let mut system = Vec::new();
    let mut declared_tools = tool_declarations(body.get("tools"));

    if let Some(text) = body.get("instructions").and_then(Value::as_str) {
        system.push(system_segment(text));
    }

    let items = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    let mut start = 0;
    for item in items {
        let role = item.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "system" && role != "developer" {
            break;
        }
        start += 1;
        // A declaration envelope, not a segment: it carries `role: developer`
        // but no content, so treating it as one would emit an empty segment
        // ahead of the real prompt and leave the harness unidentifiable.
        if item.get("type").and_then(Value::as_str) == Some("additional_tools") {
            declared_tools.extend(tool_declarations(item.get("tools")));
            continue;
        }
        let text = content_text(item.get("content")).unwrap_or_default();
        if !text.is_empty() {
            system.push(system_segment(&text));
        }
    }

    Request {
        system,
        declared_tools,
        turns: &items[start..],
    }
}

/// OpenAI has no `cache_control` marker — the provider caches implicitly and the
/// client never declares a breakpoint.
fn system_segment(text: &str) -> SystemSegment {
    SystemSegment {
        text: text.to_owned(),
        approx_size: ApproxSize::of_text(text),
        cache_control: false,
    }
}

/// Tool declarations, in either the flat or the nested `function` shape.
///
/// Codex groups related tools under a `namespace` entry whose own `tools` array
/// holds the real functions, so this flattens rather than counting the group as
/// one declaration.
fn tool_declarations(tools: Option<&Value>) -> Vec<ToolDecl> {
    tools
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|tool| {
            if tool.get("type").and_then(Value::as_str) == Some("namespace") {
                return tool_declarations(tool.get("tools"));
            }
            // Chat Completions nests under `function`; Responses is flat.
            let spec = tool.get("function").unwrap_or(tool);
            let Some(name) = spec.get("name").and_then(Value::as_str) else {
                return Vec::new();
            };
            vec![ToolDecl {
                name: name.to_owned(),
                description: spec
                    .get("description")
                    .and_then(Value::as_str)
                    .map(String::from),
                input_schema: spec
                    .get("parameters")
                    .or_else(|| spec.get("input_schema"))
                    // A `custom` tool states its contract as a grammar instead
                    // of a JSON schema. Those are bytes on the wire too.
                    .or_else(|| spec.get("format"))
                    .cloned(),
            }]
        })
        .collect()
}

/// One turn from the request side of the conversation.
fn request_turn(item: &Value) -> Turn {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    let role = match item.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        Some("system") | Some("developer") => Role::System,
        _ if kind == "function_call_output" => Role::Tool,
        _ => Role::Other,
    };

    let mut blocks = Vec::new();

    // Responses API items carry their kind rather than a role.
    match kind {
        "function_call" => {
            blocks.push(Block {
                kind: BlockKind::ToolUse,
                content_tag: None,
                approx_size: ApproxSize::of_value(item),
                content: None,
                tool_name: item.get("name").and_then(Value::as_str).map(String::from),
                input: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok()),
                tool_use_id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(String::from),
                is_error: None,
            });
            return Turn {
                role: Role::Assistant,
                origin: Origin::History,
                blocks,
            };
        }
        "function_call_output" => {
            let text = content_text(item.get("output")).unwrap_or_default();
            blocks.push(tool_result_block(
                text,
                item.get("call_id").and_then(Value::as_str),
            ));
            return Turn {
                role: Role::Tool,
                origin: Origin::History,
                blocks,
            };
        }
        _ => {}
    }

    // A Chat Completions `role: "tool"` turn is a tool result.
    if matches!(role, Role::Tool) {
        let text = content_text(item.get("content")).unwrap_or_default();
        blocks.push(tool_result_block(
            text,
            item.get("tool_call_id").and_then(Value::as_str),
        ));
        return Turn {
            role,
            origin: Origin::History,
            blocks,
        };
    }

    if let Some(text) = content_text(item.get("content")) {
        if !text.is_empty() {
            blocks.push(text_block(&text));
        }
    }
    blocks.extend(tool_use_blocks(item.get("tool_calls")));
    Turn {
        role,
        origin: Origin::History,
        blocks,
    }
}

/// The assistant turn, from an assembled Chat Completions or Responses object.
fn response_turn(response: &Value) -> Option<Turn> {
    let mut blocks = Vec::new();

    // Chat Completions: the first choice's message.
    if let Some(message) = response.pointer("/choices/0/message") {
        if let Some(text) = content_text(message.get("content")) {
            if !text.is_empty() {
                blocks.push(text_block(&text));
            }
        }
        if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
            blocks.push(Block {
                kind: BlockKind::Thinking,
                content_tag: None,
                approx_size: ApproxSize::of_text(reasoning),
                content: Some(reasoning.to_owned()),
                tool_name: None,
                input: None,
                tool_use_id: None,
                is_error: None,
            });
        }
        blocks.extend(tool_use_blocks(message.get("tool_calls")));
    }

    // Responses API: walk the output items.
    for item in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(text) = content_text(item.get("content")) {
                    if !text.is_empty() {
                        blocks.push(text_block(&text));
                    }
                }
            }
            Some("reasoning") => {
                let text = content_text(item.get("summary")).unwrap_or_default();
                blocks.push(Block {
                    kind: BlockKind::Thinking,
                    content_tag: None,
                    approx_size: ApproxSize::of_value(item),
                    content: Some(text),
                    tool_name: None,
                    input: None,
                    tool_use_id: None,
                    is_error: None,
                });
            }
            Some("function_call") => blocks.push(Block {
                kind: BlockKind::ToolUse,
                content_tag: None,
                approx_size: ApproxSize::of_value(item),
                content: None,
                tool_name: item.get("name").and_then(Value::as_str).map(String::from),
                input: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok()),
                tool_use_id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(String::from),
                is_error: None,
            }),
            _ => {}
        }
    }

    (!blocks.is_empty()).then_some(Turn {
        role: Role::Assistant,
        origin: Origin::New,
        blocks,
    })
}

fn tool_use_blocks(tool_calls: Option<&Value>) -> Vec<Block> {
    tool_calls
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            let function = call.get("function").unwrap_or(call);
            Block {
                kind: BlockKind::ToolUse,
                content_tag: None,
                approx_size: ApproxSize::of_value(call),
                content: None,
                tool_name: function
                    .get("name")
                    .and_then(Value::as_str)
                    .map(String::from),
                // Arguments arrive as a JSON *string*, so they need decoding
                // before they are an object.
                input: function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok()),
                tool_use_id: call.get("id").and_then(Value::as_str).map(String::from),
                is_error: None,
            }
        })
        .collect()
}

fn tool_result_block(text: String, id: Option<&str>) -> Block {
    Block {
        kind: BlockKind::ToolResult,
        content_tag: None,
        approx_size: ApproxSize::of_text(&text),
        content: Some(text),
        tool_name: None,
        input: None,
        tool_use_id: id.map(String::from),
        // OpenAI has no is_error flag on a tool result; absent, not false.
        is_error: None,
    }
}

fn text_block(text: &str) -> Block {
    Block {
        kind: BlockKind::Text,
        content_tag: None,
        approx_size: ApproxSize::of_text(text),
        content: Some(text.to_owned()),
        tool_name: None,
        input: None,
        tool_use_id: None,
        is_error: None,
    }
}

/// Flatten a content field that may be a string or an array of typed parts.
fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let joined: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.as_str())
                })
                .collect();
            Some(joined.join(""))
        }
        other => Some(other.to_string()),
    }
}

/// Token usage, from either dialect's naming.
///
/// `cache_creation` has no OpenAI analogue — the provider caches implicitly and
/// never reports a write — so it stays absent rather than being set to zero.
fn usage_of(response: &Value) -> Usage {
    let Some(usage) = response.get("usage") else {
        return Usage::default();
    };
    let field = |name: &str| usage.get(name).and_then(Value::as_u64);
    Usage {
        input: field("prompt_tokens").or_else(|| field("input_tokens")),
        output: field("completion_tokens").or_else(|| field("output_tokens")),
        cache_creation: None,
        cache_read: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .or_else(|| usage.pointer("/input_tokens_details/cached_tokens"))
            .and_then(Value::as_u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CallRecord;
    use serde_json::json;

    fn call(body: Value, response: Value) -> StoredCall {
        StoredCall {
            id: 1,
            record: CallRecord {
                timestamp_start: "2026-07-26T10:00:00Z".into(),
                method: "POST".into(),
                url: "/v1/chat/completions".into(),
                request_headers: json!({"user-agent": "codex_cli_rs/1.0"}),
                request_body: Some(body),
                response_status: Some(200),
                response_body: Some(response),
                ..Default::default()
            },
        }
    }

    #[test]
    fn normalizes_a_chat_completions_exchange() {
        let stored = call(
            json!({
                "model": "gpt-x",
                "messages": [
                    {"role": "system", "content": "Be terse."},
                    {"role": "user", "content": "What is 2+2?"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "call_1", "type": "function",
                         "function": {"name": "calc", "arguments": "{\"expr\":\"2+2\"}"}}
                    ]},
                    {"role": "tool", "tool_call_id": "call_1", "content": "4"}
                ],
                "tools": [{"type": "function", "function": {
                    "name": "calc", "description": "evaluate",
                    "parameters": {"type": "object"}}}]
            }),
            json!({
                "choices": [{"message": {"role": "assistant", "content": "4"},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 40, "completion_tokens": 3,
                          "prompt_tokens_details": {"cached_tokens": 32}}
            }),
        );

        let parsed = OpenAiParser.parse(&stored);
        assert_eq!(parsed.provider, Provider::OpenAi);
        assert_eq!(parsed.model.as_deref(), Some("gpt-x"));
        assert_eq!(parsed.system.len(), 1);
        assert_eq!(parsed.declared_tools[0].name, "calc");

        // Usage maps onto the same shape the Anthropic parser produces.
        assert_eq!(parsed.usage.input, Some(40));
        assert_eq!(parsed.usage.output, Some(3));
        assert_eq!(parsed.usage.cache_read, Some(32));
        // OpenAI never reports a cache write, so it is absent, not zero.
        assert_eq!(parsed.usage.cache_creation, None);

        // The tool call and its result pair up through the shared signal code.
        let tool = &parsed.intra.tool_calls[0];
        assert_eq!(tool.name, "calc");
        assert_eq!(tool.input.as_ref().unwrap()["expr"], "2+2");
        assert!(tool.result.is_some());
    }

    #[test]
    fn normalizes_a_responses_api_exchange() {
        let stored = call(
            json!({
                "model": "gpt-x",
                "instructions": "Be terse.",
                "input": [
                    {"role": "user", "content": [{"type": "input_text", "text": "hi"}]},
                    {"type": "function_call", "call_id": "fc_1", "name": "lookup",
                     "arguments": "{\"q\":1}"},
                    {"type": "function_call_output", "call_id": "fc_1", "output": "done"}
                ]
            }),
            json!({
                "output": [
                    {"type": "reasoning", "summary": "considered options"},
                    {"type": "message", "content": [{"type": "output_text", "text": "hello"}]}
                ],
                "usage": {"input_tokens": 12, "output_tokens": 4,
                          "input_tokens_details": {"cached_tokens": 8}}
            }),
        );

        let parsed = OpenAiParser.parse(&stored);
        assert_eq!(parsed.system[0].text, "Be terse.");
        assert_eq!(parsed.usage.input, Some(12));
        assert_eq!(parsed.usage.cache_read, Some(8));

        let last = parsed.thread.last().unwrap();
        assert!(matches!(last.origin, Origin::New));
        assert!(last
            .blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Thinking)));
        assert!(last
            .blocks
            .iter()
            .any(|block| block.content.as_deref() == Some("hello")));

        // A function_call_output is a tool result, not an ordinary turn.
        assert!(parsed
            .thread
            .iter()
            .flat_map(|turn| &turn.blocks)
            .any(|block| matches!(block.kind, BlockKind::ToolResult)));
    }

    /// The shape Codex actually sends: no `instructions`, no top-level `tools`,
    /// and the whole harness packed into the leading `input[]` items.
    #[test]
    fn codex_packs_its_harness_into_the_leading_input_items() {
        let stored = call(
            json!({
                "model": "gpt-5.6-luna",
                "input": [
                    {"role": "developer", "type": "additional_tools", "tools": [
                        {"type": "custom", "name": "exec", "description": "run js",
                         "format": {"type": "grammar", "syntax": "lark", "definition": "start: x"}},
                        {"type": "function", "name": "wait", "description": "wait",
                         "parameters": {"type": "object"}},
                        {"type": "namespace", "name": "collaboration", "description": "group",
                         "tools": [
                            {"type": "function", "name": "spawn_agent", "parameters": {}},
                            {"type": "function", "name": "wait_agent", "parameters": {}}
                         ]}
                    ]},
                    {"role": "developer", "type": "message",
                     "content": [{"type": "input_text", "text": "You are Codex."}]},
                    {"role": "developer", "type": "message", "content": [
                        {"type": "input_text", "text": "AGENTS.md says "},
                        {"type": "input_text", "text": "be terse."}
                    ]},
                    {"role": "user", "type": "message",
                     "content": [{"type": "input_text", "text": "who are you"}]}
                ]
            }),
            json!({"output": [{"type": "message",
                               "content": [{"type": "output_text", "text": "Codex."}]}]}),
        );

        let parsed = OpenAiParser.parse(&stored);

        // The two developer messages are the prompt. The tool envelope carries
        // no content, so it contributes no segment.
        assert_eq!(parsed.system.len(), 2);
        assert_eq!(parsed.system[0].text, "You are Codex.");
        assert_eq!(parsed.system[1].text, "AGENTS.md says be terse.");

        // A namespace is a grouping, not a declaration: its members are.
        let names: Vec<&str> = parsed
            .declared_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, ["exec", "wait", "spawn_agent", "wait_agent"]);
        // A `custom` tool declares its contract as a grammar, not a schema.
        assert!(parsed.declared_tools[0].input_schema.is_some());

        // Only the conversation is a conversation: one user turn plus the
        // response. Counting the harness here is what reported a 31k request as
        // 100% conversation with an empty system prompt.
        assert_eq!(parsed.thread.len(), 2);
        assert!(matches!(parsed.thread[0].role, Role::User));
        assert!(matches!(parsed.thread[1].origin, Origin::New));
    }

    /// The split is the *leading* run, so injected content stays conversation.
    #[test]
    fn a_system_turn_after_the_conversation_starts_is_not_the_harness() {
        let stored = call(
            json!({
                "model": "gpt-x",
                "messages": [
                    {"role": "system", "content": "Be terse."},
                    {"role": "user", "content": "hi"},
                    {"role": "system", "content": "skill instructions, injected"},
                    {"role": "user", "content": "again"}
                ]
            }),
            json!({"choices": []}),
        );

        let parsed = OpenAiParser.parse(&stored);
        assert_eq!(
            parsed.system.len(),
            1,
            "only the leading run is the harness"
        );
        assert_eq!(parsed.system[0].text, "Be terse.");
        assert_eq!(parsed.thread.len(), 3);
        assert!(
            matches!(parsed.thread[1].role, Role::System),
            "an inline system turn keeps its role: {:?}",
            parsed.thread[1].role
        );
    }

    #[test]
    fn a_response_with_no_usage_leaves_counts_absent() {
        let stored = call(json!({"model": "gpt-x"}), json!({"choices": []}));
        let parsed = OpenAiParser.parse(&stored);
        assert_eq!(parsed.usage.input, None);
        assert_eq!(parsed.usage.output, None);
        assert!(!parsed.raw_fallback);
    }
}
