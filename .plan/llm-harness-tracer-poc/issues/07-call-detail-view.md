# 07 — Call detail view (system / messages / tools / response / errors)

**What to build:** Selecting a call from the list opens a detail view that renders the full captured exchange: the system prompt, the message history, the declared tools, the reconstructed response, and any error. For streaming calls the reconstructed response is shown (raw SSE available as needed). This is the POC done-line — being able to open a call and read what the harness actually sent proves the whole thesis. Stop and evaluate.

**Blocked by:** 05 — Reconstruct streamed response into JSON; 06 — Read-only JSON API + HTML call list.

**Status:** ready-for-agent

- [ ] A read-only JSON API endpoint returns a single call's full detail by id.
- [ ] The detail view renders system prompt, message history, and declared tools from the request body.
- [ ] The detail view renders the reconstructed response (streaming and non-streaming) and any recorded error.
- [ ] Redacted auth values are shown as redacted, never the real secret.
- [ ] Navigating from the call list (ticket 06) to a call opens its detail view.
