//! Which PortZero components are running, at which version, and do they agree.
//!
//! PortZero installs four binaries from one build: `portzero` (the CLI, which is
//! also the process the daemon runs in), `portzero-tray`, and `portzero-app`.
//! Every crate inherits the workspace version (see the root `Cargo.toml`), so a
//! single build stamps them all identically — which means a *disagreement* at
//! runtime always signals the same real problem: an upgrade replaced the
//! binaries on disk while an older daemon, tray, or app kept running.
//!
//! The long-lived components announce themselves by writing
//! `~/.portzero/components/<name>.json` at startup and removing it on a clean
//! exit. A record whose PID is no longer alive is treated as absent, so a
//! crashed component never leaves a phantom version behind. The CLI is not
//! long-lived and has no record: its version is read by running the installed
//! binary with `--version`.
//!
//! Consumers: the desktop app's Version panel, `portzero version`, and the
//! `component versions` check in `portzero doctor`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::discovery_loop::{read_daemon_pid, DaemonConfig};
use crate::management::pid_lookup::pid_is_alive;

/// The version this binary was built from — the workspace version.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long to wait for `<binary> --version` before giving up and killing it.
/// This is a local process that prints one line and exits; the only realistic
/// way to exceed it is a binary that does not understand `--version` at all.
const VERSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

/// One of the binaries a PortZero install ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Component {
    /// The background daemon (a `portzero start --foreground` process).
    Daemon,
    /// The system-tray companion, `portzero-tray`.
    Tray,
    /// The desktop app, `portzero-app`.
    App,
    /// The `portzero` command-line tool.
    Cli,
}

impl Component {
    /// Stable identifier used for the record file name and in JSON.
    pub fn id(self) -> &'static str {
        match self {
            Component::Daemon => "daemon",
            Component::Tray => "tray",
            Component::App => "app",
            Component::Cli => "cli",
        }
    }

    /// Human-readable name for user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Component::Daemon => "Daemon",
            Component::Tray => "Tray",
            Component::App => "Desktop app",
            Component::Cli => "CLI",
        }
    }

    /// Every component, in the order surfaces should display them.
    pub const ALL: [Component; 4] = [
        Component::App,
        Component::Tray,
        Component::Daemon,
        Component::Cli,
    ];
}

/// What a running component wrote to `~/.portzero/components/<name>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRecord {
    pub component: Component,
    pub version: String,
    pub pid: u32,
}

/// Where a running component's version is known from — surfaced so the UI can
/// say "running" versus "installed on disk" rather than implying more than we
/// checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionSource {
    /// Reported by the live process (this one, or its record file).
    Running,
    /// Read from the installed binary on disk; nothing of it is running.
    Installed,
    /// The component is not running and its version could not be read.
    Unknown,
}

/// One row of the version report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentVersion {
    pub component: Component,
    /// Display name, so surfaces don't each re-map the enum.
    pub label: String,
    /// `None` when the version could not be determined at all.
    pub version: Option<String>,
    pub running: bool,
    pub pid: Option<u32>,
    pub source: VersionSource,
    /// One line explaining where this version came from, or why it is missing —
    /// including what the user can do about it.
    pub detail: String,
}

/// The full picture: every component's version plus whether they agree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionReport {
    /// The version of the binary that produced this report.
    pub build_version: String,
    pub components: Vec<ComponentVersion>,
    /// True when every version we could read is the same one.
    pub consistent: bool,
    /// One-line verdict, suitable for a heading.
    pub summary: String,
    /// What to do about a mismatch; empty when everything agrees.
    pub next_steps: Vec<String>,
}

/// `~/.portzero/components` — beside the daemon's state directory, like
/// `~/.portzero/tray.pid`, because it holds records for components other than
/// the daemon.
pub fn components_dir(config: &DaemonConfig) -> PathBuf {
    config
        .state_dir
        .parent()
        .map(|p| p.join("components"))
        .unwrap_or_else(|| PathBuf::from(".portzero/components"))
}

fn record_path(dir: &Path, component: Component) -> PathBuf {
    dir.join(format!("{}.json", component.id()))
}

/// Record that this process is running `component` at `version`.
///
/// Best-effort by design: a component that cannot write its record still works,
/// it just shows up as "version unknown" in the report. Failing to start over a
/// version file would be a much worse trade.
pub fn announce(config: &DaemonConfig, component: Component, version: &str) {
    if let Err(e) = announce_in(
        &components_dir(config),
        component,
        version,
        std::process::id(),
    ) {
        tracing::debug!(
            "could not record the {} version in {}: {e}",
            component.id(),
            components_dir(config).display()
        );
    }
}

/// [`announce`] against an explicit directory and PID, so tests can use a
/// scratch directory instead of the real `~/.portzero`.
pub fn announce_in(
    dir: &Path,
    component: Component,
    version: &str,
    pid: u32,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let record = ComponentRecord {
        component,
        version: version.to_string(),
        pid,
    };
    let json = serde_json::to_string_pretty(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(record_path(dir, component), json)
}

/// Remove this component's record on a clean exit. A missing record reads as
/// "not running", which is exactly what a stopped component is.
pub fn withdraw(config: &DaemonConfig, component: Component) {
    let _ = std::fs::remove_file(record_path(&components_dir(config), component));
}

/// Read a component's record, or `None` if it is absent, unreadable, or owned by
/// a PID that is no longer alive (a component that crashed without cleaning up).
pub fn read_record(dir: &Path, component: Component) -> Option<ComponentRecord> {
    let raw = std::fs::read_to_string(record_path(dir, component)).ok()?;
    let record: ComponentRecord = serde_json::from_str(&raw).ok()?;
    if record.component != component || !pid_is_alive(record.pid) {
        return None;
    }
    Some(record)
}

/// Extract the version from a `<binary> --version` line such as
/// `portzero 1.2.3`, tolerating a leading `v` and any binary name.
pub fn parse_version_output(output: &str) -> Option<String> {
    let line = output.lines().find(|l| !l.trim().is_empty())?;
    let token = line.split_whitespace().last()?;
    let version = token.strip_prefix('v').unwrap_or(token);
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// Run `<bin> --version` and return the reported version.
///
/// Guarded by a timeout: a binary from a build old enough not to handle
/// `--version` would otherwise start its own UI and never exit, hanging whatever
/// surface asked for the report.
fn installed_version(bin: &Path) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not run {} --version: {e}", bin.display()))?;

    let deadline = Instant::now() + VERSION_COMMAND_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not answer --version within {}s",
                    bin.display(),
                    VERSION_COMMAND_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(format!("could not wait for {}: {e}", bin.display())),
        }
    }

    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    parse_version_output(&output).ok_or_else(|| {
        format!(
            "{} --version printed something unexpected: {:?}",
            bin.display(),
            output.trim()
        )
    })
}

/// Build the full report for this machine.
///
/// `self_component` / `self_version` describe the caller, which always knows its
/// own version at compile time and should never learn it the roundabout way.
pub fn collect(
    config: &DaemonConfig,
    self_component: Component,
    self_version: &str,
) -> VersionReport {
    let dir = components_dir(config);
    let components = Component::ALL
        .iter()
        .map(|&component| {
            if component == self_component {
                return ComponentVersion {
                    component,
                    label: component.label().to_string(),
                    version: Some(self_version.to_string()),
                    running: true,
                    pid: Some(std::process::id()),
                    source: VersionSource::Running,
                    detail: "Running now — this is the build you are looking at.".to_string(),
                };
            }
            match component {
                Component::Cli => cli_version(),
                Component::Daemon => daemon_version(config, &dir),
                _ => running_component_version(&dir, component),
            }
        })
        .collect();

    evaluate(self_version, components)
}

/// The CLI is never long-lived, so there is nothing running to ask: read the
/// version out of the installed binary instead.
fn cli_version() -> ComponentVersion {
    let bin = portzero_domain::app::cli_bin();
    match installed_version(&bin) {
        Ok(version) => ComponentVersion {
            component: Component::Cli,
            label: Component::Cli.label().to_string(),
            version: Some(version),
            running: false,
            pid: None,
            source: VersionSource::Installed,
            detail: format!("Installed at {}.", bin.display()),
        },
        Err(reason) => ComponentVersion {
            component: Component::Cli,
            label: Component::Cli.label().to_string(),
            version: None,
            running: false,
            pid: None,
            source: VersionSource::Unknown,
            detail: format!(
                "Could not read the installed CLI version ({reason}). \
                 If `portzero` is not on your PATH, reinstall PortZero or set \
                 PORTZERO_BIN to the full path of the `portzero` binary."
            ),
        },
    }
}

/// The daemon runs inside the `portzero` binary, so a daemon that is running an
/// older version than the installed CLI is the classic post-upgrade mismatch.
fn daemon_version(config: &DaemonConfig, dir: &Path) -> ComponentVersion {
    if let Some(record) = read_record(dir, Component::Daemon) {
        return ComponentVersion {
            component: Component::Daemon,
            label: Component::Daemon.label().to_string(),
            version: Some(record.version),
            running: true,
            pid: Some(record.pid),
            source: VersionSource::Running,
            detail: format!("Running as PID {}.", record.pid),
        };
    }
    match read_daemon_pid(config) {
        // Running, but from a build that predates version reporting.
        Some(pid) => ComponentVersion {
            component: Component::Daemon,
            label: Component::Daemon.label().to_string(),
            version: None,
            running: true,
            pid: Some(pid),
            source: VersionSource::Unknown,
            detail: format!(
                "Running as PID {pid} but not reporting a version, which means it \
                 is older than the build you have installed. Run `portzero restart` \
                 to run the daemon from the installed binary."
            ),
        },
        None => ComponentVersion {
            component: Component::Daemon,
            label: Component::Daemon.label().to_string(),
            version: None,
            running: false,
            pid: None,
            source: VersionSource::Unknown,
            detail: "Not running. Run `portzero start` to start it.".to_string(),
        },
    }
}

/// Tray and app: both are long-lived processes that announce themselves, so
/// "no record" simply means they are not running.
fn running_component_version(dir: &Path, component: Component) -> ComponentVersion {
    match read_record(dir, component) {
        Some(record) => ComponentVersion {
            component,
            label: component.label().to_string(),
            version: Some(record.version),
            running: true,
            pid: Some(record.pid),
            source: VersionSource::Running,
            detail: format!("Running as PID {}.", record.pid),
        },
        None => ComponentVersion {
            component,
            label: component.label().to_string(),
            version: None,
            running: false,
            pid: None,
            source: VersionSource::Unknown,
            detail: match component {
                Component::Tray => "Not running. Start it from your applications \
                                    menu (PortZero Tray) to see its version."
                    .to_string(),
                _ => "Not running. Open the PortZero app to see its version.".to_string(),
            },
        },
    }
}

/// Turn a set of component versions into a verdict. Pure — all the collection
/// I/O happens before this, so the comparison itself is easy to test.
pub fn evaluate(build_version: &str, components: Vec<ComponentVersion>) -> VersionReport {
    let mut distinct: Vec<&str> = Vec::new();
    for c in &components {
        if let Some(v) = c.version.as_deref() {
            if !distinct.contains(&v) {
                distinct.push(v);
            }
        }
    }

    let consistent = distinct.len() <= 1;
    let summary = if distinct.is_empty() {
        format!("PortZero {build_version}. No other component is running to compare against.")
    } else if consistent {
        let checked = components.iter().filter(|c| c.version.is_some()).count();
        if checked == 1 {
            format!("PortZero {build_version}. Nothing else is running to compare against.")
        } else {
            format!(
                "All {checked} PortZero components report version {}.",
                distinct[0]
            )
        }
    } else {
        let listed = components
            .iter()
            .filter_map(|c| c.version.as_deref().map(|v| format!("{} {v}", c.label)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("PortZero components disagree on their version: {listed}.")
    };

    let next_steps = if consistent {
        Vec::new()
    } else {
        vec![
            "This usually means an upgrade replaced the PortZero binaries while an \
             older daemon, tray, or app kept running."
                .to_string(),
            "Restart the daemon so it runs from the installed binary: `portzero restart`."
                .to_string(),
            "Quit and reopen the PortZero app and the tray icon (Quit from the tray menu, \
             then start PortZero again)."
                .to_string(),
            "If versions still differ afterwards, the binaries on disk are from different \
             builds — run `portzero update` to install one matching set."
                .to_string(),
        ]
    };

    VersionReport {
        build_version: build_version.to_string(),
        components,
        consistent,
        summary,
        next_steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(component: Component, version: Option<&str>) -> ComponentVersion {
        ComponentVersion {
            component,
            label: component.label().to_string(),
            version: version.map(str::to_string),
            running: version.is_some(),
            pid: None,
            source: if version.is_some() {
                VersionSource::Running
            } else {
                VersionSource::Unknown
            },
            detail: String::new(),
        }
    }

    #[test]
    fn matching_versions_are_consistent() {
        let report = evaluate(
            "1.2.3",
            vec![
                cv(Component::App, Some("1.2.3")),
                cv(Component::Daemon, Some("1.2.3")),
                cv(Component::Cli, Some("1.2.3")),
            ],
        );
        assert!(report.consistent);
        assert!(report.next_steps.is_empty());
        assert!(report.summary.contains("1.2.3"), "{}", report.summary);
    }

    #[test]
    fn one_stale_component_is_a_mismatch_and_names_both_versions() {
        let report = evaluate(
            "1.2.3",
            vec![
                cv(Component::App, Some("1.2.3")),
                cv(Component::Daemon, Some("1.1.0")),
            ],
        );
        assert!(!report.consistent);
        assert!(report.summary.contains("1.2.3"), "{}", report.summary);
        assert!(report.summary.contains("1.1.0"), "{}", report.summary);
        // A user-facing mismatch must say what to do about it.
        assert!(report
            .next_steps
            .iter()
            .any(|s| s.contains("portzero restart")));
    }

    #[test]
    fn components_that_are_not_running_do_not_count_as_a_mismatch() {
        let report = evaluate(
            "1.2.3",
            vec![
                cv(Component::App, Some("1.2.3")),
                cv(Component::Tray, None),
                cv(Component::Daemon, None),
            ],
        );
        assert!(report.consistent);
        assert!(report.next_steps.is_empty());
    }

    #[test]
    fn a_lone_component_says_there_was_nothing_to_compare() {
        let report = evaluate("1.2.3", vec![cv(Component::App, Some("1.2.3"))]);
        assert!(report.consistent);
        assert!(
            report.summary.contains("Nothing else is running"),
            "{}",
            report.summary
        );
    }

    #[test]
    fn record_round_trips_through_the_components_dir() {
        let dir = tempfile::tempdir().unwrap();
        announce_in(dir.path(), Component::Tray, "1.2.3", std::process::id()).unwrap();

        let record = read_record(dir.path(), Component::Tray).expect("record should be readable");
        assert_eq!(record.version, "1.2.3");
        assert_eq!(record.component, Component::Tray);
    }

    #[test]
    fn a_record_from_a_dead_process_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        // PID 0 is never a live process on any platform this runs on.
        announce_in(dir.path(), Component::Tray, "1.2.3", 0).unwrap();

        assert!(read_record(dir.path(), Component::Tray).is_none());
    }

    #[test]
    fn a_corrupt_record_reads_as_absent_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("tray.json"), "{not json").unwrap();

        assert!(read_record(dir.path(), Component::Tray).is_none());
    }

    #[test]
    fn version_output_is_parsed_from_the_usual_clap_line() {
        assert_eq!(
            parse_version_output("portzero 1.2.3\n").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            parse_version_output("portzero-tray v1.2.3-rc.4").as_deref(),
            Some("1.2.3-rc.4")
        );
        assert_eq!(parse_version_output("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_version_output(""), None);
        assert_eq!(parse_version_output("command not found"), None);
    }

    #[test]
    fn components_dir_sits_beside_the_daemon_state_dir() {
        let config = DaemonConfig {
            state_dir: PathBuf::from("/home/someone/.portzero/daemon"),
            ..Default::default()
        };
        assert_eq!(
            components_dir(&config),
            PathBuf::from("/home/someone/.portzero/components")
        );
    }
}
