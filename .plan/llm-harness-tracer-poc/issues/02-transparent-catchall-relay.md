# 02 — Transparent catch-all relay (non-streaming)

**What to build:** Any request to any path is forwarded verbatim to the upstream Anthropic API and the response is returned unchanged. Incoming auth (`Authorization: Bearer …`) is passed through untouched — the proxy never manages a secret. A non-streaming `/v1/messages` call routed through the proxy returns the correct answer with no perceptible difference from calling the API directly. Tracing is best-effort and must never block or alter the request; any internal error goes to stderr only.

**Blocked by:** 01 — Scaffold workspace + bootable `tracer start`.

**Status:** ready-for-agent

- [ ] A catch-all route relays every path and method to upstream via reqwest.
- [ ] Request headers, body, and query are forwarded verbatim; incoming auth is passed through unchanged.
- [ ] The upstream status, headers, and body are returned to the client unchanged.
- [ ] A real non-streaming `/v1/messages` request through the proxy yields the same result as calling upstream directly.
- [ ] A relay/tracing failure never blocks the client request; errors surface on stderr only.
