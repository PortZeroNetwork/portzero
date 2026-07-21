---
id: 1af9361a-ce73-4568-b0df-36453fe7a02e
slug: task-1
status: done
title: Implement full smoltcp user-space TCP stack and proxy loop
relations:
  contains:
  - milestone-1
created_at: 2026-06-21T11:12:11.235958Z
updated_at: 2026-06-21T11:12:11.235958Z
---

## Description
The current `stack.rs` is a stub. Implement the real async packet-routing loop:

- Read raw L3 packets from TUN
- Feed to smoltcp Interface + SocketSet
- Accept TCP on virtual IPs + service ports
- On established connections, proxy payload bidirectionally to `tokio::net::TcpStream` to the real ephemeral backend
- Proper state management, close handling, buffering
- Use smoltcp for all TCP handshake/state, never OS sockets for the client side

Include unit tests with mock devices if possible. Ensure it handles multiple services/connections.

## Acceptance Criteria
- [ ] Connections to VIP:port establish via smoltcp
- [ ] Data flows correctly to real backend and back
- [ ] Graceful shutdown and error handling
- [ ] Works with the existing ServiceTable updates
