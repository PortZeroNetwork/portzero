---
id: 2058fd5c-a41b-43c0-a35e-13737f8c30f2
slug: task-96
status: todo
title: Forward to IPv6 local servers (dial the family the process is bound to)
created_at: 2026-08-08T18:30:54.952634889Z
updated_at: 2026-08-08T18:30:54.952634889Z
---

Discovery records only a port and a 2-variant BindAddr, then synthesizes 127.0.0.1 as the backend address. A process listening on [::1] only (Vite/Node >=17 binding "localhost" on dual-stack Linux) is discovered fine but never reachable — TCP connects, zero bytes back.

Fix: carry the bound IpAddr on ListeningPort, map wildcard binds to loopback of the same family, and dial that address at every site that currently hardcodes Ipv4Addr::LOCALHOST (discovery/process.rs, discovery/docker.rs). Thread the address (not just a u16 port) through the Cloud tunnel forwarder.

Closes GitHub #20, obsoletes #17.