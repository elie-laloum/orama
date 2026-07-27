# CLAUDE.md

Working notes for agents on this repository. See `DESIGN.md` for why the system
is shaped the way it is, and `README.md` for what it does.

## What this is

A local transparent HTTP proxy between a coding-agent CLI (Claude Code, Codex,
opencode) and its model provider. It captures every round trip into SQLite,
derives analytics from those captures, and serves a dashboard over them. No SDK,
no MITM, no certificates — the client points `ANTHROPIC_BASE_URL` or
`OPENAI_BASE_URL` at it, or the Settings surface writes that config for it.

One listener serves every dialect: `relay.rs` picks the upstream per request —
`/backend-api/*` by path prefix, everything else via `parse::detect` — so Claude
Code and Codex can be traced at once.

## Commands

```sh
cargo build                       # workspace; build.rs embeds apps/web/dist if present
cargo test                        # 157 tests: unit + spawn-on-port-0 integration

# WSL discovery is Windows-only at runtime. To exercise the real probe from
# inside a distro (starts it; prints the paths and gateway it resolves):
ORAMA_WSL=1 cargo test -p orama-core --lib probe_a_real_machine -- --ignored --nocapture
cargo fmt --all && cargo clippy --all-targets

cargo run -- start --db orama.sqlite          # relay + API + dashboard on :8787
cargo run -- derive --db orama.sqlite         # backfill derived tables
cargo run -- derive --db X --rebuild          # discard and re-derive

cargo run -- catalog show --db orama.sqlite   # which price snapshot is in force
cargo run -- catalog refresh --db X           # fetch rates now and re-price
cargo run -- catalog vendor                   # regenerate the bundled snapshot

npm --prefix apps/web run build    # emits dist/, embedded on the next cargo build
npm --prefix apps/web run dev      # Vite on :5173, proxies /api to :8787
npm --prefix apps/web run typecheck

npm --prefix apps/desktop run build   # installers into target/release/bundle/
cargo test -p orama-desktop -- --test-threads=1   # not in the default set
```

The default database is `orama.sqlite`. Older captures may live in
`tracer.sqlite` — pass `--db` explicitly.

Model rates and limits are not maintained in this repo. They come from
[models.dev](https://models.dev): a snapshot ships in the binary, a running proxy
refreshes it daily, and `catalog vendor` is the only supported way to regenerate
the bundled copy. `ORAMA_CATALOG_REFRESH=0` disables the network entirely — the
test suite sets it so a build machine's connectivity cannot change what the tests
assert.

## Layout

```
core/src/
  relay.rs        catch-all proxy; routes by dialect, tees SSE unchanged
  store.rs        raw `calls` table, migrations, background writer
  reconstruct/    SSE → assembled response, per provider dialect
  parse/          provider parsers → one NormalizedCall shape
  derive/         normalized → materialized generations/tool_calls/sessions
  detect/         SQL detector catalogue → persisted alerts
  pricing.rs      cache multipliers, price bands, cost attribution
  catalog/        cached models.dev snapshot: rates, limits, capabilities
  connect.rs      the only module that writes: harness config files
  api/            v2 read API + SSE + settings; v1 is legacy
apps/web/src/     Vite + Tailwind v4 + React dashboard
apps/desktop/     Tauri shell: a window onto the same server, three platforms
```

## Invariants

These are load-bearing. Breaking one is a correctness bug, not a style choice.

**`calls` is append-only truth.** Every other table is a pure function of it, the
parser version, and the model catalogue snapshot. Never write derived data into
`calls`, and never make a derived table authoritative — a rebuild must be able to
reproduce it exactly. The catalogue is the third input because rates are no
longer maintained here; that is why each snapshot has a digest, why every priced
row records the digest that priced it, and why derivation reads the snapshot
stored in the database rather than the network. A rebuild against the same
snapshot is still exactly reproducible.

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

**A streamed capture persists from `Drop`, not from the end of the stream.**
An `async_stream` body only runs past its last `yield` if it is polled again,
and a client that stops reading the moment it has what it needs never gives it
that poll. With the enqueue in the tail, every SSE turn a client abandoned was
relayed perfectly and never recorded — Codex lost every turn after the first,
and nothing reported a capture missing. Never move persistence back into the
generator body.

**The relay must never alter the exchange.** Capture failures go to stderr;
tracing is best-effort and the client's bytes are forwarded unchanged. Derivation
runs after the raw insert commits, inside `catch_unwind`, so a parser panic
records a `derive_failures` row rather than losing the capture. Choosing which
upstream to forward to is not altering the exchange — but the choice must come
from `parse::detect`, the same function that later picks the parser, or a call
could be relayed as one dialect and read back as another.

**Stopping is the one refusal, and it is explicit and unpersisted.**
`POST /api/v2/proxy/stop` flips a flag the catch-all reads, and every proxied
request is then refused with a 503 naming Orama — never dropped, never quietly
passed through. The listener is not released: the dashboard is served by it, and
giving it up would take down the surface holding the button that puts it back.
The flag lives in memory only. Persisting it would mean a proxy that comes up
refusing traffic because of a click in a previous session, which from the
harness's side is indistinguishable from a broken install.

**`connect.rs` is the only writer outside `store.rs`, and it writes no data.**
It edits files that belong to a harness, never anything Orama derives from. Its
three rules are load-bearing: edits are surgical (one key, comments and ordering
preserved), reversible (the replaced value is recorded, and a key we did not set
is never removed), and atomic (temp file plus rename). A config that cannot be
parsed is refused rather than rewritten — clobbering settings we never read
would be worse than not connecting. All three hold across the WSL share too:
`rename` over `\\wsl.localhost\...` replaces atomically and the written file
lands owned by the distro's own user.

**A connector target is a harness *and* a place.** `Target = (Harness, Site)`,
where `Site` is `Local` or `Wsl(distro)`, because one harness can exist in more
than one environment at once on the same machine. Two things follow that are easy
to get wrong. A local target's id stays the bare `claude-code` — the state file
is keyed by it, so changing it would orphan the record of what we overwrote and
turn the next disconnect into a guess. And every question that used to have one
answer per harness now has one per target: which config file, which base URL,
which environment `OPENAI_API_KEY` is read from. Answering any of them for the
wrong site writes a working-looking config that captures nothing. The dashboard
splits the list by site for the same reason — one flat list of near-identical
rows is an invitation to connect the wrong install.

**Context for the WSL bridge: a guest cannot reach a loopback-bound Windows
listener.** Measured, not inferred — a Windows server on `127.0.0.1` refuses a
WSL 2 guest's connection, and the same server reachable on the vEthernet address
answers it. So bridging needs both halves: the address written into the guest is
the gateway of its default route, and the proxy binds that address as well. The
extra bind is always one specific address and **never `0.0.0.0`** — the dashboard
shares the listener with the relay, so binding every interface would publish the
whole capture to the local network unauthenticated. Under `networkingMode=mirrored`
and on WSL 1 loopback already works and nothing is bound. `ORAMA_WSL=0` disables
discovery, and the connector tests set it so a run cannot spawn `wsl.exe`.

**A relocated config directory inside WSL can only be found with a tty.**
`CLAUDE_CONFIG_DIR` is typically exported from an *interactive* shell rc. `sh -lc`
misses it and so does `zsh -lc`; `zsh -lic` finds it, but only when stdin is a
tty, which a windowed Windows process does not give its children. The probe
therefore runs the login shell under `script -qec`. Without that it reports "not
set" for a variable that is set, and the connector edits a file the harness never
opens — which is indistinguishable from the bug this bridge was built to fix.

**Derived tables store fingerprints, not content.** Request bodies are ~96% of
the database because each call re-sends the whole conversation. Derived rows hold
hashes, counters, and short excerpts; the raw bytes stay addressable by
`call_id`.

**The catalogue is a cache, not a capture and not a derivation.** `model_catalog`
holds one row: the models.dev payload in force, verbatim. It is the only table
whose content can change without any call being recorded, so it is also the only
one that carries its own provenance — digest, ETag, when it was downloaded, and
when it was last confirmed current. `catalog/cache.rs` writing it is a deliberate
extension of the `connect.rs` rule above, not an oversight: the payload is
neither raw capture nor a function of one, so it fits under neither existing
rule. A payload that fails to parse, or that is missing the providers we price
against, is refused and the previous snapshot stays in force — un-pricing traffic
that was priced a minute ago would be worse than being a day out of date.

**The desktop shell owns a window, not a copy of anything.** `apps/desktop`
binds the same server via `orama_core::bind` and points a webview at that
server's own `/ui` over loopback. There is deliberately no second frontend
bundle, no `VITE_API_BASE`, and no Tauri IPC command: the page is same-origin
with the API, so a fix in the browser is a fix in the app. Adding an IPC
surface would create a second way to read the data, and the two would drift.
Its binary is `orama-desktop`, never `orama` — two workspace members emitting
the same filename overwrite each other in `target/`. It is a workspace member
but not a default one, so `cargo build` and `cargo test` stay green on a
machine with no WebKit headers.

**Quitting the app restores what it changed, but only what it owns.** A
harness pointed at a proxy that has stopped fails every request with a
connection refused, so exit disconnects the harnesses on the way out. Two
limits are real and must not be papered over: a *running* agent session cannot
be rescued, because both harnesses read their config at startup; and a window
that attached to an already-running `orama start` restores nothing, because
that server outlives it.

## Conventions

- Schema changes are a new `Migration` entry in `store.rs`. Steps run in order
  under `PRAGMA user_version`; databases predating migrations sit at 0 with v1
  tables already present, so every step must tolerate re-running.
- The three versions invalidate different things, and only two of them are
  compared. Bumping `PARSER_VERSION` means the derived rows are wrong, so the
  layer is discarded and rebuilt from the captures. A change to
  `pricing_version()` — the rules version *or* the catalogue snapshot behind it —
  means only the cost columns are wrong, so `reprice` updates them in place;
  re-parsing megabytes of request bodies to redo a multiplication would be
  waste, and a snapshot refreshes far more often than a parser is bumped. Both
  are recorded in `meta` and checked at startup. `POLICY_VERSION` is compared
  against nothing: `detect::evaluate` deletes and rebuilds every alert on each
  derive, so alerts cannot go stale in the first place. Raw captures are never
  touched by any of it.
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
python3 -c "import sqlite3; s=sqlite3.connect('file:orama.sqlite?mode=ro',uri=True); \
            s.backup(sqlite3.connect('/tmp/check.sqlite'))"
cargo run -- derive --db /tmp/check.sqlite --rebuild
cargo run -- start --port 8790 --db /tmp/check.sqlite
```

Use SQLite's backup API rather than `cp` — the live database has an 8 MB WAL, and
`cp` alone yields a malformed image.

The capture grows as you work, so absolute counts drift between runs; compare a
copy against itself before and after a change rather than against a number
written down here. What must hold on any copy:

- **0 derive failures.** Anything else is a parser regression.
- **Every unpriced generation reported no usage at all.** The query that must
  return zero:
  `SELECT COUNT(*) FROM generations WHERE cost_total_usd IS NULL AND (input_tokens
  IS NOT NULL OR output_tokens IS NOT NULL OR cache_read_tokens IS NOT NULL)`.
  A row with counters and no cost means the catalogue lost a model.
- **A detector firing on more than a few dozen rows** is almost certainly noise.

The last full check: 1,308 generations, $178.29, 17 unpriced (all of them
responses that were never captured — the query above still returns zero), 0
`data_quality.pricing_unknown` alerts, 0 derive failures.

Earlier notes here said 69 generations on a `tracer.sqlite` that no longer exists
in the tree, then 695 and $88.13. All are stale — the numbers move because the
capture keeps growing, not because anything regressed. Compare a copy against
itself, never against the figure written here.

## Known gaps

- Codex 0.145 on a ChatGPT subscription is captured end to end and *does* send
  its token through a custom `openai_base_url`. `core/tests/codex.rs` pins the
  real wire shapes. opencode is still unexercised, and no Codex capture yet
  contains a tool call, so the OpenAI tool path remains fixture-only.
- Orama does not proxy WebSockets. The Codex connector sidesteps this with
  `supports_websockets = false`, so the probe never happens — but any client
  that insists on a WebSocket transport is invisible to capture, and the relay
  answers its upgrade with a plain 405.
- **Anthropic's long-context premium is invisible to us.** Anthropic charges more
  above 200k input tokens, and models.dev publishes no `tiers` for any Anthropic
  model — only OpenAI entries carry them. Peak observed context is 418,978 tokens
  on `claude-opus-5`, so those calls are priced in the base band and their cost is
  understated. The tier machinery is in place and will pick the premium up the day
  models.dev publishes it. The hand-maintained table had the same blind spot, so
  this is inherited, not new.
- **Fast mode is priced as if it were standard.** models.dev lists an
  `experimental.modes.fast` band at double base for Claude models, gated on the
  `anthropic-beta: fast-mode-2026-02-01` header. Nothing in the capture uses it,
  and the parser already reads request headers, so honouring it is a small
  follow-up — but until then a fast-mode call would be billed at half what it
  cost.
- Model ids that are neither an exact catalogue entry nor an exact entry after
  stripping a build date or `-latest` are left unpriced. `gpt-5.6-codex` is the
  live example. This is deliberate: 926 ids in the catalogue are priced
  differently from the shorter sibling that prefixes them, so guessing from a
  prefix is how you bill `gpt-4o` at `gpt-4` rates — twelve times over.
- The connectors are tested against sandboxed config directories
  (`CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `ORAMA_HOME` are repointed at a temp
  dir, and `ORAMA_WSL=0` keeps discovery from spawning `wsl.exe`). Never run
  those tests without the sandbox — they write real files.
- **The WSL bridge has never been run from a Windows build.** Its decisions are
  pure functions tested on recorded bytes, the probe was verified against a real
  distro via `ORAMA_WSL=1`, the reachability model was measured with real
  sockets, and a temp-plus-rename write over `\\wsl.localhost` was confirmed to
  land with the right ownership. What is unverified is the assembled whole
  running as a Windows process: no `orama-desktop.exe` was compiled or launched.
  The parts most likely to be wrong there are the ones no Linux test can reach —
  whether binding the vEthernet address is permitted without elevation, and
  whether a windowed process's `wsl.exe` child behaves like the one a shell
  spawns.
- Mirrored networking is handled but unexercised: this machine runs NAT, and
  confirming the mirrored path means `wsl --shutdown`, which cannot be done
  mid-session. `127.0.0.1` reaching Windows in that mode is documented by
  Microsoft rather than measured here.
- Subagent nesting is implemented but unexercised: no capture contains an `Agent`
  tool invocation. It degrades to a flat trace rather than guessing.
- No frontend tests.
- v1 API and the read-time `parse/session` + `parse/diagnostics` modules are
  superseded and unused by the dashboard.
