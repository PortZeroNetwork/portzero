//! Diagnostic: is the Port Zero MCP server registered with any detected AI
//! coding agent? Split out from `checks` to stay under the file-size budget;
//! it follows the same `pub(super) fn check_xxx() -> Option<Diagnostic>`
//! pattern as everything in `checks.rs`.

use std::path::{Path, PathBuf};

use super::{Diagnostic, Fix, FixKind, Severity};

/// AI coding agents `portzero agents setup` knows how to configure, and the
/// config file whose presence signals the agent is installed / already has a
/// "portzero" MCP entry. Kept in sync with `cli::agents`.
fn known_agent_config_paths(home: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Claude Code", home.join(".claude.json")),
        ("Codex", home.join(".codex").join("config.toml")),
        ("pi.dev", home.join(".pi").join("agent").join("mcp.json")),
        (
            "opencode",
            dirs::config_dir()
                .unwrap_or_else(|| home.join(".config"))
                .join("opencode")
                .join("opencode.json"),
        ),
        ("Grok Build", home.join(".grok").join("config.toml")),
    ]
}

pub(super) fn check_ai_agent_mcp_registration() -> Option<Diagnostic> {
    let home = dirs::home_dir()?;
    ai_agent_mcp_registration_diagnostic(&home)
}

/// Testable core of [`check_ai_agent_mcp_registration`]: an agent counts as
/// "detected" when its config file exists (best-effort — it's the same file
/// `portzero agents setup` would write to), and as "needs setup" when that
/// file doesn't yet mention "portzero".
fn ai_agent_mcp_registration_diagnostic(home: &Path) -> Option<Diagnostic> {
    let mut needs_setup: Vec<&str> = Vec::new();
    let mut any_detected = false;

    for (name, path) in known_agent_config_paths(home) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        any_detected = true;
        if !content.contains("portzero") {
            needs_setup.push(name);
        }
    }

    if !any_detected || needs_setup.is_empty() {
        return None;
    }

    let list = needs_setup.join(", ");
    Some(Diagnostic {
        id: "ai_agent_mcp_not_registered".into(),
        severity: Severity::Info,
        category: "agent".into(),
        title: format!("Port Zero MCP server not registered with {list}"),
        detail: format!(
            "{list} {} installed but the Port Zero MCP server isn't registered, so it can't see \
             live tunnel/service state.",
            if needs_setup.len() == 1 { "is" } else { "are" }
        ),
        fix: Some(Fix {
            kind: FixKind::Confirm,
            description: "Register the Port Zero MCP server and refresh user-level agent \
                           instructions for every detected AI coding agent."
                .to_string(),
            command: Some("portzero agents setup".to_string()),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pz-diag-agent-test-{}-{}",
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
    fn no_detected_agents_yields_no_diagnostic() {
        let home = tmp_home();
        assert!(ai_agent_mcp_registration_diagnostic(&home).is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn detected_but_unregistered_agent_yields_fix() {
        let home = tmp_home();
        std::fs::write(home.join(".claude.json"), r#"{"mcpServers": {}}"#).unwrap();

        let diag = ai_agent_mcp_registration_diagnostic(&home).expect("expected a diagnostic");
        assert_eq!(diag.id, "ai_agent_mcp_not_registered");
        assert!(diag.title.contains("Claude Code"));
        assert_eq!(
            diag.fix.unwrap().command.as_deref(),
            Some("portzero agents setup")
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn already_registered_agent_yields_no_diagnostic() {
        let home = tmp_home();
        std::fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers": {"portzero": {"command": "portzero", "args": ["mcp"]}}}"#,
        )
        .unwrap();

        assert!(ai_agent_mcp_registration_diagnostic(&home).is_none());
        let _ = std::fs::remove_dir_all(&home);
    }
}
