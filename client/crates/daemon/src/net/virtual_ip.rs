//! Virtual IP allocator for the overlay network.
//!
//! We use a non-routable documentation/example block:
//!   10.254.0.0/16
//!
//! Each full `*.portzero.local` name from PZ_TUNNEL gets a stable IP
//! for the lifetime of the daemon.

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// The virtual network base (10.254.0.0/16).
const VNET_PREFIX: [u8; 2] = [10, 254];
const VNET_GATEWAY_OCTET3: u8 = 0;
const VNET_GATEWAY_OCTET4: u8 = 1; // 10.254.0.1 reserved for future gateway

/// Reserved VIP for the management API service (portzero.local).
pub const MANAGEMENT_VIP: Ipv4Addr = Ipv4Addr::new(10, 254, 0, 2);

/// Reserved VIP for the management REST API (api.portzero.local).
pub const API_MANAGEMENT_VIP: Ipv4Addr = Ipv4Addr::new(10, 254, 0, 3);

/// Allocator for virtual IPs inside 10.254.0.0/16.
#[derive(Debug, Clone)]
pub struct VirtualIpAllocator {
    next: u16,
    name_to_ip: HashMap<String, Ipv4Addr>,
    ip_to_name: HashMap<Ipv4Addr, String>,
}

impl Default for VirtualIpAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualIpAllocator {
    pub fn new() -> Self {
        let mut name_to_ip = HashMap::new();
        let mut ip_to_name = HashMap::new();
        // Pre-seed the management service reservation.
        name_to_ip.insert("portzero".to_string(), MANAGEMENT_VIP);
        ip_to_name.insert(MANAGEMENT_VIP, "portzero".to_string());
        name_to_ip.insert("portzero-api".to_string(), API_MANAGEMENT_VIP);
        ip_to_name.insert(API_MANAGEMENT_VIP, "portzero-api".to_string());
        Self {
            next: 4, // start at 10.254.0.4; .2 and .3 are reserved for portzero management
            name_to_ip,
            ip_to_name,
        }
    }

    /// Get or allocate a virtual IP for a given network name.
    /// The same name always yields the same IP while the allocator lives.
    pub fn assign(&mut self, name: &str) -> Ipv4Addr {
        if let Some(&ip) = self.name_to_ip.get(name) {
            return ip;
        }

        // Find a free slot (very simple linear scan; 64k is tiny).
        let mut candidate = self.next;
        loop {
            let ip = Ipv4Addr::new(
                VNET_PREFIX[0],
                VNET_PREFIX[1],
                (candidate >> 8) as u8,
                (candidate & 0xff) as u8,
            );

            if self.is_reserved(ip) || self.ip_to_name.contains_key(&ip) {
                candidate = candidate.wrapping_add(1);
                if candidate == 0 {
                    candidate = 2;
                }
                continue;
            }

            self.name_to_ip.insert(name.to_string(), ip);
            self.ip_to_name.insert(ip, name.to_string());
            self.next = candidate.wrapping_add(1);
            if self.next == 0 {
                self.next = 2;
            }
            return ip;
        }
    }

    /// Release a name (and its IP) if present.
    pub fn release(&mut self, name: &str) {
        if let Some(ip) = self.name_to_ip.remove(name) {
            self.ip_to_name.remove(&ip);
        }
    }

    /// Preserve an existing name -> IP assignment from another allocator.
    ///
    /// Used when rebuilding service tables from a fresh discovery scan: DNS may
    /// have already handed a VIP to a client before discovery found the backend,
    /// so the next table must keep that exact answer stable.
    pub fn preserve(&mut self, name: &str, ip: Ipv4Addr) {
        if self.name_to_ip.contains_key(name) || self.ip_to_name.contains_key(&ip) {
            return;
        }
        self.name_to_ip.insert(name.to_string(), ip);
        self.ip_to_name.insert(ip, name.to_string());
    }

    /// Iterate all assigned names and IPs.
    pub fn assignments(&self) -> impl Iterator<Item = (&str, Ipv4Addr)> {
        self.name_to_ip
            .iter()
            .map(|(name, ip)| (name.as_str(), *ip))
    }

    /// Lookup by name.
    pub fn lookup_name(&self, name: &str) -> Option<Ipv4Addr> {
        self.name_to_ip.get(name).copied()
    }

    /// Reverse lookup by IP.
    pub fn lookup_ip(&self, ip: Ipv4Addr) -> Option<&str> {
        self.ip_to_name.get(&ip).map(|s| s.as_str())
    }

    fn is_reserved(&self, ip: Ipv4Addr) -> bool {
        let o = ip.octets();
        // 10.254.0.0, 10.254.0.1, and broadcast-ish 10.254.255.255
        // 10.254.0.2 is reserved for the portzero management service
        // 10.254.0.3 is reserved for the portzero management REST API
        (o[2] == 0 && o[3] <= 3) || (o[2] == 255 && o[3] == 255)
    }
}

/// Return the conventional gateway address for the virtual net (if ever needed).
pub fn gateway_ip() -> Ipv4Addr {
    Ipv4Addr::new(
        VNET_PREFIX[0],
        VNET_PREFIX[1],
        VNET_GATEWAY_OCTET3,
        VNET_GATEWAY_OCTET4,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_stable_ip() {
        let mut alloc = VirtualIpAllocator::new();
        let a = alloc.assign("my-db");
        let b = alloc.assign("my-db");
        assert_eq!(a, b);
        assert!(a.octets()[0] == 10 && a.octets()[1] == 254);
    }

    #[test]
    fn different_names_get_different_ips() {
        let mut alloc = VirtualIpAllocator::new();
        let a = alloc.assign("db");
        let b = alloc.assign("api");
        assert_ne!(a, b);
    }

    #[test]
    fn release_frees_ip() {
        let mut alloc = VirtualIpAllocator::new();
        let _ip = alloc.assign("temp");
        alloc.release("temp");
        let _ip2 = alloc.assign("other");
        // We don't guarantee immediate reuse, but lookup must be gone
        assert!(alloc.lookup_name("temp").is_none());
    }
}
