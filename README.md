# Orama — LLM Harness Tracer

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE.md)

A local, open-source tool that transparently intercepts a coding agent's API
traffic and shows you exactly what the harness sends on every call — the full
system prompt, message history, declared tools, and the reconstructed response.

Unlike SDK-based tools, Orama requires **no code changes and no SDK**: it works
with an unmodified Claude Code or Codex binary by pointing its base URL at a
local proxy. No MITM, no certificates. One listener serves both wire dialects —
each request is routed upstream by its own format, so Claude Code and Codex can
be traced at the same time.

Everything stays on your machine. The proxy binds to `127.0.0.1` by default, and
nothing it captures is ever sent anywhere. Besides relaying your traffic to the
upstream API, it makes exactly one other request: a daily conditional fetch of
the public model catalogue from [models.dev](https://models.dev), which is what
prices your captures. That request sends nothing but an `If-None-Match` header
and is usually answered with a 304. Set `ORAMA_CATALOG_REFRESH=0` to disable it —
a snapshot ships in the binary, so pricing still works offline.

## How it works

```text
Claude Code ─┐                      ┌─HTTPS─▶ api.anthropic.com
             ├─HTTP─▶ Orama proxy ──┤   (routed by wire dialect)
Codex ───────┘             │        └─HTTPS─▶ api.openai.com
                           │
                           ├─ tees every request/response (streaming included)
                           ├─ redacts auth headers, stores the rest verbatim
                           ├─ normalizes + derives signals at read time
                           └─ serves a read-only dashboard over the capture DB
```

Each request→response exchange is stored as one SQLite row (raw SSE **and**
reconstructed JSON). A background writer task owns the connection so persistence
never sits on the request path. Tracing is best-effort: it never blocks or
alters the client's request.

The raw capture is the source of truth and is never mutated. The normalized
conversation model, session grouping, and alerts are all **derived at read
time** — so the parser can evolve without a schema migration or reprocessing.

## Install

Requirements:

- **Rust** (stable, edition 2021) — via [rustup](https://rustup.rs/) or
  [mise](https://mise.jdx.dev/)
- **Node.js 20+** — only to build the dashboard bundle

```bash
git clone https://github.com/elie-laloum/orama
cd orama

# Build the web dashboard (embedded into the binary at compile time)
cd apps/web && npm install && npm run build && cd ../..

# Build the proxy
cargo build --release
```

The frontend step is optional: `core/build.rs` embeds a placeholder page when
`apps/web/dist/` is absent, so a clean clone always compiles and the JSON API
stays fully available. Only the HTML dashboard degrades.

If your toolchain is managed by mise, prefix cargo commands with
`mise exec rust --`.

### Desktop app

The same proxy and dashboard, in a window, for macOS, Windows and Linux.
Tagged releases publish installers (`.dmg`, `.msi`, `.deb`, `.AppImage`); to
build one yourself:

```bash
npm --prefix apps/desktop install
npm --prefix apps/desktop run build    # → target/release/bundle/
```

Linux additionally needs the system webview headers, which is why the desktop
crate is excluded from the default workspace build — `cargo build` and
`cargo test` stay green without them:

```bash
sudo apt install libwebkit2gtk-4.1-dev librsvg2-dev patchelf \
  build-essential curl wget file libxdo-dev libssl-dev
```

The app runs the relay in-process on port 8787 and opens a window on its own
`/ui`, so nothing about the dashboard is duplicated for the desktop. The
capture database lives in `$ORAMA_HOME` (default `~/.orama`) rather than the
working directory, because an app launched from a dock does not have a useful
one. `ORAMA_PORT`, `ORAMA_DB`, `ORAMA_UPSTREAM` and `ORAMA_UPSTREAM_OPENAI`
override the defaults.

**Quitting stops the proxy**, so the app disconnects any harness it had
pointed at itself before it exits — otherwise the next request from that
harness would fail against a closed port. An agent session that is *already
running* cannot be saved this way: both Claude Code and Codex read their
config at startup, so a live session keeps dialling the port until it is
restarted. If a session is mid-flight, leave the app open.

If port 8787 is already serving an Orama — say you also have `orama start`
running in a terminal — the app shows that one instead of refusing to open,
and leaves its configuration alone on exit.

## Usage

```bash
cargo run -- start
```

This boots the proxy and prints an export snippet. Paste the line for whichever
agent you are running:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787       # Claude Code
export OPENAI_BASE_URL=http://127.0.0.1:8787/v1       # Codex

claude            # runs normally; every call is captured
```

Your existing credentials are forwarded untouched — there is no token to
configure, and auth headers are redacted before anything is written to disk.

Then open the dashboard:

```text
http://127.0.0.1:8787/ui
```

### Connecting without the shell

The **Settings** surface (`/ui/#/settings`) shows what the proxy is bound to,
which upstreams it forwards to, whether capture is actually running, and — the
question the dashboard cannot otherwise answer — whether anything is pointed at
it at all. An unconfigured proxy and an unused one both produce an empty
dashboard, so connection state is read from the harness config files rather than
inferred from an empty capture.

From there, **Connect** wires up Claude Code or Codex by editing that harness's
own config file:

| Harness | File | Change |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | sets `env.ANTHROPIC_BASE_URL` |
| Codex | `~/.codex/config.toml` | adds `model_providers.orama` and selects it |

The Codex entry sets `requires_openai_auth`, not `env_key`. That distinction is
the whole game: `env_key` names an environment variable Codex must find, and a
Codex signed in through a ChatGPT plan has no API key to put in one, so it
refuses to start — breaking exactly the setup it was meant to trace.
`requires_openai_auth` hands the request to Codex's own credentials instead, and
a subscription then authenticates through the proxy normally.

It is a provider entry rather than a bare `openai_base_url` override because
that is the only place `supports_websockets = false` can go. Codex otherwise
opens each session by probing a WebSocket transport the proxy cannot forward,
retries it five times, and drops to HTTP several seconds later.

The base URL depends on how Codex authenticates, because the two modes are
different backends — `chatgpt.com/backend-api/codex` for a subscription,
`api.openai.com/v1` for an API key. The connector infers the mode from whether
`OPENAI_API_KEY` is set and says which it picked. Traffic finds its way back out
by path prefix: `/backend-api/*` is relayed to the ChatGPT backend, `/v1/*` to
the OpenAI one.

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` are honoured, so a relocated config
directory is edited where the harness will actually read it. The original file
is copied to `<name>.orama.bak` before the first edit, writes are atomic, and
**Disconnect** restores the previous value rather than assuming a default — a
base URL Orama did not set is never removed. Both harnesses read their config at
startup, so restart the agent for the change to take effect.

### Windows with WSL

Running the app on Windows while the agent runs inside WSL is two environments,
not one, and both halves of that have to be handled or neither works.

The config file is on the far side of a share, so Settings lists the harnesses
inside each distribution as their own rows — `Claude Code in Ubuntu` — alongside
the Windows ones, and edits them at `\\wsl.localhost\<distro>\...`. Which file
that is gets asked of the distribution rather than assumed: `CLAUDE_CONFIG_DIR`
is usually exported from an interactive shell rc, so the probe runs the login
shell under a pty to see what the harness itself would see.

The address is the other half. A WSL 2 guest has its own loopback, so
`127.0.0.1:8787` there is *the guest*, and a proxy bound only to the Windows
loopback is unreachable from it. Under the default NAT networking the proxy
therefore also listens on the virtual adapter the guest routes through, and that
is the address written into the guest's config. Two consequences worth knowing:

- **That address is reassigned when WSL restarts.** Settings says so on the row.
  Reconnect if capture stops. Setting `networkingMode=mirrored` under `[wsl2]` in
  `.wslconfig` makes `127.0.0.1` reach Windows for good, and the connector uses
  it instead when it is on.
- **The extra listener is one specific address, never `0.0.0.0`.** The dashboard
  is served by the same listener as the relay, so binding every interface would
  publish every captured prompt and response to the local network with nothing
  in front of it. The virtual adapter reaches the WSL VM and nothing else.

`ORAMA_WSL=0` turns discovery off entirely, and `orama start --no-wsl-bridge`
skips the extra listener. Nothing here applies on any other platform, or to a
proxy that is itself running inside WSL — that one is already in the same
network namespace as the harness, and needs none of it.

Anything else — the Anthropic and OpenAI SDKs, or an OpenAI-compatible host — is
listed on the same page with the exact line to paste.

### Flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `--port` | `8787` | Port the proxy listens on |
| `--host` | `127.0.0.1` | Interface to bind |
| `--upstream` | `https://api.anthropic.com` | Where Anthropic-dialect traffic goes |
| `--upstream-openai` | `https://api.openai.com` | Where OpenAI-dialect traffic goes |
| `--upstream-chatgpt` | `https://chatgpt.com` | Where `/backend-api/*` goes (Codex on a subscription) |
| `--db` | `orama.sqlite` | SQLite capture database path |

`--upstream-openai` is what makes an OpenAI-compatible provider traceable: the
parsers key off the wire format, not the vendor, so pointing it at OpenRouter,
Together, vLLM or Ollama captures that traffic the same way.

Log verbosity follows `RUST_LOG` (default `info`); logs go to stderr.

The binary also exposes `orama derive`, which rebuilds the derived analytics
tables from the raw captures (`--rebuild` forces a full re-derivation). Raw
captures are never modified.

> **Binding to a non-loopback `--host` exposes your captured traffic — including
> full prompts — to anyone who can reach that port. There is no authentication
> on the API.** See [SECURITY.md](SECURITY.md).

## API

Read-only over the capture database (GET only):

| Endpoint | Returns |
| --- | --- |
| `GET /api/calls` | Captured calls, most recent first |
| `GET /api/calls/:id` | Full raw detail of one call |
| `GET /api/calls/:id/normalized` | Provider-normalized conversation model |
| `GET /api/v2/harness` | Distinct system prompts, tool sets, and where the context goes |
| `GET /api/v2/harness/system/:hash` | One system prompt, verbatim, segment by segment |
| `GET /api/v2/harness/tools/:hash` | One tool set: every declaration, its size, its use |
| `GET /api/v2/generations/:span/context` | One call's context, split into system / tools / thread |
| `GET /api/calls/:id/diagnostics` | Alerts derived for that call |
| `GET /api/sessions` | Sessions assembled from captured calls |
| `GET /api/sessions/:key` | One session with its call timeline |
| `GET /api/sessions/:key/diagnostics` | Session-level alerts |
| `GET /api/signal-policy` | Thresholds the detectors run against |
| `GET /api/ui/dashboard` | Aggregates backing the dashboard |
| `GET /api/ui/alerts` | Alert feed (filterable, paginated) |
| `GET /api/ui/events` | SSE stream of live updates |
| `GET /api/v2/settings` | Proxy state, connector status, and setup snippets |
| `GET /ui` | The dashboard SPA |
| `GET /healthz` | Health probe |

Two routes write, and they are the only ones. Neither touches the capture
database — they edit a harness's own config file, as described above:

| Endpoint | Effect |
| --- | --- |
| `POST /api/v2/connectors/:id/connect` | Point `claude-code` or `codex` at this proxy (`claude-code@Ubuntu` for one inside WSL) |
| `POST /api/v2/connectors/:id/disconnect` | Restore what was there before |

Two more change what the proxy does without writing anything at all — the state
lives in memory for the life of the process:

| Endpoint | Effect |
| --- | --- |
| `POST /api/v2/proxy/stop` | Stop forwarding. Proxied requests are refused with a 503; the dashboard, served by the same listener, stays up |
| `POST /api/v2/proxy/start` | Forward again |

This is the badge in the top-left of the dashboard: green and `live` while
traffic is flowing, red and `stopped` when it is not, and a click either way.
Stopping is a decision about the running session only — a restart always comes
back forwarding, because a proxy that refused traffic on launch because of a
click from yesterday is indistinguishable from a broken one.

Every other path and method is transparently relayed upstream.

### The harness view

The other surfaces answer *what happened in this call*. The **Harness** surface
answers *what was the model actually looking at*: the system prompt and the tool
declarations wrapped around every conversation.

That framing matters because the wrapper is usually bigger than the thing it
wraps. On a real Claude Code main-loop call:

```text
system prompt      11 K chars    2.8%
tool declarations 326 K chars   81.6%   ← 121 tools, re-sent every turn
conversation       62 K chars   15.6%
```

Both are re-sent in full on every turn, so the surface shows each distinct
system prompt and tool set once, keyed by a content fingerprint — a rewritten
tool description is a different harness, and shows up as one. Per tool it
reports the characters that declaration costs against how many times it was
actually called, which is the only place the two can be compared.

Alerts are read-only diagnostics: each carries a severity, what was observed,
the likely cause, the impact, a suggested action, and a link back to the source
call or block. Missing data is reported as an explicit neutral state, never as
zero or "healthy".

## Layout

Cargo workspace plus a frontend app:

- `core/` — the library: config, relay, streaming tee, SSE reconstruction,
  SQLite store, provider parsing/normalization, signals and diagnostics,
  read-only API. Both front ends are thin wrappers over it.
- `cli/` — thin `orama` binary.
- `apps/web/` — React + TypeScript dashboard (Vite), embedded into the binary
  at build time.
- `apps/desktop/` — Tauri shell for macOS, Windows and Linux. Runs the same
  server in-process and opens a window on its `/ui`, so there is one dashboard,
  not two.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
cargo fmt

cd apps/web
npm run dev          # dashboard dev server
npm run typecheck
```

Integration tests in `core/tests/` cover the relay, streaming tee, persistence,
the JSON API, and structured-data normalization. They run against ephemeral
databases and a stub upstream — no network and no real API key required.

## Contributing

Issues and pull requests are welcome on
[GitHub](https://github.com/elie-laloum/orama). Before opening a PR:

1. Keep the capture path untouched unless that is the point of the change —
   tracing must stay best-effort and never block or alter the client's request.
2. Derive new analysis at read time from stored raw data rather than adding
   write-path schema.
3. Run `cargo test`, `cargo clippy --all-targets`, and `cargo fmt`.
4. Never commit a capture database, a real API key, or a transcript containing
   one.

Maintainers and review expectations: [MAINTAINERS.md](MAINTAINERS.md).
Vulnerability reports: [SECURITY.md](SECURITY.md) — please do not open a public
issue for those.

## Security note

The dashboard renders captured, potentially untrusted request/response bodies.
All DOM is built through React's escaping text paths (never `innerHTML` /
`dangerouslySetInnerHTML`), so no captured string can inject markup. Auth
headers (`authorization`, `x-api-key`, `proxy-authorization`) are redacted
before write — both at the call site and again in the store as a final gate —
and are never exposed by the API.

Everything else is stored **verbatim**, including full prompts, file contents
read by the agent, and tool output. Treat the capture database as sensitive.

## License

[Apache-2.0](LICENSE.md).
