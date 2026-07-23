//! `portzero agents setup`: register the Port Zero MCP server and refresh
//! user-level instructions for detected AI coding agents, so nobody has to
//! hand copy-paste JSON into `claude_desktop_config.json` or an AGENTS.md
//! again.
//!
//! Supported agents: Claude Code, Codex, pi.dev, opencode, Grok Build. Each
//! agent falls into one of two MCP-registration styles:
//! - **CLI-mediated** (Claude Code, Codex): shell out to the agent's own
//!   `mcp add` subcommand, which does a safe read-modify-write merge itself.
//! - **Hand-edited** (pi.dev, opencode, Grok Build): read-modify-write the
//!   agent's config file directly, preserving any entries already there.
//!
//! Detection is best-effort (an agent's config dir/file existing, or its
//! binary on `PATH`) and setup is best-effort across agents: one agent
//! failing to configure must not skip the rest (mirrors `portzero setup`).
//!
//! Instructions files (a user-level CLAUDE.md/AGENTS.md equivalent) get a
//! marker-delimited block so re-running updates in place instead of
//! duplicating content on every run.

mod cost_hook;
mod repo;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const BEGIN_MARKER: &str = "<!-- portzero:begin -->";
const END_MARKER: &str = "<!-- portzero:end -->";

const INSTRUCTIONS_BODY: &str = "\
## Port Zero MCP

This machine has the Port Zero MCP server registered (`portzero mcp`). It \
exposes the local dev daemon's live runtime truth over stdio — prefer these \
tools over guessing ports or grepping `.env` files:

- `overview` — everything at once (services, tunnels, edges, routes)
- `list_services` — discovered processes/containers, their ports, images
- `list_tunnels` — tunnel domains (Local + Cloud), URLs, health paths
- `observed_edges` — who-talks-to-whom dependency edges observed at runtime
- `exercised_routes` — HTTP routes actually hit per tunnel (smoke-test list)
- `list_feedback` / `propose_fix` — portzero.cloud review threads (requires `portzero login`)
- `submit_bug_report` — report a bug in Port Zero itself to the team (requires `portzero login`)
- `submit_feature_request` — request a Port Zero feature, but only when it would make things \
much easier for the user, and only after asking the user first (requires `portzero login`)

Run `portzero inspect` for the human-readable equivalent, or `portzero skill \
install` to add a PaaS-agnostic production-config extraction skill to a \
project.

_This block is managed by `portzero agents setup` — edits between the \
markers above will be overwritten the next time it runs._";

/// One step's outcome: whether it changed anything, and a short message
/// describing what happened (shown in the report).
enum Step {
    /// The agent was not detected on this machine — nothing attempted.
    NotDetected,
    /// The step ran and changed something.
    Updated(String),
    /// The step ran but the agent was already configured correctly.
    AlreadyConfigured(String),
    /// The agent doesn't support this step (documented gap, not an error).
    Unsupported(String),
    /// The step ran and failed.
    Failed(String),
}

struct AgentReport {
    name: &'static str,
    mcp: Step,
    instructions: Step,
}

/// Run `portzero agents setup`: machine-level config for every detected
/// agent, plus repo-level provisioning when the current directory is inside
/// a git repository (see the `repo` module).
///
/// `dry_run`: compute and print what would change without writing anything or
/// invoking any agent's `mcp add` subcommand.
/// `repo_only` / `machine_only`: restrict to one scope (default: both, with
/// the repo scope auto-skipped when not inside a git repository).
pub fn setup(dry_run: bool, repo_only: bool, machine_only: bool) -> Result<()> {
    let repo_root = if machine_only {
        None
    } else {
        let cwd = std::env::current_dir().context("could not determine current directory")?;
        let root = repo::find_repo_root(&cwd);
        if repo_only && root.is_none() {
            bail!(
                "--repo-only requires running inside a git repository, but {} is not in one.\n\
                 cd into the repository you want to provision (or drop --repo-only to \
                 configure machine-level agent config instead).",
                cwd.display()
            );
        }
        root
    };

    let machine_reports = if repo_only {
        Vec::new()
    } else {
        let home = dirs::home_dir().context("could not determine home directory")?;
        vec![
            claude_code::setup(&home, dry_run),
            codex::setup(&home, dry_run),
            pi::setup(&home, dry_run),
            opencode::setup(&home, dry_run),
            grok::setup(&home, dry_run),
        ]
    };
    let repo_report = repo_root.map(|root| repo::setup(&root, dry_run));

    print_report(&machine_reports, dry_run);
    if let Some(report) = &repo_report {
        if !machine_reports.is_empty() {
            println!();
        }
        repo::print_report(report);
    }
    Ok(())
}

/// Run `portzero agents cost-hook`: the session-end cost hook (see the
/// `cost_hook` module). Invoked by the managed Stop hook, not by hand.
pub fn run_cost_hook() -> Result<()> {
    cost_hook::run()
}

fn print_report(reports: &[AgentReport], dry_run: bool) {
    if dry_run {
        println!("portzero agents setup --dry-run (no files or configs were changed)\n");
    } else {
        println!("portzero agents setup\n");
    }

    for report in reports {
        if matches!(report.mcp, Step::NotDetected) {
            println!("{}: not detected, skipped", report.name);
            continue;
        }
        println!("{}:", report.name);
        print_step("  MCP registration", &report.mcp);
        print_step("  Instructions file", &report.instructions);
    }
}

fn print_step(label: &str, step: &Step) {
    match step {
        Step::NotDetected => {}
        Step::Updated(msg) => println!("{label}: updated — {msg}"),
        Step::AlreadyConfigured(msg) => println!("{label}: already up to date — {msg}"),
        Step::Unsupported(msg) => println!("{label}: not supported — {msg}"),
        Step::Failed(msg) => println!("{label}: FAILED — {msg}"),
    }
}

/// True if `bin` resolves to an executable file on `PATH`.
fn command_exists(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        dir.join(bin).is_file() || (cfg!(windows) && dir.join(format!("{bin}.exe")).is_file())
    })
}

/// Run `mcp add` for a CLI-mediated agent. Treats "already registered"
/// failures (the agent's own idempotency error) as `AlreadyConfigured`
/// rather than `Failed`.
fn run_mcp_add(bin: &str, args: &[&str], dry_run: bool) -> Step {
    if dry_run {
        return Step::Updated(format!("would run `{bin} {}`", args.join(" ")));
    }
    let output = match Command::new(bin).args(args).output() {
        Ok(o) => o,
        Err(e) => return Step::Failed(format!("could not run `{bin}`: {e}")),
    };
    if output.status.success() {
        return Step::Updated(format!("ran `{bin} {}`", args.join(" ")));
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    if stderr.contains("already exist") || stderr.contains("duplicate") {
        Step::AlreadyConfigured("already registered".to_string())
    } else {
        Step::Failed(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Insert or update a marker-delimited block (containing `body`) in a
/// Markdown instructions file. Returns whether the file changed.
fn upsert_markdown_block(path: &Path, body: &str, dry_run: bool) -> Step {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let block = format!("{BEGIN_MARKER}\n{body}\n{END_MARKER}");

    let new_content = match (existing.find(BEGIN_MARKER), existing.find(END_MARKER)) {
        (Some(start), Some(end_marker_start)) if end_marker_start >= start => {
            let end = end_marker_start + END_MARKER.len();
            let mut updated = existing.clone();
            updated.replace_range(start..end, &block);
            updated
        }
        (Some(_), _) => {
            return Step::Failed(format!(
                "{} has a portzero begin marker but no matching end marker; resolve manually",
                path.display()
            ));
        }
        (None, _) => {
            let mut updated = existing.clone();
            if !updated.is_empty() && !updated.ends_with("\n\n") {
                updated.push_str(if updated.ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                });
            }
            updated.push_str(&block);
            updated.push('\n');
            updated
        }
    };

    if new_content == existing {
        return Step::AlreadyConfigured(path.display().to_string());
    }
    if dry_run {
        return Step::Updated(format!("would write {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Step::Failed(format!("creating {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, new_content) {
        Ok(()) => Step::Updated(path.display().to_string()),
        Err(e) => Step::Failed(format!("writing {}: {e}", path.display())),
    }
}

/// Read-modify-write a JSON config file, inserting `mcp_entry` at
/// `pointer_path` (a sequence of object keys ending at the map that holds
/// per-server entries) under key `"portzero"`. Preserves all other content.
fn upsert_json_mcp_entry(path: &Path, keys: &[&str], mcp_entry: Value, dry_run: bool) -> Step {
    let existing_text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
    let mut root: Value = match serde_json::from_str(&existing_text) {
        Ok(v) => v,
        Err(e) => return Step::Failed(format!("parsing {}: {e}", path.display())),
    };

    let mut cursor = &mut root;
    for key in keys {
        if !cursor.is_object() {
            *cursor = json!({});
        }
        cursor = cursor
            .as_object_mut()
            .expect("just ensured object")
            .entry(key.to_string())
            .or_insert_with(|| json!({}));
    }
    if !cursor.is_object() {
        *cursor = json!({});
    }
    let map = cursor.as_object_mut().expect("just ensured object");

    if map.get("portzero") == Some(&mcp_entry) {
        return Step::AlreadyConfigured(path.display().to_string());
    }
    map.insert("portzero".to_string(), mcp_entry);

    if dry_run {
        return Step::Updated(format!("would write {}", path.display()));
    }
    write_json(path, &root)
}

fn write_json(path: &Path, value: &Value) -> Step {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Step::Failed(format!("creating {}: {e}", parent.display()));
        }
    }
    let pretty = match serde_json::to_string_pretty(value) {
        Ok(s) => s,
        Err(e) => return Step::Failed(format!("serializing {}: {e}", path.display())),
    };
    match std::fs::write(path, pretty + "\n") {
        Ok(()) => Step::Updated(path.display().to_string()),
        Err(e) => Step::Failed(format!("writing {}: {e}", path.display())),
    }
}

/// Read-modify-write a TOML config file, ensuring `[mcp_servers.portzero]`
/// exists with the given command/args. Uses `toml_edit` so unrelated content
/// (comments, other tables) survives untouched.
fn upsert_toml_mcp_entry(path: &Path, dry_run: bool) -> Step {
    let existing_text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc = match existing_text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => return Step::Failed(format!("parsing {}: {e}", path.display())),
    };

    if doc.get("mcp_servers").is_none() {
        doc["mcp_servers"] = toml_edit::table();
    }
    let servers = doc["mcp_servers"].as_table_mut().unwrap();
    let already_present = servers.get("portzero").is_some_and(|entry| {
        entry.get("command").and_then(|v| v.as_str()) == Some("portzero")
            && entry
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>())
                == Some(vec!["mcp"])
    });
    if already_present {
        return Step::AlreadyConfigured(path.display().to_string());
    }

    let mut entry = toml_edit::table();
    entry["command"] = toml_edit::value("portzero");
    let mut args = toml_edit::Array::new();
    args.push("mcp");
    entry["args"] = toml_edit::value(args);
    entry["enabled"] = toml_edit::value(true);
    servers["portzero"] = entry;

    if dry_run {
        return Step::Updated(format!("would write {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Step::Failed(format!("creating {}: {e}", parent.display()));
        }
    }
    match std::fs::write(path, doc.to_string()) {
        Ok(()) => Step::Updated(path.display().to_string()),
        Err(e) => Step::Failed(format!("writing {}: {e}", path.display())),
    }
}

mod claude_code {
    use super::*;

    fn config_path(home: &Path) -> PathBuf {
        home.join(".claude.json")
    }

    fn instructions_path(home: &Path) -> PathBuf {
        home.join(".claude").join("CLAUDE.md")
    }

    fn detected(home: &Path) -> bool {
        command_exists("claude") || config_path(home).exists() || home.join(".claude").is_dir()
    }

    pub(super) fn setup(home: &Path, dry_run: bool) -> AgentReport {
        if !detected(home) {
            return AgentReport {
                name: "Claude Code",
                mcp: Step::NotDetected,
                instructions: Step::NotDetected,
            };
        }
        let mcp = if command_exists("claude") {
            run_mcp_add(
                "claude",
                &[
                    "mcp", "add", "portzero", "--scope", "user", "--", "portzero", "mcp",
                ],
                dry_run,
            )
        } else {
            Step::Failed("`claude` CLI not found on PATH".to_string())
        };
        let instructions =
            upsert_markdown_block(&instructions_path(home), INSTRUCTIONS_BODY, dry_run);
        AgentReport {
            name: "Claude Code",
            mcp,
            instructions,
        }
    }
}

mod codex {
    use super::*;

    fn detected(home: &Path) -> bool {
        command_exists("codex") || home.join(".codex").is_dir()
    }

    pub(super) fn setup(home: &Path, dry_run: bool) -> AgentReport {
        if !detected(home) {
            return AgentReport {
                name: "Codex",
                mcp: Step::NotDetected,
                instructions: Step::NotDetected,
            };
        }
        let mcp = if command_exists("codex") {
            run_mcp_add(
                "codex",
                &["mcp", "add", "portzero", "--", "portzero", "mcp"],
                dry_run,
            )
        } else {
            Step::Failed("`codex` CLI not found on PATH".to_string())
        };
        AgentReport {
            name: "Codex",
            mcp,
            // Codex only documents a project-level AGENTS.md; there is no
            // confirmed user-level instructions file to write.
            instructions: Step::Unsupported(
                "Codex has no user-level AGENTS.md; only project-level, which this command \
                 doesn't touch"
                    .to_string(),
            ),
        }
    }
}

mod pi {
    use super::*;

    fn agent_dir(home: &Path) -> PathBuf {
        std::env::var_os("PI_CODING_AGENT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".pi").join("agent"))
    }

    fn config_path(home: &Path) -> PathBuf {
        agent_dir(home).join("mcp.json")
    }

    fn detected(home: &Path) -> bool {
        command_exists("pi") || home.join(".pi").is_dir()
    }

    pub(super) fn setup(home: &Path, dry_run: bool) -> AgentReport {
        if !detected(home) {
            return AgentReport {
                name: "pi.dev",
                mcp: Step::NotDetected,
                instructions: Step::NotDetected,
            };
        }
        let mcp = upsert_json_mcp_entry(
            &config_path(home),
            &["mcpServers"],
            json!({"command": "portzero", "args": ["mcp"]}),
            dry_run,
        );
        AgentReport {
            name: "pi.dev",
            mcp,
            instructions: Step::Unsupported(
                "pi.dev has no documented user-level instructions file".to_string(),
            ),
        }
    }
}

mod opencode {
    use super::*;

    fn config_dir(home: &Path) -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| home.join(".config"))
            .join("opencode")
    }

    fn config_path(home: &Path) -> PathBuf {
        config_dir(home).join("opencode.json")
    }

    fn instructions_path(home: &Path) -> PathBuf {
        config_dir(home).join("AGENTS.md")
    }

    fn detected(home: &Path) -> bool {
        command_exists("opencode") || config_dir(home).is_dir()
    }

    pub(super) fn setup(home: &Path, dry_run: bool) -> AgentReport {
        if !detected(home) {
            return AgentReport {
                name: "opencode",
                mcp: Step::NotDetected,
                instructions: Step::NotDetected,
            };
        }
        let mcp = upsert_json_mcp_entry(
            &config_path(home),
            &["mcp"],
            json!({"type": "local", "command": ["portzero", "mcp"], "enabled": true}),
            dry_run,
        );
        let instructions =
            upsert_markdown_block(&instructions_path(home), INSTRUCTIONS_BODY, dry_run);
        AgentReport {
            name: "opencode",
            mcp,
            instructions,
        }
    }
}

mod grok {
    use super::*;

    fn config_path(home: &Path) -> PathBuf {
        std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".grok"))
            .join("config.toml")
    }

    fn instructions_path(home: &Path) -> PathBuf {
        std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".grok"))
            .join("AGENTS.md")
    }

    fn detected(home: &Path) -> bool {
        command_exists("grok") || home.join(".grok").is_dir()
    }

    pub(super) fn setup(home: &Path, dry_run: bool) -> AgentReport {
        if !detected(home) {
            return AgentReport {
                name: "Grok Build",
                mcp: Step::NotDetected,
                instructions: Step::NotDetected,
            };
        }
        let mcp = upsert_toml_mcp_entry(&config_path(home), dry_run);
        let instructions =
            upsert_markdown_block(&instructions_path(home), INSTRUCTIONS_BODY, dry_run);
        AgentReport {
            name: "Grok Build",
            mcp,
            instructions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pz-agents-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn markdown_block_insert_is_idempotent_and_updatable() {
        let home = tmp_home();
        let path = home.join("CLAUDE.md");
        std::fs::write(&path, "# My notes\n\nSome existing content.\n").unwrap();

        let first = upsert_markdown_block(&path, INSTRUCTIONS_BODY, false);
        assert!(matches!(first, Step::Updated(_)));
        let after_first = std::fs::read_to_string(&path).unwrap();
        assert!(after_first.contains("Some existing content."));
        assert!(after_first.contains(BEGIN_MARKER));
        assert!(after_first.contains("list_services"));

        // Re-running with unchanged content is a no-op.
        let second = upsert_markdown_block(&path, INSTRUCTIONS_BODY, false);
        assert!(matches!(second, Step::AlreadyConfigured(_)));
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after_first, after_second);

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn markdown_block_dry_run_does_not_write() {
        let home = tmp_home();
        let path = home.join("CLAUDE.md");

        let step = upsert_markdown_block(&path, INSTRUCTIONS_BODY, true);
        assert!(matches!(step, Step::Updated(_)));
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn json_mcp_entry_preserves_other_servers() {
        let home = tmp_home();
        let path = home.join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"other": {"command": "other-tool"}}}"#,
        )
        .unwrap();

        let step = upsert_json_mcp_entry(
            &path,
            &["mcpServers"],
            json!({"command": "portzero", "args": ["mcp"]}),
            false,
        );
        assert!(matches!(step, Step::Updated(_)));

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["other"]["command"], "other-tool");
        assert_eq!(written["mcpServers"]["portzero"]["command"], "portzero");

        // Idempotent on re-run.
        let second = upsert_json_mcp_entry(
            &path,
            &["mcpServers"],
            json!({"command": "portzero", "args": ["mcp"]}),
            false,
        );
        assert!(matches!(second, Step::AlreadyConfigured(_)));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn toml_mcp_entry_preserves_unrelated_content() {
        let home = tmp_home();
        let path = home.join("config.toml");
        std::fs::write(&path, "[other]\nkey = \"value\"\n").unwrap();

        let step = upsert_toml_mcp_entry(&path, false);
        assert!(matches!(step, Step::Updated(_)));

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[other]"));
        assert!(written.contains("key = \"value\""));
        assert!(written.contains("[mcp_servers.portzero]"));
        assert!(written.contains("command = \"portzero\""));

        let second = upsert_toml_mcp_entry(&path, false);
        assert!(matches!(second, Step::AlreadyConfigured(_)));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn undetected_agent_skips_both_steps() {
        let home = tmp_home();
        let report = codex::setup(&home, false);
        assert!(matches!(report.mcp, Step::NotDetected));
        assert!(matches!(report.instructions, Step::NotDetected));
        let _ = std::fs::remove_dir_all(&home);
    }
}
