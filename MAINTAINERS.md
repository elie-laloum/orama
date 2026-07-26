# Maintainers

This file lists who maintains Orama, what they are responsible for, and how
decisions get made. It is the authority on review ownership — if you need a
decision, ask someone here.

## Current maintainers

| Name | GitHub | Areas |
| --- | --- | --- |
| Elie Laloum | [@elie-laloum](https://github.com/elie-laloum) | Everything (lead) |

Reach maintainers through GitHub issues, pull request review threads, or — for
anything security-sensitive — a private advisory. No email addresses are
published here on purpose.

Emeritus maintainers: _none yet._

## Areas of ownership

As the project grows, changes to these areas should pull in the listed owner.
Until a second maintainer joins, the lead reviews all of it.

| Area | Paths | Owner |
| --- | --- | --- |
| Relay & capture path | `core/src/relay.rs`, `core/src/server.rs` | Lead |
| Persistence | `core/src/store.rs` | Lead |
| Reconstruction | `core/src/reconstruct.rs` | Lead |
| Parsing, signals, diagnostics | `core/src/parse/` | Lead |
| Read-only API | `core/src/api.rs` | Lead |
| Dashboard | `apps/web/` | Lead |
| CLI | `cli/` | Lead |
| Release & packaging | `Cargo.toml`, `core/build.rs`, CI | Lead |

## Responsibilities

Maintainers are expected to:

- Triage new issues within a week, and label them.
- Review pull requests, or say plainly when they cannot get to one.
- Handle security reports under the process in [SECURITY.md](SECURITY.md),
  including private coordination and advisory publication.
- Keep the capture path's invariants intact (see below).
- Cut releases and keep the changelog honest about breaking changes.
- Uphold a respectful, harassment-free environment in issues and reviews.

## Invariants a maintainer must not merge away

These are the properties the project exists to guarantee. A change that breaks
one needs an explicit, documented decision — not a silent merge.

1. **Tracing is best-effort.** Capture must never block, delay, or alter the
   client's request. A failure to persist degrades observability, never the
   proxied call.
2. **The raw capture is the source of truth and is immutable.** Analysis is
   derived at read time. No rewriting stored bodies.
3. **Auth headers never hit disk, the API, the UI, or the logs.** Redaction is
   enforced at the call site *and* again in the store. Both gates stay.
4. **The inspection API is read-only.** GET only; no route mutates captured
   data.
5. **Captured content is untrusted input.** It is rendered through escaping
   paths only — never `innerHTML` / `dangerouslySetInnerHTML`, never `eval`.
6. **Default bind is loopback.** No change may widen the default exposure.
7. **A clean clone compiles** without having built the frontend first.

## Review and merge policy

- Every change lands through a pull request; no direct pushes to the default
  branch.
- One maintainer approval is required to merge. Changes touching an invariant
  above, or the security posture, need the lead's explicit sign-off.
- Maintainers do not merge their own non-trivial changes without a second pair
  of eyes, once there is a second maintainer to ask.
- CI must be green: `cargo test`, `cargo clippy --all-targets`, `cargo fmt
  --check`, and `npm run typecheck` in `apps/web/`.
- New behaviour needs a test. Bug fixes need a regression test that fails
  before the fix.
- Prefer small, reviewable PRs with a clear description of what changed and
  why.

## Decision making

Day-to-day decisions are made by whoever reviews the change. Disagreements are
resolved by discussion in the issue or PR; if that stalls, the lead decides and
records the reasoning in the thread. Anything that changes the project's scope,
its licensing, or one of the invariants above is announced in an issue before it
is implemented.

## Becoming a maintainer

There is no fixed quota. A contributor is invited to become a maintainer after a
sustained record of good judgement: several merged non-trivial PRs, helpful
reviews of other people's work, and a demonstrated grasp of the invariants
above. Existing maintainers extend the invitation by consensus; the new
maintainer is added to this file in the same PR that grants access.

Stepping down is normal and carries no stigma — open a PR moving yourself to
emeritus.

## Contact

- General questions and bug reports: GitHub issues.
- Security reports: **do not** use issues — follow [SECURITY.md](SECURITY.md).
