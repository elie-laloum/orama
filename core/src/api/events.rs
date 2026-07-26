//! Live change notifications for the UI.
//!
//! The previous endpoint was a five-second timer wearing an SSE costume: it
//! fired whether or not anything had happened, and the four other event names
//! the client listened for were never emitted at all. This publishes real
//! events from the writer, after each capture is durable and derived.
//!
//! The bus is process-wide because the process serves exactly one capture
//! database. It carries notifications, never state — a client that misses one
//! re-reads the API and is immediately correct again, so there is nothing to
//! reconcile and no reason to thread a channel through every constructor.

use std::sync::OnceLock;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

/// How many events a slow client may fall behind before it is told to resync.
const BACKLOG: usize = 256;

/// Something changed that the UI may be displaying.
#[derive(Debug, Clone, Serialize)]
pub struct DomainEvent {
    /// Monotonic within a process run; surfaced as the SSE `id`.
    pub seq: u64,
    /// `capture.started`, `generation.derived`, `derive.failed`.
    pub kind: &'static str,
    pub call_id: i64,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
}

fn bus() -> &'static broadcast::Sender<DomainEvent> {
    static BUS: OnceLock<broadcast::Sender<DomainEvent>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(BACKLOG).0)
}

fn next_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Announce a change. Never fails and never blocks: with no subscribers there
/// is nothing to deliver to, which is the normal case when no UI is open.
pub fn publish(
    kind: &'static str,
    call_id: i64,
    session_id: Option<String>,
    trace_id: Option<String>,
) {
    let _ = bus().send(DomainEvent {
        seq: next_seq(),
        kind,
        call_id,
        session_id,
        trace_id,
    });
}

/// GET /api/v2/events — the live stream.
pub async fn stream() -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut receiver = bus().subscribe();
    let events = async_stream::stream! {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let payload = serde_json::to_string(&event)
                        .unwrap_or_else(|_| "{}".to_owned());
                    yield Ok(Event::default()
                        .id(event.seq.to_string())
                        .event(event.kind)
                        .data(payload));
                }
                // The client fell too far behind to be caught up event by
                // event. Telling it to re-read is honest; silently resuming
                // would leave it displaying a state that never existed.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    yield Ok(Event::default()
                        .event("resync")
                        .data(json!({ "missed": missed }).to_string()));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    // Keep-alive comments hold the connection open through idle periods, which
    // are the normal state for a local tool nobody is actively driving.
    Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_events_in_order() {
        let mut receiver = bus().subscribe();
        publish("capture.started", 1, Some("s".into()), None);
        publish("generation.derived", 1, Some("s".into()), Some("t".into()));

        let first = receiver.recv().await.unwrap();
        let second = receiver.recv().await.unwrap();
        assert_eq!(first.kind, "capture.started");
        assert_eq!(second.kind, "generation.derived");
        assert_eq!(second.trace_id.as_deref(), Some("t"));
        // Sequence numbers are monotonic so a client can spot a gap.
        assert!(second.seq > first.seq);
    }

    #[test]
    fn publishing_with_no_subscribers_is_not_an_error() {
        // The normal case: captures happen with no UI open.
        publish("capture.started", 99, None, None);
    }
}
