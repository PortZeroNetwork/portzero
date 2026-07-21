---
id: e1b214ee-2cac-469c-b7bf-db70a406b34a
slug: task-46
status: done
title: Auto-generate OpenAPI spec from axum handlers
relations:
  contains:
  - e0776fa1-3fc9-4807-bcd7-12e412360163
created_at: 2026-07-01T23:40:04.787761763Z
updated_at: 2026-07-01T23:40:04.787761763Z
---

Replace the hand-maintained `api/management-v1.yaml` with a spec generated
from the daemon's axum handlers (e.g. via `utoipa` annotations on the
handlers/types in `client/crates/daemon/src/management/handlers.rs`).

- Add utoipa (or equivalent) derives/annotations to request/response types
  and route handlers in `management/handlers.rs` and `management/server.rs`.
- Add a `just openapi` recipe that regenerates `api/management-v1.yaml`
  from code.
- Add a CI check (in `ci.yml`) that regenerates the spec and fails the
  build if it differs from the committed file, so the spec can't drift
  from the handlers.
- Delete the hand-maintained parts of the old yaml once generation
  reproduces equivalent coverage.

This is the prerequisite for sdk-generate and the release-publish ticket.
