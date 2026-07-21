# Ticketry — ticket management

Tickets are Markdown files with YAML frontmatter, committed to git
(default: `work/`). A SQLite index (`.ticketry/index.db`,
never committed) enables fast queries across branches.

## Commit convention

**Always include the task slug in the first line of commit messages**
(e.g. `task-42`). This enables `ticketry commits` to trace work back
to tickets without noisy annotations:

```
git commit -m "task-42: add user authentication"
git commit -m "feat(auth): implement login (task-42)"
```

Slugs go anywhere in the subject line — prefix, suffix, or inline.
When exactly one slug appears, the commit is displayed cleanly (no
sub-line). When zero or multiple slugs appear, a sub-line shows the
match source.

## Core commands

```
ticketry new "title"                        # create a ticket
ticketry show <slug-or-id>                  # show a ticket (--plain for scripts)
ticketry status <id> done                   # set status
ticketry list                               # all tickets
ticketry list --status todo                 # filter with -s/-a/-m/-l/-f
ticketry list -q "keyword"                  # text search
ticketry list -c slug,title,assignee        # custom columns (reduce context)
ticketry list next                          # unblocked tickets
ticketry list blocked                       # waiting on dependencies
ticketry board                              # kanban view
ticketry commits <slug-or-id> [...]         # find commits mentioning ticket slugs/UUIDs
ticketry commits --plain <slug>             # machine-readable commit list
ticketry new "t" --body-file -              # create WITH body from stdin (one shot)
ticketry edit <id> --body-file -            # replace body from stdin, atomic reindex
ticketry new "t" --type decision|doc        # built-in record types (ADR / doc templates)
ticketry new|list|show --json               # machine-readable JSON (do not scrape prose)
ticketry plan apply <plan.yaml>             # batch: milestone + tickets + deps, atomic
ticketry field <id> <key>                   # get a custom frontmatter field
ticketry field <id> <key> <value>           # set it (e.g. preferred-model opus)
ticketry field <id> <key> --unset           # remove it
```

Append `--help` to any command for full flag reference.

MCP tools mirror CLI structure: `list_tickets` accepts `mode` ("all"|"next"|"blocked")
and `query` for search. `find_ticket_commits` searches git history for ticket
references (case-insensitive, with child-ticket awareness). Use the MCP `describe`
helper to inspect parameters.

## Agent-ergonomic creation, output, and batch planning

Prefer these over the read → edit → reindex loop:

- **Body at creation/update**: `new --body "text"` or `--body-file <path>` (`-` reads
  stdin) writes the body and indexes in one call — no manual `ticketry index`. The same
  flags on `edit <id>` replace an existing body and reindex atomically. An explicit body
  always wins over the type template (`.ticketry/templates/<type>.md`, which otherwise
  fills the body for the implicit `task` type).
- **`--json`**: `new`, `list`, and `show` accept `--json`, emitting pure JSON on stdout
  (prose and health warnings go to stderr). `new --json` → `{id, slug, path, full_path}`.
  Parse this instead of scraping slugs out of human-readable lines.
- **Built-in record types**: `--type decision` (ADR shape: Context / Options / Decision /
  Consequences) and `--type doc` work with no `[[ticket_types]]` config. A
  `.ticketry/templates/<type>.md` overrides the built-in template.
- **Loud filters**: `-f key=value` errors on an unknown or relational key instead of
  silently returning nothing (e.g. filter milestones with `-m`, not `-f milestone=`).
- **Batch planning**: `ticketry plan apply <plan.yaml>` creates a milestone, all its
  tickets, and the dependency edges in one atomic operation. Tickets are referenced by a
  local `alias`; ticketry assigns the real slugs and wires `depends_on` (forward
  references allowed). `--json` returns an `alias → {id, slug, path, full_path}` map.
  Validation (unique aliases, known deps, acyclic) runs before any write, and a failure
  leaves no partial graph on disk.

  ```yaml
  milestone: Search revamp
  tickets:
    - alias: schema
      title: Design the index schema
    - alias: api
      title: Expose the query API
      depends_on: [schema]    # forward references are allowed
  ```

## Key concepts

- **IDs are UUIDs** (canonical), **slugs are labels** (`task-42`). Use either.
- **Status lifecycle**: draft, todo, in-progress, done, blocked, archived.
- **Dependencies**: `ticketry new --depends-on <slug>`. `ticketry list next`
  shows unblocked work, `ticketry list blocked` shows waiting work.
- **Milestones**: `ticketry milestone new "title"`, then
  `ticketry new --milestone <slug>` to add tickets.
- **Custom fields**: any YAML frontmatter fields survive round-trips. Get/set/unset one
  with `ticketry field <id> <key> [value] [--unset] [--json]` — e.g. `ticketry field <id>
  preferred-model opus` to assign a ticket to a specific model. No value prints the
  current one; `--json` emits `{"key","value"}`; `--unset` removes the field.
  Filter with `-f key=value`, sort with `-S key`.
  Limit output columns with `-c slug,title,preferred-model` to reduce context.
- **Path fields**: `path` is repo-relative; `full_path` is absolute (`list -c`, `show --plain`,
  MCP `list_tickets`/`get_ticket`/`thread_show`). CLI `file:` / colored `File:` and MCP
  `file_path` are legacy aliases for the absolute path.
- **Manual edits**: if you edit a ticket file directly, run `ticketry index`.
  Indexing is incremental — branches whose tip hasn't moved are skipped
  (`ticketry index --full` forces a complete rebuild).
- **Auto-indexing**: `ticketry init` installs non-blocking git hooks that
  reindex in the background after commit/merge/checkout/rebase. It is
  idempotent — re-run it in every new clone to set up the hooks. Toggle with
  `ticketry autoindex on|off` (state lives in `.ticketry/`, never committed).

## Thread Mode (agent conversations)

Like old wiki pages, tickets accumulate voices in "Thread Mode" —
a back-and-forth edited directly into the body. Eventually a thread
should be refactored into "Document Mode": a single-voice summary
representing the consensus. The original thread lives in git history
(`ticketry thread history <id>`).

To participate, edit the ticket body and commit with your identity:

```bash
export GIT_AUTHOR_NAME="Opus (PM)"
export GIT_AUTHOR_EMAIL="opus@example.com"
git add work/task-42.task.md
git commit -m "task-42: thread reply"
```

View the conversation with `ticketry thread show <id>` or
`thread_show` (MCP). Git blame provides attribution — no signatures needed.
Refactor threads to Document Mode by replacing the body and committing
with "refactor thread" in the subject. View pre-refactor history with
`ticketry thread history <id>` or `thread_history` (MCP).
