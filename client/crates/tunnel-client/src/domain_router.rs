//! Domain router: maps incoming hostnames to local service addresses.
//!
//! The domain router maintains a mapping of domain names to the local address
//! (loopback address + port) each service listens on, used by the tunnel
//! client to forward traffic from the edge to the correct local service.
//!
//! The address carries the family, not just the port: a service bound to
//! `[::1]` is unreachable from `127.0.0.1`, so forwarding has to dial the same
//! family the process is listening on.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

/// Thread-safe domain-to-address routing table.
#[derive(Debug, Clone)]
pub struct DomainRouter {
    routes: Arc<RwLock<HashMap<String, SocketAddr>>>,
}

impl DomainRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a domain -> local address mapping.
    pub fn add_route(&self, domain: String, addr: SocketAddr) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        routes.insert(domain, addr);
    }

    /// Remove a domain mapping.
    pub fn remove_route(&self, domain: &str) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        routes.remove(domain);
    }

    /// Look up the local address for a domain.
    pub fn resolve(&self, domain: &str) -> Option<SocketAddr> {
        let routes = self.routes.read().expect("route lock poisoned");
        routes.get(domain).copied()
    }

    /// Replace all routes at once (used after a discovery scan).
    pub fn replace_all(&self, new_routes: HashMap<String, SocketAddr>) {
        let mut routes = self.routes.write().expect("route lock poisoned");
        *routes = new_routes;
    }

    /// Get a snapshot of all current routes.
    pub fn snapshot(&self) -> HashMap<String, SocketAddr> {
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

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), port)
    }

    #[test]
    fn test_add_and_resolve() {
        let router = DomainRouter::new();
        router.add_route("api-myapp.alice.portzero.cloud".into(), v4(8080));
        assert_eq!(
            router.resolve("api-myapp.alice.portzero.cloud"),
            Some(v4(8080))
        );
        assert_eq!(router.resolve("unknown-svc.alice.portzero.cloud"), None);
    }

    #[test]
    fn resolve_keeps_the_address_family_of_an_ipv6_only_service() {
        // A service bound to [::1] must not be resolved to 127.0.0.1 — dialing
        // the wrong family is a connection to nothing.
        let router = DomainRouter::new();
        router.add_route("vite.alice.tunnel.portzero.cloud".into(), v6(5173));
        assert_eq!(
            router.resolve("vite.alice.tunnel.portzero.cloud"),
            Some(v6(5173))
        );
    }

    #[test]
    fn test_remove_route() {
        let router = DomainRouter::new();
        router.add_route("api-myapp.alice.portzero.cloud".into(), v4(8080));
        router.remove_route("api-myapp.alice.portzero.cloud");
        assert_eq!(router.resolve("api-myapp.alice.portzero.cloud"), None);
    }

    #[test]
    fn test_replace_all() {
        let router = DomainRouter::new();
        router.add_route("old-svc.alice.portzero.cloud".into(), v4(3000));

        let mut new_routes = HashMap::new();
        new_routes.insert("new-svc.alice.portzero.cloud".into(), v4(4000));
        router.replace_all(new_routes);

        assert_eq!(router.resolve("old-svc.alice.portzero.cloud"), None);
        assert_eq!(
            router.resolve("new-svc.alice.portzero.cloud"),
            Some(v4(4000))
        );
    }

    #[test]
    fn test_snapshot() {
        let router = DomainRouter::new();
        router.add_route("a-svc.alice.portzero.cloud".into(), v4(1000));
        router.add_route("b-svc.alice.portzero.cloud".into(), v6(2000));

        let snap = router.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap["a-svc.alice.portzero.cloud"], v4(1000));
        assert_eq!(snap["b-svc.alice.portzero.cloud"], v6(2000));
    }
}
