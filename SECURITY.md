# Security Policy

Orama sits on the path between an agent harness and an LLM API and writes what
it sees to disk. Security issues here are about **data exposure** first and
foremost. Please read the threat model below before reporting — some behaviours
that look alarming are documented and intentional.

## Supported versions

The project is pre-1.0. Only the latest commit on the default branch receives
security fixes. There are no backports to earlier tags.

| Version | Supported |
| --- | --- |
| `main` (latest) | ✅ |
| Older tags / releases | ❌ |

## Reporting a vulnerability

**Do not open a public issue for a vulnerability.**

Report it privately through GitHub **Security Advisories**: go to the
[Security tab](https://github.com/elie-laloum/orama/security/advisories/new) of
the repository and choose *Report a vulnerability*. The report stays private
until an advisory is published, and the discussion stays attached to the code.

This is the only accepted channel — there is no security mailing address.

Please include:

- What the issue is and the impact you believe it has.
- Version or commit SHA, OS, and how Orama was launched (flags, `--host`).
- Steps to reproduce, or a minimal proof of concept.
- Any suggested fix, if you have one.

**Never include real API keys, real capture databases, or unredacted prompt
transcripts** in a report. Redact them or describe the shape of the data
instead.

### What to expect

| Stage | Target |
| --- | --- |
| Acknowledgement of your report | within 5 business days |
| Initial assessment and severity | within 10 business days |
| Fix or documented mitigation | depends on severity; you will get status updates |

This is a volunteer-maintained project with no paid bug-bounty programme. We
will credit you in the advisory and release notes unless you ask us not to.
Please give us a reasonable window to ship a fix before disclosing publicly;
coordinated disclosure is appreciated.

## Threat model

Understanding what Orama is helps separate bugs from designed behaviour.

**Assumptions.** Orama is a local developer tool run by a single trusted user
on their own machine. It listens on loopback by default. The captured data
belongs to the user running it.

**In scope** — report these:

- An auth header (`authorization`, `x-api-key`, `proxy-authorization`) or any
  other credential reaching the database, the JSON API, the UI, or the logs.
- A path that lets a captured (attacker-influenced) string execute as code or
  markup in the dashboard — XSS, prototype pollution, template injection.
- A read-only API surface that turns out to be writable, or a route that lets a
  caller read files outside the capture database.
- The proxy relaying to an unintended upstream, downgrading TLS, or failing to
  verify certificates on the upstream leg.
- Anything that lets a remote party who cannot already reach the bound port read
  captured traffic.
- Denial of service reachable by an ordinary upstream response — a crash, an
  unbounded allocation, or a panic that takes down the relay.

**Out of scope** — documented behaviour, not vulnerabilities:

- **The capture database contains sensitive data by design.** Full prompts,
  file contents the agent read, and tool output are stored verbatim. Only auth
  headers are redacted. Protect the file with filesystem permissions and never
  commit it; `*.sqlite` is gitignored.
- **The local API and UI have no authentication.** Anyone who can reach the
  bound port can read every capture. This is why the default bind is
  `127.0.0.1`. Running with `--host 0.0.0.0` on an untrusted network is a
  configuration mistake, not a product bug.
- **The client → proxy leg is plain HTTP.** It is loopback traffic by design.
  The proxy → upstream leg uses HTTPS with rustls and normal certificate
  verification.
- **A malicious upstream URL passed via `--upstream`** — you chose where to send
  your own traffic.
- Anything requiring an attacker who already has local code execution or read
  access to your home directory.

## Operational guidance for users

- Keep the default `--host 127.0.0.1` unless you fully control the network.
- Store the capture database outside any directory you sync, back up, or commit
  (`--db /path/outside/the/repo/tracer.sqlite`).
- Restrict the file yourself if others share the machine: `chmod 600
  tracer.sqlite`.
- Delete captures you no longer need. There is no retention policy — the
  database grows until you remove it.
- Redact before sharing. A capture pasted into an issue may contain your source
  code, your prompts, and your customers' data.
