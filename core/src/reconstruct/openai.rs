//! Reassemble an OpenAI streaming response back into the object a
//! non-streaming call would have returned.
//!
//! Two wire formats share this module because both reach the proxy through the
//! same endpoint family:
//!
//! * **Chat Completions** — bare `data:` frames of `chat.completion.chunk`,
//!   each carrying `choices[].delta`, terminated by `data: [DONE]`. Text and
//!   tool-call arguments arrive as fragments to concatenate, indexed by
//!   `choices[].index` and `tool_calls[].index` respectively.
//! * **Responses** — named `response.*` events; the terminal
//!   `response.completed` carries the finished object outright, so there is
//!   nothing to assemble.
//!
//! Best-effort throughout, matching the Anthropic path: a truncated stream
//! yields whatever was assembled plus an error note.

use serde_json::{json, Map, Value};

use super::{parse_sse, Reconstructed};

/// One assistant choice under construction.
#[derive(Default)]
struct Choice {
    role: Option<String>,
    content: String,
    reasoning: String,
    finish_reason: Option<String>,
    /// Tool calls by their wire index; arguments arrive as fragments.
    tools: Vec<ToolCall>,
}

#[derive(Default, Clone)]
struct ToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

pub(super) fn reconstruct(raw: &str) -> Reconstructed {
    let mut out = Reconstructed::default();
    let mut errors: Vec<String> = Vec::new();
    let mut header: Map<String, Value> = Map::new();
    let mut choices: Vec<Choice> = Vec::new();
    let mut usage: Option<Value> = None;

    for event in parse_sse(raw) {
        let payload = event.data.trim();
        // The Chat Completions terminator is a sentinel, not JSON.
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let data: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(err) => {
                errors.push(format!("unparsable event data: {err}"));
                continue;
            }
        };

        // Responses API: the terminal event carries the whole object, so the
        // deltas that preceded it can be ignored entirely.
        let name = event
            .event
            .as_deref()
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        if name == "response.completed" || name == "response.incomplete" {
            if let Some(response) = data.get("response") {
                out.message = Some(response.clone());
                out.error = join(&errors);
                return out;
            }
        }

        // Chat Completions: carry the identity fields from whichever frame has
        // them, then fold each delta into its choice.
        for key in [
            "id",
            "model",
            "created",
            "system_fingerprint",
            "service_tier",
        ] {
            if let Some(value) = data.get(key) {
                header
                    .entry(key.to_owned())
                    .or_insert_with(|| value.clone());
            }
        }
        if let Some(reported) = data.get("usage").filter(|value| !value.is_null()) {
            usage = Some(reported.clone());
        }
        let Some(frame) = data.get("choices").and_then(Value::as_array) else {
            continue;
        };
        for choice in frame {
            let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if choices.len() <= index {
                choices.resize_with(index + 1, Choice::default);
            }
            apply(&mut choices[index], choice);
        }
    }

    if header.is_empty() && choices.is_empty() {
        out.error = join(&errors).or_else(|| Some("no assembled response".to_owned()));
        return out;
    }

    let assembled: Vec<Value> = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| finish(index, choice))
        .collect();
    header.insert("object".into(), json!("chat.completion"));
    header.insert("choices".into(), Value::Array(assembled));
    if let Some(usage) = usage {
        header.insert("usage".into(), usage);
    }
    out.message = Some(Value::Object(header));
    out.error = join(&errors);
    out
}

/// Fold one streamed choice frame into the choice being assembled.
fn apply(choice: &mut Choice, frame: &Value) {
    if let Some(reason) = frame.get("finish_reason").and_then(Value::as_str) {
        choice.finish_reason = Some(reason.to_owned());
    }
    let Some(delta) = frame.get("delta") else {
        return;
    };
    if let Some(role) = delta.get("role").and_then(Value::as_str) {
        choice.role = Some(role.to_owned());
    }
    if let Some(text) = delta.get("content").and_then(Value::as_str) {
        choice.content.push_str(text);
    }
    // Reasoning models stream their summary under a separate key.
    for key in ["reasoning_content", "reasoning"] {
        if let Some(text) = delta.get(key).and_then(Value::as_str) {
            choice.reasoning.push_str(text);
        }
    }
    let Some(tools) = delta.get("tool_calls").and_then(Value::as_array) else {
        return;
    };
    for tool in tools {
        let index = tool.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if choice.tools.len() <= index {
            choice.tools.resize(index + 1, ToolCall::default());
        }
        let slot = &mut choice.tools[index];
        if let Some(id) = tool.get("id").and_then(Value::as_str) {
            slot.id = Some(id.to_owned());
        }
        if let Some(function) = tool.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                slot.name = Some(name.to_owned());
            }
            if let Some(args) = function.get("arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
    }
}

/// Render an assembled choice in the shape a non-streaming call returns.
fn finish(index: usize, choice: &Choice) -> Value {
    let mut message = Map::new();
    message.insert(
        "role".into(),
        json!(choice.role.clone().unwrap_or_else(|| "assistant".into())),
    );
    message.insert("content".into(), json!(choice.content));
    if !choice.reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(choice.reasoning));
    }
    if !choice.tools.is_empty() {
        let tools: Vec<Value> = choice
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "id": tool.id,
                    "type": "function",
                    "function": {"name": tool.name, "arguments": tool.arguments},
                })
            })
            .collect();
        message.insert("tool_calls".into(), Value::Array(tools));
    }
    json!({
        "index": index,
        "message": Value::Object(message),
        "finish_reason": choice.finish_reason,
    })
}

fn join(errors: &[String]) -> Option<String> {
    (!errors.is_empty()).then(|| errors.join("; "))
}

#[cfg(test)]
mod tests {
    use super::super::reconstruct as dispatch;

    #[test]
    fn assembles_streamed_text_and_usage() {
        let raw = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        );
        let out = dispatch(raw);
        let message = out.message.expect("a message should assemble");
        assert_eq!(message["id"], "chatcmpl-1");
        assert_eq!(message["model"], "gpt-x");
        assert_eq!(message["choices"][0]["message"]["content"], "Hello");
        assert_eq!(message["choices"][0]["finish_reason"], "stop");
        assert_eq!(message["usage"]["prompt_tokens"], 9);
        assert!(out.error.is_none());
    }

    #[test]
    fn concatenates_tool_call_arguments_across_frames() {
        let raw = concat!(
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\"}}]}}]}\n\n",
            "data: {\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let message = dispatch(raw).message.unwrap();
        let tool = &message["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tool["id"], "call_1");
        assert_eq!(tool["function"]["name"], "lookup");
        // Fragments concatenate into valid JSON, which is the whole point.
        assert_eq!(tool["function"]["arguments"], "{\"q\":1}");
        assert_eq!(message["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn responses_api_uses_the_terminal_object_verbatim() {
        let raw = concat!(
            "event: response.output_text.delta\ndata: {\"delta\":\"ignored\"}\n\n",
            "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":4}}}\n\n",
        );
        let message = dispatch(raw).message.unwrap();
        assert_eq!(message["id"], "resp_1");
        assert_eq!(message["usage"]["input_tokens"], 4);
    }

    #[test]
    fn a_truncated_stream_keeps_what_it_assembled() {
        let raw = concat!(
            "data: {\"id\":\"chatcmpl-2\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        );
        let out = dispatch(raw);
        let message = out.message.unwrap();
        assert_eq!(message["choices"][0]["message"]["content"], "partial");
        // No finish_reason: the stream never said it was done.
        assert!(message["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn an_anthropic_stream_still_routes_to_the_anthropic_path() {
        let raw = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[]}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let message = dispatch(raw).message.unwrap();
        assert_eq!(message["id"], "msg_1");
    }
}
