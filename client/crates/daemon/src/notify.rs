//! Visibility layer: duplicate-name detection, persisted "issues" state, and
//! best-effort native notifications.
//!
//! When the same `*.portzero.local` overlay name — or the same cloud tunnel URL
//! (`*.tunnel.portzero.cloud`) — is claimed by two different process contexts
//! (different working directory / container — i.e. two worktrees), routing is
//! ambiguous: only one claimant can win and the other is silently dropped. The
//! user almost certainly meant to give the worktrees distinct names. This module
//! makes that situation loud:
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

use crate::discovery::{DiscoveredNetworkService, DiscoveredService, ServiceSource};

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
    /// The same cloud tunnel URL (e.g. `api.alice.tunnel.portzero.cloud`) is
    /// claimed by more than one distinct process context on this machine. The
    /// route table is keyed by domain, so only one claimant can win — the others
    /// are silently dropped and their traffic never reaches the tunnel. This is
    /// the cloud-side analogue of [`Issue::DuplicateName`].
    DuplicateCloudUrl {
        /// The cloud tunnel URL in conflict, e.g. "api.alice.tunnel.portzero.cloud".
        url: String,
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
    /// `myservice--<cloud-username>.tunnel.portzero.cloud`.
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
    /// A structurally-valid cloud tunnel could not be published because its
    /// `--<namespace>` scope is one the account may not use. Either the client
    /// determined this locally (the namespace is neither the user's username nor
    /// a team slug the edge reported at connect) or the edge rejected the
    /// registration (namespace not owned, a team requiring its own subdomain, or
    /// a team naming policy the name violates). The tunnel stays registered
    /// locally but is not reachable from the internet.
    CloudTunnelNotAllowed {
        /// The cloud tunnel domain that was refused.
        domain: String,
        /// Human-readable explanation and fix — either the local ownership hint
        /// (listing the namespaces the account may use) or the edge's own error.
        reason: String,
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
                "Ambiguous .portzero.local name \"{}\" claimed by {} live backends: {}",
                name,
                claimants.len(),
                claimants.join(", ")
            ),
            Issue::DuplicateCloudUrl { url, claimants } => format!(
                "Duplicate cloud tunnel URL \"{}\" claimed by {} contexts: {}",
                url,
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
            Issue::CloudTunnelNotAllowed { domain, .. } => {
                format!("Cloud tunnel \"{domain}\" is not reachable: namespace not allowed")
            }
        }
    }

    /// Owning process id, when known — lets the UI show which process to look
    /// at without parsing it back out of `summary()`/`fix_hint()` text.
    pub fn pid(&self) -> Option<u32> {
        match self {
            Issue::LegacyListener { pid, .. } => Some(*pid),
            Issue::InvalidCloudTunnelScope { pid, .. }
            | Issue::InvalidUsernamePlaceholder { pid, .. }
            | Issue::InvalidResolvedName { pid, .. }
            | Issue::CloudTunnelNotAllowed { pid, .. } => *pid,
            Issue::DuplicateName { .. }
            | Issue::DuplicateCloudUrl { .. }
            | Issue::DockerPortConflict { .. } => None,
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
                "Only the backend marked [serving] answers requests for \
                 \"{name}.portzero.local\"; the others receive no traffic, so a \
                 stale one can serve responses that look like application bugs. \
                 If the extra backends are leftovers from an earlier run, stop \
                 them (`portzero status` lists each pid). If they are meant to \
                 coexist, give each a unique PZ_TUNNEL — e.g. a template like \
                 \"{name}-{{branch}}.portzero.local\" or \
                 \"{name}-{{worktree}}.portzero.local\" so the resolved name \
                 differs per checkout."
            ),
            Issue::DuplicateCloudUrl { url, .. } => {
                // Show the first label of the URL in the templated suggestion so
                // the hint reads naturally (e.g. "api" -> "api-{branch}...").
                let label = url.split('.').next().unwrap_or("service");
                let rest = url
                    .strip_prefix(label)
                    .and_then(|r| r.strip_prefix('.'))
                    .unwrap_or(url);
                format!(
                    "Two or more processes advertise the same cloud tunnel URL, so only one \
                     can win and the others are silently dropped. Give each a unique PZ_TUNNEL \
                     — e.g. a template like \"{label}-{{branch}}.{rest}\" so the resolved URL \
                     differs per checkout — or stop all but one. Run `portzero status` to see \
                     which context currently owns the URL."
                )
            }
            Issue::LegacyListener { port, .. } => format!(
                "Set PZ_TUNNEL on this process (e.g. \
                 PZ_TUNNEL=my-svc.portzero.local for the local overlay, or \
                 my-svc--<user>.tunnel.portzero.cloud for a cloud tunnel) and reach it by name \
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
            // `reason` is either the local ownership hint (which namespaces the
            // account may use) or the edge's own rejection message; both already
            // spell out the fix.
            Issue::CloudTunnelNotAllowed { reason, .. } => reason.clone(),
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

/// Render a claimant description from a source + pid. Uses the existing
/// `ServiceSource` Display for processes (`(~/path)`) and containers. Shared by
/// the overlay-name and cloud-URL duplicate detectors.
fn describe_claimant(pid: u32, source: &ServiceSource) -> String {
    match source {
        ServiceSource::Process { .. } => format!("pid {} {}", pid, source),
        ServiceSource::Container { .. } => format!("{}", source),
    }
}

/// A "context key" that distinguishes two genuinely different claimants. Two
/// entries for the same name/URL are only a *conflict* if they come from
/// different process contexts — different cwd (or a container vs a process, or
/// two distinct containers). The same process appearing twice, or two scans of
/// the same worktree, must not be flagged.
fn context_key(source: &ServiceSource) -> (Option<PathBuf>, Option<String>) {
    match source {
        // For processes, the working directory is the worktree identity. PID
        // alone is too noisy (restarts), so the cwd is the primary signal.
        ServiceSource::Process { cwd } => (cwd.clone(), None),
        // Containers are identified by their container id.
        ServiceSource::Container { id, .. } => (None, Some(id.clone())),
    }
}

/// Group claimants by conflict key, returning the distinct-context descriptions
/// for any key claimed by two or more contexts. Shared core of both duplicate
/// detectors. `entries` yields `(grouping_key, pid, source)` per claimant.
fn duplicate_claimants<'a, I>(entries: I) -> BTreeMap<String, Vec<String>>
where
    I: IntoIterator<Item = (String, u32, &'a ServiceSource)>,
{
    // grouping key -> (distinct context key -> claimant description)
    let mut by_key: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (group, pid, source) in entries {
        // Stringify the context key so it is Ord and dedups identical contexts.
        let ctx = format!("{:?}", context_key(source));
        by_key
            .entry(group)
            .or_default()
            .entry(ctx)
            .or_insert_with(|| describe_claimant(pid, source));
    }
    by_key
        .into_iter()
        .filter(|(_, contexts)| contexts.len() >= 2)
        .map(|(group, contexts)| (group, contexts.into_values().collect()))
        .collect()
}

/// Detect overlay names claimed by more than one live backend.
///
/// Returns one [`Issue::DuplicateName`] per contested name, listing every
/// claimant best-first and marking which one actually serves traffic (see
/// [`crate::claims`]).
///
/// Two claimants are distinct when they are *different live backends*: a
/// different listening address, or a different context (worktree / container).
/// Grouping by context alone — the previous rule — hid the most expensive case
/// there is: several leaked backends from repeated test runs in a single
/// directory, which share a cwd and so collapsed to one context. Requests were
/// answered by an arbitrary one of them with nothing reported anywhere.
///
/// Pure — operates only on the in-memory service list, so it is fully
/// unit-testable without privileges or shell-out.
pub fn detect_duplicate_names(services: &[DiscoveredNetworkService]) -> Vec<Issue> {
    crate::claims::group_by_claim(services.iter())
        .into_iter()
        .filter_map(|(name, claimants)| {
            let claimants = describe_ranked_claimants(&claimants);
            (claimants.len() >= 2).then_some(Issue::DuplicateName { name, claimants })
        })
        .collect()
}

/// Render a best-first claimant list for display, dropping entries that are the
/// same backend seen twice and tagging each survivor with its standing.
///
/// Identity is `(listening address, context)`: one process inherited by a child,
/// or a single backend picked up by two scan paths, is one claimant — flagging
/// that would be noise. Two processes on different ports are two claimants even
/// from the same directory, which is the leaked-backend case.
fn describe_ranked_claimants(claimants: &[&DiscoveredNetworkService]) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for svc in claimants {
        let identity = format!("{}|{:?}", svc.real_addr, context_key(&svc.source));
        if !seen.insert(identity) {
            continue;
        }
        let standing = crate::claims::standing(out.len());
        out.push(format!(
            "{} on {} [{standing}]",
            describe_claimant(svc.pid, &svc.source),
            svc.real_addr
        ));
    }
    out
}

/// Detect two or more contexts advertising the *same cloud tunnel URL* on this
/// machine.
///
/// The cloud analogue of [`detect_duplicate_names`]. The route table is keyed by
/// domain, so identical cloud URLs collapse to a single winning route (lowest
/// pid) and the losers are silently dropped — a footgun that is otherwise
/// invisible. This runs over the freshly-scanned services *before* that dedup so
/// both claimants are still present.
///
/// Only cloud tunnel domains are considered; `.portzero.local` overlay names are
/// handled by [`detect_duplicate_names`] and are skipped here to avoid
/// double-reporting. Pure and fully unit-testable.
pub fn detect_duplicate_cloud_urls(services: &[DiscoveredService]) -> Vec<Issue> {
    duplicate_claimants(
        services
            .iter()
            .filter(|svc| !crate::discovery::is_local_overlay_domain(&svc.domain))
            .map(|svc| (svc.domain.clone(), svc.pid, &svc.source)),
    )
    .into_iter()
    .map(|(url, claimants)| Issue::DuplicateCloudUrl { url, claimants })
    .collect()
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
#[path = "notify_tests.rs"]
mod tests;
