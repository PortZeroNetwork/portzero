//! Repo-level provisioning for `portzero agents setup`: prepare the current
//! git repository for cloud AI dev environments (Claude Code on the web and
//! similar headless sandboxes), so an agent session can install portzero
//! fast, knows how to authenticate, and knows how to use tunnels.
//!
//! Three files are written, each idempotently (read-modify-write, unrelated
//! content preserved, second run is a no-op):
//! - `.claude/settings.json` — a SessionStart hook that installs portzero,
//!   guarded so it only runs in a headless cloud sandbox (root, no systemd,
//!   portzero not yet installed), plus a Stop (session-end) cost hook that
//!   invokes `portzero agents cost-hook`.
//! - `AGENTS.md` — a marker-delimited instructions block on tunnels + auth
//!   (mirrored into `CLAUDE.md` only when the repo has one that does not
//!   already @-include AGENTS.md).
//! - `.mcp.json` — a `portzero` stdio MCP server entry (`portzero mcp`).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{print_step, upsert_json_mcp_entry, upsert_markdown_block, write_json, Step};

/// Substring identifying the portzero-managed SessionStart hook, so a
/// re-run can find and replace it in place instead of appending duplicates.
const HOOK_ID: &str = "cloud-agent-env-install.sh";

/// Substring identifying the portzero-managed Stop (session-end) cost hook.
const COST_HOOK_ID: &str = "agents cost-hook";

const INSTALL_URL: &str = "https://raw.githubusercontent.com/PortZeroNetwork/portzero/staging/scripts/cloud-agent-env-install.sh";

/// The full SessionStart hook command: only in a headless cloud sandbox
/// (running as root, PID 1 is not systemd, portzero not already installed)
/// fetch and run the cloud-agent installer. `|| true` keeps the hook silent
/// everywhere else.
fn hook_command() -> String {
    format!(
        "[ \"$(id -u)\" = 0 ] && [ ! -d /run/systemd/system ] && \
         ! command -v portzero >/dev/null 2>&1 && \
         curl -fsSL {INSTALL_URL} | bash || true"
    )
}

/// The Stop (session-end) cost hook command: when portzero is installed, hand
/// the Claude Code hook's stdin JSON to `portzero agents cost-hook`, which
/// computes this session's agent-labor cost from usage metadata only and — if
/// cost tracking is consented for the repo — records it (otherwise it prints a
/// one-line teaser and persists nothing). `|| true` keeps it silent where
/// portzero is absent.
fn cost_hook_command() -> String {
    "command -v portzero >/dev/null 2>&1 && portzero agents cost-hook || true".to_string()
}

const REPO_INSTRUCTIONS_BODY: &str = "\
## Port Zero — tunnels in this repo

This repo is provisioned for Port Zero in cloud AI dev environments (Claude \
Code on the web and similar headless sandboxes). A SessionStart hook in \
`.claude/settings.json` installs the `portzero` daemon automatically in such \
sandboxes; nothing runs on a normal dev machine.

**Expose a service:** set `PZ_TUNNEL=<name>.portzero.local:<port>` in the \
process environment BEFORE launching it (use `<name>.tunnel.portzero.cloud` \
for a public Cloud tunnel). ALWAYS bind port 0 and let the daemon discover \
the real port — never hardcode a port. Set `PZ_HEALTH_PATH` to the service's \
readiness path so the tunnel reports health.

**Wait for / consume a tunnel:**

- `portzero wait <domain> [--healthy]` — block until the tunnel is up (and healthy)
- `portzero url <domain>` — print the tunnel's URL
- `portzero inspect` — human-readable overview of services, tunnels, and routes
- In-sandbox curls must bypass any HTTP proxy: `curl --noproxy '*' …` or set `NO_PROXY`

**Auth for Cloud tunnels:** Local (`*.portzero.local`) tunnels need no \
login. For Cloud tunnels either:

- `portzero login` — prints a device-code URL; relay that URL to the user and \
wait for them to approve it in a browser (works from remote sandboxes), or
- `portzero login --github-repo --team <slug>` — autonomous CI-style auth \
that proves push access to this repository (requires a team trust rule and \
the Port Zero GitHub App on the repo). Add the trust rule and install/link \
the GitHub App under Teams → your team → CI & agent credentials on \
<https://app.portzero.cloud>; see \
<https://portzero.net/docs/ci-agent-credentials> for the full walkthrough.

Never store tokens or credentials in the repo.

_This block is managed by `portzero agents setup` — edits between the \
markers above will be overwritten the next time it runs._";

/// Outcome of provisioning one repository.
pub(super) struct RepoReport {
    root: PathBuf,
    settings_hook: Step,
    cost_hook: Step,
    agents_md: Step,
    claude_md_mirror: Step,
    mcp_json: Step,
}

/// Walk up from `start` to find the enclosing git repository root (the first
/// ancestor containing `.git`, which may be a dir or a worktree gitfile).
pub(super) fn find_repo_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Provision `root` (a git repository root) for cloud AI dev environments.
pub(super) fn setup(root: &Path, dry_run: bool) -> RepoReport {
    let settings_path = root.join(".claude").join("settings.json");
    RepoReport {
        root: root.to_path_buf(),
        settings_hook: upsert_session_start_hook(&settings_path, dry_run),
        cost_hook: upsert_stop_cost_hook(&settings_path, dry_run),
        agents_md: upsert_markdown_block(&root.join("AGENTS.md"), REPO_INSTRUCTIONS_BODY, dry_run),
        claude_md_mirror: mirror_into_claude_md(&root.join("CLAUDE.md"), dry_run),
        mcp_json: upsert_json_mcp_entry(
            &root.join(".mcp.json"),
            &["mcpServers"],
            json!({"type": "stdio", "command": "portzero", "args": ["mcp"]}),
            dry_run,
        ),
    }
}

pub(super) fn print_report(report: &RepoReport) {
    println!("Repository ({}):", report.root.display());
    print_step(
        "  SessionStart install hook (.claude/settings.json)",
        &report.settings_hook,
    );
    print_step(
        "  Stop cost hook (.claude/settings.json)",
        &report.cost_hook,
    );
    print_step("  Instructions block (AGENTS.md)", &report.agents_md);
    print_step(
        "  Instructions mirror (CLAUDE.md)",
        &report.claude_md_mirror,
    );
    print_step("  MCP registration (.mcp.json)", &report.mcp_json);
}

/// Mirror the instructions block into an existing `CLAUDE.md` — but only if
/// the repo has one, and only if it doesn't already @-include AGENTS.md
/// (in which case the content flows through and a mirror would duplicate it).
fn mirror_into_claude_md(path: &Path, dry_run: bool) -> Step {
    let Ok(existing) = std::fs::read_to_string(path) else {
        // No CLAUDE.md: AGENTS.md alone covers the repo; don't create one.
        return Step::NotDetected;
    };
    if existing.contains("@AGENTS.md") {
        return Step::AlreadyConfigured(format!(
            "{} @-includes AGENTS.md; no separate mirror needed",
            path.display()
        ));
    }
    upsert_markdown_block(path, REPO_INSTRUCTIONS_BODY, dry_run)
}

/// Read-modify-write `.claude/settings.json`, inserting (or replacing in
/// place) the portzero-managed SessionStart install hook. All unrelated
/// settings, hook events, and matcher groups are preserved.
fn upsert_session_start_hook(path: &Path, dry_run: bool) -> Step {
    upsert_managed_hook(path, "SessionStart", HOOK_ID, &hook_command(), dry_run)
}

/// Read-modify-write `.claude/settings.json`, inserting (or replacing in
/// place) the portzero-managed Stop (session-end) cost hook. Uses the same
/// marker-substring idempotency and foreign-key preservation as the
/// SessionStart hook, under a distinct event and marker.
fn upsert_stop_cost_hook(path: &Path, dry_run: bool) -> Step {
    upsert_managed_hook(path, "Stop", COST_HOOK_ID, &cost_hook_command(), dry_run)
}

/// Read-modify-write `.claude/settings.json`, inserting (or replacing in
/// place) a portzero-managed hook under `hooks.<event>`, identified by the
/// `marker` substring. All unrelated settings, hook events, and matcher groups
/// are preserved.
fn upsert_managed_hook(
    path: &Path,
    event: &str,
    marker: &str,
    command: &str,
    dry_run: bool,
) -> Step {
    let existing_text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
    let mut root: Value = match serde_json::from_str(&existing_text) {
        Ok(v) => v,
        Err(e) => return Step::Failed(format!("parsing {}: {e}", path.display())),
    };
    if !root.is_object() {
        return Step::Failed(format!(
            "{} is not a JSON object; resolve manually",
            path.display()
        ));
    }

    let hooks = root
        .as_object_mut()
        .expect("checked object above")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Step::Failed(format!(
            "\"hooks\" in {} is not a JSON object; resolve manually",
            path.display()
        ));
    }
    let group = hooks
        .as_object_mut()
        .expect("checked object above")
        .entry(event.to_string())
        .or_insert_with(|| json!([]));
    if !group.is_array() {
        return Step::Failed(format!(
            "\"hooks.{event}\" in {} is not a JSON array; resolve manually",
            path.display()
        ));
    }

    match find_managed_hook(group, marker) {
        Some(slot) if slot.as_str() == Some(command) => {
            return Step::AlreadyConfigured(path.display().to_string());
        }
        Some(slot) => *slot = json!(command),
        None => group
            .as_array_mut()
            .expect("checked array above")
            .push(json!({"hooks": [{"type": "command", "command": command}]})),
    }

    if dry_run {
        return Step::Updated(format!("would write {}", path.display()));
    }
    write_json(path, &root)
}

/// Find the `command` slot of an existing portzero-managed hook (identified
/// by the `marker` substring) anywhere in an event's matcher groups.
fn find_managed_hook<'a>(group: &'a mut Value, marker: &str) -> Option<&'a mut Value> {
    group
        .as_array_mut()?
        .iter_mut()
        .filter_map(|g| g.get_mut("hooks")?.as_array_mut())
        .flatten()
        .filter_map(|hook| hook.as_object_mut()?.get_mut("command"))
        .find(|command| command.as_str().is_some_and(|c| c.contains(marker)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pz-agents-repo-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn find_repo_root_walks_up_and_misses_cleanly() {
        let repo = tmp_repo();
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_repo_root(&nested), Some(repo.clone()));
        assert_eq!(
            find_repo_root(&std::env::temp_dir().join("pz-no-such-repo")),
            None
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn session_start_hook_preserves_foreign_keys_and_is_idempotent() {
        let repo = tmp_repo();
        let path = repo.join(".claude").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "permissions": {"allow": ["Bash(ls:*)"]},
              "hooks": {
                "SessionStart": [
                  {"hooks": [{"type": "command", "command": "echo unrelated"}]}
                ],
                "Stop": [{"hooks": [{"type": "command", "command": "echo done"}]}]
              }
            }"#,
        )
        .unwrap();

        let first = upsert_session_start_hook(&path, false);
        assert!(matches!(first, Step::Updated(_)));
        let after_first = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&after_first).unwrap();
        // Foreign keys survive.
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "echo done");
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "echo unrelated"
        );
        // Managed hook appended, guarded and pointing at the installer.
        let managed = v["hooks"]["SessionStart"][1]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(managed.contains(HOOK_ID));
        assert!(managed.contains("id -u"));
        assert!(managed.contains("/run/systemd/system"));
        assert!(managed.contains("command -v portzero"));

        // Second run: no diff.
        let second = upsert_session_start_hook(&path, false);
        assert!(matches!(second, Step::AlreadyConfigured(_)));
        assert_eq!(after_first, std::fs::read_to_string(&path).unwrap());

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn stop_cost_hook_preserves_foreign_keys_and_coexists_with_install_hook() {
        let repo = tmp_repo();
        let path = repo.join(".claude").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "permissions": {"allow": ["Bash(ls:*)"]},
              "hooks": {
                "Stop": [
                  {"hooks": [{"type": "command", "command": "echo foreign-stop"}]}
                ]
              }
            }"#,
        )
        .unwrap();

        // Both managed hooks land without disturbing each other or the foreign one.
        assert!(matches!(
            upsert_session_start_hook(&path, false),
            Step::Updated(_)
        ));
        let cost = upsert_stop_cost_hook(&path, false);
        assert!(matches!(cost, Step::Updated(_)));

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // Foreign key + foreign Stop hook survive.
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo foreign-stop"
        );
        // The SessionStart install hook is present.
        assert!(v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(HOOK_ID));
        // The managed cost hook is appended to Stop, invoking `agents cost-hook`.
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2, "foreign Stop hook + managed cost hook");
        let managed = stop[1]["hooks"][0]["command"].as_str().unwrap();
        assert!(managed.contains(COST_HOOK_ID), "got: {managed}");
        assert!(managed.contains("command -v portzero"));

        // Idempotent: re-running the cost hook is a no-op.
        let before = std::fs::read_to_string(&path).unwrap();
        let second = upsert_stop_cost_hook(&path, false);
        assert!(matches!(second, Step::AlreadyConfigured(_)));
        assert_eq!(before, std::fs::read_to_string(&path).unwrap());

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn session_start_hook_replaces_stale_managed_hook_in_place() {
        let repo = tmp_repo();
        let path = repo.join(".claude").join("settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                r#"{{"hooks": {{"SessionStart": [{{"hooks": [{{"type": "command", "command": "old-guard && curl old-url/{HOOK_ID} | bash"}}]}}]}}}}"#
            ),
        )
        .unwrap();

        let step = upsert_session_start_hook(&path, false);
        assert!(matches!(step, Step::Updated(_)));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = v["hooks"]["SessionStart"].as_array().unwrap();
        // Replaced in place — still exactly one matcher group, one hook.
        assert_eq!(session_start.len(), 1);
        let command = session_start[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.contains(INSTALL_URL));
        assert!(!command.contains("old-guard"));

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn session_start_hook_creates_settings_when_absent() {
        let repo = tmp_repo();
        let path = repo.join(".claude").join("settings.json");

        let step = upsert_session_start_hook(&path, false);
        assert!(matches!(step, Step::Updated(_)));
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(INSTALL_URL));

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn session_start_hook_dry_run_does_not_write() {
        let repo = tmp_repo();
        let path = repo.join(".claude").join("settings.json");
        let step = upsert_session_start_hook(&path, true);
        assert!(matches!(step, Step::Updated(_)));
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn claude_md_mirror_skips_missing_and_at_include() {
        let repo = tmp_repo();
        let path = repo.join("CLAUDE.md");

        // Absent: nothing created.
        assert!(matches!(
            mirror_into_claude_md(&path, false),
            Step::NotDetected
        ));
        assert!(!path.exists());

        // @-include: content flows through, no mirror written.
        std::fs::write(&path, "@AGENTS.md\n").unwrap();
        assert!(matches!(
            mirror_into_claude_md(&path, false),
            Step::AlreadyConfigured(_)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "@AGENTS.md\n");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn claude_md_mirror_writes_block_into_standalone_file() {
        let repo = tmp_repo();
        let path = repo.join("CLAUDE.md");
        std::fs::write(&path, "# Project notes\n").unwrap();

        let step = mirror_into_claude_md(&path, false);
        assert!(matches!(step, Step::Updated(_)));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Project notes"));
        assert!(content.contains("PZ_TUNNEL"));

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn full_repo_setup_is_idempotent_end_to_end() {
        let repo = tmp_repo();
        std::fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers": {"other": {"command": "other-tool"}}}"#,
        )
        .unwrap();

        let first = setup(&repo, false);
        assert!(matches!(first.settings_hook, Step::Updated(_)));
        assert!(matches!(first.cost_hook, Step::Updated(_)));
        assert!(matches!(first.agents_md, Step::Updated(_)));
        assert!(matches!(first.claude_md_mirror, Step::NotDetected));
        assert!(matches!(first.mcp_json, Step::Updated(_)));

        let agents = std::fs::read_to_string(repo.join("AGENTS.md")).unwrap();
        assert!(agents.contains("PZ_TUNNEL"));
        assert!(agents.contains("bind port 0"));
        assert!(agents.contains("portzero wait"));
        assert!(agents.contains("--github-repo"));
        assert!(agents.contains("NO_PROXY"));

        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other-tool");
        assert_eq!(mcp["mcpServers"]["portzero"]["command"], "portzero");

        let snapshot = |name: &str| std::fs::read_to_string(repo.join(name)).unwrap();
        let before = (
            snapshot(".claude/settings.json"),
            snapshot("AGENTS.md"),
            snapshot(".mcp.json"),
        );
        let second = setup(&repo, false);
        assert!(matches!(second.settings_hook, Step::AlreadyConfigured(_)));
        assert!(matches!(second.cost_hook, Step::AlreadyConfigured(_)));
        assert!(matches!(second.agents_md, Step::AlreadyConfigured(_)));
        assert!(matches!(second.mcp_json, Step::AlreadyConfigured(_)));
        let after = (
            snapshot(".claude/settings.json"),
            snapshot("AGENTS.md"),
            snapshot(".mcp.json"),
        );
        assert_eq!(before, after);

        let _ = std::fs::remove_dir_all(&repo);
    }
}
