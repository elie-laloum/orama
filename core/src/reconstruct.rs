//! Reassemble an Anthropic Messages *streaming* response (SSE) back into the
//! single JSON object a non-streaming call would have returned.
//!
//! The Anthropic streaming protocol emits this event sequence:
//!
//! ```text
//! message_start          -> { message: { id, model, role, content: [], usage, ... } }
//! content_block_start    -> { index, content_block: { type, ... } }
//! content_block_delta    -> { index, delta: { type: text_delta|input_json_delta|thinking_delta|... } }
//! content_block_stop     -> { index }
//! message_delta          -> { delta: { stop_reason, stop_sequence }, usage: { output_tokens } }
//! message_stop
//! ```
//!
//! We seed the message object from `message_start`, grow each content block from
//! its deltas, then fold in the final `message_delta` (stop reason + output
//! token count). Reconstruction is best-effort: a partial or malformed stream
//! yields whatever could be assembled plus an `error` string.

use serde_json::{json, Map, Value};

/// Outcome of reconstructing a raw SSE stream.
#[derive(Debug, Clone, Default)]
pub struct Reconstructed {
    /// The assembled message object (shape matches a non-streaming response).
    /// `None` when not even a `message_start` was seen.
    pub message: Option<Value>,
    /// A best-effort error note when the stream was partial/malformed.
    pub error: Option<String>,
}

/// One parsed SSE event: an optional `event:` name and its `data:` payload.
struct SseEvent {
    event: Option<String>,
    data: String,
}

/// Split a raw SSE byte string into events. Events are separated by a blank
/// line; `data:` lines within one event are concatenated with newlines per the
/// SSE spec.
fn parse_sse(raw: &str) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let mut cur_event: Option<String> = None;
    let mut cur_data: Vec<String> = Vec::new();

    let flush = |event: &mut Option<String>, data: &mut Vec<String>, out: &mut Vec<SseEvent>| {
        if !data.is_empty() || event.is_some() {
            out.push(SseEvent {
                event: event.take(),
                data: data.join("\n"),
            });
            data.clear();
        }
    };

    // Normalise CRLF, then walk line by line.
    for line in raw.replace("\r\n", "\n").split('\n') {
        if line.is_empty() {
            flush(&mut cur_event, &mut cur_data, &mut events);
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            cur_event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            cur_data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
        // Other SSE fields (id:, retry:, comments) are ignored.
    }
    flush(&mut cur_event, &mut cur_data, &mut events);
    events
}

/// Reconstruct the assembled message JSON from a raw SSE stream.
pub fn reconstruct(raw: &str) -> Reconstructed {
    let mut out = Reconstructed::default();
    let events = parse_sse(raw);

    // The message under construction and its content blocks by index.
    let mut message: Option<Map<String, Value>> = None;
    let mut blocks: Vec<Value> = Vec::new();
    // Per-block accumulator for input_json_delta partial JSON strings.
    let mut json_accum: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for ev in &events {
        // Derive the event type from the `event:` name, falling back to the
        // `type` field inside the data payload.
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) if ev.data.trim().is_empty() => continue,
            Err(e) => {
                errors.push(format!("unparseable event data: {e}"));
                continue;
            }
        };
        let etype = ev
            .event
            .clone()
            .or_else(|| data.get("type").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_default();

        match etype.as_str() {
            "message_start" => {
                if let Some(msg) = data.get("message").and_then(|m| m.as_object()) {
                    message = Some(msg.clone());
                }
            }
            "content_block_start" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let block = data
                    .get("content_block")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                ensure_len(&mut blocks, index, &mut json_accum);
                blocks[index] = block;
            }
            "content_block_delta" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                ensure_len(&mut blocks, index, &mut json_accum);
                if let Some(delta) = data.get("delta") {
                    apply_delta(&mut blocks[index], &mut json_accum[index], delta);
                }
            }
            "content_block_stop" => {
                let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                // Finalise an accumulated input_json_delta into `input`.
                if let Some(acc) = json_accum.get(index) {
                    if !acc.is_empty() {
                        if let Some(block) = blocks.get_mut(index) {
                            match serde_json::from_str::<Value>(acc) {
                                Ok(v) => {
                                    if let Some(obj) = block.as_object_mut() {
                                        obj.insert("input".to_string(), v);
                                    }
                                }
                                Err(e) => errors.push(format!(
                                    "tool input JSON (block {index}) did not parse: {e}"
                                )),
                            }
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(msg) = message.as_mut() {
                    if let Some(delta) = data.get("delta").and_then(|d| d.as_object()) {
                        for (k, v) in delta {
                            msg.insert(k.clone(), v.clone());
                        }
                    }
                    // Merge usage (output_tokens arrives here).
                    if let Some(usage) = data.get("usage").and_then(|u| u.as_object()) {
                        merge_usage(msg, usage);
                    }
                }
            }
            "message_stop" => {}
            "error" => {
                errors.push(format!("stream error event: {}", data));
            }
            _ => {}
        }
    }

    if let Some(mut msg) = message {
        msg.insert("content".to_string(), Value::Array(blocks));
        out.message = Some(Value::Object(msg));
    } else if !events.is_empty() {
        errors.push("no message_start event in stream".to_string());
    }

    if !errors.is_empty() {
        out.error = Some(errors.join("; "));
    }
    out
}

/// Grow the block/accumulator vectors so `index` is addressable.
fn ensure_len(blocks: &mut Vec<Value>, index: usize, json_accum: &mut Vec<String>) {
    while blocks.len() <= index {
        blocks.push(json!({}));
    }
    while json_accum.len() <= index {
        json_accum.push(String::new());
    }
}

/// Apply one delta to its content block.
fn apply_delta(block: &mut Value, json_acc: &mut String, delta: &Value) {
    let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match dtype {
        "text_delta" => append_str_field(block, "text", delta.get("text")),
        "thinking_delta" => append_str_field(block, "thinking", delta.get("thinking")),
        "signature_delta" => append_str_field(block, "signature", delta.get("signature")),
        "input_json_delta" => {
            if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                json_acc.push_str(partial);
            }
        }
        // Unknown delta type: try common text-ish fields as a fallback.
        _ => {
            if let Some(t) = delta.get("text") {
                append_str_field(block, "text", Some(t));
            }
        }
    }
}

/// Append a string delta onto a named field of the block, creating it if absent.
fn append_str_field(block: &mut Value, field: &str, value: Option<&Value>) {
    let Some(s) = value.and_then(|v| v.as_str()) else {
        return;
    };
    let obj = match block.as_object_mut() {
        Some(o) => o,
        None => {
            *block = json!({});
            block.as_object_mut().unwrap()
        }
    };
    match obj.get_mut(field) {
        Some(Value::String(existing)) => existing.push_str(s),
        _ => {
            obj.insert(field.to_string(), Value::String(s.to_string()));
        }
    }
}

/// Merge streaming `usage` fields into the message usage object.
fn merge_usage(msg: &mut Map<String, Value>, usage: &Map<String, Value>) {
    let entry = msg
        .entry("usage".to_string())
        .or_insert_with(|| json!({}));
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in usage {
            obj.insert(k.clone(), v.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-x\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\", world\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    #[test]
    fn reconstructs_text_message() {
        let r = reconstruct(SAMPLE);
        assert!(r.error.is_none(), "no error: {:?}", r.error);
        let msg = r.message.unwrap();
        assert_eq!(msg["id"], "msg_1");
        assert_eq!(msg["model"], "claude-x");
        assert_eq!(msg["content"][0]["type"], "text");
        assert_eq!(msg["content"][0]["text"], "Hello, world");
        assert_eq!(msg["stop_reason"], "end_turn");
        assert_eq!(msg["usage"]["input_tokens"], 10);
        assert_eq!(msg["usage"]["output_tokens"], 15);
    }

    #[test]
    fn reconstructs_tool_use_input_json() {
        let raw = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"role\":\"assistant\",\"content\":[],\"usage\":{}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"get_weather\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"city\\\":\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"Paris\\\"}\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

";
        let r = reconstruct(raw);
        let msg = r.message.unwrap();
        assert_eq!(msg["content"][0]["type"], "tool_use");
        assert_eq!(msg["content"][0]["name"], "get_weather");
        assert_eq!(msg["content"][0]["input"]["city"], "Paris");
    }

    #[test]
    fn partial_stream_records_error_but_assembles_what_it_can() {
        // Truncated after one delta, no message_delta / stop.
        let raw = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"role\":\"assistant\",\"content\":[]}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}
";
        let r = reconstruct(raw);
        // Still assembled the text so far.
        let msg = r.message.unwrap();
        assert_eq!(msg["content"][0]["text"], "partial");
    }

    #[test]
    fn empty_stream_yields_no_message() {
        let r = reconstruct("");
        assert!(r.message.is_none());
    }

    #[test]
    fn malformed_data_is_noted_not_fatal() {
        let raw = "\
event: message_start
data: {not json}

";
        let r = reconstruct(raw);
        assert!(r.error.is_some());
    }
}
