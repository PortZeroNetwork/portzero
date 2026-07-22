//! Routing table: maps domains to local endpoints.
//!
//! The route table is the output of discovery. It is persisted to
//! `~/.portzero/daemon/routes.json` and consumed by the tunnel client.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::discovery::{DiscoveredService, PortMapping, ServiceSource};

/// A single route entry: domain -> local endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// The full PZ_TUNNEL value (e.g. "api-myapp-main.alice.tunnel.portzero.cloud").
    pub domain: String,
    /// Original PZ_TUNNEL domain value before template substitution.
    #[serde(default)]
    pub domain_template: String,
    /// Values available for template substitution when this route was found.
    #[serde(default)]
    pub substitutions: BTreeMap<String, String>,
    /// Host to forward to (usually "127.0.0.1").
    pub host: String,
    /// Port to forward to.
    pub port: u16,
    /// Additional raw port mappings from PZ_TUNNEL_PORTS.
    #[serde(default)]
    pub extra_ports: Vec<PortMapping>,
    /// Optional readiness path from PZ_HEALTH_PATH (e.g. "/health"). `None` when
    /// the endpoint does not declare one.
    #[serde(default)]
    pub health_path: Option<String>,
    /// How the service was discovered.
    pub source: ServiceSource,
    /// Process ID that owns the service.
    pub pid: u32,
    /// When this route was first discovered.
    pub discovered_at: DateTime<Utc>,
}

/// The full routing table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteTable {
    /// Domain -> route mapping.
    pub routes: HashMap<String, Route>,
}

/// Changes detected after an update cycle.
#[derive(Debug, Default)]
pub struct RouteChanges {
    /// Newly discovered routes.
    pub added: Vec<Route>,
    /// Routes that disappeared.
    pub removed: Vec<Route>,
    /// Routes whose port or source changed.
    pub changed: Vec<Route>,
}

impl RouteChanges {
    /// Returns true if there are any changes.
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.changed.is_empty()
    }
}

impl RouteTable {
    /// Create an empty route table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the route table with newly discovered services.
    ///
    /// Returns a summary of what changed.
    pub fn update(&mut self, discovered: Vec<DiscoveredService>) -> RouteChanges {
        let mut changes = RouteChanges::default();
        let now = Utc::now();

        let mut seen_domains: HashMap<String, &DiscoveredService> = HashMap::new();
        for svc in &discovered {
            let should_replace = match seen_domains.get(&svc.domain) {
                None => true,
                Some(existing) => {
                    if existing.port == 0 && svc.port > 0 {
                        true
                    } else if svc.port == 0 && existing.port > 0 {
                        false
                    } else {
                        svc.pid < existing.pid
                    }
                }
            };
            if should_replace {
                seen_domains.insert(svc.domain.clone(), svc);
            }
        }

        for (domain, svc) in &seen_domains {
            if let Some(existing) = self.routes.get(domain) {
                if existing.port != svc.port
                    || existing.pid != svc.pid
                    || existing.health_path != svc.health_path
                {
                    let route = Route {
                        domain: domain.clone(),
                        domain_template: svc.domain_template.clone(),
                        substitutions: svc.substitutions.clone(),
                        host: "127.0.0.1".to_string(),
                        port: svc.port,
                        extra_ports: svc.extra_ports.clone(),
                        health_path: svc.health_path.clone(),
                        source: svc.source.clone(),
                        pid: svc.pid,
                        discovered_at: existing.discovered_at,
                    };
                    // Only notify cloud/domain-router when the port changes.
                    // PID-only changes (e.g. process restart via nodemon) update
                    // routes.json for tracking but don't need re-registration.
                    if existing.port != svc.port {
                        changes.changed.push(route.clone());
                    }
                    self.routes.insert(domain.clone(), route);
                }
            } else {
                let route = Route {
                    domain: domain.clone(),
                    domain_template: svc.domain_template.clone(),
                    substitutions: svc.substitutions.clone(),
                    host: "127.0.0.1".to_string(),
                    port: svc.port,
                    extra_ports: svc.extra_ports.clone(),
                    health_path: svc.health_path.clone(),
                    source: svc.source.clone(),
                    pid: svc.pid,
                    discovered_at: now,
                };
                changes.added.push(route.clone());
                self.routes.insert(domain.clone(), route);
            }
        }

        let to_remove: Vec<String> = self
            .routes
            .keys()
            .filter(|domain| !seen_domains.contains_key(*domain))
            .cloned()
            .collect();

        for domain in to_remove {
            if let Some(route) = self.routes.remove(&domain) {
                changes.removed.push(route);
            }
        }

        changes
    }

    /// Save the route table to a JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(self).context("Failed to serialize route table")?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write route table to: {}", path.display()))?;

        Ok(())
    }

    /// Load the route table from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read route table from: {}", path.display()))?;
        let table: RouteTable = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse route table from: {}", path.display()))?;

        Ok(table)
    }

    /// Get number of routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

/// A route entry for a local overlay (`.portzero.local`) service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayRoute {
    /// Full domain name (e.g. "web-main.portzero.local").
    pub domain: String,
    /// Original PZ_TUNNEL domain value before template substitution.
    #[serde(default)]
    pub domain_template: String,
    /// Values available for template substitution when this route was found.
    #[serde(default)]
    pub substitutions: BTreeMap<String, String>,
    /// Port clients are expected to connect to on the virtual IP (e.g. 8080).
    pub service_port: u16,
    /// Best-effort protocol detected on the backend, when known. Drives whether
    /// (and with which scheme) a non-80/443 web tunnel gets a clickable browser
    /// link; `None` for a raw TCP service (Postgres, Redis, …) that has no URL.
    #[serde(default)]
    pub backend_protocol: Option<crate::protocol_detect::Canonical>,
    /// Actual host address the daemon proxies to (e.g. "127.0.0.1:32771").
    pub real_addr: String,
    /// Optional readiness path from PZ_HEALTH_PATH (e.g. "/health"). `None` when
    /// the endpoint does not declare one.
    #[serde(default)]
    pub health_path: Option<String>,
    /// Process ID that owns the service (0 for containers).
    pub pid: u32,
    /// How the service was discovered.
    pub source: ServiceSource,
}

/// Snapshot of all discovered overlay services, persisted to `overlay.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OverlayState {
    /// Whether the virtual overlay network (TUN device) is running.
    pub overlay_active: bool,
    pub routes: Vec<OverlayRoute>,
}

impl OverlayState {
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize overlay state")?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to write overlay state to: {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read overlay state from: {}", path.display()))?;
        let state: OverlayState = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse overlay state from: {}", path.display()))?;
        Ok(state)
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ServiceSource;
    use std::path::PathBuf;

    fn make_service(domain: &str, port: u16, pid: u32) -> DiscoveredService {
        DiscoveredService {
            domain: domain.to_string(),
            domain_template: domain.to_string(),
            substitutions: Default::default(),
            port,
            extra_ports: vec![],
            health_path: None,
            pid,
            source: ServiceSource::Process {
                cwd: Some(PathBuf::from("/tmp/test")),
            },
        }
    }

    #[test]
    fn test_update_adds_new_routes() {
        let mut table = RouteTable::new();
        let services = vec![
            make_service("api.example.com", 8080, 100),
            make_service("web.example.com", 3000, 101),
        ];

        let changes = table.update(services);

        assert_eq!(changes.added.len(), 2);
        assert!(changes.removed.is_empty());
        assert!(changes.changed.is_empty());
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn test_update_detects_removed() {
        let mut table = RouteTable::new();

        table.update(vec![
            make_service("api.example.com", 8080, 100),
            make_service("web.example.com", 3000, 101),
        ]);

        let changes = table.update(vec![make_service("api.example.com", 8080, 100)]);

        assert!(changes.added.is_empty());
        assert_eq!(changes.removed.len(), 1);
        assert_eq!(changes.removed[0].domain, "web.example.com");
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_update_detects_changed_port() {
        let mut table = RouteTable::new();

        table.update(vec![make_service("api.example.com", 8080, 100)]);

        let changes = table.update(vec![make_service("api.example.com", 9090, 100)]);

        assert!(changes.added.is_empty());
        assert!(changes.removed.is_empty());
        assert_eq!(changes.changed.len(), 1);
        assert_eq!(changes.changed[0].port, 9090);
    }

    #[test]
    fn test_update_no_changes() {
        let mut table = RouteTable::new();

        table.update(vec![make_service("api.example.com", 8080, 100)]);
        let changes = table.update(vec![make_service("api.example.com", 8080, 100)]);

        assert!(!changes.has_changes());
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join(format!("portzero-route-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("routes.json");

        let mut table = RouteTable::new();
        table.update(vec![make_service("api.example.com", 8080, 100)]);
        table.save(&path).unwrap();

        let loaded = RouteTable::load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded.routes.contains_key("api.example.com"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_health_path_carried_onto_route_and_persisted() {
        let dir = std::env::temp_dir().join(format!("portzero-health-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("routes.json");

        let mut svc = make_service("api.alice.tunnel.portzero.cloud", 8080, 100);
        svc.health_path = Some("/health".to_string());

        let mut table = RouteTable::new();
        table.update(vec![svc]);
        let route = table
            .routes
            .get("api.alice.tunnel.portzero.cloud")
            .expect("route present");
        assert_eq!(route.health_path.as_deref(), Some("/health"));

        // Survives a save/load round-trip.
        table.save(&path).unwrap();
        let loaded = RouteTable::load(&path).unwrap();
        assert_eq!(
            loaded
                .routes
                .get("api.alice.tunnel.portzero.cloud")
                .and_then(|r| r.health_path.as_deref()),
            Some("/health")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_health_path_change_updates_route() {
        let mut table = RouteTable::new();
        let mut svc = make_service("api.example.com", 8080, 100);
        table.update(vec![svc.clone()]);

        // Same port/pid, but a newly-declared health path must be persisted.
        svc.health_path = Some("/ready".to_string());
        table.update(vec![svc]);
        assert_eq!(
            table
                .routes
                .get("api.example.com")
                .and_then(|r| r.health_path.as_deref()),
            Some("/ready")
        );
    }

    #[test]
    fn test_load_nonexistent() {
        let table = RouteTable::load(Path::new("/nonexistent/routes.json")).unwrap();
        assert!(table.is_empty());
    }

    #[test]
    fn test_duplicate_domain_prefers_port() {
        let mut table = RouteTable::new();
        let services = vec![
            make_service("api.example.com", 0, 100),
            make_service("api.example.com", 8080, 101),
        ];

        let changes = table.update(services);

        assert_eq!(changes.added.len(), 1);
        assert_eq!(table.routes.get("api.example.com").unwrap().port, 8080);
    }

    #[test]
    fn test_duplicate_domain_lower_pid_wins() {
        let mut table = RouteTable::new();

        // Higher PID seen first.
        let services = vec![
            make_service("api.example.com", 3001, 200),
            make_service("api.example.com", 3000, 100),
        ];
        let changes = table.update(services);

        assert_eq!(changes.added.len(), 1);
        assert_eq!(table.routes.get("api.example.com").unwrap().pid, 100);
        assert_eq!(table.routes.get("api.example.com").unwrap().port, 3000);
    }
}
