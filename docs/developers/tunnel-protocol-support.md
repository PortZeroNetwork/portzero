# What each tunnel kind can carry

Local and Cloud tunnels are built on different data paths, so they do not
support the same traffic. This page records which, and why — the difference is
not obvious from the outside, and getting it wrong produces a hang rather than
an error.

The user-facing version of this belongs at
[portzero.net/docs](https://portzero.net/docs) (sourced from portzero-cloud's
`cloud/landing/blog/src/data/docs/`); update it there when this changes.

## Local tunnels (`*.portzero.local`)

Byte-level TCP proxy. The daemon terminates the connection in a user-space
smoltcp stack (`net/stack/engine.rs`), opens a `TcpStream` to the real backend,
and copies bytes both ways. Nothing parses the payload as HTTP.

Consequences:

- **WebSockets work.** Vite HMR, live reload, socket.io, and anything else that
  upgrades a connection is carried unchanged. This is the tunnel to point HMR
  at.
- Non-HTTP protocols work too (Postgres, Redis, gRPC) — the proxy is protocol
  agnostic.
- On virtual port 443 the daemon may terminate TLS with the local CA
  (`tls/stack.rs`) and forward plaintext, or pass TLS straight through to a
  backend that speaks it. Either way the post-TLS stream is copied verbatim, so
  upgrades survive.
- The HTTP tap that feeds `observed_edges` / `exercised_routes` only *observes*
  the stream; it never rewrites or blocks it.

## Cloud tunnels (`*.tunnel.portzero.cloud`)

Request/response relay. The edge server sends a whole
`ServerMessage::HttpRequest` over the control connection and expects a whole
`ClientMessage::HttpResponse` back (`proto/src/lib.rs`); `forwarder.rs` replays
it against the local service with reqwest.

Consequences:

- **WebSockets do not work.** The protocol has no frame for handing a
  connection over, and reqwest owns the framing, so there is nothing to upgrade.
  `forward_request` detects a handshake (`Connection: Upgrade` plus
  `Upgrade: websocket`) and answers **501** naming Local tunnels as the place
  WebSockets do work.

  Rejecting explicitly matters: the hop-by-hop filter strips `Upgrade` and
  `Connection` before forwarding, so without the check the handshake reaches the
  local service as an ordinary GET, gets a 200, and the client waits forever on
  a socket that will never open.
- Response bodies are capped at `MAX_RESPONSE_BODY_BYTES` (16 MiB) and requests
  time out at `REQUEST_TIMEOUT` (30s), because both are buffered whole.
- Server-sent events and long-polling are subject to that same timeout.

## If Cloud WebSocket support is ever needed

It is a protocol change, not a forwarder change: `portzero_proto` would need
frames for opening, streaming, and closing an upgraded connection, plus edge and
daemon halves that keep per-connection state. Until then, the 501 is the honest
answer.
