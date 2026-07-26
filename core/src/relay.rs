//! Transparent catch-all relay to the upstream Anthropic API.
//!
//! Best-effort tracing rule: relaying the request is the job; any internal
//! tracing/capture error must surface on stderr only and never block or alter
//! the client's request/response.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::store::{CallRecord, StoreHandle};
use crate::util::{body_to_json, headers_to_json, now_rfc3339};

/// Shared state handed to the catch-all handler.
#[derive(Clone)]
pub struct RelayState {
    pub client: reqwest::Client,
    pub upstream: Arc<str>,
    /// Optional capture sink. When `None`, the relay is pure pass-through.
    pub store: Option<StoreHandle>,
}

impl RelayState {
    pub fn new(upstream: impl Into<String>) -> Self {
        Self::with_store(upstream, None)
    }

    pub fn with_store(upstream: impl Into<String>, store: Option<StoreHandle>) -> Self {
        let client = reqwest::Client::builder()
            // Claude Code manages its own timeouts; don't impose our own on the
            // relay path or we could truncate long agentic calls.
            .build()
            .expect("reqwest client builds with default config");
        Self {
            client,
            upstream: Arc::from(upstream.into()),
            store,
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
        request_body: body_to_json(&body_bytes),
        ..Default::default()
    };

    match forward(&state, &method, &uri, &headers, body_bytes, record).await {
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
fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

/// Perform the outbound request and translate the upstream response back.
///
/// On any pre-response failure the (unfinished) record is handed back so the
/// caller can note the error. On success, this function owns finalising and
/// enqueuing the record — non-streaming buffers then stores; streaming tees
/// each chunk to the client and stores when the stream ends.
async fn forward(
    state: &RelayState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Vec<u8>,
    mut record: CallRecord,
) -> Result<Response, (anyhow::Error, CallRecord)> {
    let url = upstream_url(&state.upstream, uri);

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

    if is_event_stream(&resp_headers) {
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

/// Build a streaming response that forwards each upstream SSE chunk to the
/// client the instant it arrives while accumulating a verbatim copy. When the
/// stream ends (or errors), the record is finalised with the raw SSE, the
/// first-chunk/end timestamps, and any error, then enqueued for persistence.
fn stream_teeing_response(
    builder: axum::http::response::Builder,
    upstream_resp: reqwest::Response,
    record: CallRecord,
    store: Option<StoreHandle>,
) -> Response {
    use futures::StreamExt;

    let mut upstream = upstream_resp.bytes_stream();
    let mut record = record;
    let mut raw_sse = String::new();
    let mut first_chunk_at: Option<String> = None;

    let tee = async_stream::stream! {
        loop {
            match upstream.next().await {
                Some(Ok(chunk)) => {
                    if first_chunk_at.is_none() {
                        first_chunk_at = Some(now_rfc3339());
                    }
                    // Accumulate a verbatim copy (lossy UTF-8 for storage only;
                    // the bytes forwarded to the client are untouched).
                    raw_sse.push_str(&String::from_utf8_lossy(&chunk));
                    // Forward the exact bytes downstream immediately.
                    yield Ok::<_, std::io::Error>(chunk);
                }
                Some(Err(err)) => {
                    // A mid-stream upstream error: surface it, record it, and
                    // stop. The client stream ends here rather than hanging.
                    eprintln!("orama: upstream stream error: {err}");
                    record.error = Some(format!("stream error: {err}"));
                    break;
                }
                None => break,
            }
        }

        record.timestamp_first_chunk = first_chunk_at.clone();
        record.timestamp_end = Some(now_rfc3339());
        record.response_raw_sse = Some(raw_sse.clone());
        finalize_stream_record(&mut record);
        if let Some(store) = &store {
            store.record(record.clone());
        }
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

    #[test]
    fn host_is_hop_by_hop() {
        assert!(is_hop_by_hop(&HeaderName::from_static("host")));
        assert!(is_hop_by_hop(&HeaderName::from_static("connection")));
        assert!(!is_hop_by_hop(&HeaderName::from_static("authorization")));
        assert!(!is_hop_by_hop(&HeaderName::from_static("content-type")));
        assert!(is_hop_by_hop(&HeaderName::from_static("accept-encoding")));
    }
}
