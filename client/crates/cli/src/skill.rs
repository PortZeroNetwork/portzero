//! `portzero skill install`: drop the PaaS-agnostic extraction skill into the
//! user's project so an AI coding agent can graduate a Port Zero dev setup to
//! production hosting on any platform.
//!
//! The skill is embedded in the binary, so installation is offline and needs no
//! network. It contains extraction + interpretation guidance only — zero
//! platform-specific emitters (see the skill body).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The embedded skill body. Agent-tool-agnostic Markdown with YAML frontmatter:
/// Claude Code reads the frontmatter as a skill; the prose body is reusable by
/// any agent tool that can be pointed at a Markdown instruction file.
const SKILL_BODY: &str = include_str!("../skills/extract-production-config/SKILL.md");

/// The skill's directory name (used under the install root).
const SKILL_NAME: &str = "portzero-extract-production-config";

/// Install the extraction skill into `dir` (default: `.claude/skills/<name>/`).
///
/// - `print`: write the skill to stdout instead of a file (tool-agnostic escape
///   hatch — pipe it wherever your agent tool expects instructions).
/// - `force`: overwrite an existing `SKILL.md`.
pub fn install(dir: Option<PathBuf>, force: bool, print: bool) -> Result<()> {
    if print {
        print!("{SKILL_BODY}");
        return Ok(());
    }

    // Default to the Claude Code skill convention, but honor an explicit --dir so
    // the same content can live wherever another agent tool expects it.
    let base = dir.unwrap_or_else(|| Path::new(".claude").join("skills"));
    let skill_dir = base.join(SKILL_NAME);
    let skill_file = skill_dir.join("SKILL.md");

    if skill_file.exists() && !force {
        bail!(
            "{} already exists. Re-run with --force to overwrite, or --print to \
             emit the skill to stdout for another agent tool.",
            skill_file.display()
        );
    }

    std::fs::create_dir_all(&skill_dir)
        .with_context(|| format!("creating skill directory {}", skill_dir.display()))?;
    std::fs::write(&skill_file, SKILL_BODY)
        .with_context(|| format!("writing skill to {}", skill_file.display()))?;

    println!("Installed the Port Zero extraction skill:");
    println!("  {}", skill_file.display());
    println!();
    println!(
        "Your AI coding agent can now use it to extract a production config from \
         this project's Port Zero runtime truth (try `portzero inspect` or the \
         `portzero mcp` server). The skill is PaaS-agnostic: it extracts and \
         interprets only — point your agent at the target platform's own docs for \
         the config format."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_skill_is_agnostic_and_well_formed() {
        // Has frontmatter and the expected name.
        assert!(SKILL_BODY.starts_with("---"));
        assert!(SKILL_BODY.contains("name: portzero-extract-production-config"));
        // Contains extraction/interpretation guidance and the observability caveat.
        assert!(SKILL_BODY.contains("runtime truth"));
        assert!(SKILL_BODY.contains("Observability caveat"));
        assert!(SKILL_BODY.contains("PZ_HEALTH_PATH"));
        // Guards against platform-specific emitters sneaking in.
        for banned in [
            "render.yaml",
            "fly.toml",
            "Procfile",
            "app.yaml",
            "vercel.json",
        ] {
            assert!(
                !SKILL_BODY.contains(banned),
                "skill must contain no platform-specific emitter, found: {banned}"
            );
        }
    }

    #[test]
    fn install_writes_skill_file() {
        let tmp = std::env::temp_dir().join(format!("pz-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        install(Some(tmp.clone()), false, false).unwrap();
        let expected = tmp.join(SKILL_NAME).join("SKILL.md");
        assert!(expected.exists());
        let written = std::fs::read_to_string(&expected).unwrap();
        assert_eq!(written, SKILL_BODY);

        // Second install without --force fails; with --force succeeds.
        assert!(install(Some(tmp.clone()), false, false).is_err());
        assert!(install(Some(tmp.clone()), true, false).is_ok());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
