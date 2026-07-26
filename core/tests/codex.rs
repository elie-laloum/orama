//! Integration test: a real Codex round trip, end to end through the relay.
//!
//! Modelled byte-for-byte on the first authenticated Codex capture this project
//! ever took, which broke three separate assumptions the synthetic fixtures had
//! quietly baked in:
//!
//! * the request body arrives **zstd-compressed**, so storing it verbatim left
//!   mojibake with no model, no tools and no prompt;
//! * the response carries **no `content-type` at all** — just chunked SSE — so
//!   the stream was buffered to completion instead of teed, which both delayed
//!   the client and left `response_raw_sse` empty so nothing was reconstructed;
//! * the harness opens with a `GET` WebSocket upgrade probe that the backend
//!   405s, which was being derived into an errored generation.
//!
//! Together those made a working session look like mostly-failed calls with no
//! usage. The assertions here are what stops that regressing.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use axum::{body::Body, http::StatusCode, response::IntoResponse, routing::any, Router};
use orama_core::{
    derive::write::backfill,
    server::router,
    store::{apply_schema, list_calls},
    Config,
};
use tokio::net::TcpListener;

/// The Responses API stream as ChatGPT's Codex backend actually sends it:
/// named `response.*` events, and usage only on the terminal event.
fn codex_sse() -> String {
    [
        r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1","object":"response","status":"in_progress","model":"gpt-5.6-luna","usage":null}}

"#,
        r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"I'm Codex"}

"#,
        r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","object":"response","status":"completed","model":"gpt-5.6-luna","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I'm Codex"}]}],"usage":{"input_tokens":12616,"input_tokens_details":{"cache_write_tokens":0,"cached_tokens":9984},"output_tokens":42,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":12658}}}

"#,
    ]
    .concat()
}

/// Answers like the real backend: 405 on the WebSocket upgrade probe, then the
/// SSE stream with no content-type on the POST.
async fn backend(method: axum::http::Method) -> axum::response::Response {
    if method == axum::http::Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"detail":"Method Not Allowed"}"#,
        )
            .into_response();
    }
    // Deliberately no content-type header — this is the shape that broke it.
    // Streamed so hyper chunks it, exactly as the real backend does.
    let chunks = codex_sse()
        .split_inclusive("\n\n")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    // A real turn arrives over time, so a client that stops reading leaves the
    // tee genuinely mid-stream. Delivered all at once it would already be
    // buffered and the abandonment would prove nothing.
    let stream = futures::stream::unfold(chunks.into_iter(), |mut rest| async move {
        let next = rest.next()?;
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        Some((Ok::<_, std::io::Error>(next), rest))
    });
    Body::from_stream(stream).into_response()
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn temp_db(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orama-codex-{name}.sqlite"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    apply_schema(&conn).unwrap();
    path
}

#[tokio::test]
async fn a_codex_turn_is_captured_parsed_and_priced() {
    let upstream = spawn(Router::new().fallback(any(backend))).await;
    let db = temp_db("turn");

    let config = Config::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, "https://unused.invalid")
        .with_chatgpt_upstream(format!("http://{upstream}"))
        .with_db_path(&db);

    let store = orama_core::store::spawn_writer(&db).unwrap();
    let proxy = spawn(router(config, Some(store))).await;

    let client = reqwest::Client::new();

    // 1. The WebSocket upgrade probe Codex opens with.
    let probe = client
        .get(format!("http://{proxy}/backend-api/codex/responses"))
        .header("user-agent", "codex_exec/0.145.0")
        .send()
        .await
        .unwrap();
    assert_eq!(probe.status(), StatusCode::METHOD_NOT_ALLOWED);

    // 2. The real turn: a zstd-compressed JSON body, asking for SSE back.
    let request_json = br#"{"model":"gpt-5.6-luna","input":[{"role":"user","content":"who are you"}],"stream":true}"#;
    let compressed = zstd::stream::encode_all(&request_json[..], 0).unwrap();
    let response = client
        .post(format!("http://{proxy}/backend-api/codex/responses"))
        .header("content-type", "application/json")
        .header("content-encoding", "zstd")
        .header("accept", "text/event-stream")
        .header("user-agent", "codex_exec/0.145.0")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The client still gets the stream byte for byte.
    assert_eq!(response.text().await.unwrap(), codex_sse());

    // Let the background writer drain.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        let conn = rusqlite::Connection::open(&db).unwrap();
        if list_calls(&conn).map(|c| c.len()).unwrap_or(0) >= 2 {
            break;
        }
    }

    let conn = rusqlite::Connection::open(&db).unwrap();
    let calls = list_calls(&conn).unwrap();
    assert_eq!(calls.len(), 2, "both round trips are captured");

    let post = calls
        .iter()
        .find(|call| call.record.method == "POST")
        .expect("the turn was captured");

    // The stored request is readable JSON, not the compressed bytes.
    let body = post.record.request_body.as_ref().expect("body stored");
    assert_eq!(
        body["model"], "gpt-5.6-luna",
        "a zstd body must be decoded for the record: {body:?}"
    );

    // A content-type-less SSE body still went down the streaming path, so the
    // verbatim stream was kept and the terminal object reassembled from it.
    let sse = post
        .record
        .response_raw_sse
        .as_ref()
        .expect("SSE captured despite the missing content-type");
    assert!(sse.contains("response.completed"), "{sse}");
    let reconstructed = post
        .record
        .response_reconstructed
        .as_ref()
        .expect("terminal Responses object reassembled");
    assert_eq!(reconstructed["model"], "gpt-5.6-luna");
    assert_eq!(reconstructed["usage"]["input_tokens"], 12616);

    // And the derived layer sees a single real generation — the 405 upgrade
    // probe is kept as a raw call but is not a generation, let alone a failed
    // one.
    backfill(&conn).unwrap();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM generations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "the 405 upgrade probe is not a generation");

    let row = conn
        .query_row(
            "SELECT call_id, model, input_tokens, output_tokens, is_error FROM generations",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(row.0, post.id);
    assert_eq!(row.1.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(row.2, Some(12616), "input tokens off the terminal event");
    assert_eq!(row.3, Some(42));
    assert_eq!(row.4, 0, "a captured turn is not an error");
}

/// A client that stops reading once it has what it needs must still be captured.
///
/// Codex reads an SSE turn until `response.completed` and then drops the
/// connection; it does not drain to EOF. The tee enqueued its record only after
/// the stream generator ran to completion, so an early drop meant the whole
/// exchange was silently lost — the relay had already forwarded it, and nothing
/// anywhere said a capture had gone missing.
#[tokio::test]
async fn a_turn_is_captured_even_when_the_client_stops_reading_early() {
    let upstream = spawn(Router::new().fallback(any(backend))).await;
    let db = temp_db("early-drop");
    let config = Config::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, "https://unused.invalid")
        .with_chatgpt_upstream(format!("http://{upstream}"))
        .with_db_path(&db);
    let store = orama_core::store::spawn_writer(&db).unwrap();
    let proxy = spawn(router(config, Some(store))).await;

    // A raw socket, because the point is the abandonment itself. An HTTP client
    // drains and recycles connections in the background, which quietly gives
    // the relay the extra poll this test exists to withhold.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for turn in 0..3 {
        let body = format!(r#"{{"model":"gpt-5.6-luna","turn":{turn}}}"#);
        let mut socket = tokio::net::TcpStream::connect(proxy).await.unwrap();
        socket
            .write_all(
                format!(
                    "POST /backend-api/codex/responses HTTP/1.1\r\nHost: {proxy}\r\n\
                     Accept: text/event-stream\r\nUser-Agent: codex-tui/0.145.0\r\n\
                     Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        // Abandon at the FIRST event, while later chunks are still pending
        // upstream. Breaking on the terminal event instead would leave nothing
        // to abandon — the stream would already be finishing on its own, and
        // the relay would get the final poll this test exists to withhold.
        let mut seen = String::new();
        let mut buf = [0_u8; 4096];
        while !seen.contains("response.created") {
            match socket.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
        assert!(seen.contains("200 OK"), "turn {turn} was relayed: {seen}");
        drop(socket);
    }

    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let conn = rusqlite::Connection::open(&db).unwrap();
        if list_calls(&conn).map(|c| c.len()).unwrap_or(0) >= 3 {
            break;
        }
    }

    let conn = rusqlite::Connection::open(&db).unwrap();
    let calls = list_calls(&conn).unwrap();
    assert_eq!(
        calls.len(),
        3,
        "every turn must be captured, not only the ones drained to EOF"
    );
}
