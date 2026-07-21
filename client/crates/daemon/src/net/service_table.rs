//! Service table: maps overlay names (from full PZ_TUNNEL=*.portzero.local) to
//! their backing endpoints and virtual IPs.
//!
//! A "service" here is something like:
//!   name = "my-db"
//!   real_addr = "127.0.0.1:32768"   (the discovered ephemeral port)
//!   service_port = 5432              (what clients connect to on the VIP)
//!   vip = 10.254.0.7

use std::collections::HashMap;
use std::net::SocketAddr;

use crate::net::virtual_ip::VirtualIpAllocator;
use crate::protocol_detect::Canonical;
use smoltcp::wire::Ipv4Address;

/// A discovered network service reachable via the overlay.
#[derive(Debug, Clone)]
pub struct NetworkService {
    /// The label (e.g. "my-db" from the full "my-db.portzero.local" value).
    pub name: String,
    /// Virtual IP assigned to this name.
    pub vip: Ipv4Address,
    /// The port clients are expected to connect to (e.g. 5432).
    /// This is usually the "well-known" or container port, not the ephemeral host port.
    pub service_port: u16,
    /// The actual address on the host that we proxy to.
    pub real_addr: SocketAddr,
    /// Best-effort protocol detected on the backend, when known.
    pub backend_protocol: Option<Canonical>,
    /// Owning PID (0 if unknown / container).
    pub pid: u32,
}

#[derive(Debug, Clone, Copy)]
struct PendingReservation {
    port: u16,
    hold_connections: bool,
}

#[derive(Debug, Default, Clone)]
pub struct ServiceTable {
    allocator: VirtualIpAllocator,
    by_name: HashMap<String, NetworkService>,
    /// vip -> name for fast reverse lookup in the packet path
    vip_to_name: HashMap<Ipv4Address, String>,
    /// VIP reservations that do not have a backend yet, keyed by name.
    pending: HashMap<String, PendingReservation>,
}

impl ServiceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or update a service.
    ///
    /// If the name already exists, we keep the same VIP but update the real_addr
    /// and service_port (handles process restarts or new port 0 assignment).
    pub fn register(
        &mut self,
        name: String,
        real_addr: SocketAddr,
        service_port: u16,
        pid: u32,
    ) -> NetworkService {
        self.register_with_backend_protocol(name, real_addr, service_port, pid, None)
    }

    /// Register or update a service with optional backend protocol metadata.
    pub fn register_with_backend_protocol(
        &mut self,
        name: String,
        real_addr: SocketAddr,
        service_port: u16,
        pid: u32,
        backend_protocol: Option<Canonical>,
    ) -> NetworkService {
        let vip = if let Some(existing) = self.by_name.get(&name) {
            existing.vip
        } else {
            let std_ip = self.allocator.assign(&name);
            Ipv4Address::from_bytes(&std_ip.octets())
        };

        let svc = NetworkService {
            name: name.clone(),
            vip,
            service_port,
            real_addr,
            backend_protocol,
            pid,
        };

        // Remove old reverse mapping if VIP changed (shouldn't for same name)
        if let Some(old) = self.by_name.get(&name) {
            if old.vip != vip {
                self.vip_to_name.remove(&old.vip);
            }
        }

        self.by_name.insert(name.clone(), svc.clone());
        self.vip_to_name.insert(vip, name.clone());
        self.pending.remove(&name);
        svc
    }

    /// Reserve a VIP for a name before its backend endpoint is known.
    ///
    /// This lets DNS answer immediately for a just-started tunnel while
    /// discovery continues scanning for the process and port. The reservation is
    /// not considered a routable service until [`Self::register`] fills in the
    /// backend.
    pub fn reserve_name(&mut self, name: &str) -> Ipv4Address {
        self.reserve_name_on_port(name, 80, false)
    }

    /// Reserve a VIP and temporary service port for a name before its backend is known.
    pub fn reserve_name_on_port(
        &mut self,
        name: &str,
        service_port: u16,
        hold_connections: bool,
    ) -> Ipv4Address {
        if let Some(existing) = self.by_name.get(name) {
            return existing.vip;
        }
        let std_ip = self.allocator.assign(name);
        self.pending
            .entry(name.to_string())
            .or_insert(PendingReservation {
                port: service_port,
                hold_connections,
            });
        Ipv4Address::from_bytes(&std_ip.octets())
    }

    /// Lookup the VIP assigned to a name, whether or not a backend is registered.
    pub fn assigned_vip(&self, name: &str) -> Option<Ipv4Address> {
        if let Some(service) = self.by_name.get(name) {
            return Some(service.vip);
        }
        self.allocator
            .lookup_name(name)
            .map(|ip| Ipv4Address::from_bytes(&ip.octets()))
    }

    /// Preserve all VIP assignments from another table.
    ///
    /// Fresh discovery scans rebuild the table from scratch. Carrying forward
    /// assignments keeps DNS answers stable, including proactive reservations.
    pub fn preserve_assignments_from(&mut self, other: &ServiceTable) {
        for (name, ip) in other.allocator.assignments() {
            self.allocator.preserve(name, ip);
        }
        for (name, pending) in &other.pending {
            if !self.by_name.contains_key(name) {
                self.pending.entry(name.clone()).or_insert(*pending);
            }
        }
    }

    /// Get the name assigned to a VIP, whether or not a backend is registered.
    pub fn assigned_name_by_vip(&self, vip: Ipv4Address) -> Option<&str> {
        if let Some(name) = self.vip_to_name.get(&vip) {
            return Some(name);
        }
        let b = vip.0;
        let ip = std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]);
        self.allocator.lookup_ip(ip)
    }

    /// Ports that should be listened on for pending VIP reservations.
    pub fn pending_ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.pending.values().map(|pending| pending.port)
    }

    /// Whether accepted sockets for a pending VIP should wait for discovery.
    pub fn should_hold_pending_connection(&self, vip: Ipv4Address) -> bool {
        let Some(name) = self.assigned_name_by_vip(vip) else {
            return false;
        };
        self.pending
            .get(name)
            .is_some_and(|pending| pending.hold_connections)
    }

    /// Remove a service by name.
    pub fn unregister(&mut self, name: &str) -> Option<NetworkService> {
        if let Some(svc) = self.by_name.remove(name) {
            self.vip_to_name.remove(&svc.vip);
            // We keep the IP reserved in the allocator for stability across quick restarts.
            // Call self.allocator.release(name) only on explicit long-term removal if desired.
            Some(svc)
        } else {
            None
        }
    }

    /// Get by name.
    pub fn get(&self, name: &str) -> Option<&NetworkService> {
        self.by_name.get(name)
    }

    /// Get by virtual IP (used by the TCP stack to decide where to proxy).
    pub fn get_by_vip(&self, vip: Ipv4Address) -> Option<&NetworkService> {
        self.vip_to_name.get(&vip).and_then(|n| self.by_name.get(n))
    }

    /// Get by (vip, dst_port). We primarily key on VIP; the dst_port is validated
    /// against service_port for defense-in-depth.
    pub fn resolve_for_connect(&self, vip: Ipv4Address, dst_port: u16) -> Option<&NetworkService> {
        self.get_by_vip(vip).filter(|s| s.service_port == dst_port)
    }

    /// Iterate all services.
    pub fn all(&self) -> impl Iterator<Item = &NetworkService> {
        self.by_name.values()
    }

    /// Number of registered services.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_reservation_exposes_port_and_vip_name() {
        let mut table = ServiceTable::new();
        let vip = table.reserve_name_on_port("fresh", 80, true);

        assert_eq!(table.assigned_vip("fresh"), Some(vip));
        assert_eq!(table.assigned_name_by_vip(vip), Some("fresh"));
        assert_eq!(table.pending_ports().collect::<Vec<_>>(), vec![80]);
        assert!(table.should_hold_pending_connection(vip));
        assert!(table.get("fresh").is_none());
    }

    #[test]
    fn registering_service_clears_pending_port() {
        let mut table = ServiceTable::new();
        let pending_vip = table.reserve_name_on_port("fresh", 80, false);
        let service = table.register(
            "fresh".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            80,
            1234,
        );

        assert_eq!(service.vip, pending_vip);
        assert!(table.pending_ports().collect::<Vec<_>>().is_empty());
        assert!(!table.should_hold_pending_connection(pending_vip));
    }

    #[test]
    fn full_local_domain_prefixes_are_distinct_keys() {
        let mut table = ServiceTable::new();
        let nested = table.register(
            "staging.portzero.net".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            443,
            100,
        );
        let short = table.register(
            "staging".to_string(),
            "127.0.0.1:9001".parse().unwrap(),
            443,
            101,
        );

        assert_eq!(table.len(), 2);
        assert_eq!(
            table.get("staging.portzero.net").map(|svc| svc.real_addr),
            Some("127.0.0.1:9000".parse().unwrap())
        );
        assert_eq!(
            table.get("staging").map(|svc| svc.real_addr),
            Some("127.0.0.1:9001".parse().unwrap())
        );
        assert_ne!(nested.vip, short.vip);
    }
}
