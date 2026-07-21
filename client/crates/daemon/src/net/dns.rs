//! Embedded authoritative DNS server for the overlay.
//!
//! Serves only A records for `<name>.portzero.local` -> virtual IP.
//! The server listens on 127.0.0.1:53 (or a configurable port) and is reached
//! via scoped OS configuration, never by hijacking the whole system resolver.
//!
//! We use hickory-proto for clean DNS message handling.

#[cfg(target_os = "windows")]
use std::net::UdpSocket as StdUdpSocket;
use std::net::{Ipv4Addr, SocketAddr};
#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
#[cfg(target_os = "windows")]
use std::time::Duration;

use anyhow::Result;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::SOA;
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use tokio::net::UdpSocket;
#[cfg(target_os = "windows")]
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::sync::{Notify, RwLock};

use crate::net::service_table::ServiceTable;
use crate::net::stack::StackCommand;
use crate::net::virtual_ip::{API_MANAGEMENT_VIP, MANAGEMENT_VIP};

/// How long a query for an as-yet-unknown `*.portzero.local` name is held open
/// while we trigger a discovery rescan, so a just-started service resolves on the
/// first query instead of returning NXDOMAIN. Comfortably covers a scan cycle
/// plus the HTTP protocol probe; if the service still hasn't appeared we fall
/// back to NXDOMAIN.
const DNS_FIRST_HIT_WAIT: std::time::Duration = std::time::Duration::from_millis(2000);

/// What DNS should do when an A query arrives for an unknown
/// `*.portzero.local` name.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DnsFirstHitPolicy {
    /// Current behavior: trigger process discovery and hold the DNS query open
    /// briefly so the response includes a routable service when the scan wins.
    #[default]
    FastScan,
    /// Allocate and return a VIP immediately, then trigger discovery in the
    /// background. The first TCP connection succeeds once discovery registers
    /// the backend and listeners.
    ProactiveVip,
    /// Like `proactive-vip`, but the TCP stack holds accepted connections for a
    /// short window while discovery finds the backend. This is benchmarkable
    /// because OS TCP behavior differs materially here.
    ProactiveVipHold,
}

impl DnsFirstHitPolicy {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            DnsFirstHitPolicy::FastScan => 0,
            DnsFirstHitPolicy::ProactiveVip => 1,
            DnsFirstHitPolicy::ProactiveVipHold => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => DnsFirstHitPolicy::ProactiveVip,
            2 => DnsFirstHitPolicy::ProactiveVipHold,
            _ => DnsFirstHitPolicy::FastScan,
        }
    }
}

/// Runs a simple UDP DNS server that answers A queries for the overlay.
pub struct OverlayDnsServer {
    services: Arc<RwLock<ServiceTable>>,
    listen_addr: SocketAddr,
    /// Stored as AtomicU8 so the policy can be updated live by the config
    /// poller (from another task) without restarting the DNS server. The
    /// value only affects "first hit" behavior for unknown *.portzero.local
    /// names; it never impacts established services or in-flight connections.
    first_hit_policy: Arc<AtomicU8>,
    stack_updates: Option<mpsc::Sender<StackCommand>>,
    /// Pinged when a query arrives for an unknown `*.portzero.local` name, so the
    /// discovery loop rescans immediately. `None` in standalone/test setups.
    /// Uses `notify_one` at the call site so a wakeup is not lost if the refresh
    /// task is between scans and has not yet parked on `notified()`.
    dns_rescan: Option<Arc<Notify>>,
    /// Awaited (briefly) after a miss; the discovery loop notifies it once the
    /// service table has been refreshed. `None` in standalone/test setups.
    dns_updated: Option<Arc<Notify>>,
}

impl OverlayDnsServer {
    pub fn new(services: Arc<RwLock<ServiceTable>>, listen_addr: SocketAddr) -> Self {
        Self {
            services,
            listen_addr,
            first_hit_policy: Arc::new(AtomicU8::new(DnsFirstHitPolicy::default().to_u8())),
            stack_updates: None,
            dns_rescan: None,
            dns_updated: None,
        }
    }

    pub fn with_first_hit_policy(self, policy: DnsFirstHitPolicy) -> Self {
        self.first_hit_policy
            .store(policy.to_u8(), Ordering::Relaxed);
        self
    }

    pub fn with_stack_updates(mut self, stack_updates: mpsc::Sender<StackCommand>) -> Self {
        self.stack_updates = Some(stack_updates);
        self
    }

    /// Returns a shareable atomic for the first-hit policy. The discovery loop's
    /// config poller keeps a clone of this so it can change the value at runtime
    /// (the DNS server task will observe it on the next query).
    pub fn first_hit_policy_holder(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.first_hit_policy)
    }

    /// Update policy live (equivalent to calling store via the holder).
    pub fn update_first_hit_policy(&self, policy: DnsFirstHitPolicy) {
        self.first_hit_policy
            .store(policy.to_u8(), Ordering::Relaxed);
    }

    /// Wire up the rescan/refresh signals so a miss on an unknown subdomain
    /// triggers an immediate discovery rescan and the query is held briefly for
    /// the result. Without this the server returns NXDOMAIN immediately on a miss.
    pub fn with_rescan(mut self, dns_rescan: Arc<Notify>, dns_updated: Arc<Notify>) -> Self {
        self.dns_rescan = Some(dns_rescan);
        self.dns_updated = Some(dns_updated);
        self
    }

    /// Run the DNS server until cancelled.
    pub async fn run(self) -> Result<()> {
        let sock = UdpSocket::bind(self.listen_addr).await?;
        tracing::info!("overlay DNS listening on {}", self.listen_addr);

        let mut buf = vec![0u8; 512];

        loop {
            let (len, src) = match sock.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("DNS recv error: {}", e);
                    continue;
                }
            };

            tracing::info!("overlay DNS query received from {}", src);
            let req_bytes = &buf[..len];
            let response = match self.handle_query(req_bytes).await {
                Some(resp) => resp,
                None => continue,
            };

            if let Err(e) = sock.send_to(&response, src).await {
                tracing::debug!("DNS send error to {}: {}", src, e);
            }
        }
    }

    /// Run the DNS server on a dedicated blocking thread.
    ///
    /// Windows startup/discovery can involve blocking OS inspection. Keeping DNS
    /// on a plain socket thread prevents those paths from starving name
    /// resolution once the resolver is configured.
    #[cfg(target_os = "windows")]
    pub fn run_blocking(self, runtime: Handle) -> Result<()> {
        self.run_blocking_until(runtime, Arc::new(AtomicBool::new(false)))
    }

    /// Run the Windows blocking DNS loop until `stop` is set.
    #[cfg(target_os = "windows")]
    pub fn run_blocking_until(self, runtime: Handle, stop: Arc<AtomicBool>) -> Result<()> {
        let sock = StdUdpSocket::bind(self.listen_addr)?;
        sock.set_read_timeout(Some(Duration::from_millis(200)))?;
        tracing::info!("overlay DNS listening on {}", self.listen_addr);

        let mut buf = vec![0u8; 512];
        while !stop.load(Ordering::Relaxed) {
            let (len, src) = match sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    tracing::debug!("overlay DNS receive wait timed out");
                    continue;
                }
                Err(e) => {
                    tracing::debug!("DNS recv error: {}", e);
                    continue;
                }
            };

            tracing::debug!("overlay DNS query received from {}", src);
            let req_bytes = &buf[..len];
            let response = match runtime.block_on(self.handle_query(req_bytes)) {
                Some(resp) => resp,
                None => continue,
            };

            if let Err(e) = sock.send_to(&response, src) {
                tracing::debug!("DNS send error to {}: {}", src, e);
            }
        }
        tracing::debug!("overlay DNS blocking loop exiting");
        Ok(())
    }

    async fn handle_query(&self, data: &[u8]) -> Option<Vec<u8>> {
        // Fast path: resolve against the current table.
        {
            let services = self.services.read().await;
            if !has_unresolved_overlay_query(data, &services) {
                return handle_query_with_services(data, &services);
            }
        }

        let current_policy =
            DnsFirstHitPolicy::from_u8(self.first_hit_policy.load(Ordering::Relaxed));
        if matches!(
            current_policy,
            DnsFirstHitPolicy::ProactiveVip | DnsFirstHitPolicy::ProactiveVipHold
        ) {
            let (reserved, table) = {
                let mut services = self.services.write().await;
                let reserved = reserve_unresolved_overlay_queries(
                    data,
                    &mut services,
                    current_policy == DnsFirstHitPolicy::ProactiveVipHold,
                );
                (reserved, services.clone())
            };
            if reserved {
                if let Some(rescan) = &self.dns_rescan {
                    rescan.notify_one();
                }
                if let Some(stack_updates) = &self.stack_updates {
                    let _ = stack_updates.try_send(StackCommand::UpdateServices(Box::new(table)));
                }
            }
            let services = self.services.read().await;
            return handle_query_with_services(data, &services);
        }

        // The query is for a `*.portzero.local` name we don't know yet. If the
        // rescan signals are wired up, trigger an immediate discovery rescan and
        // briefly hold the query open, so a service that was just started resolves
        // on the first attempt rather than returning NXDOMAIN.
        if let (Some(rescan), Some(updated)) = (&self.dns_rescan, &self.dns_updated) {
            rescan.notify_one();
            let _ = tokio::time::timeout(DNS_FIRST_HIT_WAIT, updated.notified()).await;
        }

        let services = self.services.read().await;
        handle_query_with_services(data, &services)
    }
}

/// True if `data` is a query containing at least one `*.portzero.local` A-record
/// question that does not currently resolve to a VIP. Used to decide whether to
/// trigger a rescan and hold the query open.
fn has_unresolved_overlay_query(data: &[u8], services: &ServiceTable) -> bool {
    let msg = match Message::from_bytes(data) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if msg.metadata.op_code != OpCode::Query {
        return false;
    }
    msg.queries.iter().any(|q| {
        q.query_class() == DNSClass::IN
            && q.query_type() == RecordType::A
            && is_portzero_local(q.name())
            && resolve_name_to_vip(q.name(), services).is_none()
    })
}

fn reserve_unresolved_overlay_queries(
    data: &[u8],
    services: &mut ServiceTable,
    hold_connections: bool,
) -> bool {
    let msg = match Message::from_bytes(data) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if msg.metadata.op_code != OpCode::Query {
        return false;
    }

    let mut reserved = false;
    for q in &msg.queries {
        if q.query_class() == DNSClass::IN
            && q.query_type() == RecordType::A
            && is_portzero_local(q.name())
            && resolve_name_to_vip(q.name(), services).is_none()
        {
            if let Some(label) = service_label(q.name()) {
                services.reserve_name_on_port(&label, 80, hold_connections);
                reserved = true;
            }
        }
    }
    reserved
}

fn handle_query_with_services(data: &[u8], services: &ServiceTable) -> Option<Vec<u8>> {
    let msg = match Message::from_bytes(data) {
        Ok(m) => m,
        Err(_) => return None,
    };

    if msg.metadata.op_code != OpCode::Query {
        return None;
    }

    let mut out = Message::new(msg.metadata.id, MessageType::Response, OpCode::Query);
    out.metadata.recursion_desired = msg.metadata.recursion_desired;
    out.metadata.recursion_available = true;
    out.metadata.authoritative = true;
    out.metadata.response_code = ResponseCode::NoError;

    for q in &msg.queries {
        let name = q.name().clone();
        if q.query_class() != DNSClass::IN {
            out.metadata.response_code = ResponseCode::NotImp;
            continue;
        }

        if q.query_type() == RecordType::A {
            if let Some(ip) = resolve_name_to_vip(&name, services) {
                let rec = Record::from_rdata(name.clone(), 5, RData::A(ip.into()));
                out.add_answer(rec);
                out.metadata.response_code = ResponseCode::NoError;
            } else if is_portzero_local(&name) {
                out.metadata.response_code = ResponseCode::NXDomain;
            }
        } else if is_portzero_local(&name) && resolve_name_to_vip(&name, services).is_none() {
            out.metadata.response_code = ResponseCode::NXDomain;
        }
    }

    // Add the original question
    for q in &msg.queries {
        out.add_query(q.clone());
    }

    // On a negative answer for our own zone, attach a zero-TTL SOA so resolvers
    // do not negatively cache the miss. A name may be unknown simply because the
    // service hasn't been discovered yet (an immediate rescan is already in
    // flight), so the client's next attempt must hit us again rather than a
    // cached NXDOMAIN.
    if out.metadata.response_code == ResponseCode::NXDomain {
        out.add_authority(portzero_zero_ttl_soa());
    }

    out.to_bytes().ok()
}

/// SOA record for the `portzero.local` zone with a zero `minimum` (and zero TTL)
/// so negative responses are not cached by downstream resolvers (RFC 2308).
fn portzero_zero_ttl_soa() -> Record {
    let zone = Name::from_ascii("portzero.local.").unwrap_or_else(|_| Name::root());
    let mname = zone.clone();
    let rname = Name::from_ascii("hostmaster.portzero.local.").unwrap_or_else(|_| Name::root());
    let soa = SOA::new(mname, rname, 1, 3600, 600, 86400, 0);
    Record::from_rdata(zone, 0, RData::SOA(soa))
}

fn is_portzero_local(name: &Name) -> bool {
    // `Name::to_ascii()` renders a fully-qualified name with a trailing dot
    // (e.g. `foo.portzero.local.`), so strip it before matching — otherwise real
    // resolver queries (which are always FQDNs) would never be recognised as
    // belonging to our zone.
    let s = name.to_ascii();
    let s = s.strip_suffix('.').unwrap_or(&s);
    s.ends_with(".portzero.local") || s == "portzero.local"
}

/// Given a DNS name like "my-db.portzero.local", look up the corresponding VIP.
fn resolve_name_to_vip(name: &Name, services: &ServiceTable) -> Option<Ipv4Addr> {
    let labels: Vec<_> = name
        .iter()
        .map(|l| std::str::from_utf8(l).unwrap_or(""))
        .collect();

    // Special case: "portzero.local" (2 labels) resolves to the management VIP
    // directly without a service table lookup, so it works before registration.
    if labels.len() == 2 && labels[0] == "portzero" && labels[1] == "local" {
        return Some(MANAGEMENT_VIP);
    }

    // Special case: "api.portzero.local" (3 labels) resolves to the management REST API VIP.
    if labels.len() == 3 && labels[0] == "api" && labels[1] == "portzero" && labels[2] == "local" {
        return Some(API_MANAGEMENT_VIP);
    }

    if let Some(candidate) = service_label(name) {
        return services.assigned_vip(&candidate).map(|vip| {
            // smoltcp Ipv4Address can be converted via its Display or as_bytes in 0.11
            let b: [u8; 4] = vip.0;
            std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3])
        });
    }

    None
}

fn service_label(name: &Name) -> Option<String> {
    let labels: Vec<_> = name
        .iter()
        .map(|l| std::str::from_utf8(l).unwrap_or(""))
        .collect();
    if labels.len() >= 3
        && labels[labels.len() - 2] == "portzero"
        && labels[labels.len() - 1] == "local"
    {
        let prefix = labels[..labels.len() - 2]
            .iter()
            .map(|label| label.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(".");
        (!prefix.is_empty()).then_some(prefix)
    } else {
        None
    }
}

/// Helper to update the shared service table from outside.
pub async fn update_dns_services(
    dns_services: &Arc<RwLock<ServiceTable>>,
    new_table: ServiceTable,
) {
    let mut guard = dns_services.write().await;
    *guard = new_table;
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::Query;

    fn a_query_bytes(name: &str) -> Vec<u8> {
        let mut q = Query::new();
        q.set_name(Name::from_ascii(name).unwrap());
        q.set_query_type(RecordType::A);
        q.set_query_class(DNSClass::IN);
        let mut msg = Message::new(1, MessageType::Query, OpCode::Query);
        msg.add_query(q);
        msg.to_bytes().unwrap()
    }

    fn table_with(name: &str) -> ServiceTable {
        let mut t = ServiceTable::new();
        t.register(
            name.to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            80,
            1234,
        );
        t
    }

    fn first_a_record(resp: &[u8]) -> Option<Ipv4Addr> {
        let msg = Message::from_bytes(resp).unwrap();
        msg.answers.iter().find_map(|r| match &r.data {
            RData::A(ip) => Some(Ipv4Addr::from(*ip)),
            _ => None,
        })
    }

    #[test]
    fn unresolved_overlay_query_detects_unknown_subdomain() {
        let empty = ServiceTable::new();
        let q = a_query_bytes("foo.portzero.local.");
        // Unknown service -> should be flagged for a rescan.
        assert!(has_unresolved_overlay_query(&q, &empty));
        // Once registered, it resolves and must NOT trigger a rescan.
        assert!(!has_unresolved_overlay_query(&q, &table_with("foo")));
    }

    #[test]
    fn unresolved_overlay_query_ignores_non_overlay_and_management_names() {
        let empty = ServiceTable::new();
        // Names outside the zone are never our concern.
        assert!(!has_unresolved_overlay_query(
            &a_query_bytes("example.com."),
            &empty
        ));
        // The pre-seeded management name resolves without registration.
        assert!(!has_unresolved_overlay_query(
            &a_query_bytes("portzero.local."),
            &empty
        ));
    }

    #[test]
    fn nxdomain_for_unknown_subdomain_carries_zero_ttl_soa() {
        let resp = handle_query_with_services(
            &a_query_bytes("ghost.portzero.local."),
            &ServiceTable::new(),
        )
        .expect("response");
        let msg = Message::from_bytes(&resp).unwrap();
        assert_eq!(msg.metadata.response_code, ResponseCode::NXDomain);
        let soa = msg
            .authorities
            .iter()
            .find(|r| r.record_type() == RecordType::SOA)
            .expect("NXDOMAIN must include an SOA so the miss is not negatively cached");
        assert_eq!(soa.ttl, 0, "SOA TTL must be 0 to disable negative caching");
    }

    #[test]
    fn known_subdomain_resolves_to_its_vip() {
        let resp =
            handle_query_with_services(&a_query_bytes("foo.portzero.local."), &table_with("foo"))
                .expect("response");
        let msg = Message::from_bytes(&resp).unwrap();
        assert_eq!(msg.metadata.response_code, ResponseCode::NoError);
        assert!(
            msg.answers.iter().any(|r| r.record_type() == RecordType::A),
            "a registered service must return an A record"
        );
    }

    #[test]
    fn multi_label_subdomain_resolves_by_full_prefix() {
        let mut table = ServiceTable::new();
        let nested = table.register(
            "staging.portzero.net".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            80,
            1234,
        );
        let short = table.register(
            "staging".to_string(),
            "127.0.0.1:9001".parse().unwrap(),
            80,
            1235,
        );

        let nested_resp = handle_query_with_services(
            &a_query_bytes("staging.portzero.net.portzero.local."),
            &table,
        )
        .expect("nested response");
        let short_resp =
            handle_query_with_services(&a_query_bytes("staging.portzero.local."), &table)
                .expect("short response");

        let nested_ip = first_a_record(&nested_resp).expect("nested A record");
        let short_ip = first_a_record(&short_resp).expect("short A record");
        assert_eq!(nested_ip.octets(), nested.vip.0);
        assert_eq!(short_ip.octets(), short.vip.0);
        assert_ne!(nested_ip, short_ip);
    }

    #[test]
    fn unresolved_overlay_query_uses_full_prefix_before_zone() {
        let mut table = ServiceTable::new();
        table.register(
            "staging".to_string(),
            "127.0.0.1:9001".parse().unwrap(),
            80,
            1235,
        );

        assert!(has_unresolved_overlay_query(
            &a_query_bytes("staging.portzero.net.portzero.local."),
            &table
        ));

        table.register(
            "staging.portzero.net".to_string(),
            "127.0.0.1:9000".parse().unwrap(),
            80,
            1234,
        );
        assert!(!has_unresolved_overlay_query(
            &a_query_bytes("staging.portzero.net.portzero.local."),
            &table
        ));
    }

    #[test]
    fn management_names_resolve_to_reserved_vips_even_if_registered() {
        let mut table = ServiceTable::new();
        let service = table.register(
            "api".to_string(),
            "127.0.0.1:9001".parse().unwrap(),
            80,
            1235,
        );

        let root_resp = handle_query_with_services(&a_query_bytes("portzero.local."), &table)
            .expect("management response");
        let api_resp = handle_query_with_services(&a_query_bytes("api.portzero.local."), &table)
            .expect("api response");
        let user_resp =
            handle_query_with_services(&a_query_bytes("api.foo.portzero.local."), &table)
                .expect("user response");

        assert_eq!(first_a_record(&root_resp), Some(MANAGEMENT_VIP));
        assert_eq!(first_a_record(&api_resp), Some(API_MANAGEMENT_VIP));
        assert_ne!(
            first_a_record(&api_resp),
            Some(Ipv4Addr::from(service.vip.0))
        );
        assert_eq!(
            Message::from_bytes(&user_resp)
                .unwrap()
                .metadata
                .response_code,
            ResponseCode::NXDomain
        );
    }

    #[tokio::test]
    async fn proactive_first_hit_reserves_and_answers_vip() {
        let services = Arc::new(RwLock::new(ServiceTable::new()));
        let server = OverlayDnsServer::new(services.clone(), "127.0.0.1:0".parse().unwrap())
            .with_first_hit_policy(DnsFirstHitPolicy::ProactiveVip);

        let resp = server
            .handle_query(&a_query_bytes("fresh.portzero.local."))
            .await
            .expect("response");
        let msg = Message::from_bytes(&resp).unwrap();
        assert_eq!(msg.metadata.response_code, ResponseCode::NoError);
        assert!(
            msg.answers.iter().any(|r| r.record_type() == RecordType::A),
            "proactive mode must answer with the reserved VIP immediately"
        );

        let guard = services.read().await;
        assert!(guard.get("fresh").is_none());
        assert!(guard.assigned_vip("fresh").is_some());
    }

    #[tokio::test]
    async fn proactive_first_hit_reserves_full_prefix() {
        let services = Arc::new(RwLock::new(ServiceTable::new()));
        let server = OverlayDnsServer::new(services.clone(), "127.0.0.1:0".parse().unwrap())
            .with_first_hit_policy(DnsFirstHitPolicy::ProactiveVip);

        let resp = server
            .handle_query(&a_query_bytes("staging.portzero.net.portzero.local."))
            .await
            .expect("response");
        let msg = Message::from_bytes(&resp).unwrap();
        assert_eq!(msg.metadata.response_code, ResponseCode::NoError);

        let guard = services.read().await;
        assert!(guard.assigned_vip("staging").is_none());
        assert!(guard.assigned_vip("staging.portzero.net").is_some());
    }
}
