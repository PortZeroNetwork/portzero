//! Visibility layer: duplicate-name detection, persisted "issues" state, and
//! best-effort native notifications.
//!
//! When the same `*.portzero.local` overlay name is claimed by two different
//! process contexts (different PID *and* different working directory — i.e.
//! two worktrees), routing is ambiguous and the user almost certainly meant to
//! give the worktrees distinct names. This module makes that situation loud:
//!
//! 1. **Detection** — pure logic over the already-scanned overlay services.
//! 2. **Persisted state** — `issues.json` in the daemon state dir, following the
//!    same pattern as `cloud_state.json`, so `portzero status` can surface
//!    problems even though it runs in a separate process from the daemon.
//! 3. **Notifications** — shell-out to the platform's native mechanism
//!    (`osascript` / `notify-send` / PowerShell toast). No GUI crates. Always
//!    best-effort and non-fatal; never fired from tests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::discovery::{DiscoveredNetworkService, ServiceSource};

/// A single detected problem the daemon wants to make visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Issue {
    /// The same `.portzero.local` name is claimed by more than one distinct
    /// process context (different worktrees / cwds).
    DuplicateName {
        /// The overlay name (label) in conflict, e.g. "my-db".
        name: String,
        /// Human-readable descriptions of each claimant (cwd / container).
        claimants: Vec<String>,
    },
    /// A process is listening on a "common" dev port (or on a port a known
    /// overlay service uses) but is *not* going through port-zero (no
    /// `PZ_TUNNEL` set). This is the classic "I'm still hitting localhost
    /// directly" footgun; we surface migration guidance.
    LegacyListener {
        /// The TCP port the legacy process is listening on.
        port: u16,
        /// Owning process id, if known (0 if unknown).
        pid: u32,
        /// Best-effort description of the owning context (cwd / git context).
        context: String,
    },
    /// A Docker container failed to start because a published host port is
    /// already bound by something else (a port-bind conflict). Often the
    /// "other" binder is a stale process or a non-tunnel service.
    DockerPortConflict {
        /// The host port that could not be bound.
        port: u16,
        /// The container (name or id) that failed to start.
        container: String,
    },
    /// A `PZ_TUNNEL` value looked like a cloud tunnel request (it doesn't end
    /// in `.local`/`.portzero.local`) but is missing the required
    /// `<cloud-username>` scope — e.g. `myservice.portzero.cloud` or
    /// `myservice.tunnel.portzero.cloud` instead of
    /// `myservice.<cloud-username>.tunnel.portzero.cloud`.
    InvalidCloudTunnelScope {
        /// The resolved (post-template) domain that failed validation.
        domain: String,
        /// The specific validation failure from `validate_tunnel_domain`.
        reason: String,
        /// Best-effort description of the owning context (cwd / container).
        context: String,
        /// Owning process id, when the source is a process (0/None for containers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// A `PZ_TUNNEL` template misused the `{local-username}`/`{cloud-username}`
    /// placeholders — either `{local-username}` was used in a cloud tunnel
    /// (which would break the orthogonality between `.local` and `.cloud`
    /// tunnel names), or `{cloud-username}` was used in a `.local` tunnel
    /// while not logged in.
    InvalidUsernamePlaceholder {
        /// The raw (pre-resolution) `PZ_TUNNEL` template.
        template: String,
        /// The specific validation failure from `validate_username_placeholders`.
        reason: String,
        /// Best-effort description of the owning context (cwd / container).
        context: String,
        /// Whether the fix is to run `portzero login` — lets the UI offer a
        /// one-click "Log in" button instead of just printing guidance text.
        requires_login: bool,
        /// Owning process id, when the source is a process (0/None for containers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// A `PZ_TUNNEL` template resolved to a name that is still broken: it either
    /// carries an unresolved `{token}` (e.g. `{pr}` used outside a pull request,
    /// `{run-id}` outside a GitHub Actions run) or a dot-separated label with an
    /// ambiguous internal `--`. Discovery is skipped for this tunnel rather than
    /// registering a garbled name.
    InvalidResolvedName {
        /// The raw (pre-resolution) `PZ_TUNNEL` template.
        template: String,
        /// The resolved (post-template) value that failed validation.
        resolved: String,
        /// The specific validation failure from `validate_resolved_name`.
        reason: String,
        /// Best-effort description of the owning context (cwd / container).
        context: String,
        /// Owning process id, when the source is a process (0/None for containers).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
}

impl Issue {
    /// A short, single-line summary suitable for a notification body or a
    /// `status` line.
    pub fn summary(&self) -> String {
        match self {
            Issue::DuplicateName { name, claimants } => format!(
                "Duplicate .portzero.local name \"{}\" claimed by {} contexts: {}",
                name,
                claimants.len(),
                claimants.join(", ")
            ),
            Issue::LegacyListener { port, pid, context } => format!(
                "Port {} is served directly (not via port-zero) by pid {} {}",
                port, pid, context
            ),
            Issue::DockerPortConflict { port, container } => format!(
                "Docker container \"{}\" failed to start: host port {} is already in use",
                container, port
            ),
            Issue::InvalidCloudTunnelScope {
                domain, context, ..
            } => format!(
                "Invalid cloud tunnel domain \"{}\" ({}): missing username scope",
                domain, context
            ),
            Issue::InvalidUsernamePlaceholder {
                template, context, ..
            } => format!(
                "Invalid username placeholder in PZ_TUNNEL template \"{}\" ({})",
                template, context
            ),
            Issue::InvalidResolvedName {
                template,
                resolved,
                context,
                ..
            } => format!(
                "PZ_TUNNEL template \"{}\" resolved to an invalid name \"{}\" ({})",
                template, resolved, context
            ),
        }
    }

    /// Owning process id, when known — lets the UI show which process to look
    /// at without parsing it back out of `summary()`/`fix_hint()` text.
    pub fn pid(&self) -> Option<u32> {
        match self {
            Issue::LegacyListener { pid, .. } => Some(*pid),
            Issue::InvalidCloudTunnelScope { pid, .. }
            | Issue::InvalidUsernamePlaceholder { pid, .. }
            | Issue::InvalidResolvedName { pid, .. } => *pid,
            Issue::DuplicateName { .. } | Issue::DockerPortConflict { .. } => None,
        }
    }

    /// Whether the UI should offer a one-click "Log in" fix for this issue.
    pub fn needs_login(&self) -> bool {
        matches!(
            self,
            Issue::InvalidUsernamePlaceholder {
                requires_login: true,
                ..
            }
        )
    }

    /// Actionable guidance explaining how to fix the issue.
    pub fn fix_hint(&self) -> String {
        match self {
            Issue::DuplicateName { name, .. } => format!(
                "Give each worktree a unique PZ_TUNNEL name — e.g. use a template \
                 like \"{name}-{{branch}}.portzero.local\" or \"{name}-{{worktree}}.portzero.local\" \
                 so the resolved name differs per checkout."
            ),
            Issue::LegacyListener { port, .. } => format!(
                "Set PZ_TUNNEL on this process (e.g. \
                 PZ_TUNNEL=my-svc.portzero.local for the local overlay, or \
                 my-svc.<user>.tunnel.portzero.cloud for a cloud tunnel) and reach it by name \
                 instead of localhost:{port}. Until then this service bypasses the tunnel."
            ),
            Issue::DockerPortConflict { port, .. } => format!(
                "Free host port {port} (stop whatever is bound to it) or remap the container's \
                 published port. To route the container through the tunnel, set PZ_TUNNEL in \
                 its environment instead of publishing a fixed host port."
            ),
            // `reason` is the message from `validate_tunnel_domain`, which already
            // spells out the expected format and points at `portzero whoami`.
            Issue::InvalidCloudTunnelScope { reason, .. } => reason.clone(),
            // `reason` is the message from `validate_username_placeholders`, which
            // already explains the fix (switch placeholders, or `portzero login`)
            // and, when relevant, that local tunnels stay free either way.
            Issue::InvalidUsernamePlaceholder { reason, .. } => reason.clone(),
            // `reason` is the message from `validate_resolved_name`, which spells
            // out which token could not resolve (and in which context it is
            // available) or which label carried an ambiguous `--`.
            Issue::InvalidResolvedName { reason, .. } => reason.clone(),
        }
    }
}

/// The persisted set of current issues. Stored as a stable, sorted structure so
/// repeated scans that find the same problems produce byte-identical JSON (which
/// lets the loop cheaply detect "did the issue set change?" for notification
/// de-duplication).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuesState {
    pub issues: Vec<Issue>,
}

impl IssuesState {
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    /// Serialize to pretty JSON. Infallible in practice; returns an empty object
    /// on the (impossible) serialization error.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{\"issues\":[]}".to_string())
    }

    /// Parse from JSON, returning an empty state on any error.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// A single problem the daemon wants to surface to the user, regardless of
/// whether it came from passive discovery (`issues.json`) or an active
/// diagnostics run (`diagnostics.json`). This is the single source of truth
/// the tray, dashboard, and `/status.json` all render from — keeping the two
/// origins separate at the UI layer just recreates the "issues vs.
/// diagnostics" confusion this type exists to remove.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    pub severity: crate::diagnostics::Severity,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_command: Option<String>,
    /// Owning process id, when this problem traces back to a single process
    /// (e.g. a misconfigured `PZ_TUNNEL` or a legacy listener). `None` for
    /// problems with no single owning process (duplicate names, environment
    /// checks, DNS probes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub needs_login: bool,
}

/// Flatten `issues.json` + `diagnostics.json` into one severity-ranked list.
/// Informational diagnostics (probe_ok, "not logged in", VPN present, …) are
/// status noise, not problems, and are skipped.
pub fn collect_problems(
    issues: &IssuesState,
    diagnostics: Option<&crate::diagnostics::DiagnosticsReport>,
) -> Vec<Problem> {
    let mut problems: Vec<Problem> = issues
        .issues
        .iter()
        // LegacyListener is advisory, not actionable: it fires for any process
        // (including unrelated system services like sshd) serving a common dev
        // port directly, which is extremely common and not something most
        // users can or need to fix. It is logged (see `publish_issues`) but
        // never surfaced as a user-facing problem/diagnostic.
        .filter(|issue| !matches!(issue, Issue::LegacyListener { .. }))
        .map(|issue| Problem {
            // issues.json problems carry no severity of their own; treat them
            // as errors so they rank above informational diagnostics.
            severity: crate::diagnostics::Severity::Error,
            title: issue.summary(),
            detail: None,
            fix: Some(issue.fix_hint()),
            fix_command: None,
            pid: issue.pid(),
            needs_login: issue.needs_login(),
        })
        .collect();

    if let Some(report) = diagnostics {
        problems.extend(
            report
                .issues
                .iter()
                .filter(|d| d.severity != crate::diagnostics::Severity::Info)
                .map(|d| Problem {
                    severity: d.severity.clone(),
                    title: d.title.clone(),
                    detail: Some(d.detail.clone()),
                    fix: d.fix.as_ref().map(|f| f.description.clone()),
                    fix_command: d.fix.as_ref().and_then(|f| f.command.clone()),
                    pid: None,
                    needs_login: false,
                }),
        );
    }

    problems.sort_by(|a, b| a.severity.cmp(&b.severity));
    problems
}

/// Render a claimant description for a discovered overlay service. Uses the
/// existing `ServiceSource` Display for processes (`(~/path)`) and containers.
fn describe_claimant(svc: &DiscoveredNetworkService) -> String {
    match &svc.source {
        ServiceSource::Process { .. } => format!("pid {} {}", svc.pid, svc.source),
        ServiceSource::Container { .. } => format!("{}", svc.source),
    }
}

/// A "context key" that distinguishes two genuinely different claimants. Two
/// entries for the same name are only a *conflict* if they come from different
/// process contexts — different PID and different cwd (or a container vs a
/// process). The same process appearing twice, or two scans of the same
/// worktree, must not be flagged.
fn context_key(svc: &DiscoveredNetworkService) -> (Option<PathBuf>, Option<String>) {
    match &svc.source {
        // For processes, the working directory is the worktree identity. PID
        // alone is too noisy (restarts), so the cwd is the primary signal.
        ServiceSource::Process { cwd } => (cwd.clone(), None),
        // Containers are identified by their container id.
        ServiceSource::Container { id, .. } => (None, Some(id.clone())),
    }
}

/// Detect duplicate overlay names across distinct process contexts.
///
/// Returns one [`Issue::DuplicateName`] per name that is claimed by two or more
/// *distinct* contexts in this scan. Pure — operates only on the in-memory
/// service list, so it is fully unit-testable without privileges or shell-out.
pub fn detect_duplicate_names(services: &[DiscoveredNetworkService]) -> Vec<Issue> {
    // name -> (distinct context key -> claimant description)
    let mut by_name: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for svc in services {
        let key = context_key(svc);
        // Stringify the key so it is Ord and dedups identical contexts.
        let key_str = format!("{:?}", key);
        by_name
            .entry(svc.name.clone())
            .or_default()
            .entry(key_str)
            .or_insert_with(|| describe_claimant(svc));
    }

    let mut issues = Vec::new();
    for (name, contexts) in by_name {
        if contexts.len() >= 2 {
            let claimants: Vec<String> = contexts.into_values().collect();
            issues.push(Issue::DuplicateName { name, claimants });
        }
    }
    issues
}

/// Write the current issues to `path`. Best-effort: logs and ignores I/O errors.
pub fn write_issues(path: &std::path::Path, state: &IssuesState) {
    if let Err(e) = std::fs::write(path, state.to_json()) {
        tracing::warn!("Failed to write issues state: {}", e);
    }
}

/// Read the issues written by the running daemon. Returns an empty state if the
/// file is absent or unparseable.
pub fn read_issues(path: &std::path::Path) -> IssuesState {
    match std::fs::read_to_string(path) {
        Ok(content) => IssuesState::from_json(&content),
        Err(_) => IssuesState::default(),
    }
}

/// The platform-specific command + arguments to display a desktop notification.
/// Returned as data (not executed) so the construction is unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
///
/// Only used by the macOS notification path.
#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a string for embedding inside a single-quoted PowerShell literal.
///
/// Only used by the Windows notification path.
#[cfg(target_os = "windows")]
fn powershell_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Build the notification command for the current platform.
///
/// - macOS: `osascript -e 'display notification "body" with title "title"'`
/// - Linux: `notify-send "title" "body"`
/// - Windows: PowerShell toast via the legacy notify-icon balloon (no extra deps)
///
/// Pure: returns the command to run without running it.
pub fn build_notify_command(title: &str, body: &str) -> NotifyCommand {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            applescript_escape(body),
            applescript_escape(title),
        );
        NotifyCommand {
            program: "osascript".to_string(),
            args: vec!["-e".to_string(), script],
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Use a balloon tip via System.Windows.Forms.NotifyIcon — available on
        // stock Windows PowerShell with no extra modules or crates.
        let script = format!(
            "[reflection.assembly]::loadwithpartialname('System.Windows.Forms') | Out-Null; \
             $n = New-Object System.Windows.Forms.NotifyIcon; \
             $n.Icon = [System.Drawing.SystemIcons]::Warning; \
             $n.BalloonTipTitle = '{}'; \
             $n.BalloonTipText = '{}'; \
             $n.Visible = $true; \
             $n.ShowBalloonTip(10000); \
             Start-Sleep -Seconds 10; \
             $n.Dispose()",
            powershell_escape(title),
            powershell_escape(body),
        );
        NotifyCommand {
            program: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                script,
            ],
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux / other unix: notify-send is the de-facto standard and matches
        // the existing shell-out pattern (resolvectl, systemctl, etc.). The
        // applescript/powershell escapers are `#[cfg]`-gated to their own
        // platforms, so there is nothing to reference here.
        NotifyCommand {
            program: "notify-send".to_string(),
            args: vec![
                "--urgency=critical".to_string(),
                "--app-name=port-zero".to_string(),
                title.to_string(),
                body.to_string(),
            ],
        }
    }
}

/// Fire a best-effort native notification. Spawns the platform command and does
/// not wait for it; failures (e.g. `notify-send` not installed, headless box)
/// are downgraded to a debug log. Never panics, never blocks the daemon loop.
///
/// NOTE: this performs a real shell-out and must never be called from tests.
pub fn send_notification(title: &str, body: &str) {
    let cmd = build_notify_command(title, body);
    match std::process::Command::new(&cmd.program)
        .args(&cmd.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            tracing::debug!("Dispatched native notification via {}", cmd.program);
        }
        Err(e) => {
            tracing::debug!(
                "Could not send native notification via {} (continuing): {}",
                cmd.program,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;

    fn proc_svc(name: &str, pid: u32, cwd: Option<&str>) -> DiscoveredNetworkService {
        DiscoveredNetworkService {
            name: name.to_string(),
            domain_template: format!("{name}.portzero.local"),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30000),
            service_port: 5432,
            backend_protocol: None,
            pid,
            source: ServiceSource::Process {
                cwd: cwd.map(PathBuf::from),
            },
            health_path: None,
        }
    }

    fn container_svc(name: &str, id: &str) -> DiscoveredNetworkService {
        DiscoveredNetworkService {
            name: name.to_string(),
            domain_template: format!("{name}.portzero.local"),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30000),
            service_port: 5432,
            backend_protocol: None,
            pid: 0,
            source: ServiceSource::Container {
                id: id.to_string(),
                name: name.to_string(),
            },
            health_path: None,
        }
    }

    #[test]
    fn test_no_duplicates_when_unique_names() {
        let svcs = vec![
            proc_svc("db", 100, Some("/work/a")),
            proc_svc("api", 101, Some("/work/b")),
        ];
        assert!(detect_duplicate_names(&svcs).is_empty());
    }

    #[test]
    fn test_same_name_same_cwd_is_not_duplicate() {
        // Same worktree scanned twice (e.g. parent + child process) — one cwd.
        let svcs = vec![
            proc_svc("db", 100, Some("/work/a")),
            proc_svc("db", 200, Some("/work/a")),
        ];
        assert!(
            detect_duplicate_names(&svcs).is_empty(),
            "same cwd must not be flagged as a worktree conflict"
        );
    }

    #[test]
    fn test_same_name_different_cwd_is_duplicate() {
        let svcs = vec![
            proc_svc("db", 100, Some("/work/feature-a")),
            proc_svc("db", 200, Some("/work/feature-b")),
        ];
        let issues = detect_duplicate_names(&svcs);
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            Issue::DuplicateName { name, claimants } => {
                assert_eq!(name, "db");
                assert_eq!(claimants.len(), 2);
            }
            other => panic!("unexpected issue: {:?}", other),
        }
    }

    #[test]
    fn test_container_vs_process_same_name_is_duplicate() {
        let svcs = vec![
            proc_svc("db", 100, Some("/work/a")),
            container_svc("db", "abc123"),
        ];
        assert_eq!(detect_duplicate_names(&svcs).len(), 1);
    }

    #[test]
    fn test_two_containers_same_name_distinct_ids_is_duplicate() {
        let svcs = vec![container_svc("db", "aaa"), container_svc("db", "bbb")];
        assert_eq!(detect_duplicate_names(&svcs).len(), 1);
    }

    #[test]
    fn test_issue_summary_and_fix_hint() {
        let issue = Issue::DuplicateName {
            name: "db".to_string(),
            claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
        };
        assert!(issue.summary().contains("db"));
        assert!(issue.summary().contains("2 contexts"));
        assert!(issue.fix_hint().contains("{branch}"));
        assert!(issue.fix_hint().contains("db-"));
    }

    #[test]
    fn test_issues_state_roundtrip() {
        let state = IssuesState {
            issues: vec![Issue::DuplicateName {
                name: "db".to_string(),
                claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
            }],
        };
        let json = state.to_json();
        let parsed = IssuesState::from_json(&json);
        assert_eq!(state, parsed);
    }

    #[test]
    fn test_legacy_and_docker_issue_roundtrip() {
        let state = IssuesState {
            issues: vec![
                Issue::LegacyListener {
                    port: 5432,
                    pid: 4321,
                    context: "(~/work/api)".to_string(),
                },
                Issue::DockerPortConflict {
                    port: 8080,
                    container: "web-1".to_string(),
                },
            ],
        };
        let parsed = IssuesState::from_json(&state.to_json());
        assert_eq!(state, parsed);

        // summary / fix_hint mention the salient details.
        let legacy = &state.issues[0];
        assert!(legacy.summary().contains("5432"));
        assert!(legacy.summary().contains("4321"));
        assert!(legacy.fix_hint().contains("PZ_TUNNEL"));

        let docker = &state.issues[1];
        assert!(docker.summary().contains("web-1"));
        assert!(docker.summary().contains("8080"));
        assert!(docker.fix_hint().contains("8080"));
    }

    #[test]
    fn test_invalid_cloud_tunnel_scope_issue_roundtrip() {
        let state = IssuesState {
            issues: vec![Issue::InvalidCloudTunnelScope {
                domain: "myservice.portzero.cloud".to_string(),
                reason: "'myservice.portzero.cloud' is not a valid cloud tunnel domain \
                    it must end with '.tunnel.portzero.cloud' and be scoped to your \
                    username, e.g. 'myservice.<username>.tunnel.portzero.cloud'. \
                    Run `portzero whoami` to find your username."
                    .to_string(),
                context: "pid 1234".to_string(),
                pid: Some(1234),
            }],
        };
        let parsed = IssuesState::from_json(&state.to_json());
        assert_eq!(state, parsed);

        let issue = &state.issues[0];
        assert!(issue.summary().contains("myservice.portzero.cloud"));
        assert!(issue.summary().contains("pid 1234"));
        assert!(issue.fix_hint().contains("portzero whoami"));
        assert!(issue.fix_hint().contains("<username>"));
        assert_eq!(issue.pid(), Some(1234));
    }

    #[test]
    fn test_issues_state_empty_roundtrip() {
        let state = IssuesState::default();
        assert!(state.is_empty());
        let parsed = IssuesState::from_json(&state.to_json());
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_issues_state_from_garbage_is_empty() {
        assert!(IssuesState::from_json("not json").is_empty());
        assert!(IssuesState::from_json("").is_empty());
    }

    #[test]
    fn test_detection_is_stable_ordering() {
        // Two distinct names in conflict; output order must be deterministic
        // (sorted by name) so persisted JSON is stable across scans.
        let svcs = vec![
            proc_svc("zebra", 1, Some("/a")),
            proc_svc("zebra", 2, Some("/b")),
            proc_svc("alpha", 3, Some("/c")),
            proc_svc("alpha", 4, Some("/d")),
        ];
        let issues = detect_duplicate_names(&svcs);
        assert_eq!(issues.len(), 2);
        match (&issues[0], &issues[1]) {
            (Issue::DuplicateName { name: n0, .. }, Issue::DuplicateName { name: n1, .. }) => {
                assert_eq!(n0, "alpha");
                assert_eq!(n1, "zebra");
            }
            other => panic!("unexpected issues: {:?}", other),
        }
    }

    #[test]
    fn test_build_notify_command_nonempty() {
        let cmd = build_notify_command("Title", "Body with \"quotes\" and 'apostrophes'");
        assert!(!cmd.program.is_empty());
        assert!(!cmd.args.is_empty());
        // The body content must appear somewhere in the constructed args.
        let joined = cmd.args.join(" ");
        assert!(joined.contains("Body"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_build_notify_command_linux() {
        let cmd = build_notify_command("My Title", "My Body");
        assert_eq!(cmd.program, "notify-send");
        assert!(cmd.args.contains(&"My Title".to_string()));
        assert!(cmd.args.contains(&"My Body".to_string()));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_applescript_escape() {
        assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_powershell_escape() {
        assert_eq!(powershell_escape("it's"), "it''s");
    }

    #[test]
    fn test_write_then_read_issues() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issues.json");
        let state = IssuesState {
            issues: vec![Issue::DuplicateName {
                name: "db".to_string(),
                claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
            }],
        };
        write_issues(&path, &state);
        assert_eq!(read_issues(&path), state);
    }

    #[test]
    fn test_read_issues_missing_file() {
        let state = read_issues(std::path::Path::new("/nonexistent/issues.json"));
        assert!(state.is_empty());
    }

    #[test]
    fn test_collect_problems_combines_and_ranks_by_severity() {
        use crate::diagnostics::{Diagnostic, DiagnosticsReport, Fix, FixKind, Severity};

        // A LegacyListener issue plus a DuplicateName issue — only the latter
        // is a user-facing problem (LegacyListener is log-only, see
        // `publish_issues`).
        let issues = IssuesState {
            issues: vec![
                Issue::LegacyListener {
                    port: 5432,
                    pid: 4321,
                    context: "(~/work/api)".to_string(),
                },
                Issue::DuplicateName {
                    name: "db".to_string(),
                    claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
                },
            ],
        };
        let report = DiagnosticsReport {
            generated_at: "2026-07-11T00:00:00Z".to_string(),
            checks_run: 3,
            issues: vec![
                Diagnostic {
                    id: "binary_missing".into(),
                    severity: Severity::Critical,
                    category: "binary".into(),
                    title: "Binary missing".to_string(),
                    detail: "detail".to_string(),
                    fix: Some(Fix {
                        kind: FixKind::Manual,
                        description: "Reinstall".to_string(),
                        command: Some("portzero install".to_string()),
                    }),
                },
                Diagnostic {
                    id: "dns_probe_ok".into(),
                    severity: Severity::Info,
                    category: "dns".into(),
                    title: "portzero.local resolution works".to_string(),
                    detail: "ok".to_string(),
                    fix: None,
                },
            ],
        };

        let problems = collect_problems(&issues, Some(&report));

        // Info-level diagnostics and the LegacyListener issue are filtered
        // out; the duplicate-name issue and the critical diagnostic survive.
        assert_eq!(problems.len(), 2);
        // Sorted severity-first: Critical before the (assumed) Error-ranked issue.
        assert_eq!(problems[0].severity, Severity::Critical);
        assert_eq!(problems[0].title, "Binary missing");
        assert_eq!(problems[0].fix_command.as_deref(), Some("portzero install"));

        assert!(
            !problems.iter().any(|p| p.title.contains("5432")),
            "LegacyListener must not be surfaced as a user-facing problem"
        );
        assert!(problems.iter().any(|p| p.title.contains("db")));

        assert!(
            !problems
                .iter()
                .any(|p| p.title.contains("resolution works")),
            "informational diagnostics must be filtered out"
        );
    }
}
