# Part 2 — Structured Data Overview (grilling outcome)

Build a **per-provider parsing/normalization layer** (Claude Code first) that turns
captured raw traffic into a **normalized conversation model** and then **extracts and
highlights analytical signals** from it — powering a **session-centric, informational UI**
that replaces today's raw-JSON dump.

Follows Part 1 (the POC): a transparent proxy already captures one SQLite row per
round-trip with raw SSE + reconstructed JSON, served through a read-only API/UI. Part 2
does **not** touch the capture/write path — it derives everything at read time.

---

## Thesis

- The raw is the source of truth and stays untouched. The normalized model + signals are
  **derived at read time** from the already-stored `request_body`, `response_reconstructed`
  and `request_headers`. No schema migration, no reprocessing, the parser can evolve freely.
- Analytics (B) require a **normalized substrate first**: a provider-agnostic conversation
  model. Signals are computed on that model, never on the Anthropic dialect directly — so
  future providers (OpenAI/Codex) plug in without rewriting the analytics.
- Session grouping — deferred in the POC — is **trivial and reliable for Claude Code**: the
  native `x-claude-code-session-id` header (also in `metadata.user_id.session_id`) is stable
  across every call. The `messages[]`-prefix heuristic is demoted to *intra-session*
  chaining / break detection, not the session key.

---

## Facts resolved from the environment (no decision needed)

Verified against `tracer.sqlite`:

- **Provider detection**: `x-app: cli`, `user-agent: claude-cli/2.1.219 (external, cli)`,
  endpoint `/v1/messages?beta=true`. Enough to route to the Claude Code parser.
- **Session key**: `x-claude-code-session-id` is identical across all calls of a run;
  `metadata.user_id` JSON carries `session_id`, `account_uuid`, `device_id`.
- **Sub-conversations**: within one session, `n_msgs` varies non-monotonically
  (`1,1,3,5,5,1,5,7,9`) — sub-agents / branches / compaction under one session id. Detect
  these with the `messages[]`-prefix heuristic *inside* a session.
- **Sizing**: `usage` only exposes aggregate totals (input/output/cache), never per-block.
  Per-block size must be approximated.

---

## Decisions (all resolved in grilling)

| Area | Decision |
| --- | --- |
| Approach | **(B) analytical / extraction** — not just a pretty conversation render |
| Substrate | **Normalized model first**; all signals computed on it |
| Timing | **Derived at read time** (raw stays source of truth; no migration) |
| Scale | **Both intra-call and inter-call (session)** |
| Session key | **Native provider id** (`x-claude-code-session-id`) + prefix fallback |
| Prefix heuristic role | Intra-session chaining / break detection only |
| Parser architecture | **`Provider` trait** + `core/src/parse/` module per dialect |
| Provider scope | **Trait + detection in place, only `claude_code.rs` implemented**; unknown → `raw` fallback |
| Req+Resp modeling | **Single thread**: `messages[]` turns + response appended as the last turn, flagged `new` |
| Per-block size | **chars/bytes approximation** (labeled "approx"); exact totals from `usage` |
| API | **New endpoints** for normalized + sessions; raw endpoints untouched |
| UI | **Session-centric redesign**; raw demoted to a "raw" view |
| Build order | model → parser → intra → session → inter → API → UI |

---

## Normalized model (`core/src/parse/model.rs`)

Provider-agnostic types the analytics layer consumes:

```
Provider              enum { ClaudeCode, Unknown }         // detected from headers

NormalizedCall
  id, provider, model, session_key: Option<String>
  timestamps { start, first_chunk, end }                  // → TTFT, latency
  system: Vec<SystemSegment>                               // split blocks
  declared_tools: Vec<ToolDecl>                            // name, (schema summary)
  thread: Vec<Turn>                                        // history + response, one timeline
  usage: Usage
  intra: IntraSignals                                      // computed
  error: Option<String>

Turn
  role: user | assistant | tool
  origin: history | new                                    // sent vs received (response)
  blocks: Vec<Block>

Block
  kind: text | thinking | tool_use | tool_result | image | other
  approx_size: { chars, bytes }                            // labeled approximate
  // kind-specific:
  //   tool_use    → tool_name, input (parsed), tool_use_id
  //   tool_result → tool_use_id (paired), is_error
  //   text/thinking → content (or preview + length)

SystemSegment  { text, approx_size, cache_control: bool }
Usage          { input, output, cache_creation, cache_read }  // from usage, exact
```

`Turn.origin` distinguishes the sent history from the newly received assistant turn. Blocks
carry an **approximate** size (chars/bytes, `~tokens ≈ chars/4` indicative only); exact
counts come from `Usage`.

---

## Provider abstraction (`core/src/parse/`)

```
parse/
  mod.rs            detect(headers) -> Provider ; parse_call(StoredCall) -> NormalizedCall
  model.rs          the normalized types above
  claude_code.rs    the only concrete impl: Anthropic messages[] + reconstructed → thread
  signals.rs        intra-call + inter-call signal computation over the normalized model
```

- `detect()` routes on `x-app` / `user-agent`. Unknown providers → `Provider::Unknown`,
  which yields a minimal `raw` normalization (no signals) so the UI never breaks.
- A `Provider` trait defines `parse(&StoredCall) -> NormalizedCall`; `claude_code.rs` is the
  single implementation. This proves extensibility without writing speculative OpenAI code.
- `claude_code.rs` reuses `reconstruct.rs` output for the response turn; it does **not**
  re-implement SSE parsing. Reconstruction (low-level) and normalization (semantic) stay
  separate.

---

## Signals

### Intra-call (`IntraSignals`, one call)

- **Token breakdown** — input / output / `cache_creation_input_tokens` /
  `cache_read_input_tokens`, surfaced prominently.
- **Typed-block inventory + sizes** — per message: count and kind of blocks, approx size of
  each, flag oversized blocks.
- **Tools declared vs called + args** — declared tools list, which were actually invoked
  (`tool_use`), parsed name + arguments, and `tool_use ↔ tool_result` pairing.
- **System prompt split + cache markers** — segment the system (string or array), mark
  `cache_control` cache points, size per segment.

### Inter-call (`SessionSignals`, calls of a session sorted by time)

- **Context growth** — series of context volume (`input_tokens`, and approx `messages[]`
  size) call over call. The headline signal.
- **Compaction / truncation detection** — flag a sharp drop in `n_msgs` / `input_tokens`
  between consecutive calls of a session.
- **System-prompt drift** — hash/diff the system prompt across a session's calls; flag when
  it changes mid-run.
- **Session timeline** — chronological calls with TTFT (`timestamp_first_chunk`), total
  latency, model, status/errors.

Session assembly: group by `session_key`; within a session, use the `messages[]`-prefix
heuristic to order/chain turns and detect sub-conversation breaks.

---

## Read-only API (extends `core/src/api.rs`)

Raw endpoints stay intact (source of truth). New derived endpoints:

```
GET /api/calls/:id/normalized   NormalizedCall + IntraSignals for one call
GET /api/sessions               grouped session list (key, calls count, model, span, flags)
GET /api/sessions/:key          calls sorted in time + SessionSignals
```

All derived on the fly from stored raw data; no new persistence.

---

## UI redesign (session-centric, `core/src/ui.html`)

Primary entry becomes the **session**:

1. **Sessions list** — one row per session (key, model, #calls, time span, error/compaction
   badges).
2. **Session view** — timeline of calls + **context-growth chart** + **system-prompt drift**
   markers + compaction flags.
3. **Call view (normalized)** — the single-thread conversation: typed blocks, token/cache
   badges, tools declared-vs-called, system segments with cache markers. Toggle to **raw**
   (existing JSON dump) as the fallback source-of-truth view.

---

## Build order

1. **Normalized types + `Provider` trait + detection** (`parse/model.rs`, `parse/mod.rs`).
2. **`claude_code.rs` parser** — `messages[]` + reconstructed response → single `thread`.
3. **Intra-call signals** (`signals.rs`) — tokens, block inventory, tools, system split.
4. **Session grouping** — native id key + prefix-based intra-session chaining.
5. **Inter-call signals** — context growth, compaction, drift, timeline.
6. **API endpoints** — `/normalized`, `/sessions`, `/sessions/:key`.
7. **Session-centric UI** — built last on already-solid data.

Each step is independently testable (unit tests on parser + signals against captured rows;
the existing `tracer.sqlite` gives real fixtures).

---

## Done-line (explicit)

Done when:

- Any captured Claude Code call renders as a **normalized single-thread conversation** with
  typed blocks, token/cache breakdown, and tools declared-vs-called.
- Calls are **grouped into sessions** via the native id, with a **context-growth** view,
  **compaction/truncation** flags, **system-prompt drift** markers, and a **timeline**.
- Everything is **derived at read time**; the raw capture and write path are unchanged.
- A `Provider` trait + header detection are in place with a single Claude Code impl and a
  safe `raw` fallback for unknown providers.

---

## Explicitly out of scope (Part 3+)

- Concrete OpenAI/Codex (Responses/Chat) parser implementations.
- Real tokenizer-based per-block counts (staying on chars/bytes approximation).
- Persisting the normalized model / caching derived views.
- Full cross-session conversation reconstruction (dedup-merged single thread).
- OpenTelemetry GenAI export, Tauri desktop app, configurable redaction.
