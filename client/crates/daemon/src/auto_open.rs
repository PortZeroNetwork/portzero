//! Auto-open newly-detected HTTP/HTTPS local tunnels in the browser.
//!
//! When the discovery loop finds a `.portzero.local` backend on port 80 or 443,
//! and the `auto_open_http_tunnels` setting is on, we pop the tunnel's URL in the
//! default browser once — the moment a freshly-started app (e.g. an example)
//! becomes reachable. This is what makes "run an example and watch a tab appear"
//! work without the user typing the URL.
//!
//! Tracking is keyed by domain, but the *identity* of a tunnel is its owning
//! process or container: we remember the identity we opened each domain for. A
//! domain still served by the same identity opens at most once, no matter how
//! the scan flickers. A domain reclaimed by a *different* identity is a genuine
//! restart (the old app stopped, a new one took the name) and opens again. The
//! identity check is what stops a transient scan gap — a single empty or slow
//! scan — from masquerading as a teardown-and-restart and reopening the
//! browser.
//!
//! The tracker is persisted to disk (see [`AutoOpenTracker::load`] /
//! [`AutoOpenTracker::save`]) and reloaded at daemon startup, so tunnels that
//! were already running before a daemon restart are recognized as already
//! opened rather than treated as freshly appeared. Only a tunnel whose process
//! or container genuinely starts after the daemon comes back up is opened.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::discovery::{DiscoveredNetworkService, ServiceSource};

/// Number of consecutive scans an absent domain is remembered before we drop it
/// to reclaim memory.
///
/// This is only a cleanup fallback: reopen decisions are driven by the PID (a
/// live process keeps its PID, a restarted one gets a new one), so dropping a
/// long-absent entry is safe — if that exact PID somehow reappears we would
/// merely open once more, and in practice a process gone this long is dead. The
/// window (roughly this many × `scan_interval_secs`, ~30s at the 2s default) is
/// deliberately generous so it never fires during an ordinary scan blip.
const MISSES_BEFORE_FORGET: u32 = 15;

/// What we remember about a web tunnel we have already handled.
#[derive(Clone, Serialize, Deserialize)]
struct OpenedTunnel {
    /// Identity of the process or container we opened this domain for. A
    /// different identity on the same domain means something else took it
    /// over — a genuine restart — so we open the browser again.
    identity: String,
    /// Consecutive scans the domain has been absent (0 while present). Used only
    /// to age the entry out of memory once it has been gone a long time.
    #[serde(default)]
    misses: u32,
}

/// The scheme+URL a browser should open for a `.portzero.local` service, or
/// `None` when the service is not an HTTP/HTTPS web port.
fn web_url(name: &str, service_port: u16) -> Option<String> {
    match service_port {
        80 => Some(format!("http://{name}.portzero.local")),
        443 => Some(format!("https://{name}.portzero.local")),
        _ => None,
    }
}

/// A stable identity for the process or container behind a discovered
/// service. PID alone is not enough: Docker containers are always reported
/// with `pid == 0` (see [`DiscoveredNetworkService::pid`]), so two different
/// containers on the same domain would otherwise look identical. Containers
/// use their (stable) container ID instead; plain processes use their PID.
fn service_identity(svc: &DiscoveredNetworkService) -> String {
    match &svc.source {
        ServiceSource::Container { id, .. } => format!("container:{id}"),
        ServiceSource::Process { .. } => format!("pid:{}", svc.pid),
    }
}

/// Remembers which web tunnels (domain + owning process/container) have
/// already been opened, so a steady-state scan does not reopen them every
/// cycle, and so a daemon restart does not reopen tunnels that were already
/// running before the daemon came back up.
#[derive(Default, Serialize, Deserialize)]
pub struct AutoOpenTracker {
    opened: HashMap<String, OpenedTunnel>,
}

impl AutoOpenTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the tracker from disk, defaulting to empty if the file does not
    /// exist yet (e.g. first-ever daemon start) or fails to parse.
    ///
    /// Loading prior state (rather than always starting empty) is what makes
    /// "only open on first appearance" hold across a daemon restart: tunnels
    /// that were already running keep the identity we previously recorded, so
    /// the next reconcile sees them as already-opened instead of new.
    pub fn load(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(path).and_then(|content| {
            serde_json::from_str::<Self>(&content).map_err(std::io::Error::other)
        }) {
            Ok(tracker) => tracker,
            Err(err) => {
                tracing::debug!(%err, path = %path.display(), "failed to load auto-open state, starting fresh");
                Self::default()
            }
        }
    }

    /// Persist the tracker to disk so a subsequent daemon restart can reload
    /// it (see [`Self::load`]).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("Failed to serialize auto-open state")?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write auto-open state to: {}", path.display()))?;
        Ok(())
    }

    /// Reconcile the currently-detected services against what we have opened.
    ///
    /// A web tunnel is opened (when `enabled`) the first time we see its domain,
    /// or when the domain reappears under a *different* identity (a real
    /// restart of the process/container). Either way it is recorded as seen —
    /// so toggling the setting on later does not blast every pre-existing
    /// tunnel. A domain still served by the same identity only has its absence
    /// counter reset. Domains absent this scan are aged out and dropped after
    /// `MISSES_BEFORE_FORGET` consecutive misses.
    pub fn reconcile(&mut self, enabled: bool, services: &[DiscoveredNetworkService]) {
        let mut present: HashSet<String> = HashSet::new();
        for svc in services {
            let Some(url) = web_url(&svc.name, svc.service_port) else {
                continue;
            };
            present.insert(url.clone());

            let identity = service_identity(svc);
            // New domain, or the same domain now owned by a different
            // process/container (a restart), is treated as freshly appeared.
            // The same identity reappearing — even after the domain flickered
            // out of a scan, or after a daemon restart reloaded this state
            // from disk — is the same tunnel and must not reopen.
            let is_new = self.opened.get(&url).map(|e| e.identity.as_str()) != Some(identity.as_str());
            // Overwriting resets the absence counter and records the current
            // identity.
            self.opened.insert(
                url.clone(),
                OpenedTunnel {
                    identity: identity.clone(),
                    misses: 0,
                },
            );
            if is_new && enabled {
                tracing::info!(%url, %identity, "auto-opening newly detected web tunnel");
                if !crate::browser::open_url(&url) {
                    tracing::debug!(%url, "no browser opener available for auto-open");
                }
            }
        }
        // Age out domains absent this scan; drop one only after it has been gone
        // long enough to reclaim memory. Reopen correctness comes from the
        // identity check above, not from this window.
        self.opened.retain(|url, entry| {
            if present.contains(url) {
                return true;
            }
            entry.misses += 1;
            entry.misses < MISSES_BEFORE_FORGET
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::discovery::ServiceSource;

    fn svc(name: &str, service_port: u16, pid: u32) -> DiscoveredNetworkService {
        DiscoveredNetworkService {
            name: name.to_owned(),
            real_addr: "127.0.0.1:0".parse().unwrap(),
            service_port,
            backend_protocol: None,
            pid,
            source: ServiceSource::Process { cwd: None },
            domain_template: format!("{name}.portzero.local"),
            substitutions: BTreeMap::new(),
            health_path: None,
        }
    }

    #[test]
    fn web_url_only_matches_web_ports() {
        assert_eq!(
            web_url("api", 80).as_deref(),
            Some("http://api.portzero.local")
        );
        assert_eq!(
            web_url("api", 443).as_deref(),
            Some("https://api.portzero.local")
        );
        assert_eq!(web_url("db", 5432), None);
    }

    #[test]
    fn a_present_tunnel_is_only_recorded_once() {
        let mut tracker = AutoOpenTracker::new();
        let services = [svc("api", 80, 100)];
        tracker.reconcile(false, &services);
        tracker.reconcile(false, &services);
        assert_eq!(
            tracker
                .opened
                .get("http://api.portzero.local")
                .map(|e| e.identity.as_str()),
            Some("pid:100")
        );
    }

    #[test]
    fn a_transient_missing_scan_does_not_reopen_the_same_process() {
        let mut tracker = AutoOpenTracker::new();
        let services = [svc("api", 80, 100)];
        tracker.reconcile(false, &services);

        // Several empty scans — e.g. overlay-refresh timeouts — must not forget
        // the tunnel within the grace window...
        for _ in 0..(MISSES_BEFORE_FORGET - 1) {
            tracker.reconcile(false, &[]);
            assert!(tracker.opened.contains_key("http://api.portzero.local"));
        }
        // ...and when the *same* PID returns, the counter resets and it is still
        // considered already-opened (no reopen).
        tracker.reconcile(false, &services);
        assert_eq!(
            tracker.opened.get("http://api.portzero.local").map(|e| e.misses),
            Some(0)
        );
    }

    #[test]
    fn same_domain_new_pid_is_treated_as_a_restart() {
        let mut tracker = AutoOpenTracker::new();
        tracker.reconcile(false, &[svc("api", 80, 100)]);

        // A different PID on the same domain is a genuine restart: the entry now
        // tracks the new PID (and would have reopened the browser).
        tracker.reconcile(false, &[svc("api", 80, 200)]);
        assert_eq!(
            tracker
                .opened
                .get("http://api.portzero.local")
                .map(|e| e.identity.as_str()),
            Some("pid:200")
        );
    }

    #[test]
    fn a_domain_gone_past_the_grace_window_is_forgotten() {
        let mut tracker = AutoOpenTracker::new();
        tracker.reconcile(false, &[svc("api", 80, 100)]);
        for _ in 0..MISSES_BEFORE_FORGET {
            tracker.reconcile(false, &[]);
        }
        assert!(!tracker.opened.contains_key("http://api.portzero.local"));
    }

    #[test]
    fn containers_are_identified_by_container_id_not_pid_zero() {
        // Docker-sourced services always report pid == 0, so two distinct
        // containers on the same domain must not look identical.
        let mut tracker = AutoOpenTracker::new();
        let mut first = svc("api", 80, 0);
        first.source = ServiceSource::Container {
            id: "abc123".to_owned(),
            name: "api-container".to_owned(),
        };
        tracker.reconcile(false, std::slice::from_ref(&first));
        assert_eq!(
            tracker
                .opened
                .get("http://api.portzero.local")
                .map(|e| e.identity.as_str()),
            Some("container:abc123")
        );

        let mut second = first.clone();
        second.source = ServiceSource::Container {
            id: "def456".to_owned(),
            name: "api-container".to_owned(),
        };
        // A genuinely different container reclaiming the domain is a restart.
        let mut present = HashSet::new();
        let url = web_url(&second.name, second.service_port).unwrap();
        present.insert(url.clone());
        let is_new = tracker.opened.get(&url).map(|e| e.identity.as_str())
            != Some(service_identity(&second).as_str());
        assert!(is_new);
    }

    #[test]
    fn restarting_the_tracker_from_saved_state_does_not_reopen_existing_tunnels() {
        let dir = std::env::temp_dir().join(format!(
            "portzero-auto-open-test-{}",
            std::process::id()
        ));
        let path = dir.join("auto_open.json");

        let mut tracker = AutoOpenTracker::new();
        let services = [svc("api", 80, 100)];
        tracker.reconcile(true, &services);
        tracker.save(&path).unwrap();

        // Simulate a daemon restart: a fresh tracker loads the saved state, and
        // the same still-running process is reconciled again. It must not be
        // treated as new even though this tracker instance never saw it before.
        let mut restarted = AutoOpenTracker::load(&path);
        let is_new = restarted.opened.get("http://api.portzero.local").map(|e| e.identity.as_str())
            != Some(service_identity(&services[0]).as_str());
        assert!(!is_new, "pre-existing tunnel should not look new after restart");
        restarted.reconcile(true, &services);
        assert_eq!(
            restarted
                .opened
                .get("http://api.portzero.local")
                .map(|e| e.identity.as_str()),
            Some("pid:100")
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
