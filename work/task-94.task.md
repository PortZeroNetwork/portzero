---
id: 7dd050c9-c242-4371-90a1-a6ce3ab529aa
slug: task-94
title: "Act on agent feedback: PZ_TUNNEL discoverability, ambiguous domain claims, url exit code, WebSocket support"
status: in-progress
type: task
---

Feedback from a Claude Code session that used PortZero to develop a web app.

## 1. The PZ_TUNNEL contract is undiscoverable

The reporter had the MCP server, `--help` on every subcommand, and
`portzero inspect`, and none of them stated the core contract: *set
`PZ_TUNNEL` on a process that listens and the daemon does the rest*.
They found it only by reading `~/.portzero/examples`.

Fix: state the contract in `portzero --help` (top-level long help), in
`portzero status` when nothing is registered, in the bare-`portzero`
front door, and in the MCP server instructions (that is the surface an
agent actually reads).

## 2. Ambiguous domain claims are silent

Several leaked backends from repeated test runs all claimed
`api.jc-58fd492c.portzero.local`. Requests were served by an arbitrary
one — including one pointing at a deleted database volume, producing a
500 that looked like an application bug. `portzero status` reported the
domain as healthy throughout.

Root causes, all verified in code:

- `notify::detect_duplicate_names` groups claimants by
  `context_key` = the process cwd. Repeated runs from *one* directory
  collapse to a single context, so no `Issue::DuplicateName` is ever
  raised — no status `Issues:` line, no notification.
- `cli::inspect::render`, `cli::export::discovered_tunnels`, and
  `cli::mcp::tunnels` all `dedup_by` domain, dropping every claimant but
  one with no indication that anything was dropped. The MCP view is what
  the reporting agent was reading.
- `net::service_table::register` is last-write-wins over a `BTreeMap`
  iteration order, so *which* claimant serves traffic is arbitrary and
  is never reported.

Fix: treat distinct live backend addresses as distinct claimants, make
the winner deterministic (lowest pid, matching the cloud route table),
and name the winner everywhere the conflict is shown.

## 3. `portzero url` cannot be distinguished from other failures

Asked for a distinct non-zero exit for "no such tunnel" so `set -e`
scripts need not test for an empty string. `url` already exits 1, but 1
is also every other failure. Give "no such tunnel" its own code.

## 4. Question: does PortZero proxy WebSockets?

Answer, from the code: Local (`*.portzero.local`) tunnels do — the
overlay is a byte-level TCP proxy, so Vite HMR works. Cloud tunnels do
not — the edge protocol carries only `HttpRequest`/`HttpResponse`, and
`forwarder.rs` strips `Upgrade`/`Connection`, so a handshake silently
degrades instead of failing loudly. Document both, and make the cloud
path return an explanatory 501.

## What worked (do not regress)

- Deriving the hostname *before* the service starts: no discovery step,
  no startup ordering dependency.
- Graceful absence: `portzero status` as a one-line probe made the
  daemon optional in CI for about four lines of script.
