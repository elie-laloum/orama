# 04 — Streaming SSE tee + live relay

**What to build:** For streaming responses, each SSE chunk is forwarded to Claude Code live *and* a verbatim copy is accumulated. A real streaming Claude Code session runs through the proxy with no perceptible difference — chunks arrive with the same timing behaviour as talking to upstream directly. The accumulated raw chunk sequence is persisted verbatim, and the first-chunk and end timestamps are recorded so TTFT and latency are derivable. Reconstruction of the assembled JSON is out of scope here (ticket 05).

**Blocked by:** 03 — Persist round-trips to SQLite via async writer.

**Status:** ready-for-agent

- [ ] Streaming responses are detected and each SSE chunk is relayed to the client as it arrives (no buffering the whole stream before forwarding).
- [ ] Each chunk is simultaneously accumulated into a verbatim copy without disturbing the live stream (tee).
- [ ] A real streaming Claude Code session completes through the proxy with no perceptible difference.
- [ ] The row's `response_raw_sse` holds the verbatim chunk sequence for streaming calls.
- [ ] `timestamp_first_chunk` and `timestamp_end` are recorded so TTFT and latency can be derived.
- [ ] A stream error never blocks or corrupts the client stream; it surfaces on stderr and is recorded in `error`.
