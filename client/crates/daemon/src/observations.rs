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
//!
//! Two paths feed the store:
//!
//! - the **cloud connector** records every HTTP request the edge forwards
//!   ([`ObservationStore::record_http`]), and
//! - the **local overlay stack** records connections it proxies to
//!   `*.portzero.local` VIPs — TCP-level edges via
//!   [`ObservationStore::record_connection`] (task-91) and, for plaintext HTTP,
//!   per-request routes via [`HttpTap`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    /// Application protocol (`"http"`, `"https"`, or `"tcp"` for opaque
    /// overlay connections like Postgres).
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
///
/// Matches only the `tunnel.` label itself, not the whole `.portzero.cloud`
/// apex — `app.`/`api.`/`agent.`/`edge.portzero.cloud` are control-plane hosts
/// (kept alive there for already-shipped clients; see
/// docs/operators/runbooks/control-plane-domain-migration.md in
/// portzero-cloud), not tunnels, and a bare `.portzero.cloud` suffix match
/// would misclassify them.
fn looks_like_tunnel(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h.ends_with(".portzero.local")
        || h.ends_with(".local")
        || h == "tunnel.portzero.cloud"
        || h.ends_with(".tunnel.portzero.cloud")
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

impl std::fmt::Debug for ObservationStore {
    // Manual: the store sits inside `Debug`-deriving config structs
    // (`OverlayConfig`), and its guts are a mutex nobody wants dumped.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservationStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Do not write `observations.json` more than this often, so a burst of
/// requests does not thrash the disk.
const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Upper bound on distinct exercised routes kept in memory (and on disk). The
/// route key includes the request path, which is unbounded in cardinality for a
/// real app (`/users/1`, `/users/2`, …, 404 probes, bot scans), so without a
/// cap the map — and `observations.json` — would grow without bound over a
/// long-lived daemon. When exceeded we evict the least-recently-seen routes.
const MAX_ROUTES: usize = 5_000;

/// Upper bound on distinct observed edges. Edge cardinality is naturally low
/// (one entry per tunnel→tunnel pair), but bound it too so a flood of spoofed
/// `Referer`/`Origin` hosts can never grow the map without limit.
const MAX_EDGES: usize = 2_000;

/// Upper bound on distinct `X-PZ-Test` attributions kept per route, so a route
/// hit under many different test ids cannot grow a single entry without bound.
const MAX_TESTS_PER_ROUTE: usize = 32;

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

        // Bound whatever was loaded from disk: an `observations.json` written by
        // an older (unbounded) daemon could already be arbitrarily large.
        let mut inner = Inner {
            edges,
            routes,
            last_flush: None,
        };
        inner.enforce_caps();

        Self {
            inner: Mutex::new(inner),
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
                if entry.tests.len() < MAX_TESTS_PER_ROUTE && !entry.tests.iter().any(|t| t == test)
                {
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
            record_edge(&mut guard, from, &to_domain, "http", now);
        }

        guard.enforce_caps();
        self.maybe_flush(guard);
    }

    /// Record one proxied overlay connection into tunnel `to_domain` (task-91).
    ///
    /// This is the TCP-level counterpart of [`Self::record_http`], fed by the
    /// local overlay stack at accept time: `from` is the calling tunnel when
    /// the client process could be attributed to one (source port → PID →
    /// discovered service), `None` for external callers (curl, a browser).
    /// `protocol` is `"http"`/`"https"` for web ports and `"tcp"` otherwise.
    pub fn record_connection(&self, to_domain: &str, protocol: &str, from: Option<&str>) {
        let to_domain = to_domain.trim().to_ascii_lowercase();
        if to_domain.is_empty() {
            return;
        }
        let from = from
            .map(|f| f.trim().to_ascii_lowercase())
            .filter(|f| !f.is_empty() && *f != to_domain);
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        record_edge(&mut guard, from, &to_domain, protocol, Utc::now());
        guard.enforce_caps();
        self.maybe_flush(guard);
    }

    /// Persist to disk if the last write is older than [`FLUSH_INTERVAL`].
    /// Consumes the lock guard so the write happens outside the critical
    /// section.
    fn maybe_flush(&self, mut guard: std::sync::MutexGuard<'_, Inner>) {
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

/// Insert or bump the `(from, to)` edge under the lock. The edge key ignores
/// protocol, so a TCP-level record and a later HTTP-level record for the same
/// pair collapse into one edge (the protocol of the first sighting wins).
fn record_edge(
    guard: &mut Inner,
    from: Option<String>,
    to_domain: &str,
    protocol: &str,
    now: DateTime<Utc>,
) {
    let key = (from.clone(), to_domain.to_string());
    let entry = guard.edges.entry(key).or_insert_with(|| ObservedEdge {
        from,
        to: to_domain.to_string(),
        protocol: protocol.to_string(),
        request_count: 0,
        last_seen: now,
    });
    entry.request_count += 1;
    entry.last_seen = now;
}

impl Inner {
    /// Enforce the in-memory caps, evicting least-recently-seen entries so the
    /// maps (and the persisted snapshot) can never grow without bound. Called
    /// after every record and once when seeding from disk.
    fn enforce_caps(&mut self) {
        evict_to_cap(&mut self.routes, MAX_ROUTES, |r| r.last_seen);
        evict_to_cap(&mut self.edges, MAX_EDGES, |e| e.last_seen);
    }
}

/// Evict the least-recently-seen entries from `map` until it is back under
/// `cap`. To avoid an O(n) scan on every single over-cap insert, this drops in
/// a batch down to ~90% of `cap`, so a saturated map amortizes one sort per
/// ~10% of `cap` new keys rather than one per insert. Ties on `last_seen` are
/// broken by key, so eviction is deterministic and always makes progress even
/// when many entries share a coarse-clock timestamp.
fn evict_to_cap<K, V>(map: &mut BTreeMap<K, V>, cap: usize, last_seen: impl Fn(&V) -> DateTime<Utc>)
where
    K: Ord + Clone,
{
    let len = map.len();
    if len <= cap {
        return;
    }
    let target = cap - cap / 10;
    let remove = len - target;
    let mut by_age: Vec<(DateTime<Utc>, K)> =
        map.iter().map(|(k, v)| (last_seen(v), k.clone())).collect();
    by_age.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    for (_, key) in by_age.into_iter().take(remove) {
        map.remove(&key);
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

/// Longest request head we will buffer while waiting for `\r\n\r\n`.
const MAX_HTTP_HEAD: usize = 16 * 1024;

/// Best-effort per-connection HTTP sniffer for the local overlay proxy
/// (task-91).
///
/// The overlay stack shuttles opaque byte chunks between the client and the
/// real backend; feed each client→backend chunk to [`HttpTap::inspect`] and
/// every request head that starts at a chunk boundary (the overwhelmingly
/// common case — clients write the head in one syscall) is parsed and recorded
/// as an exercised route (plus a `Referer`/`Origin` edge) on `to_domain`.
/// Non-HTTP traffic never matches a method token and costs one prefix check
/// per chunk. Purely best-effort telemetry: request bodies that happen to
/// start a chunk with a method token could over-count, and heads split below
/// a method token's boundary are missed.
pub struct HttpTap {
    store: Arc<ObservationStore>,
    to_domain: String,
    /// Partial request head carried across chunks; empty when idle.
    buf: Vec<u8>,
}

impl HttpTap {
    pub fn new(store: Arc<ObservationStore>, to_domain: String) -> Self {
        Self {
            store,
            to_domain,
            buf: Vec::new(),
        }
    }

    /// Inspect one client→backend chunk, recording any complete request head.
    pub fn inspect(&mut self, chunk: &[u8]) {
        if self.buf.is_empty() && !starts_with_http_method(chunk) {
            return;
        }
        let take = chunk
            .len()
            .min(MAX_HTTP_HEAD - self.buf.len().min(MAX_HTTP_HEAD));
        self.buf.extend_from_slice(&chunk[..take]);

        if let Some(head_end) = find_head_end(&self.buf) {
            let head = self.buf[..head_end].to_vec();
            self.buf.clear();
            self.record_head(&head);
        } else if self.buf.len() >= MAX_HTTP_HEAD {
            // Give up on an over-long (or non-HTTP-after-all) head.
            self.buf.clear();
        }
    }

    fn record_head(&self, head: &[u8]) {
        let head = String::from_utf8_lossy(head);
        let mut lines = head.split("\r\n");
        let Some(request_line) = lines.next() else {
            return;
        };
        let mut parts = request_line.split_whitespace();
        let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        if !version.starts_with("HTTP/") || !path.starts_with('/') {
            return;
        }
        let mut referer = None;
        let mut origin = None;
        let mut x_pz_test = None;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("referer") {
                referer = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("origin") {
                origin = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("x-pz-test") {
                x_pz_test = Some(value.to_string());
            }
        }
        let referer = referer.or(origin);
        self.store.record_http(
            &self.to_domain,
            method,
            path,
            referer.as_deref(),
            x_pz_test.as_deref(),
        );
    }
}

const HTTP_METHODS: [&str; 8] = [
    "GET ", "POST ", "PUT ", "DELETE ", "PATCH ", "HEAD ", "OPTIONS ", "TRACE ",
];

fn starts_with_http_method(chunk: &[u8]) -> bool {
    HTTP_METHODS.iter().any(|m| chunk.starts_with(m.as_bytes()))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
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
        assert!(looks_like_tunnel("tunnel.portzero.cloud"));
        assert!(!looks_like_tunnel("example.com"));
    }

    #[test]
    fn looks_like_tunnel_excludes_control_plane_hosts() {
        // app./api./agent./edge.portzero.cloud are control-plane vhosts kept
        // alive for already-shipped clients, not tunnels — a bare
        // `.portzero.cloud` suffix match would misclassify them.
        assert!(!looks_like_tunnel("app.portzero.cloud"));
        assert!(!looks_like_tunnel("api.portzero.cloud"));
        assert!(!looks_like_tunnel("agent.portzero.cloud"));
        assert!(!looks_like_tunnel("edge.portzero.cloud"));
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
    fn test_record_connection_edges() {
        let dir = std::env::temp_dir().join(format!("pz-obs-conn-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = ObservationStore::new(&dir);

        // Attributed TCP connection: web → db.
        store.record_connection(
            "db.pzdemo.portzero.local",
            "tcp",
            Some("web.pzdemo.portzero.local"),
        );
        store.record_connection(
            "db.pzdemo.portzero.local",
            "tcp",
            Some("web.pzdemo.portzero.local"),
        );
        // Unattributed (external) TCP connection.
        store.record_connection("db.pzdemo.portzero.local", "tcp", None);
        // Self-edge is dropped.
        store.record_connection(
            "db.pzdemo.portzero.local",
            "tcp",
            Some("db.pzdemo.portzero.local"),
        );

        let snap = store.snapshot();
        assert_eq!(snap.edges.len(), 2);
        let attributed = snap
            .edges
            .iter()
            .find(|e| e.from.as_deref() == Some("web.pzdemo.portzero.local"))
            .expect("web → db edge");
        assert_eq!(attributed.to, "db.pzdemo.portzero.local");
        assert_eq!(attributed.protocol, "tcp");
        assert_eq!(attributed.request_count, 2);
        let external = snap
            .edges
            .iter()
            .find(|e| e.from.is_none())
            .expect("(external) → db edge");
        // The self-edge collapsed into nothing, not into the external edge +1.
        assert_eq!(external.request_count, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_http_tap_records_routes_and_referer_edges() {
        let dir = std::env::temp_dir().join(format!("pz-obs-tap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = Arc::new(ObservationStore::new(&dir));
        let mut tap = HttpTap::new(store.clone(), "web.pzdemo.portzero.local".to_string());

        // A full request head in one chunk, with a body after the blank line.
        tap.inspect(
            b"POST /api/guestbook HTTP/1.1\r\n\
              Host: web.pzdemo.portzero.local\r\n\
              X-PZ-Test: guestbook flow\r\n\
              Content-Length: 2\r\n\r\n{}",
        );
        // A head split across two chunks (continuation does not start with a
        // method token, so it must be joined onto the buffered head).
        tap.inspect(b"GET /api/guestbook?limit=5 HTTP/1.1\r\n");
        tap.inspect(b"Referer: http://other.pzdemo.portzero.local/page\r\n\r\n");
        // Postgres-ish bytes: never mistaken for HTTP.
        tap.inspect(&[0x00, 0x00, 0x00, 0x08, 0x04, 0xd2, 0x16, 0x2f]);

        let snap = store.snapshot();
        assert_eq!(snap.routes.len(), 2);
        assert!(snap.routes.iter().any(|r| r.method == "POST"
            && r.path == "/api/guestbook"
            && r.tests == vec!["guestbook flow".to_string()]));
        assert!(snap
            .routes
            .iter()
            .any(|r| r.method == "GET" && r.path == "/api/guestbook"));
        assert_eq!(snap.edges.len(), 1);
        assert_eq!(
            snap.edges[0].from.as_deref(),
            Some("other.pzdemo.portzero.local")
        );

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

    #[test]
    fn test_evict_to_cap_drops_least_recently_seen() {
        use chrono::TimeZone;
        let mut map: BTreeMap<u32, ExercisedRoute> = BTreeMap::new();
        // 100 entries with strictly increasing last_seen (key n is the n-th oldest).
        for n in 0..100u32 {
            map.insert(
                n,
                ExercisedRoute {
                    domain: "d".into(),
                    method: "GET".into(),
                    path: format!("/{n}"),
                    count: 1,
                    tests: Vec::new(),
                    last_seen: Utc.timestamp_opt(1_000 + n as i64, 0).unwrap(),
                },
            );
        }
        // Cap of 50 → batch-evicts down to 90% of cap (45), removing the 55 oldest.
        evict_to_cap(&mut map, 50, |r| r.last_seen);
        assert_eq!(map.len(), 45);
        // The survivors are the newest keys (55..=99); the oldest are gone.
        assert!(map.keys().min().copied().unwrap() >= 55);
        assert!(map.contains_key(&99));
        assert!(!map.contains_key(&0));
    }

    #[test]
    fn test_evict_to_cap_is_noop_under_cap() {
        let mut map: BTreeMap<u32, ObservedEdge> = BTreeMap::new();
        map.insert(
            1,
            ObservedEdge {
                from: None,
                to: "db".into(),
                protocol: "tcp".into(),
                request_count: 1,
                last_seen: Utc::now(),
            },
        );
        evict_to_cap(&mut map, 10, |e| e.last_seen);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_record_http_bounds_route_map() {
        let dir = std::env::temp_dir().join(format!("pz-obs-cap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = ObservationStore::new(&dir);

        // Far more distinct paths than the cap — the map must never exceed it.
        for n in 0..(MAX_ROUTES + 500) {
            store.record_http(
                "api.portzero.local",
                "GET",
                &format!("/item/{n}"),
                None,
                None,
            );
        }
        assert!(store.snapshot().routes.len() <= MAX_ROUTES);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_record_http_bounds_tests_per_route() {
        let dir = std::env::temp_dir().join(format!("pz-obs-tests-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = ObservationStore::new(&dir);

        // One route hit under many distinct test ids — the `tests` vec is capped.
        for n in 0..(MAX_TESTS_PER_ROUTE + 20) {
            store.record_http(
                "api.portzero.local",
                "GET",
                "/",
                None,
                Some(&format!("test-{n}")),
            );
        }
        let snap = store.snapshot();
        assert_eq!(snap.routes.len(), 1);
        assert_eq!(snap.routes[0].tests.len(), MAX_TESTS_PER_ROUTE);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
