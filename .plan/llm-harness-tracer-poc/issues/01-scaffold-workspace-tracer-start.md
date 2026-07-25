# 01 — Scaffold workspace + bootable `tracer start`

**What to build:** Running the CLI's `start` command boots an axum server on a port and prints the two `export` lines the user pastes into their shell (`ANTHROPIC_BASE_URL` pointing at the proxy, `ANTHROPIC_AUTH_TOKEN` placeholder for the real token). A health probe against the server answers OK. The upstream target defaults to `https://api.anthropic.com` and is overridable with a flag. No traffic capture yet — this is the skeleton everything else hangs off.

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

Note: this repo's Rust toolchain runs through mise — build/run/test with `mise exec rust -- cargo …` (bare `cargo` will not resolve).

- [ ] Cargo workspace exists with a `core` library crate and a thin `cli` binary crate.
- [ ] `mise exec rust -- cargo run -- start` boots an axum server on a port and stays up.
- [ ] Startup prints the `ANTHROPIC_BASE_URL` and `ANTHROPIC_AUTH_TOKEN` export snippet, touching no files.
- [ ] Upstream target defaults to `https://api.anthropic.com` and is overridable via a CLI flag.
- [ ] A health probe endpoint returns a success response.
- [ ] `mise exec rust -- cargo build` and `mise exec rust -- cargo test` succeed.
