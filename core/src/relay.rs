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
            eprintln!("tracer: failed to read request body: {err}");
            return (StatusCode::BAD_GATEWAY, "tracer: bad request body").into_response();
        }
    };

    // Build the request half of the record up front (redaction happens on write).
    let mut record = CallRecord {
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

    let result = forward(&state, &method, &uri, &headers, body_bytes).await;

    let response = match result {
        Ok(fwd) => {
            record.timestamp_end = Some(now_rfc3339());
            record.response_status = Some(fwd.status as i64);
            record.response_headers = Some(fwd.headers_json);
            fwd.response
        }
        Err(err) => {
            // Best-effort: never hide the failure from the operator, but return
            // a clean gateway error to the client rather than panicking.
            eprintln!("tracer: relay to upstream failed: {err}");
            record.timestamp_end = Some(now_rfc3339());
            record.error = Some(format!("relay failed: {err}"));
            (StatusCode::BAD_GATEWAY, "tracer: upstream relay failed").into_response()
        }
    };

    if let Some(store) = &state.store {
        store.record(record);
    }

    response
}

/// Outcome of a forwarded request: the client-facing response plus the captured
/// status and response headers for persistence.
struct Forwarded {
    response: Response,
    status: u16,
    headers_json: serde_json::Value,
}

/// Perform the outbound request and translate the upstream response back.
async fn forward(
    state: &RelayState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> anyhow::Result<Forwarded> {
    let url = upstream_url(&state.upstream, uri);

    let mut req = state.client.request(method.clone(), &url);

    // Forward request headers verbatim except hop-by-hop; auth passes through.
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name) {
            continue;
        }
        req = req.header(name.as_str(), value.as_bytes());
    }

    if !body.is_empty() {
        req = req.body(body);
    }

    let upstream_resp = req.send().await?;

    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let resp_body = upstream_resp.bytes().await?;

    let headers_json = headers_to_json(&resp_headers);

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

    let response = builder
        .body(Body::from(resp_body))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
    Ok(Forwarded {
        response,
        status: status.as_u16(),
        headers_json,
    })
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
    }
}
