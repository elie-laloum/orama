# Orama — LLM Harness Tracer

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE.md)

A local, open-source tool that transparently intercepts Claude Code's API traffic
and shows you exactly what the harness sends on every call — the full system
prompt, message history, declared tools, and the reconstructed response.

Unlike SDK-based tools, Orama requires **no code changes and no SDK**: it works
with an unmodified Claude Code binary by pointing `ANTHROPIC_BASE_URL` at a local
proxy. No MITM, no certificates.

Everything stays on your machine. The proxy binds to `127.0.0.1` by default and
the only network egress is the relay to the upstream API.

## How it works

```text
Claude Code ──HTTP──▶  Orama proxy  ──HTTPS──▶ api.anthropic.com
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

## Usage

```bash
cargo run -- start
```

This boots the proxy and prints an export snippet. Paste it into the shell that
runs Claude Code:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_AUTH_TOKEN=<your-anthropic-token>
claude            # runs normally; every call is captured
```

Then open the dashboard:

```text
http://127.0.0.1:8787/ui
```

### Flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `--port` | `8787` | Port the proxy listens on |
| `--host` | `127.0.0.1` | Interface to bind |
| `--upstream` | `https://api.anthropic.com` | Upstream API base URL |
| `--db` | `tracer.sqlite` | SQLite capture database path |

Log verbosity follows `RUST_LOG` (default `info`); logs go to stderr.

> **Binding to a non-loopback `--host` exposes your captured traffic — including
> full prompts — to anyone who can reach that port. There is no authentication
> on the API.** See [SECURITY.md](SECURITY.md).

## API

Strictly read-only (GET only):

| Endpoint | Returns |
| --- | --- |
| `GET /api/calls` | Captured calls, most recent first |
| `GET /api/calls/:id` | Full raw detail of one call |
| `GET /api/calls/:id/normalized` | Provider-normalized conversation model |
| `GET /api/calls/:id/diagnostics` | Alerts derived for that call |
| `GET /api/sessions` | Sessions assembled from captured calls |
| `GET /api/sessions/:key` | One session with its call timeline |
| `GET /api/sessions/:key/diagnostics` | Session-level alerts |
| `GET /api/signal-policy` | Thresholds the detectors run against |
| `GET /api/ui/dashboard` | Aggregates backing the dashboard |
| `GET /api/ui/alerts` | Alert feed (filterable, paginated) |
| `GET /api/ui/events` | SSE stream of live updates |
| `GET /ui` | The dashboard SPA |
| `GET /healthz` | Health probe |

Every other path and method is transparently relayed upstream.

Alerts are read-only diagnostics: each carries a severity, what was observed,
the likely cause, the impact, a suggested action, and a link back to the source
call or block. Missing data is reported as an explicit neutral state, never as
zero or "healthy".

## Layout

Cargo workspace plus a frontend app:

- `core/` — the library: config, relay, streaming tee, SSE reconstruction,
  SQLite store, provider parsing/normalization, signals and diagnostics,
  read-only API. A future Tauri shell can depend on this directly.
- `cli/` — thin CLI binary. The crates and the shipped binary are still named
  `tracer-core` / `tracer`; only the project is called Orama.
- `apps/web/` — React + TypeScript dashboard (Rspack), embedded into the binary
  at build time.

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
