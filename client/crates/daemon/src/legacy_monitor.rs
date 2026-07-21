//! Legacy-port monitoring, process attribution, and migration helpers.
//!
//! This module helps users migrate onto port-zero and catch a common class
//! of footguns: services that are still served *directly* on localhost ports
//! (bypassing the tunnel/overlay), and Docker containers that fail to start
//! because a published host port is already bound.
//!
//! Design goals (per ticket task-6):
//!  - **Reuse, don't duplicate.** System-wide listener enumeration lives in
//!    [`crate::discovery::enumerate_system_listeners`] (which reuses the existing
//!    per-OS port discovery). Findings flow into the task-5 [`Issue`] model so
//!    they show up in `portzero status` + native notifications via the same
//!    de-dup mechanism in `discovery_loop`.
//!  - **Pure, testable core.** All decision logic (legacy-vs-registered
//!    comparison, common-port matching, docker-conflict parsing/formatting,
//!    issue construction) is pure and unit-tested with in-memory inputs. The
//!    only impure surface is [`scan_legacy_listeners`], a thin wrapper that
//!    calls the (already-tested) enumeration helper and feeds it to the pure
//!    core.
//!  - **Best-effort cross-platform.** Inherits whatever the underlying
//!    enumeration supports; degrades to "no findings" rather than failing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::discovery::SystemListener;
use crate::notify::Issue;

/// Common developer service ports that, when served directly (not via
/// port-zero), are worth surfacing migration guidance for. Kept small and
/// opinionated: databases, caches, web/dev servers, message brokers.
pub const COMMON_PORTS: &[u16] = &[
    3000,  // node / next / rails dev
    3001,  // common secondary dev server
    4000,  // phoenix / misc dev
    5000,  // flask / misc dev
    5173,  // vite
    5432,  // postgres
    5672,  // rabbitmq
    6379,  // redis
    8000,  // django / misc
    8025,  // mailhog
    8080,  // http alt / many dev servers
    8081,  // http alt
    8443,  // https alt
    9000,  // php-fpm / misc
    9090,  // prometheus / misc
    9200,  // elasticsearch
    27017, // mongodb
];

/// The set of ports the daemon already manages (route table ports + overlay
/// service ports) plus the cwds of those managed contexts. A listener that is
/// already managed must never be flagged as legacy.
#[derive(Debug, Clone, Default)]
pub struct ManagedContext {
    /// Ports backing a registered route or overlay service.
    pub ports: BTreeSet<u16>,
    /// Working directories of registered/managed services. Used to suppress
    /// flagging the *same worktree* that already runs a managed service.
    pub cwds: BTreeSet<PathBuf>,
}

impl ManagedContext {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Decide whether a given listener's port is "interesting" — i.e. either a
/// well-known common dev port, or a port that a known overlay/route service is
/// using (so a *second*, non-tunnel listener nearby is suspicious).
pub fn port_is_interesting(port: u16, managed: &ManagedContext, common: &[u16]) -> bool {
    common.contains(&port) || managed.ports.contains(&port)
}

/// Best-effort human-readable context for a listener, combining its working
/// directory and (if discoverable) the git repository root it lives under.
///
/// Pure with respect to its inputs: the caller supplies the optional git-root
/// path so this can be unit-tested without touching the filesystem.
pub fn describe_listener_context(cwd: Option<&Path>, git_root: Option<&Path>) -> String {
    let home = dirs::home_dir();
    let shorten = |p: &Path| -> String {
        if let Some(ref h) = home {
            if let Ok(rel) = p.strip_prefix(h) {
                return format!("~/{}", rel.display());
            }
        }
        p.display().to_string()
    };

    match (cwd, git_root) {
        (Some(dir), Some(root)) => {
            let repo = root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| shorten(root));
            format!("({} in git repo {})", shorten(dir), repo)
        }
        (Some(dir), None) => format!("({})", shorten(dir)),
        (None, _) => "(unknown dir)".to_string(),
    }
}

/// Pure core: given the system-wide listeners, the set of managed ports/cwds,
/// and the list of common ports, produce one [`Issue::LegacyListener`] per
/// distinct legacy listener.
///
/// A listener is "legacy" when ALL of the following hold:
///  - it does NOT have `PZ_TUNNEL` set (it isn't already managed by us),
///  - its port is interesting (common dev port, or a port a managed service
///    uses),
///  - its port is not itself a managed/registered port owned by us (we don't
///    want to flag our own forwarders or already-tunneled services),
///  - its owning cwd is not a managed cwd (don't nag about the worktree that
///    already runs a tunneled service).
///
/// `git_root_for` lets the caller resolve a git root for a cwd (injected so the
/// function stays pure and testable). It may always return `None`.
pub fn detect_legacy_listeners(
    listeners: &[SystemListener],
    managed: &ManagedContext,
    common: &[u16],
    git_root_for: impl Fn(&Path) -> Option<PathBuf>,
) -> Vec<Issue> {
    // Deduplicate on (port, pid): a single process may expose the same port on
    // both IPv4 and IPv6, and we don't want two identical issues.
    let mut seen: BTreeSet<(u16, u32)> = BTreeSet::new();
    let mut issues = Vec::new();

    for l in listeners {
        // Already managed by port-zero — never legacy.
        if l.has_port_zero {
            continue;
        }
        // Port must be interesting.
        if !port_is_interesting(l.port, managed, common) {
            continue;
        }
        // Don't flag a process whose cwd already runs a managed service.
        if let Some(ref cwd) = l.cwd {
            if managed.cwds.contains(cwd) {
                continue;
            }
        }
        if !seen.insert((l.port, l.pid)) {
            continue;
        }

        let git_root = l.cwd.as_deref().and_then(&git_root_for);
        let context = describe_listener_context(l.cwd.as_deref(), git_root.as_deref());
        issues.push(Issue::LegacyListener {
            port: l.port,
            pid: l.pid,
            context,
        });
    }

    // Stable ordering (by port then pid) for byte-identical persisted JSON
    // across scans, matching the de-dup contract in `notify`.
    issues.sort_by_key(issue_sort_key);
    issues
}

/// Sort key for legacy-listener issues so output is deterministic.
fn issue_sort_key(issue: &Issue) -> (u16, u32) {
    match issue {
        Issue::LegacyListener { port, pid, .. } => (*port, *pid),
        _ => (u16::MAX, u32::MAX),
    }
}

/// Parse a Docker CLI/daemon error message and, if it indicates a host
/// port-bind conflict, return the offending host port.
///
/// Handles the common Docker phrasings, e.g.:
///   "Bind for 0.0.0.0:8080 failed: port is already allocated"
///   "ports are not available: ... listen tcp 0.0.0.0:5432: bind: address already in use"
///   "driver failed programming external connectivity ... bind for 127.0.0.1:6379 failed"
///
/// Pure: operates only on the supplied string.
pub fn parse_docker_port_conflict(stderr: &str) -> Option<u16> {
    let lower = stderr.to_ascii_lowercase();
    let is_conflict = lower.contains("already allocated")
        || lower.contains("address already in use")
        || lower.contains("port is already")
        || (lower.contains("bind for") && lower.contains("failed"));
    if !is_conflict {
        return None;
    }
    extract_port_after_colon(&lower)
}

/// Extract a TCP port from an `addr:port` fragment within a Docker error.
/// Scans for the last `:<digits>` group that parses as a u16, since Docker
/// embeds the address before the port (e.g. `0.0.0.0:8080`).
fn extract_port_after_colon(s: &str) -> Option<u16> {
    let mut best: Option<u16> = None;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start {
                if let Ok(port) = s[start..end].parse::<u16>() {
                    if port > 0 {
                        best = Some(port);
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    best
}

/// Build a [`Issue::DockerPortConflict`] from a container name and the host port
/// that could not be bound. Pure constructor (kept here so callers/tests share
/// one definition of how the issue is shaped).
pub fn docker_conflict_issue(container: &str, port: u16) -> Issue {
    Issue::DockerPortConflict {
        port,
        container: container.to_string(),
    }
}

/// Scan the system for legacy listeners and return them as issues.
///
/// Thin impure wrapper: it calls the (separately tested) enumeration helper and
/// the pure [`detect_legacy_listeners`], resolving git roots via the real
/// `portzero_domain::find_git_root`. Never panics; on platforms without
/// enumeration support it yields an empty list.
pub fn scan_legacy_listeners(managed: &ManagedContext) -> Vec<Issue> {
    #[cfg(target_os = "windows")]
    {
        let _ = managed;
        Vec::new()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let listeners = crate::discovery::enumerate_system_listeners();
        detect_legacy_listeners(&listeners, managed, COMMON_PORTS, |cwd| {
            portzero_domain::find_git_root(cwd).map(|(_, root)| root)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(port: u16, pid: u32, cwd: Option<&str>, tunneled: bool) -> SystemListener {
        SystemListener {
            port,
            pid,
            cwd: cwd.map(PathBuf::from),
            has_port_zero: tunneled,
        }
    }

    fn no_git(_: &Path) -> Option<PathBuf> {
        None
    }

    #[test]
    fn test_port_is_interesting_common() {
        let managed = ManagedContext::new();
        assert!(port_is_interesting(5432, &managed, COMMON_PORTS));
        assert!(!port_is_interesting(12345, &managed, COMMON_PORTS));
    }

    #[test]
    fn test_port_is_interesting_managed_port() {
        let mut managed = ManagedContext::new();
        managed.ports.insert(45123);
        // 45123 isn't a "common" port but a managed service uses it, so a
        // *second* non-tunnel listener on it is suspicious.
        assert!(port_is_interesting(45123, &managed, COMMON_PORTS));
    }

    #[test]
    fn test_tunneled_listener_is_not_legacy() {
        let listeners = vec![listener(5432, 100, Some("/work/db"), true)];
        let issues =
            detect_legacy_listeners(&listeners, &ManagedContext::new(), COMMON_PORTS, no_git);
        assert!(issues.is_empty(), "managed listeners must not be flagged");
    }

    #[test]
    fn test_uninteresting_port_is_not_legacy() {
        let listeners = vec![listener(54999, 100, Some("/work/db"), false)];
        let issues =
            detect_legacy_listeners(&listeners, &ManagedContext::new(), COMMON_PORTS, no_git);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_common_port_direct_listener_is_legacy() {
        let listeners = vec![listener(5432, 4321, Some("/work/api"), false)];
        let issues =
            detect_legacy_listeners(&listeners, &ManagedContext::new(), COMMON_PORTS, no_git);
        assert_eq!(issues.len(), 1);
        match &issues[0] {
            Issue::LegacyListener { port, pid, context } => {
                assert_eq!(*port, 5432);
                assert_eq!(*pid, 4321);
                assert!(context.contains("/work/api"));
            }
            other => panic!("unexpected issue: {:?}", other),
        }
    }

    #[test]
    fn test_managed_cwd_is_suppressed() {
        // The worktree at /work/api already runs a managed service; a legacy
        // listener in that same cwd should not be nagged about.
        let mut managed = ManagedContext::new();
        managed.cwds.insert(PathBuf::from("/work/api"));
        let listeners = vec![listener(5432, 4321, Some("/work/api"), false)];
        let issues = detect_legacy_listeners(&listeners, &managed, COMMON_PORTS, no_git);
        assert!(issues.is_empty());
    }

    #[test]
    fn test_dedup_ipv4_ipv6_same_port_pid() {
        let listeners = vec![
            listener(8080, 7, Some("/work/web"), false),
            listener(8080, 7, Some("/work/web"), false),
        ];
        let issues =
            detect_legacy_listeners(&listeners, &ManagedContext::new(), COMMON_PORTS, no_git);
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn test_detection_stable_ordering() {
        let listeners = vec![
            listener(8080, 2, Some("/b"), false),
            listener(3000, 9, Some("/a"), false),
            listener(8080, 1, Some("/c"), false),
        ];
        let issues =
            detect_legacy_listeners(&listeners, &ManagedContext::new(), COMMON_PORTS, no_git);
        let keys: Vec<(u16, u32)> = issues.iter().map(issue_sort_key).collect();
        assert_eq!(keys, vec![(3000, 9), (8080, 1), (8080, 2)]);
    }

    #[test]
    fn test_describe_context_with_git_root() {
        let ctx = describe_listener_context(
            Some(Path::new("/work/myrepo/sub")),
            Some(Path::new("/work/myrepo")),
        );
        assert!(ctx.contains("/work/myrepo/sub"));
        assert!(ctx.contains("git repo myrepo"));
    }

    #[test]
    fn test_describe_context_without_git_root() {
        let ctx = describe_listener_context(Some(Path::new("/tmp/x")), None);
        assert_eq!(ctx, "(/tmp/x)");
    }

    #[test]
    fn test_describe_context_unknown() {
        assert_eq!(describe_listener_context(None, None), "(unknown dir)");
    }

    #[test]
    fn test_parse_docker_conflict_port_allocated() {
        let err = "Bind for 0.0.0.0:8080 failed: port is already allocated";
        assert_eq!(parse_docker_port_conflict(err), Some(8080));
    }

    #[test]
    fn test_parse_docker_conflict_address_in_use() {
        let err = "Error: ports are not available: exposing port TCP 0.0.0.0:5432 \
                   -> 0.0.0.0:5432: listen tcp 0.0.0.0:5432: bind: address already in use";
        assert_eq!(parse_docker_port_conflict(err), Some(5432));
    }

    #[test]
    fn test_parse_docker_conflict_loopback() {
        let err = "driver failed programming external connectivity on endpoint \
                   web: Bind for 127.0.0.1:6379 failed: port is already allocated";
        assert_eq!(parse_docker_port_conflict(err), Some(6379));
    }

    #[test]
    fn test_parse_docker_conflict_unrelated_error() {
        assert_eq!(
            parse_docker_port_conflict("No such image: foo:latest"),
            None
        );
    }

    #[test]
    fn test_docker_conflict_issue_summary_and_hint() {
        let issue = docker_conflict_issue("web-1", 8080);
        assert!(issue.summary().contains("web-1"));
        assert!(issue.summary().contains("8080"));
        assert!(issue.fix_hint().contains("8080"));
    }
}
