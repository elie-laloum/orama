# Design

Why Orama is shaped the way it is. `README.md` covers what it does; `CLAUDE.md`
covers how to work on it.

---

## The problem

An agentic coding session is opaque in a way ordinary LLM usage is not. One user
turn becomes dozens of API calls. Background agents run alongside the main loop
without the user ever seeing them. Nearly all the cost is cache, not prompt. And
the harness re-sends the entire conversation every turn, so the thing you are
paying for is not the thing you typed.

Existing observability answers the wrong question. It shows you requests. What
you need to know is: what did this turn cost, which agent spent it, what did the
tools actually return, and what went wrong that nobody surfaced.

## Capture: the proxy

The client points `ANTHROPIC_BASE_URL` at a local process. No SDK, no
certificate, no code change — an unmodified binary works. The proxy forwards
everything verbatim and tees a copy to SQLite.

Two properties matter more than features:

**The exchange is never altered.** Streaming responses are forwarded chunk by
chunk as they arrive while a copy accumulates for storage. Capture failures go to
stderr. If tracing breaks, the agent keeps working.

**Raw is truth.** `calls` holds the request and response as sent, and is
append-only. Every table beyond it is a pure function of `calls` and a parser
version — droppable and rebuildable at any time. That property is what makes it
safe to change the analysis: a wrong heuristic costs a rebuild, never data.

## Storage: two layers

```
calls (raw, immutable)
  └── generations ── tool_calls
      sessions ── alerts
```

The first version derived everything at read time. It could not survive
contact with real data: the alerts endpoint re-normalized every stored body
O(sessions × calls) times per request, and listing calls loaded the entire
database into memory to answer one page.

So the derived layer is materialized and indexed, written by a stage that runs
in the writer **after the raw insert commits**, wrapped in `catch_unwind`. A
parser panic records a `derive_failures` row and surfaces as a critical finding —
our own bugs are visible in the product rather than swallowed.

Derived rows store fingerprints, counters, and short excerpts — never content.
Request bodies are ~96% of the database, because every call re-sends the whole
conversation. Copying that into a second table would double the footprint to
duplicate what is already addressable by `call_id`.

## The trace model

The words mean something specific for a CLI harness, and it is not what they
mean for a request-scoped web service:

| | |
|---|---|
| **Session** | One CLI run. Comes from the harness; not inferred. |
| **Trace** | One user turn — a human message until the harness goes idle. Many calls, because each tool result costs another round trip. |
| **Generation** | One API call. The only span kind that costs money. |
| **Agent** | A distinct harness persona: the main loop, a spawned subagent, a background classifier, a quota probe. |

Ids are `blake3` over stable inputs, so a rebuild reproduces them exactly and
links keep resolving.

### Agents are classified from what the provider sends

The captured traffic turned out to carry a discriminator nobody documents: the
first system segment is a billing header whose `cc_version` suffix differs per
harness agent. Across 66 captures it separates cleanly:

| suffix | model | tools | what it is |
|---|---|---|---|
| `.85f` `.51a` `.5bd` | opus | 121–122 | main loop |
| `.ea8` | sonnet | 0 | security-monitor classifier |
| `.564` | haiku | 119 | subagent |
| `.f85` | haiku | 0 | title generator |
| `.25c` | opus | 0 | session summarizer |

Classification is therefore a rule table over observable fields — a one-token
toolless call with a `"quota"` message is a probe; a toolless call bounded by a
stop sequence is a classifier — corroborated by a provider-supplied variant id
rather than resting on a similarity heuristic.

**The billing variant outranks every shape heuristic**, and it has to. An earlier
version keyed on tool count and split one session into "main" (122 tools) and a
phantom "subagent" (121) — a single agent whose tool count wobbled when an MCP
server connected mid-session.

## Cost

Nothing else here changes behaviour as directly, and nothing else is as easy to
get quietly wrong.

Cache is priced as a multiple of the base input rate: a 5-minute write costs
1.25×, a one-hour write 2×, and a read 0.1×. That read discount is why cache
dominates agentic spend. On the reference capture:

| | |
|---|---|
| Actual spend | **$6.37** |
| Without caching | **$27.13** |
| Saved | **$20.76 (77%)** |

Cache write plus cache read is **95%** of the opus spend. Fresh input and output
are a rounding error beside it. Every generation therefore records what it would
have cost uncached, so the saving is a measured number rather than a claim.

Two rules keep the totals honest. An unknown model yields **no** cost, never
zero. And a call that reported no usage likewise yields no cost — it is not a
free call, it is one whose response was never captured, and pricing it at $0.00
would report it as free. That second rule was added after a first run showed 25
sidechain calls at $0.00.

## Detection

Rules are SQL over the derived tables, persisted to `alerts`, each carrying its
own explanation, impact, and recommendation. A finding that only names itself
leaves the reader exactly where they started.

The hard part is not writing detectors. It is not writing noise.

Three rules in the first draft fired on nearly every row, and each is instructive:

- **Every 1h cache write** — 39 of 41 priced calls. Long sessions genuinely want
  that TTL. The signal is a 1h write in a session that then *finished inside the
  hour*.
- **Every request carrying `context_management`** — all 39 that had it. Claude
  Code sends it as standing configuration. The event is a measured context drop,
  not the declaration.
- **That drop, measured on `input_tokens`** — which produced "context fell from 2
  to 1 tokens", because `input_tokens` is the uncached remainder, not the
  context.

Fixing those took the catalogue from 142 alerts to 67 on identical data with
nothing real lost. Two rules survive mainly to *avoid* false alarms: every
captured HTTP 429 is a one-token quota probe rather than failed work, and
`overage_status: rejected` appears on all 66 responses at 2% utilization.

The general principle: **a condition true of every row is not a signal.** New
rules are checked against the real capture before being trusted.

## Providers

`Provider` (the wire dialect) and `Framework` (the harness) are separate axes —
several harnesses speak the same dialect, and which one sent the traffic is what
makes per-agent analysis mean anything.

Two dialects are implemented, and the second exists to prove the abstraction is
real rather than asserted: an integration test checks that an OpenAI call
produces the same `generations` and `tool_calls` rows as an Anthropic one.
Writing it surfaced three places where the single-provider design had leaked —
reconstruction assumed Anthropic SSE, usage extraction read Anthropic field
names, and `Role` had no `System` variant despite 92 such turns in the capture.

## API

v2 is indexed SQL over the derived tables; a page costs one query regardless of
history. Rows serialize by column name rather than through hand-written DTOs, so
a migration's new column reaches the client with no second definition to drift.

Filters are an allowlist and an unknown one is a **400**. A silently ignored
filter is worse than a rejected one: the caller believes they are looking at a
subset and they are not.

Coverage travels with every total — `meta` reports usage and cost coverage, each
cost bucket carries `priced_share` — so a partial total is never presented as
complete.

SSE is published by the writer after a capture is durable and again after it is
derived, so a client reacting to the second event finds every derived table
consistent. The stream carries notifications, not state: a client that falls
behind is told to resync and re-reads, because there is nothing to reconcile.

## Interface

Density over decoration: 13px base, 30px rows, hairline borders, one accent,
tabular figures wherever numbers are compared down a column. Self-hosted fonts —
a local tool should not need a CDN.

Three rules carry more weight than the visual style.

**Absent is not zero.** Null renders as an em dash in its own hue, never `0`,
`$0.00`, or `0ms`. Coverage sits beside the totals it qualifies.

**Severity is never colour alone.** Every badge pairs a hue with an icon and a
word, and confidence is shown separately, so a finding derived from data we do
not have never reads as fact.

**Findings explain themselves.** The previous UI computed explanation, impact,
and recommendation on the server, shipped them over the wire, and rendered only
the title. Errors groups by rule — 22 identical tool failures are one problem,
not 22 — and shows the whole diagnostic.

Raw capture is a separate on-demand tab. It is evidence, not the default view,
and the bodies run to megabytes.

## Things deliberately not done

- **No content-addressed message dedup.** It would cut the database ~90%, since
  every call re-sends the same conversation. Real, and out of scope; `prune` is
  the escape hatch.
- **No alert mutation.** The API is GET-only. Alerts are derived, so acking one
  would be state that a rebuild destroys.
- **No sampling or truncation of captures.** Raw is truth or it is not.
- **Subagent nesting is not trusted.** It is implemented, but no capture contains
  an `Agent` invocation, so it degrades to a flat trace rather than guessing.

## Open gaps

No Codex or opencode captures exist, so that parser rests on synthetic fixtures.
There are no frontend tests. The v1 API and the read-time `parse/session` and
`parse/diagnostics` modules are superseded and unused.
