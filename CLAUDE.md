# CLAUDE.md

Working notes for agents on this repository. See `DESIGN.md` for why the system
is shaped the way it is, and `README.md` for what it does.

## What this is

A local transparent HTTP proxy between a coding-agent CLI (Claude Code, Codex,
opencode) and its model provider. It captures every round trip into SQLite,
derives analytics from those captures, and serves a dashboard over them. No SDK,
no MITM, no certificates — the client points `ANTHROPIC_BASE_URL` at it.

## Commands

```sh
cargo build                       # workspace; build.rs embeds apps/web/dist if present
cargo test                        # 83 tests: unit + spawn-on-port-0 integration
cargo fmt --all && cargo clippy --all-targets

cargo run -- start --db orama.sqlite          # relay + API + dashboard on :8787
cargo run -- derive --db orama.sqlite         # backfill derived tables
cargo run -- derive --db X --rebuild          # discard and re-derive

npm --prefix apps/web run build    # emits dist/, embedded on the next cargo build
npm --prefix apps/web run dev      # Vite on :5173, proxies /api to :8787
npm --prefix apps/web run typecheck
```

The default database is `orama.sqlite`. Older captures may live in
`tracer.sqlite` — pass `--db` explicitly.

## Layout

```
core/src/
  relay.rs        catch-all proxy; tees SSE while forwarding bytes unchanged
  store.rs        raw `calls` table, migrations, background writer
  reconstruct/    SSE → assembled response, per provider dialect
  parse/          provider parsers → one NormalizedCall shape
  derive/         normalized → materialized generations/tool_calls/sessions
  detect/         SQL detector catalogue → persisted alerts
  pricing.rs      model rates and cost attribution
  api/            v2 read API + SSE; v1 is legacy
apps/web/src/     Vite + Tailwind v4 + React dashboard
```

## Invariants

These are load-bearing. Breaking one is a correctness bug, not a style choice.

**`calls` is append-only truth.** Every other table is a pure function of it and
the parser version. Never write derived data into `calls`, and never make a
derived table authoritative — a rebuild must be able to reproduce it exactly.

**Absent is not zero.** A counter the provider never reported is `NULL`, never
`0`. This holds from the database through the API to the screen: pricing returns
no cost for a call with no usage, and the UI renders `—`, not `$0.00`. Reporting
an unmeasured call as free is a confident wrong answer.

**A condition true of every row is not a signal.** Detectors that fire on all
traffic are noise, and this has bitten repeatedly — `overage_status: rejected`
appears on every response, `context_management` on nearly every request, and
every captured HTTP 429 is a one-token quota probe rather than failed work. New
rules need a co-condition, and should be checked against the real capture before
being trusted.

**Context is not `input_tokens`.** Context is input + cache read + cache write.
`input_tokens` is only the uncached remainder and can be single digits on a
170k-token prompt. Any ratio, trend, or column that means "how big is the
prompt" must use the sum.

**The relay must never alter the exchange.** Capture failures go to stderr;
tracing is best-effort and the client's bytes are forwarded unchanged. Derivation
runs after the raw insert commits, inside `catch_unwind`, so a parser panic
records a `derive_failures` row rather than losing the capture.

**Derived tables store fingerprints, not content.** Request bodies are ~96% of
the database because each call re-sends the whole conversation. Derived rows hold
hashes, counters, and short excerpts; the raw bytes stay addressable by
`call_id`.

## Conventions

- Schema changes are a new `Migration` entry in `store.rs`. Steps run in order
  under `PRAGMA user_version`; databases predating migrations sit at 0 with v1
  tables already present, so every step must tolerate re-running.
- Bumping `PARSER_VERSION`, `POLICY_VERSION`, or `PRICING_VERSION` invalidates
  the derived layer and triggers a rebuild. Raw captures are never touched.
- Detector thresholds live only in `SignalPolicy`. A test fails the build if a
  rule leaves a `{placeholder}` unresolved.
- API filters are an allowlist; an unknown one is a 400. A silently ignored
  filter misrepresents the data.
- v2 rows serialize by column name, so a migration's new column reaches the API
  with no second definition to drift.
- Tests use the spawn-on-port-0 pattern in `core/tests/`. Prefer asserting
  behaviour over shape — a test that pins an implementation detail breaks on
  every refactor without catching anything.

## Verifying

Unit tests do not catch the failures that matter most here, because the bugs are
about what real traffic looks like. Check work against the real capture:

```sh
cp tracer.sqlite /tmp/check.sqlite
cargo run -- derive --db /tmp/check.sqlite --rebuild
cargo run -- start --port 8790 --db /tmp/check.sqlite
```

Expected on that capture: 73 generations, 423 tool calls, 18 traces, 0 derive
failures, $6.37 spend against $27.13 uncached. A detector that fires on more
than a few dozen rows there is almost certainly noise.

## Known gaps

- No Codex or opencode captures exist, so the OpenAI parser is covered by
  synthetic fixtures only.
- Subagent nesting is implemented but unexercised: no capture contains an `Agent`
  tool invocation. It degrades to a flat trace rather than guessing.
- No frontend tests.
- v1 API and the read-time `parse/session` + `parse/diagnostics` modules are
  superseded and unused by the dashboard.
