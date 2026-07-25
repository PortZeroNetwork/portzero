//! `portzero version`: what version each installed PortZero component is
//! running, and whether they agree.
//!
//! `portzero --version` answers only for the binary you just ran, which is the
//! least interesting answer after an upgrade: the daemon, tray, and desktop app
//! are separate processes that keep running the build they started with. This
//! command asks all of them (see [`portzero_daemon::versions`]) and says plainly
//! whether the install is coherent.

use anyhow::Result;

use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::versions::{self, Component, VersionReport, VersionSource};

/// The CLI's own version — the workspace version every PortZero crate inherits.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Print the version of every component, then the verdict.
pub fn run() -> Result<()> {
    let report = collect();
    print!("{}", render(&report));
    Ok(())
}

/// The report as the CLI sees it (the CLI is the component asking).
pub fn collect() -> VersionReport {
    versions::collect(&DaemonConfig::load(), Component::Cli, BUILD_VERSION)
}

/// Render the report as the aligned block `portzero version` prints. Split from
/// [`run`] so the formatting is testable without touching the filesystem.
fn render(report: &VersionReport) -> String {
    let width = report
        .components
        .iter()
        .map(|c| c.label.len())
        .max()
        .unwrap_or(0);

    let mut out = format!("PortZero {}\n\n", report.build_version);
    for c in &report.components {
        let version = c.version.as_deref().unwrap_or("unknown");
        let state = match c.source {
            VersionSource::Running => "running",
            VersionSource::Installed => "installed",
            VersionSource::Unknown if c.running => "running, version not reported",
            VersionSource::Unknown => "not running",
        };
        out.push_str(&format!(
            "  {:<width$}  {:<12}  {state}\n",
            c.label,
            version,
            width = width
        ));
    }

    out.push_str(&format!("\n{}\n", report.summary));
    if !report.next_steps.is_empty() {
        out.push('\n');
        for step in &report.next_steps {
            out.push_str(&format!("  - {step}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use portzero_daemon::versions::{evaluate, ComponentVersion};

    fn cv(component: Component, version: Option<&str>, source: VersionSource) -> ComponentVersion {
        ComponentVersion {
            component,
            label: component.label().to_string(),
            version: version.map(str::to_string),
            running: source == VersionSource::Running,
            pid: None,
            source,
            detail: String::new(),
        }
    }

    #[test]
    fn rendered_output_lists_every_component_and_the_verdict() {
        let report = evaluate(
            "1.2.3",
            vec![
                cv(Component::App, Some("1.2.3"), VersionSource::Running),
                cv(Component::Tray, None, VersionSource::Unknown),
                cv(Component::Cli, Some("1.2.3"), VersionSource::Installed),
            ],
        );
        let out = render(&report);

        assert!(out.starts_with("PortZero 1.2.3"), "{out}");
        assert!(out.contains("Desktop app"), "{out}");
        assert!(out.contains("Tray"), "{out}");
        assert!(out.contains("unknown"), "{out}");
        assert!(out.contains("not running"), "{out}");
    }

    #[test]
    fn a_mismatch_prints_the_next_steps() {
        let report = evaluate(
            "1.2.3",
            vec![
                cv(Component::Daemon, Some("1.1.0"), VersionSource::Running),
                cv(Component::Cli, Some("1.2.3"), VersionSource::Installed),
            ],
        );
        let out = render(&report);

        assert!(out.contains("disagree"), "{out}");
        assert!(out.contains("- "), "next steps should be bulleted: {out}");
        assert!(out.contains("portzero restart"), "{out}");
    }
}
