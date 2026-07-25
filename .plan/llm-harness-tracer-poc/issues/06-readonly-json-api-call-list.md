# 06 — Read-only JSON API + HTML call list

**What to build:** The same Rust process serves a `/ui` page backed by a read-only JSON API over SQLite. Opening the UI shows a list of every captured call with at-a-glance basics — timestamp, model, response status, and stream/error hints — most recent first. Read-only: the UI and API never mutate stored data.

**Blocked by:** 03 — Persist round-trips to SQLite via async writer.

**Status:** ready-for-agent

- [ ] A read-only JSON API endpoint returns the list of captured calls from SQLite.
- [ ] The same process serves a static HTML UI at `/ui` that renders the call list.
- [ ] Each list entry shows timestamp, model, response status, and stream/error hints.
- [ ] Calls are ordered most-recent-first and the list reflects newly captured calls on refresh.
- [ ] The API and UI are strictly read-only.
