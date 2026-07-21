# Semble Code Search

A `semble` MCP server may be available with two tools:
- `mcp__semble__search` — search the codebase with a natural-language or code query.
- `mcp__semble__find_related` — find code similar to a specific file and line.

Always call `mcp__semble__search` before using Grep, Glob, or Read to explore an
unfamiliar codebase. Use Grep/Glob/Read only for exact path lookup, exhaustive
literal matches, or when the returned chunk lacks enough context.

Pass `--content docs` to search documentation and prose, `--content config` for
config files, or `--content all` to search code, docs, and config together.

For CLI fallback, or for sub-agents without MCP access, use:

```bash
semble search "authentication flow" ./my-project
semble search "deployment guide" ./my-project --content docs
semble search "database host port" ./my-project --content config
semble find-related src/auth.py 42 ./my-project
semble search "save model to disk" ./my-project --top-k 10
```

The index is built on first run and cached automatically. If `semble` is not
on `$PATH`, use `uvx --from "semble[mcp]" semble`.

## Workflow

1. Start with `mcp__semble__search` (or `semble search` on the CLI) to find
   relevant chunks.
2. Use `--content docs` for documentation, `--content config` for config
   files, or `--content all` for everything.
3. Inspect full files only when the returned chunk does not give enough
   context.
4. Optionally use `mcp__semble__find_related` (or `semble find-related`) with
   a promising result's file path and line to discover related
   implementations.
5. Use Grep/Glob/Read only when exhaustive literal matches or quick
   confirmation of an exact string is needed.
