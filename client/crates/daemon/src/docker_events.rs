//! Robust Docker event monitoring + host-port-bind conflict detection.
//!
//! The discovery loop POLLS `docker ps`/`docker inspect` every couple of seconds
//! to find containers with `PZ_TUNNEL` set (see [`crate::discovery`]). That
//! is fine for steady-state route tracking, but it cannot see a container that
//! *fails to start* because a published host port is already bound — by the time
//! the next poll runs, the container is already gone, and its error message
//! (which names the conflicting port) is lost.
//!
//! This module adds a long-lived `docker events` stream monitor that watches
//! container lifecycle transitions in real time. On a container `die`/failed
//! start it shells out to `docker inspect` for the exit code + error string and,
//! when that error indicates a host-port bind conflict, surfaces it as an
//! [`Issue::DockerPortConflict`] using the EXISTING pure helpers in
//! [`crate::legacy_monitor`] (`parse_docker_port_conflict` + `docker_conflict_issue`).
//!
//! Design goals (mirroring the rest of the daemon):
//!  - **Reuse, don't duplicate.** Conflict parsing/issue construction lives in
//!    `legacy_monitor`; this module only feeds real container-failure output into
//!    those helpers and into the existing issue-set/notification plumbing.
//!  - **Robust + non-fatal.** If `docker` is absent or the stream ends (Docker
//!    daemon restart/stop), we log and retry with capped exponential backoff
//!    rather than crashing the daemon. When Docker returns, monitoring resumes.
//!  - **Pure, testable core.** Event-line parsing, inspect-string parsing, the
//!    conflict→issue decision, and the backoff calc are all pure and unit-tested
//!    with in-memory strings — no test ever spawns `docker` or shells out.
//!  - **Best-effort cross-platform.** Same posture as Docker discovery: where
//!    `docker` is unavailable the monitor simply idles/retries.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, Notify};

use crate::notify::Issue;

/// `--format` template for `docker events`. Pipe-delimited so parsing is a
/// trivial split and never ambiguous with the JSON-ish attribute values. Fields:
/// `type|action|actor-id|actor-name`.
pub const EVENTS_FORMAT: &str =
    "{{.Type}}|{{.Action}}|{{.Actor.ID}}|{{index .Actor.Attributes \"name\"}}";

/// `--format` template for the post-`die` `docker inspect`. Pipe-delimited:
/// `exit-code|error`. The error is where Docker reports a port-bind failure.
pub const INSPECT_FORMAT: &str = "{{.State.ExitCode}}|{{.State.Error}}";

/// Backoff bounds for restarting the `docker events` stream when Docker is
/// absent or the stream ends. Mirrors the cloud reconnect backoff in
/// `discovery_loop`: start small, double, cap.
const EVENTS_BACKOFF_BASE_SECS: u64 = 1;
const EVENTS_BACKOFF_MAX_SECS: u64 = 60;

/// A container lifecycle transition we care about, parsed from one line of
/// `docker events` output formatted with [`EVENTS_FORMAT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEvent {
    /// The lifecycle action, lower-cased (e.g. "start", "die", "stop").
    pub action: ContainerAction,
    /// The container id (long sha).
    pub id: String,
    /// The container name, if present in the event attributes (may be empty).
    pub name: String,
}

/// The subset of container lifecycle actions the monitor reacts to. Anything
/// else (`create`, `attach`, exec events, network events, …) is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerAction {
    /// Container began running.
    Start,
    /// Container's main process exited (this is where a failed start surfaces).
    Die,
    /// Container was stopped.
    Stop,
    /// Container was removed.
    Destroy,
    /// A health check transitioned (e.g. `health_status: unhealthy`).
    HealthStatus,
}

impl ContainerAction {
    /// True for actions that should trigger an immediate discovery rescan
    /// (something appeared or disappeared), so the loop need not wait the full
    /// poll interval.
    pub fn warrants_rescan(self) -> bool {
        matches!(
            self,
            ContainerAction::Start | ContainerAction::Die | ContainerAction::Destroy
        )
    }

    /// True for actions after which a port-bind conflict could have occurred
    /// (a failed start manifests as an immediate `die`).
    pub fn warrants_conflict_check(self) -> bool {
        matches!(self, ContainerAction::Die)
    }
}

/// Parse one line of `docker events` output (formatted with [`EVENTS_FORMAT`])
/// into a [`ContainerEvent`]. Returns `None` for non-container events, blank
/// lines, unrecognised actions, or malformed lines. Pure.
pub fn parse_event_line(line: &str) -> Option<ContainerEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(4, '|');
    let typ = parts.next()?.trim();
    let action_raw = parts.next()?.trim();
    let id = parts.next()?.trim();
    // Name may be absent (older docker / events without the attribute).
    let name = parts.next().unwrap_or("").trim();

    // Only container-type events matter here.
    if !typ.eq_ignore_ascii_case("container") {
        return None;
    }
    if id.is_empty() {
        return None;
    }

    let action = parse_action(action_raw)?;
    Some(ContainerEvent {
        action,
        id: id.to_string(),
        name: name.to_string(),
    })
}

/// Map a `docker events` action string to a [`ContainerAction`], or `None` if it
/// is one we don't track. Docker emits health-check transitions as
/// `health_status: healthy` / `health_status: unhealthy`, so we match the prefix.
fn parse_action(action: &str) -> Option<ContainerAction> {
    let a = action.to_ascii_lowercase();
    if a.starts_with("health_status") {
        return Some(ContainerAction::HealthStatus);
    }
    match a.as_str() {
        "start" => Some(ContainerAction::Start),
        "die" => Some(ContainerAction::Die),
        "stop" => Some(ContainerAction::Stop),
        "destroy" => Some(ContainerAction::Destroy),
        _ => None,
    }
}

/// Parsed result of the post-`die` `docker inspect` formatted with
/// [`INSPECT_FORMAT`] (`exit-code|error`). Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectState {
    pub exit_code: i64,
    pub error: String,
}

/// Parse `docker inspect --format '{{.State.ExitCode}}|{{.State.Error}}'` output.
/// The error itself may contain `|` and newlines, so only the FIRST `|` is the
/// delimiter; everything after it is the error string. Returns `None` only when
/// the line is blank. Pure.
pub fn parse_inspect_state(out: &str) -> Option<InspectState> {
    let out = out.trim();
    if out.is_empty() {
        return None;
    }
    let (code_str, error) = match out.split_once('|') {
        Some((c, e)) => (c.trim(), e.trim()),
        None => (out, ""),
    };
    let exit_code = code_str.parse::<i64>().unwrap_or(0);
    Some(InspectState {
        exit_code,
        error: error.to_string(),
    })
}

/// Given a failed container's identity and its inspected error string, decide
/// whether it represents a host-port-bind conflict and, if so, build the issue.
///
/// Reuses the existing pure helpers in [`crate::legacy_monitor`] — this module
/// does not reimplement parsing. Pure: operates only on the supplied strings.
pub fn conflict_issue_from_inspect(container_label: &str, state: &InspectState) -> Option<Issue> {
    let port = crate::legacy_monitor::parse_docker_port_conflict(&state.error)?;
    Some(crate::legacy_monitor::docker_conflict_issue(
        container_label,
        port,
    ))
}

/// Compute the next backoff delay for restarting the events stream. Doubles each
/// attempt starting from [`EVENTS_BACKOFF_BASE_SECS`], capped at
/// [`EVENTS_BACKOFF_MAX_SECS`]. `attempt` is 0-based. Pure.
pub fn events_backoff(attempt: u32) -> Duration {
    let secs = EVENTS_BACKOFF_BASE_SECS
        .saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX))
        .min(EVENTS_BACKOFF_MAX_SECS);
    Duration::from_secs(secs)
}

/// Pick a human-friendly label for a conflicting container: prefer its name,
/// falling back to a short id. Pure.
pub fn container_label(event: &ContainerEvent) -> String {
    if !event.name.is_empty() {
        event.name.clone()
    } else {
        let short: String = event.id.chars().take(12).collect();
        short
    }
}

/// Shared, mutable set of Docker port-conflict issues detected by the event
/// monitor, keyed by container label so a repeated failure for the same
/// container replaces (rather than duplicates) its entry. The discovery loop
/// reads a snapshot of these each scan and merges them into the published issue
/// set; the monitor clears a container's entry once it starts successfully.
#[derive(Clone, Default)]
pub struct ConflictRegistry {
    inner: Arc<Mutex<BTreeMap<String, Issue>>>,
}

impl ConflictRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record (or replace) a conflict issue for a container.
    pub async fn record(&self, label: String, issue: Issue) {
        self.inner.lock().await.insert(label, issue);
    }

    /// Clear any recorded conflict for a container (e.g. it started OK now).
    pub async fn clear(&self, label: &str) {
        self.inner.lock().await.remove(label);
    }

    /// Snapshot the current conflict issues, in stable (sorted-by-key) order so
    /// the merged/published issue set stays byte-stable across scans.
    pub async fn snapshot(&self) -> Vec<Issue> {
        self.inner.lock().await.values().cloned().collect()
    }
}

/// Run the Docker event monitor until `shutdown` is notified.
///
/// Robust loop: (re)spawns `docker events` with [`EVENTS_FORMAT`], reads lines
/// async, and on each relevant event updates the [`ConflictRegistry`] and pings
/// `rescan` so the discovery loop can react immediately. If `docker` is missing
/// or the stream ends, it backs off (capped exponential) and retries — never
/// fatal. Returns when `shutdown` fires.
pub async fn run_event_monitor(
    conflicts: ConflictRegistry,
    rescan: Arc<Notify>,
    shutdown: Arc<Notify>,
) {
    let mut attempt: u32 = 0;
    loop {
        if !command_on_path("docker") {
            tracing::debug!("Docker event monitor: docker not on PATH; idle");
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                _ = shutdown.notified() => {
                    tracing::debug!("Docker event monitor: shutdown while docker is absent");
                    return;
                }
            }
            continue;
        }

        // Attempt to (re)start the stream. On success, reset backoff.
        match stream_events_once(&conflicts, &rescan, &shutdown).await {
            StreamOutcome::Shutdown => {
                tracing::debug!("Docker event monitor: shutdown requested");
                return;
            }
            StreamOutcome::Ended => {
                // Stream produced at least some output then ended (Docker stop /
                // daemon restart). Treat as a soft failure: small backoff.
                attempt = attempt.saturating_add(1);
            }
            StreamOutcome::SpawnFailed => {
                // `docker` not on PATH or not runnable. Back off and retry; this
                // is the normal state on machines without Docker.
                attempt = attempt.saturating_add(1);
            }
        }

        let delay = events_backoff(attempt);
        tracing::debug!(
            "Docker event monitor: will (re)try `docker events` in {}s",
            delay.as_secs()
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.notified() => {
                tracing::debug!("Docker event monitor: shutdown during backoff");
                return;
            }
        }
    }
}

/// Outcome of a single `docker events` stream attempt.
enum StreamOutcome {
    /// Shutdown was requested mid-stream.
    Shutdown,
    /// The stream ended on its own (Docker stopped / restarted).
    Ended,
    /// We could not even spawn `docker events`.
    SpawnFailed,
}

/// Spawn `docker events` once and pump its lines until it ends, fails, or
/// shutdown fires. Impure (spawns a child + shells out to `docker inspect`), so
/// it is intentionally NOT unit-tested; the parsing/decision logic it calls is
/// tested directly.
async fn stream_events_once(
    conflicts: &ConflictRegistry,
    rescan: &Arc<Notify>,
    shutdown: &Arc<Notify>,
) -> StreamOutcome {
    use tokio::process::Command;

    let mut command = Command::new("docker");
    command
        .args(["events", "--format", EVENTS_FORMAT])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    // This child is LONG-LIVED. On macOS, pipe fds created by concurrent
    // spawns elsewhere in the daemon (docker inspect, lsof, …) can leak into
    // it at fork time (no atomic CLOEXEC pipe on macOS), and a leaked pipe
    // write-end held by this immortal child means the sibling's
    // `Command::output()` NEVER sees EOF — wedging the discovery loop. Close
    // every fd above stdio in the child before exec so it cannot capture
    // foreign pipes.
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            for fd in 3..1024 {
                libc::close(fd);
            }
            Ok(())
        });
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                "Docker event monitor: could not spawn `docker events` ({})",
                e
            );
            return StreamOutcome::SpawnFailed;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return StreamOutcome::SpawnFailed,
    };
    tracing::info!("Docker event monitor: streaming container lifecycle events");

    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                let _ = child.start_kill();
                return StreamOutcome::Shutdown;
            }
            next = lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        if let Some(event) = parse_event_line(&line) {
                            handle_event(event, conflicts, rescan).await;
                        }
                    }
                    // EOF: docker daemon stopped or `docker events` exited.
                    Ok(None) => {
                        tracing::info!("Docker event monitor: event stream ended");
                        return StreamOutcome::Ended;
                    }
                    Err(e) => {
                        tracing::debug!("Docker event monitor: read error ({})", e);
                        return StreamOutcome::Ended;
                    }
                }
            }
        }
    }
}

/// React to a single parsed container event: ping rescan when warranted, run a
/// conflict check on `die`, and clear a stale conflict on a successful `start`.
async fn handle_event(event: ContainerEvent, conflicts: &ConflictRegistry, rescan: &Arc<Notify>) {
    let label = container_label(&event);

    if event.action.warrants_rescan() {
        rescan.notify_one();
    }

    match event.action {
        ContainerAction::Start => {
            // A previously failing container that now started clears its issue.
            conflicts.clear(&label).await;
        }
        ContainerAction::Die => {
            if let Some(state) = inspect_container_state(&event.id).await {
                if let Some(issue) = conflict_issue_from_inspect(&label, &state) {
                    tracing::warn!("{} — {}", issue.summary(), issue.fix_hint());
                    conflicts.record(label, issue).await;
                }
            }
        }
        _ => {}
    }
}

/// Shell out to `docker inspect <id>` for the exit code + error string. Impure;
/// best-effort (returns `None` on any failure). The pure parsing is
/// [`parse_inspect_state`].
async fn inspect_container_state(id: &str) -> Option<InspectState> {
    use tokio::process::Command;

    if !command_on_path("docker") {
        return None;
    }

    // Bounded: a docker child whose pipe write-end leaks into the long-lived
    // `docker events` stream never EOFs, and an unbounded .output() would
    // stall event processing forever (same wedge class as the discovery-loop
    // docker scans; see DOCKER_COMMAND_TIMEOUT in discovery/docker.rs).
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Command::new("docker")
            .args(["inspect", id, "--format", INSPECT_FORMAT])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_inspect_state(&stdout)
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let candidates: Vec<String> = {
        let pathext = std::env::var_os("PATHEXT")
            .and_then(|v| v.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let has_ext = std::path::Path::new(program).extension().is_some();
        if has_ext {
            vec![program.to_string()]
        } else {
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| format!("{program}{ext}"))
                .chain(std::iter::once(program.to_string()))
                .collect()
        }
    };

    #[cfg(not(windows))]
    let candidates: Vec<String> = vec![program.to_string()];

    std::env::split_paths(&paths).any(|dir| {
        candidates
            .iter()
            .any(|candidate| dir.join(candidate).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_start() {
        let ev = parse_event_line("container|start|abc123def456|web-1").unwrap();
        assert_eq!(ev.action, ContainerAction::Start);
        assert_eq!(ev.id, "abc123def456");
        assert_eq!(ev.name, "web-1");
    }

    #[test]
    fn test_parse_event_die() {
        let ev = parse_event_line("container|die|deadbeef|db").unwrap();
        assert_eq!(ev.action, ContainerAction::Die);
        assert_eq!(ev.name, "db");
    }

    #[test]
    fn test_parse_event_health_status() {
        let ev = parse_event_line("container|health_status: unhealthy|id1|svc").unwrap();
        assert_eq!(ev.action, ContainerAction::HealthStatus);
    }

    #[test]
    fn test_parse_event_missing_name() {
        // Older docker / events without a name attribute.
        let ev = parse_event_line("container|stop|id9").unwrap();
        assert_eq!(ev.action, ContainerAction::Stop);
        assert_eq!(ev.id, "id9");
        assert_eq!(ev.name, "");
    }

    #[test]
    fn test_parse_event_non_container_ignored() {
        assert!(parse_event_line("network|connect|netid|bridge").is_none());
        assert!(parse_event_line("image|pull|img|nginx").is_none());
    }

    #[test]
    fn test_parse_event_unknown_action_ignored() {
        assert!(parse_event_line("container|create|id|x").is_none());
        assert!(parse_event_line("container|attach|id|x").is_none());
    }

    #[test]
    fn test_parse_event_blank_and_malformed() {
        assert!(parse_event_line("").is_none());
        assert!(parse_event_line("   ").is_none());
        assert!(parse_event_line("container").is_none());
        assert!(parse_event_line("container|start").is_none());
        // Empty id.
        assert!(parse_event_line("container|start||name").is_none());
    }

    #[test]
    fn test_action_predicates() {
        assert!(ContainerAction::Start.warrants_rescan());
        assert!(ContainerAction::Die.warrants_rescan());
        assert!(ContainerAction::Destroy.warrants_rescan());
        assert!(!ContainerAction::Stop.warrants_rescan());
        assert!(!ContainerAction::HealthStatus.warrants_rescan());

        assert!(ContainerAction::Die.warrants_conflict_check());
        assert!(!ContainerAction::Start.warrants_conflict_check());
    }

    #[test]
    fn test_parse_inspect_state_with_error() {
        let s = parse_inspect_state("1|Bind for 0.0.0.0:8080 failed: port is already allocated")
            .unwrap();
        assert_eq!(s.exit_code, 1);
        assert!(s.error.contains("8080"));
    }

    #[test]
    fn test_parse_inspect_state_no_error() {
        let s = parse_inspect_state("0|").unwrap();
        assert_eq!(s.exit_code, 0);
        assert_eq!(s.error, "");
    }

    #[test]
    fn test_parse_inspect_state_error_contains_pipe() {
        // The error string itself can contain '|'; only the first one splits.
        let s = parse_inspect_state("125|driver failed | bind for 0.0.0.0:5432 failed").unwrap();
        assert_eq!(s.exit_code, 125);
        assert!(s.error.contains("5432"));
        assert!(s.error.starts_with("driver failed"));
    }

    #[test]
    fn test_parse_inspect_state_blank() {
        assert!(parse_inspect_state("").is_none());
        assert!(parse_inspect_state("   ").is_none());
    }

    #[test]
    fn test_parse_inspect_state_nonnumeric_code() {
        // Defensive: a non-numeric code degrades to 0 rather than failing.
        let s = parse_inspect_state("oops|some error").unwrap();
        assert_eq!(s.exit_code, 0);
        assert_eq!(s.error, "some error");
    }

    #[test]
    fn test_conflict_issue_from_inspect_detected() {
        let state = InspectState {
            exit_code: 1,
            error: "Bind for 0.0.0.0:5432 failed: port is already allocated".to_string(),
        };
        let issue = conflict_issue_from_inspect("db-1", &state).unwrap();
        match issue {
            Issue::DockerPortConflict { port, container } => {
                assert_eq!(port, 5432);
                assert_eq!(container, "db-1");
            }
            other => panic!("unexpected issue: {:?}", other),
        }
    }

    #[test]
    fn test_conflict_issue_from_inspect_unrelated() {
        let state = InspectState {
            exit_code: 1,
            error: "container exited with non-zero status".to_string(),
        };
        assert!(conflict_issue_from_inspect("db-1", &state).is_none());
    }

    #[test]
    fn test_conflict_issue_from_inspect_clean_exit() {
        let state = InspectState {
            exit_code: 0,
            error: String::new(),
        };
        assert!(conflict_issue_from_inspect("db-1", &state).is_none());
    }

    #[test]
    fn test_events_backoff_doubles_and_caps() {
        assert_eq!(events_backoff(0), Duration::from_secs(1));
        assert_eq!(events_backoff(1), Duration::from_secs(2));
        assert_eq!(events_backoff(2), Duration::from_secs(4));
        assert_eq!(events_backoff(3), Duration::from_secs(8));
        // Caps at the max regardless of how high the attempt climbs.
        assert_eq!(
            events_backoff(20),
            Duration::from_secs(EVENTS_BACKOFF_MAX_SECS)
        );
        assert_eq!(
            events_backoff(u32::MAX),
            Duration::from_secs(EVENTS_BACKOFF_MAX_SECS)
        );
    }

    #[test]
    fn test_container_label_prefers_name() {
        let ev = ContainerEvent {
            action: ContainerAction::Die,
            id: "abcdef0123456789".to_string(),
            name: "web-1".to_string(),
        };
        assert_eq!(container_label(&ev), "web-1");
    }

    #[test]
    fn test_container_label_falls_back_to_short_id() {
        let ev = ContainerEvent {
            action: ContainerAction::Die,
            id: "abcdef0123456789".to_string(),
            name: String::new(),
        };
        assert_eq!(container_label(&ev), "abcdef012345");
    }

    #[tokio::test]
    async fn test_conflict_registry_record_snapshot_clear() {
        let reg = ConflictRegistry::new();
        assert!(reg.snapshot().await.is_empty());

        let issue = crate::legacy_monitor::docker_conflict_issue("db", 5432);
        reg.record("db".to_string(), issue.clone()).await;
        let snap = reg.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0], issue);

        // Recording the same container replaces (no duplicate).
        reg.record("db".to_string(), issue.clone()).await;
        assert_eq!(reg.snapshot().await.len(), 1);

        reg.clear("db").await;
        assert!(reg.snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn test_conflict_registry_snapshot_is_sorted() {
        let reg = ConflictRegistry::new();
        reg.record(
            "zeta".to_string(),
            crate::legacy_monitor::docker_conflict_issue("zeta", 1),
        )
        .await;
        reg.record(
            "alpha".to_string(),
            crate::legacy_monitor::docker_conflict_issue("alpha", 2),
        )
        .await;
        let snap = reg.snapshot().await;
        // Keyed by label, sorted ascending → alpha before zeta.
        match (&snap[0], &snap[1]) {
            (
                Issue::DockerPortConflict { container: c0, .. },
                Issue::DockerPortConflict { container: c1, .. },
            ) => {
                assert_eq!(c0, "alpha");
                assert_eq!(c1, "zeta");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }
}
