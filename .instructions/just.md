# Just as the task runner

- Always use [`just`](https://github.com/casey/just) as the task runner for standalone scripts and recurring repo operations (dev, test, build, deploy helpers, seed, launch-state, etc.).
- Always place the `justfile` at the **repo root** so recipes can be run from any directory in the repo (`just` walks parents for a justfile).
- Always write standalone scripts invoked by recipes in the **primary programming language** of the repository. If multiple languages are in use, use the primary **backend** language.
- If it is not clear which language is primary, ask the user and record the choice in the project-level agent instructions (`AGENTS.md` / `CLAUDE.md`) so later sessions do not re-ask.
- Prefer adding a `just` recipe over documenting a multi-step shell one-liner. New recurring operations become recipes, not tribal knowledge.
