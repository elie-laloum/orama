//! Integration test: a streaming (SSE) upstream is teed to the client chunk by
//! chunk (no full-buffer stall) and the verbatim raw stream + timing is stored.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tracer_core::{server::router, store::list_calls, Config};

/// Mock upstream that emits three SSE events with a delay between each, so a
/// correctly-teeing proxy delivers them incrementally rather than all at once.
async fn sse_upstream() -> Response {
    let stream = async_stream::stream! {
        yield Ok::<_, std::io::Error>(bytes::Bytes::from(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":5}}}\n\n",
        ));
        sleep(Duration::from_millis(100)).await;
        yield Ok(bytes::Bytes::from(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        ));
        sleep(Duration::from_millis(100)).await;
        yield Ok(bytes::Bytes::from(
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ));
    };
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn streaming_is_teed_live_and_persisted() {
    let upstream_addr = spawn(Router::new().fallback(any(sse_upstream))).await;

    let db = std::env::temp_dir().join(format!("tracer-stream-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db);

    let cfg = Config::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        0,
        format!("http://{upstream_addr}"),
    )
    .with_db_path(&db);
    let store = tracer_core::store::spawn_writer(&db).unwrap();
    let proxy_addr = spawn(router(cfg, Some(store))).await;

    // Consume the stream through the proxy, timestamping chunk arrivals.
    let resp = reqwest::Client::new()
        .post(format!("http://{proxy_addr}/v1/messages"))
        .header("authorization", "Bearer secret")
        .body("{\"stream\":true}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let start = Instant::now();
    let mut arrival_spread = Duration::ZERO;
    let mut last_arrival = start;
    let mut collected = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let chunk = item.unwrap();
        collected.push_str(&String::from_utf8_lossy(&chunk));
        let now = Instant::now();
        arrival_spread = arrival_spread.max(now.duration_since(last_arrival));
        last_arrival = now;
    }

    // The client received the full verbatim stream.
    assert!(collected.contains("message_start"));
    assert!(collected.contains("content_block_delta"));
    assert!(collected.contains("message_stop"));

    // Tee proof: chunks arrived spread out over time, not all buffered at once.
    // The upstream delays 100ms between events; a buffering proxy would deliver
    // everything within a single tick.
    assert!(
        arrival_spread >= Duration::from_millis(50),
        "expected incremental delivery, got spread {arrival_spread:?}"
    );

    // Persisted verbatim raw SSE + timing.
    let mut found = None;
    for _ in 0..50 {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let calls = list_calls(&conn).unwrap();
        if !calls.is_empty() {
            found = Some(calls);
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let calls = found.expect("streaming call should be persisted");
    let call = &calls[0].record;
    let raw = call.response_raw_sse.as_ref().expect("raw SSE stored");
    assert!(raw.contains("message_start"));
    assert!(raw.contains("message_stop"));
    assert_eq!(raw, &collected, "stored SSE is verbatim");
    assert!(call.timestamp_first_chunk.is_some(), "TTFT derivable");
    assert!(call.timestamp_end.is_some(), "latency derivable");

    // Reconstructed JSON carries both verbatim and assembled forms.
    let recon = call
        .response_reconstructed
        .as_ref()
        .expect("reconstructed JSON stored");
    assert_eq!(recon["content"][0]["text"], "hi");

    let _ = std::fs::remove_file(&db);
}
