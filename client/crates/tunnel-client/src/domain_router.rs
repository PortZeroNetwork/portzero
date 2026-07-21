//! Domain router: maps incoming hostnames to local service ports.
//!
//! The domain router maintains a mapping of domain names to local ports,
//! used by the tunnel client to forward traffic from the edge to the
//! correct local service.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Thread-safe domain-to-port routing table.
#[derive(Debug, Clone)]
pub struct DomainRouter {
    routes: Arc<RwLock<HashMap<String, u16>>>,
}

impl DomainRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a domain -> local port mapping.
    pub fn add_route(&self, domain: String, port: u16) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        routes.insert(domain, port);
    }

    /// Remove a domain mapping.
    pub fn remove_route(&self, domain: &str) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        routes.remove(domain);
    }

    /// Look up the local port for a domain.
    pub fn resolve(&self, domain: &str) -> Option<u16> {
        let routes = self.routes.read().expect("route lock poisoned");
        routes.get(domain).copied()
    }

    /// Replace all routes at once (used after a discovery scan).
    pub fn replace_all(&self, new_routes: HashMap<String, u16>) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        *routes = new_routes;
    }

    /// Get a snapshot of all current routes.
    pub fn snapshot(&self) -> HashMap<String, u16> {
        let routes = self.routes.read().expect("route lock poisoned");
        routes.clone()
    }

    /// Number of active routes.
    pub fn len(&self) -> usize {
        let routes = self.routes.read().expect("route lock poisoned");
        routes.len()
    }

    /// Check if there are no routes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for DomainRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_resolve() {
        let router = DomainRouter::new();
        router.add_route("api-myapp.alice.portzero.cloud".into(), 8080);
        assert_eq!(router.resolve("api-myapp.alice.portzero.cloud"), Some(8080));
        assert_eq!(router.resolve("unknown-svc.alice.portzero.cloud"), None);
    }

    #[test]
    fn test_remove_route() {
        let router = DomainRouter::new();
        router.add_route("api-myapp.alice.portzero.cloud".into(), 8080);
        router.remove_route("api-myapp.alice.portzero.cloud");
        assert_eq!(router.resolve("api-myapp.alice.portzero.cloud"), None);
    }

    #[test]
    fn test_replace_all() {
        let router = DomainRouter::new();
        router.add_route("old-svc.alice.portzero.cloud".into(), 3000);

        let mut new_routes = HashMap::new();
        new_routes.insert("new-svc.alice.portzero.cloud".into(), 4000);
        router.replace_all(new_routes);

        assert_eq!(router.resolve("old-svc.alice.portzero.cloud"), None);
        assert_eq!(router.resolve("new-svc.alice.portzero.cloud"), Some(4000));
    }

    #[test]
    fn test_snapshot() {
        let router = DomainRouter::new();
        router.add_route("a-svc.alice.portzero.cloud".into(), 1000);
        router.add_route("b-svc.alice.portzero.cloud".into(), 2000);

        let snap = router.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap["a-svc.alice.portzero.cloud"], 1000);
        assert_eq!(snap["b-svc.alice.portzero.cloud"], 2000);
    }
}
