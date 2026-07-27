//! Transparent catch-all relay to the upstream model provider.
//!
//! One listener serves both wire dialects. Which upstream a request goes to is
//! decided per request by [`crate::parse::detect`] — the same function the
//! derive layer uses to pick a parser, so a call can never be relayed as one
//! dialect and read back as another.
//!
//! Best-effort tracing rule: relaying the request is the job; any internal
//! tracing/capture error must surface on stderr only and never block or alter
//! the client's request/response. Choosing a destination host is not altering
//! the exchange — the method, path, headers and body are still forwarded
//! verbatim.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::Config;
use crate::parse::model::Provider;
use crate::store::{CallRecord, StoreHandle};
use crate::util::{body_to_json, decode_body, headers_to_json, now_rfc3339};

/// Shared state handed to the catch-all handler.
#[derive(Clone)]
pub struct RelayState {
    pub client: reqwest::Client,
    /// Where Anthropic-dialect traffic goes. Also the fallback for a request
    /// whose dialect cannot be identified, which keeps an unrecognised route
    /// behaving exactly as it did before OpenAI support existed.
    pub upstream: Arc<str>,
    /// Where OpenAI-dialect traffic goes.
    pub upstream_openai: Arc<str>,
    /// Where `/backend-api/*` goes — Codex on a ChatGPT subscription, which is
    /// a different backend from `api.openai.com` rather than a different path
    /// on it.
    pub upstream_chatgpt: Arc<str>,
    /// Optional capture sink. When `None`, the relay is pure pass-through.
    pub store: Option<StoreHandle>,
    /// Whether the relay is forwarding at all. Flipped by the settings API so a
    /// user can take the proxy out of the path without quitting the app.
    ///
    /// Deliberately in memory only: a proxy that remembered it was stopped would
    /// come back up refusing traffic for harnesses still pointed at it, and the
    /// reason would be a click from a previous run. Every start is a running
    /// start; stopping is a decision about *this* session.
    pub running: Arc<AtomicBool>,
}

impl RelayState {
    pub fn new(config: &Config) -> Self {
        Self::with_store(config, None)
    }

    pub fn with_store(config: &Config, store: Option<StoreHandle>) -> Self {
        let client = reqwest::Client::builder()
            // Claude Code manages its own timeouts; don't impose our own on the
            // relay path or we could truncate long agentic calls.
            .build()
            .expect("reqwest client builds with default config");
        Self {
            client,
            upstream: Arc::from(config.upstream.as_str()),
            upstream_openai: Arc::from(config.upstream_openai.as_str()),
            upstream_chatgpt: Arc::from(config.upstream_chatgpt.as_str()),
            store,
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Is the relay forwarding traffic?
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// The upstream for a captured request's headers and path.
    ///
    /// The path prefix wins over the dialect because it names a host, not a
    /// format: `/backend-api/codex/responses` is the OpenAI wire format spoken
    /// to ChatGPT's backend, and sending it to `api.openai.com` would 404 no
    /// matter how correctly it parses.
    ///
    /// `Unknown` resolves to the Anthropic upstream rather than erroring: the
    /// relay's contract is to forward everything, including the routes that
    /// carry no dialect marker at all.
    fn upstream_for(&self, headers: &Value, url: &str) -> &str {
        if url.starts_with(crate::config::CHATGPT_PREFIX) {
            return &self.upstream_chatgpt;
        }
        match crate::parse::detect(headers, url) {
            Provider::OpenAi => &self.upstream_openai,
            Provider::ClaudeCode | Provider::Unknown => &self.upstream,
        }
    }
}

/// Hop-by-hop headers that must not be forwarded verbatim (RFC 7230 §6.1).
/// `host` is dropped so reqwest sets the correct upstream host.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            // Never forward the client's compression negotiation. Doing so
            // disables reqwest's transparent decompression, which would make
            // an SSE capture binary gzip data rather than parseable events.
            | "accept-encoding"
    )
}

/// Build the upstream URL from the configured base plus the incoming path+query.
fn upstream_url(upstream: &str, uri: &Uri) -> String {
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| uri.path());
    format!("{}{}", upstream, path_and_query)
}

/// Catch-all handler: forward any method/path verbatim to upstream and return
/// the upstream response unchanged. Auth headers are passed through untouched.
///
/// Capture is best-effort and off the critical path: the request is relayed
/// regardless of whether a record can be built or persisted.
pub async fn relay(
    State(state): State<RelayState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Stopped means stopped: nothing is forwarded and nothing is recorded. The
    // refusal is explicit rather than a silent drop, and it names Orama — a
    // harness that suddenly fails should not leave the user debugging their
    // provider. It is deliberately not shaped like a provider error: a client
    // that retries on 503 will succeed the moment the proxy is started again.
    if !state.is_running() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                ("content-type", "application/json"),
                ("x-orama-state", "stopped"),
            ],
            r#"{"error":"orama is stopped and is not relaying requests; start it from the dashboard, or point this client back at the provider"}"#,
        )
            .into_response();
    }

    let timestamp_start = now_rfc3339();

    let body_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b.to_vec(),
        Err(err) => {
            eprintln!("orama: failed to read request body: {err}");
            return (StatusCode::BAD_GATEWAY, "orama: bad request body").into_response();
        }
    };

    // Build the request half of the record up front (redaction happens on write).
    let record = CallRecord {
        timestamp_start,
        method: method.to_string(),
        url: uri
            .path_and_query()
            .map(|pq| pq.to_string())
            .unwrap_or_else(|| uri.path().to_string()),
        request_headers: headers_to_json(&headers),
        // Decoded for the record only; `body_bytes` still goes upstream as it
        // arrived. A compressed body stored verbatim is unparseable forever.
        request_body: body_to_json(&decode_body(
            headers
                .get(axum::http::header::CONTENT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            &body_bytes,
        )),
        ..Default::default()
    };

    // Decided from the record we just built, so the dialect that picked the
    // destination is exactly the one the parser will see on the way back out.
    let upstream = state
        .upstream_for(&record.request_headers, &record.url)
        .to_owned();

    match forward(
        &state, upstream, &method, &uri, &headers, body_bytes, record,
    )
    .await
    {
        Ok(response) => response,
        Err((err, mut record)) => {
            // Best-effort: never hide the failure from the operator, but return
            // a clean gateway error to the client rather than panicking.
            eprintln!("orama: relay to upstream failed: {err}");
            record.timestamp_end = Some(now_rfc3339());
            record.error = Some(format!("relay failed: {err}"));
            if let Some(store) = &state.store {
                store.record(record);
            }
            (StatusCode::BAD_GATEWAY, "orama: upstream relay failed").into_response()
        }
    }
}

/// Does this response body stream as Server-Sent Events?
///
/// `accepted_events` is whether the client asked for `text/event-stream`, and
/// it is consulted only when the response declares no content type at all.
/// ChatGPT's Codex backend sends exactly that — a chunked SSE body with no
/// `content-type` — so trusting the response header alone buffered a live
/// stream to completion before the client saw a byte of it, and left
/// `response_raw_sse` empty so nothing was ever reconstructed.
///
/// Guessing here is safe in the direction it can be wrong: the streaming path
/// forwards each chunk verbatim as it arrives, so a non-SSE body treated as a
/// stream still reaches the client unchanged; only the reconstruction attempt
/// fails, and it records that rather than inventing a message.
fn is_event_stream(headers: &reqwest::header::HeaderMap, accepted_events: bool) -> bool {
    match headers.get(reqwest::header::CONTENT_TYPE) {
        Some(value) => value
            .to_str()
            .map(|ct| ct.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false),
        None => accepted_events,
    }
}

/// Did the client ask for Server-Sent Events?
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.to_ascii_lowercase().contains("text/event-stream"))
}

/// Perform the outbound request and translate the upstream response back.
///
/// On any pre-response failure the (unfinished) record is handed back so the
/// caller can note the error. On success, this function owns finalising and
/// enqueuing the record — non-streaming buffers then stores; streaming tees
/// each chunk to the client and stores when the stream ends.
async fn forward(
    state: &RelayState,
    upstream: String,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Vec<u8>,
    mut record: CallRecord,
) -> Result<Response, (anyhow::Error, CallRecord)> {
    let url = upstream_url(&upstream, uri);

    let mut req = state.client.request(method.clone(), &url);

    // Forward request headers except hop-by-hop; auth passes through. In
    // particular, accept-encoding is omitted so reqwest negotiates and
    // transparently decodes compressed upstream SSE before we capture it.
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        req = req.header(name.as_str(), value.as_bytes());
    }

    if !body.is_empty() {
        req = req.body(body);
    }

    let upstream_resp = match req.send().await {
        Ok(r) => r,
        Err(err) => return Err((err.into(), record)),
    };

    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    record.response_status = Some(status.as_u16() as i64);
    record.response_headers = Some(headers_to_json(&resp_headers));

    let mut builder = Response::builder().status(status.as_u16());
    for (name, value) in resp_headers.iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        // content-length is recomputed by the body; drop the upstream copy to
        // avoid a mismatch if it disagrees with the buffered bytes.
        if name.as_str() == "content-length" {
            continue;
        }
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            builder = builder.header(n, v);
        }
    }

    if is_event_stream(&resp_headers, accepts_event_stream(headers)) {
        Ok(stream_teeing_response(
            builder,
            upstream_resp,
            record,
            state.store.clone(),
        ))
    } else {
        // Non-streaming: buffer the whole body, capture it, store, and return
        // the bytes unchanged. The capture is what makes usage, stop_reason and
        // the assistant turn available for non-streamed calls.
        let resp_body = match upstream_resp.bytes().await {
            Ok(b) => b,
            Err(err) => return Err((err.into(), record)),
        };
        record.timestamp_end = Some(now_rfc3339());
        record.response_body = body_to_json(&resp_body);
        if let Some(store) = &state.store {
            store.record(record);
        }
        let response = builder
            .body(Body::from(resp_body))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
        Ok(response)
    }
}

/// A streaming capture that persists itself however the stream ends.
///
/// The enqueue lives in `Drop`, not at the end of the generator, because those
/// are not the same moment. An `async_stream` body only executes past its last
/// `yield` if it is polled again — and a client that stops reading the instant
/// it has what it needs gives it no such poll. Codex does exactly that on an
/// SSE turn, so with the enqueue in the tail the second and subsequent turns of
/// a session were relayed perfectly and never recorded, with nothing anywhere
/// reporting a capture had gone missing.
///
/// `Drop` runs in every case — completed, errored, or abandoned — so a capture
/// is now lost only if the process dies.
struct StreamCapture {
    record: CallRecord,
    raw_sse: String,
    first_chunk_at: Option<String>,
    store: Option<StoreHandle>,
}

impl StreamCapture {
    fn new(record: CallRecord, store: Option<StoreHandle>) -> Self {
        Self {
            record,
            raw_sse: String::new(),
            first_chunk_at: None,
            store,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.first_chunk_at.is_none() {
            self.first_chunk_at = Some(now_rfc3339());
        }
        // A verbatim copy, lossily decoded for storage only; the bytes
        // forwarded to the client are untouched.
        self.raw_sse.push_str(&String::from_utf8_lossy(chunk));
    }

    fn note_error(&mut self, err: impl std::fmt::Display) {
        self.record.error = Some(format!("stream error: {err}"));
    }
}

impl Drop for StreamCapture {
    fn drop(&mut self) {
        let Some(store) = self.store.take() else {
            return;
        };
        // `std::mem::take` rather than a clone: this runs on the request path
        // for every streamed call, and the SSE can be hundreds of kilobytes.
        let mut record = std::mem::take(&mut self.record);
        record.timestamp_first_chunk = self.first_chunk_at.take();
        record.timestamp_end = Some(now_rfc3339());
        record.response_raw_sse = Some(std::mem::take(&mut self.raw_sse));
        finalize_stream_record(&mut record);
        store.record(record);
    }
}

/// Build a streaming response that forwards each upstream SSE chunk to the
/// client the instant it arrives while accumulating a verbatim copy. The record
/// is finalised and enqueued when the capture drops, which covers a stream that
/// ends, errors, or is abandoned by the client.
fn stream_teeing_response(
    builder: axum::http::response::Builder,
    upstream_resp: reqwest::Response,
    record: CallRecord,
    store: Option<StoreHandle>,
) -> Response {
    use futures::StreamExt;

    let mut upstream = upstream_resp.bytes_stream();
    // Moved into the generator, so it drops with it.
    let mut capture = StreamCapture::new(record, store);

    let tee = async_stream::stream! {
        loop {
            match upstream.next().await {
                Some(Ok(chunk)) => {
                    capture.push(&chunk);
                    // Forward the exact bytes downstream immediately.
                    yield Ok::<_, std::io::Error>(chunk);
                }
                Some(Err(err)) => {
                    // A mid-stream upstream error: surface it, record it, and
                    // stop. The client stream ends here rather than hanging.
                    eprintln!("orama: upstream stream error: {err}");
                    capture.note_error(err);
                    break;
                }
                None => break,
            }
        }
        // No enqueue here on purpose — dropping `capture` does it, and that
        // happens whether or not this line is ever reached.
        drop(capture);
    };

    builder
        .body(Body::from_stream(tee))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// Reconstruct the assembled JSON response from the captured raw SSE and store
/// it alongside the verbatim stream. Best-effort: a partial/malformed stream
/// records what it can plus an error note, and any existing stream error is
/// preserved.
fn finalize_stream_record(record: &mut CallRecord) {
    let Some(raw) = record.response_raw_sse.as_ref() else {
        return;
    };
    let result = crate::reconstruct::reconstruct(raw);
    record.response_reconstructed = result.message;
    if let Some(err) = result.error {
        record.error = Some(match record.error.take() {
            Some(existing) => format!("{existing}; reconstruct: {err}"),
            None => format!("reconstruct: {err}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_url_joins_path_and_query() {
        let uri: Uri = "/v1/messages?beta=true".parse().unwrap();
        let url = upstream_url("https://api.anthropic.com", &uri);
        assert_eq!(url, "https://api.anthropic.com/v1/messages?beta=true");
    }

    #[test]
    fn upstream_url_without_query() {
        let uri: Uri = "/v1/models".parse().unwrap();
        let url = upstream_url("https://api.anthropic.com", &uri);
        assert_eq!(url, "https://api.anthropic.com/v1/models");
    }

    fn state() -> RelayState {
        RelayState::new(&Config::default())
    }

    #[test]
    fn anthropic_dialect_routes_to_the_anthropic_upstream() {
        let state = state();
        let headers = serde_json::json!({"x-app": "cli", "anthropic-version": "2023-06-01"});
        assert_eq!(
            state.upstream_for(&headers, "/v1/messages"),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn openai_dialect_routes_to_the_openai_upstream() {
        let state = state();
        // Codex speaks the Responses API; before per-dialect routing this went
        // to api.anthropic.com and could never have been captured.
        let headers = serde_json::json!({"user-agent": "codex_cli_rs/1.0"});
        assert_eq!(
            state.upstream_for(&headers, "/v1/responses"),
            "https://api.openai.com"
        );
        assert_eq!(
            state.upstream_for(&serde_json::json!({}), "/v1/chat/completions"),
            "https://api.openai.com"
        );
    }

    #[test]
    fn the_chatgpt_prefix_outranks_the_dialect() {
        // Codex on a subscription speaks the OpenAI wire format to a backend
        // that is not api.openai.com. Routing on format alone would send it
        // somewhere it does not exist.
        let state = state();
        assert_eq!(
            state.upstream_for(
                &serde_json::json!({"user-agent": "codex_exec/0.145.0"}),
                "/backend-api/codex/responses"
            ),
            "https://chatgpt.com"
        );
    }

    #[test]
    fn a_versionless_responses_route_is_still_openai_dialect() {
        // `/backend-api/codex/responses` carries no `/v1`, so matching on the
        // versioned path would have stored it unparsed.
        assert_eq!(
            crate::parse::detect(&serde_json::json!({}), "/backend-api/codex/responses"),
            Provider::OpenAi
        );
    }

    #[test]
    fn an_unrecognised_request_still_goes_somewhere() {
        // The relay forwards everything; a route with no dialect marker keeps
        // its pre-OpenAI behaviour rather than failing.
        let state = state();
        assert_eq!(
            state.upstream_for(&serde_json::json!({}), "/healthcheck"),
            "https://api.anthropic.com"
        );
    }

    #[test]
    fn an_openai_compatible_host_is_reachable_through_the_same_listener() {
        let state =
            RelayState::new(&Config::default().with_openai_upstream("https://openrouter.ai/api"));
        assert_eq!(
            state.upstream_for(&serde_json::json!({}), "/v1/chat/completions"),
            "https://openrouter.ai/api"
        );
    }

    #[test]
    fn host_is_hop_by_hop() {
        assert!(is_hop_by_hop(&HeaderName::from_static("host")));
        assert!(is_hop_by_hop(&HeaderName::from_static("connection")));
        assert!(!is_hop_by_hop(&HeaderName::from_static("authorization")));
        assert!(!is_hop_by_hop(&HeaderName::from_static("content-type")));
        assert!(is_hop_by_hop(&HeaderName::from_static("accept-encoding")));
    }
}
