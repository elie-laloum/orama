# 05 — Reconstruct streamed response into JSON

**What to build:** The accumulated raw SSE chunk sequence for a streaming call is assembled into a single reconstructed JSON response — the same shape a non-streaming call would have returned — and stored for display. After a streaming call, its row carries both the verbatim `response_raw_sse` and a faithful `response_reconstructed` object (assembled message content, stop reason, and token usage).

**Blocked by:** 04 — Streaming SSE tee + live relay.

**Status:** ready-for-agent

- [ ] The verbatim SSE chunk sequence is parsed and reassembled into a single JSON response object.
- [ ] The reconstructed object reflects the full assembled message content, stop reason, and token usage.
- [ ] A streaming call's row carries both `response_raw_sse` (verbatim) and `response_reconstructed` (assembled).
- [ ] Reconstruction is best-effort: a malformed/partial stream records what it can plus an `error`, and never affects the live relay.
