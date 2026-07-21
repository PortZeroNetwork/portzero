//! Observed runtime truth: who-talks-to-whom edges and exercised HTTP routes.
//!
//! The userspace proxy already carries every request addressed to a tunnel
//! name, so it can record, with no extra instrumentation:
//!
//! - **Observed edges** — when a request to tunnel `B` carries a `Referer` /
//!   `Origin` whose host is another tunnel `A`, that is an `A → B` dependency
//!   edge (protocol, request count, last seen).
//! - **Exercised routes** — the `(method, path)` pairs actually hit on each
//!   tunnel, with counts and any `X-PZ-Test` attributions. This is a ready-made
//!   smoke-test inventory when graduating a service to a PaaS.
//!
//! **Observability caveat:** only traffic addressed *via tunnel names* is
//! observed. Container-to-container traffic over compose-internal DNS (e.g.
//! `http://db:5432` between services on the same compose network) never reaches
//! the daemon and is therefore invisible here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A `from → to` dependency edge observed between two tunnels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedEdge {
    /// The calling tunnel domain (from `Referer`/`Origin`), if identifiable.
    #[serde(default)]
    pub from: Option<String>,
    /// The tunnel domain that was addressed.
    pub to: String,
    /// Application protocol (currently always `"http"`).
    pub protocol: String,
    /// Number of requests observed for this edge.
    pub request_count: u64,
    /// When this edge was last observed.
    pub last_seen: DateTime<Utc>,
}

/// A `(method, path)` route exercised on a tunnel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExercisedRoute {
    /// The tunnel domain the route was hit on.
    pub domain: String,
    /// HTTP method (uppercased).
    pub method: String,
    /// Request path (query string stripped).
    pub path: String,
    /// Number of times this route was exercised.
    pub count: u64,
    /// Distinct `X-PZ-Test` values seen for this route (per-test attribution).
    #[serde(default)]
    pub tests: Vec<String>,
    /// When this route was last exercised.
    pub last_seen: DateTime<Utc>,
}

/// The persisted snapshot of observed runtime truth (`observations.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Observations {
    pub edges: Vec<ObservedEdge>,
    pub routes: Vec<ExercisedRoute>,
}

impl Observations {
    /// Load observations from disk, returning an empty snapshot on any error
    /// (missing file, parse failure) — this is best-effort telemetry.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

/// Strip an optional trailing `:port` and any scheme off a host value pulled
/// from a `Referer` / `Origin` header, returning the bare host.
fn host_of(referer_or_origin: &str) -> Option<String> {
    let v = referer_or_origin.trim();
    if v.is_empty() {
        return None;
    }
    // Drop scheme.
    let after_scheme = v.split_once("://").map(|(_, rest)| rest).unwrap_or(v);
    // Host ends at the first '/', '?', or '#'.
    let host_port = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Drop an optional :port (but keep IPv6-less hostnames intact).
    let host = host_port.rsplit_once(':').map_or(host_port, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host_port
        }
    });
    let host = host.trim().to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Is `host` a tunnel name (cloud or `.portzero.local` overlay)?
fn looks_like_tunnel(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h.ends_with(".portzero.local")
        || h.ends_with(".local")
        || h.ends_with("tunnel.portzero.cloud")
        || h.ends_with(".portzero.cloud")
}

struct Inner {
    edges: BTreeMap<(Option<String>, String), ObservedEdge>,
    routes: BTreeMap<(String, String, String), ExercisedRoute>,
    last_flush: Option<Instant>,
}

/// In-memory recorder for observed edges and exercised routes, persisted to
/// `observations.json`. Cheap to clone-share via `Arc`.
pub struct ObservationStore {
    inner: Mutex<Inner>,
    path: PathBuf,
}

/// Do not write `observations.json` more than this often, so a burst of
/// requests does not thrash the disk.
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

impl ObservationStore {
    /// Create a store that persists to `<state_dir>/observations.json`, seeded
    /// from any snapshot already on disk.
    pub fn new(state_dir: &Path) -> Self {
        let path = state_dir.join("observations.json");
        let existing = Observations::load(&path);

        let mut edges = BTreeMap::new();
        for e in existing.edges {
            edges.insert((e.from.clone(), e.to.clone()), e);
        }
        let mut routes = BTreeMap::new();
        for r in existing.routes {
            routes.insert((r.domain.clone(), r.method.clone(), r.path.clone()), r);
        }

        Self {
            inner: Mutex::new(Inner {
                edges,
                routes,
                last_flush: None,
            }),
            path,
        }
    }

    /// Record one HTTP request addressed to tunnel `to_domain`.
    ///
    /// `referer` is the raw `Referer`/`Origin` header (if any) used to infer a
    /// dependency edge; `x_pz_test` is the raw `X-PZ-Test` header (if any) used
    /// to attribute the route to a test.
    pub fn record_http(
        &self,
        to_domain: &str,
        method: &str,
        path: &str,
        referer: Option<&str>,
        x_pz_test: Option<&str>,
    ) {
        let to_domain = to_domain.trim().to_ascii_lowercase();
        if to_domain.is_empty() {
            return;
        }
        let method = method.trim().to_ascii_uppercase();
        // Route key is the path without a query string.
        let path_only = path.split(['?', '#']).next().unwrap_or(path).to_string();
        let now = Utc::now();

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        // Exercised route.
        {
            let key = (to_domain.clone(), method.clone(), path_only.clone());
            let entry = guard.routes.entry(key).or_insert_with(|| ExercisedRoute {
                domain: to_domain.clone(),
                method: method.clone(),
                path: path_only.clone(),
                count: 0,
                tests: Vec::new(),
                last_seen: now,
            });
            entry.count += 1;
            entry.last_seen = now;
            if let Some(test) = x_pz_test.map(str::trim).filter(|t| !t.is_empty()) {
                if !entry.tests.iter().any(|t| t == test) {
                    entry.tests.push(test.to_string());
                }
            }
        }

        // Dependency edge, when the caller is itself a tunnel.
        let from = referer
            .and_then(host_of)
            .filter(|h| looks_like_tunnel(h))
            .filter(|h| *h != to_domain);
        if from.is_some() {
            let key = (from.clone(), to_domain.clone());
            let entry = guard.edges.entry(key).or_insert_with(|| ObservedEdge {
                from: from.clone(),
                to: to_domain.clone(),
                protocol: "http".to_string(),
                request_count: 0,
                last_seen: now,
            });
            entry.request_count += 1;
            entry.last_seen = now;
        }

        // Throttled persistence.
        let should_flush = guard
            .last_flush
            .map(|t| t.elapsed() >= FLUSH_INTERVAL)
            .unwrap_or(true);
        if should_flush {
            guard.last_flush = Some(Instant::now());
            let snapshot = snapshot(&guard);
            let path = self.path.clone();
            drop(guard);
            persist(&path, &snapshot);
        }
    }

    /// Current in-memory snapshot (also what `observations.json` holds after a
    /// flush).
    pub fn snapshot(&self) -> Observations {
        match self.inner.lock() {
            Ok(g) => snapshot(&g),
            Err(_) => Observations::default(),
        }
    }

    /// Force a write of the current snapshot to disk.
    pub fn flush(&self) {
        let snapshot = self.snapshot();
        persist(&self.path, &snapshot);
    }
}

fn snapshot(inner: &Inner) -> Observations {
    Observations {
        edges: inner.edges.values().cloned().collect(),
        routes: inner.routes.values().cloned().collect(),
    }
}

fn persist(path: &Path, obs: &Observations) {
    if let Ok(json) = serde_json::to_string_pretty(obs) {
        if let Err(e) = std::fs::write(path, json) {
            tracing::debug!("failed to persist observations: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_of_strips_scheme_path_and_port() {
        assert_eq!(
            host_of("http://web.myapp.portzero.local/some/page?x=1"),
            Some("web.myapp.portzero.local".to_string())
        );
        assert_eq!(
            host_of("https://api.alice.tunnel.portzero.cloud:443/"),
            Some("api.alice.tunnel.portzero.cloud".to_string())
        );
        assert_eq!(host_of(""), None);
        assert_eq!(host_of("   "), None);
    }

    #[test]
    fn test_looks_like_tunnel() {
        assert!(looks_like_tunnel("web.portzero.local"));
        assert!(looks_like_tunnel("api.alice.tunnel.portzero.cloud"));
        assert!(!looks_like_tunnel("example.com"));
    }

    #[test]
    fn test_record_route_and_edge() {
        let dir = std::env::temp_dir().join(format!("pz-obs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = ObservationStore::new(&dir);

        store.record_http(
            "api.alice.tunnel.portzero.cloud",
            "get",
            "/users?page=2",
            Some("http://web.alice.tunnel.portzero.cloud/dashboard"),
            Some("login flow"),
        );
        store.record_http(
            "api.alice.tunnel.portzero.cloud",
            "GET",
            "/users",
            None,
            None,
        );

        let snap = store.snapshot();
        // Both requests collapse onto one (GET, /users) route with count 2.
        assert_eq!(snap.routes.len(), 1);
        let route = &snap.routes[0];
        assert_eq!(route.method, "GET");
        assert_eq!(route.path, "/users");
        assert_eq!(route.count, 2);
        assert_eq!(route.tests, vec!["login flow".to_string()]);

        // The referer produced one edge web → api.
        assert_eq!(snap.edges.len(), 1);
        let edge = &snap.edges[0];
        assert_eq!(
            edge.from.as_deref(),
            Some("web.alice.tunnel.portzero.cloud")
        );
        assert_eq!(edge.to, "api.alice.tunnel.portzero.cloud");
        assert_eq!(edge.request_count, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_no_edge_when_referer_not_a_tunnel() {
        let dir = std::env::temp_dir().join(format!("pz-obs2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = ObservationStore::new(&dir);
        store.record_http(
            "api.alice.tunnel.portzero.cloud",
            "GET",
            "/",
            Some("https://google.com/"),
            None,
        );
        assert_eq!(store.snapshot().edges.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
