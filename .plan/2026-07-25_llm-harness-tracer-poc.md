# LLM Harness Tracer — POC Design (grilling outcome)

A local, open-source tool that transparently intercepts Claude Code's API traffic
and lets you *see what the harness actually sends on every call* — the full system
prompt, message history, declared tools, and reconstructed response.

Inspiration: breadcrumb.sh — but breadcrumb is **SDK-based** (you import it into your
code). This tool is fundamentally different: **transparent interception, zero SDK,
works with an unmodified Claude Code binary.**

---

## Thesis (validated)

- Claude Code reads `ANTHROPIC_BASE_URL` and sends **all** requests there — including
  agentic sub-loops and background calls. Officially supported, no MITM, no cert.
- Auth uses `ANTHROPIC_AUTH_TOKEN` → `Authorization: Bearer`. Main 401 trap:
  a stray `ANTHROPIC_API_KEY` conflicting with a custom base URL.
- Endpoint contract is `/v1/messages` (Anthropic Messages API), proven by LiteLLM et al.
- **Risk #1 (can Claude Code be pointed at a proxy?) is LIFTED.**

Remaining hard part: **reassembling the Anthropic SSE stream** while relaying it live.

---

## Decisions (all resolved in grilling)

| Area | Decision |
| --- | --- |
| Core language | **Rust** (reused later by Tauri desktop app) |
| HTTP stack | **axum** (inbound) + **reqwest** (outbound) |
| Streaming | **Tee**: forward each SSE chunk to Claude Code live *and* accumulate a copy |
| What to store | **Both** raw SSE chunk sequence **and** reconstructed JSON |
| Storage grain | **One row per HTTP round-trip**; sessions/diffs are derived views |
| Store engine | **SQLite** (indexed columns + JSON payloads) |
| Auth handling | **Pass-through** incoming auth verbatim; proxy never manages a secret |
| Secrets on disk | **Redact only auth headers** on write; everything else verbatim |
| Session correlation | Deferred; will use **messages[] prefix heuristic** — needs nothing extra now |
| UI delivery | Same Rust process serves **static HTML + read-only JSON API** over SQLite |
| UI scope | **Call list + call detail** (system / messages / tools / response / error) |
| Failure mode | **Never block** the request; tracing is best-effort, errors → stderr only |
| Write path | **Async background writer task** owns the SQLite connection |
| Onboarding | **Print the env snippet** to export; touch no files |
| Upstream target | Default `https://api.anthropic.com`, **overridable via flag** |
| Route coverage | **Catch-all relay of every path**; SSE reassembly only when response streams |
| License | **Apache-2.0** |
| Repo shape | **Cargo workspace**: `core` lib + thin `cli` binary |

---

## Storage schema (POC)

One record = one request→response exchange:

```
id                       INTEGER PK
timestamp_start          TEXT/epoch
timestamp_first_chunk    nullable   (→ derive TTFT)
timestamp_end            nullable   (→ derive latency)
method                   TEXT
url                      TEXT       (full path + query)
request_headers          JSON       (auth values redacted)
request_body             JSON
response_status          INTEGER
response_headers         JSON
response_raw_sse         TEXT        (verbatim chunk sequence; null if non-stream)
response_reconstructed   JSON        (assembled response for display)
error                    TEXT nullable
```

Indexed hints for later: model, input/output token counts, has_error, is_stream.

---

## User workflow (POC)

```bash
tracer start                 # proxy on :PORT, prints the two exports
export ANTHROPIC_BASE_URL=http://localhost:PORT
export ANTHROPIC_AUTH_TOKEN=<real anthropic token>
claude                       # runs normally; every call is captured
# open http://localhost:PORT/ui to read calls
```

---

## Build order (POC)

1. **Faithful streaming proxy** — axum catch-all → reqwest to upstream, tee SSE to
   client + accumulator, redact auth, hand finished record to async writer.
   *(This is the whole risk. Do it first.)*
2. **SQLite store** with the schema above (background writer task owns the connection).
3. **Read-only HTML UI** — call list + call detail view.

---

## POC done-line (explicit)

Done when:

- Claude Code runs through the proxy with **no perceptible difference**.
- Every round-trip (including streaming) is persisted with **raw + reconstructed**
  data and **redacted auth**.
- The HTML UI lets you open a call and read **system prompt / messages / tools /
  response / errors**.

That proves the entire thesis. **Stop and evaluate.**

Rough effort: ~2–3 weeks solo.

---

## Explicitly Phase 2+ (do NOT build in POC)

- Session grouping (messages[]-prefix) + session timeline view
- Context-drift / compaction / truncation detection
- Token-growth chart + prominent error surfacing
- Codex (OpenAI Responses API) + OpenCode (OpenAI-compatible) dialects
- Unified schema / OpenTelemetry GenAI export
- Tauri desktop app (reuses the `core` crate)
- Configurable redaction pipeline (PII, custom rules)

```
