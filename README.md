# tracer — LLM Harness Tracer (POC)

A local, open-source tool that transparently intercepts Claude Code's API
traffic and lets you see exactly what the harness sends on every call — the full
system prompt, message history, declared tools, and the reconstructed response.

Unlike SDK-based tools, tracer requires **no code changes and no SDK**: it works
with an unmodified Claude Code binary by pointing `ANTHROPIC_BASE_URL` at a local
proxy. No MITM, no certificates.

## How it works

```text
Claude Code ──HTTP──▶ tracer proxy ──HTTPS──▶ api.anthropic.com
                          │
                          ├─ tees every request/response (streaming included)
                          ├─ redacts auth headers, stores the rest verbatim
                          └─ serves a read-only UI over the capture DB
```

Each request→response exchange is stored as one SQLite row (raw SSE **and**
reconstructed JSON). A background writer task owns the connection so persistence
never sits on the request path. Tracing is best-effort: it never blocks or
alters the client's request.

## Usage

The Rust toolchain runs through [mise](https://mise.jdx.dev/):

```bash
mise exec rust -- cargo run -- start
```

This boots the proxy and prints an export snippet. Paste it into the shell that
runs Claude Code:

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:8787
export ANTHROPIC_AUTH_TOKEN=<your-anthropic-token>
claude            # runs normally; every call is captured
```

Then open the UI:

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

## API

Strictly read-only (GET only):

- `GET /api/calls` — list of captured calls (most recent first)
- `GET /api/calls/:id` — full detail of one call
- `GET /ui` — the HTML inspector
- `GET /healthz` — health probe

Every other path/method is transparently relayed to upstream.

## Layout

Cargo workspace:

- `core/` — reusable library (config, relay, streaming tee, SSE reconstruction,
  SQLite store, read-only API + UI). A later Tauri app can depend on this.
- `cli/` — thin `tracer` binary.

## Development

```bash
mise exec rust -- cargo build
mise exec rust -- cargo test
```

## Security note

The UI renders captured, potentially untrusted request/response bodies. All DOM
is built with `createElement` + `textContent` (never `innerHTML`) so no captured
string can inject markup. Auth headers (`authorization`, `x-api-key`,
`proxy-authorization`) are redacted before write and are never exposed by the
API.

## License

Apache-2.0.
