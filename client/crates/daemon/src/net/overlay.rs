//! High-level overlay network manager.
//!
//! Starts the TUN device, the smoltcp TCP stack, and the embedded DNS server.
//! It receives service updates (from discovery) and keeps the virtual network
//! in sync with services that set a full `*.portzero.local` name via PZ_TUNNEL.

use std::net::{IpAddr, SocketAddr};
#[cfg(target_os = "windows")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Notify, RwLock};

use crate::net::dns::{DnsFirstHitPolicy, OverlayDnsServer};
use crate::net::resolver_config;
use crate::net::service_table::ServiceTable;
use crate::net::stack::{OverlayHttpsPolicy, VirtualStack};
use crate::net::tun_device::{TunConfig, TunDevice};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::net::virtual_ip::gateway_ip;
use crate::tls::{stack as tls_stack, trust, LocalCa};

/// macOS loopback port for the embedded DNS server. A high, non-privileged
/// port that won't collide with anything; change here if it ever conflicts.
/// (Must NOT be 53 — the macOS `/etc/resolver` file carries the port, so any
/// port works as long as it matches what the server binds.)
#[cfg(target_os = "macos")]
const MACOS_DNS_LOOPBACK_PORT: u16 = 10053;

/// The default address the embedded DNS server listens on AND that the scoped
/// OS resolver is pointed at.
///
/// **Linux / others:** the TUN **gateway** address on port **53**
/// (e.g. `10.254.0.1:53`). systemd-resolved sends per-link DNS queries via the
/// TUN link (`deven0`), where loopback (`127.0.0.1`) is unreachable but the
/// gateway — the TUN's own address — is reachable; and resolve1's `SetLinkDNS`
/// carries no port, so resolved always queries port 53 (hence the server must
/// listen on 53, not 5300). The dnsmasq fallback path carries the port too and
/// reaches the gateway:53 just the same.
///
/// **macOS exception:** the utun is point-to-point and has NO local route for
/// the gateway IP, so a packet to `10.254.0.1:53` routes OUT the tunnel instead
/// of reaching the local `UdpSocket` (queries time out — see task-33). macOS
/// therefore binds to loopback (`127.0.0.1:<MACOS_DNS_LOOPBACK_PORT>`), which is
/// always locally deliverable; the `/etc/resolver/portzero.local` file carries the
/// matching `port` line so the system resolver finds it.
///
/// **Windows exception:** Wintun does not make the configured gateway address
/// valid for a local UDP bind in the same way Linux does. Bind the embedded DNS
/// server to all IPv4 interfaces on port 53, and point the NRPT rule at
/// loopback. This avoids Windows filtering quirks seen with a loopback-only
/// DNS listener while keeping the resolver target local.
fn default_dns_listen() -> SocketAddr {
    #[cfg(target_os = "macos")]
    {
        SocketAddr::new(
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            MACOS_DNS_LOOPBACK_PORT,
        )
    }
    #[cfg(target_os = "windows")]
    {
        SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 53)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        SocketAddr::new(IpAddr::V4(gateway_ip()), 53)
    }
}

/// The address the scoped OS resolver is pointed at with the default overlay
/// configuration — and, equivalently, the address a client can send
/// `*.portzero.local` DNS queries to and reach the embedded DNS server.
///
/// macOS: `127.0.0.1:10053`; Linux/others: the TUN gateway `10.254.0.1:53`;
/// Windows: `127.0.0.1:53`. Exposed for the `portzero doctor` diagnostics
/// command, which probes the embedded DNS server directly and inspects the
/// scoped resolver config, both of which need this exact address.
pub fn default_resolver_dns_addr() -> SocketAddr {
    resolver_dns_addr(default_dns_listen())
}

fn resolver_dns_addr(listen_addr: SocketAddr) -> SocketAddr {
    #[cfg(target_os = "windows")]
    {
        if listen_addr.ip().is_unspecified() {
            return SocketAddr::new(
                IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                listen_addr.port(),
            );
        }
    }
    listen_addr
}

/// Configuration for the overlay network.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// Address the embedded DNS server listens on AND that the scoped OS resolver
    /// is pointed at. See [`default_dns_listen`] for the per-platform default and
    /// the macOS loopback exception.
    pub dns_listen: SocketAddr,
    /// TUN device configuration.
    pub tun: TunConfig,
    /// HTTPS behavior for services exposed through the local overlay.
    pub https_policy: OverlayHttpsPolicy,
    /// DNS behavior for first queries to unknown local overlay names.
    pub dns_first_hit_policy: DnsFirstHitPolicy,
    /// Whether startup should install the local CA into the host trust stores.
    /// Production startup keeps this enabled; e2e tests that exercise only plain
    /// TCP can disable it to avoid unrelated OS trust-store side effects.
    pub install_trust: bool,
    /// When present, the stack records observed edges + exercised routes for
    /// proxied overlay connections here (task-91). The daemon passes the same
    /// store the cloud connector uses; tests/benches leave it `None`.
    pub observations: Option<Arc<crate::observations::ObservationStore>>,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            dns_listen: default_dns_listen(),
            tun: TunConfig::default(),
            https_policy: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
            install_trust: true,
            observations: None,
        }
    }
}

/// The running overlay network.
pub struct OverlayNetwork {
    services: Arc<RwLock<ServiceTable>>,
    stack: VirtualStack,
    dns_task: tokio::task::JoinHandle<()>,
    #[cfg(target_os = "windows")]
    dns_stop: Arc<AtomicBool>,
    /// Name of the overlay's TUN link (e.g. `deven0`). The scoped resolver is
    /// attached to this real link, so teardown must revert the same one.
    link_name: String,
    /// Address the scoped OS resolver points at (our embedded DNS server). Kept
    /// so [`OverlayNetwork::ensure_resolver_installed`] can recreate the resolver
    /// with the same target if it is removed out-of-band after startup.
    resolver_addr: SocketAddr,
    /// Local CA and wildcard cert for `*.portzero.local`.  Held here so the
    /// TLS stack integration (see `tls::stack`) can consume it without
    /// re-generating.
    pub ca: LocalCa,
    /// Notified (via `notify_waiters`) after every service-table update so any
    /// DNS query currently held open waiting for a just-started service can
    /// re-resolve immediately. Paired with the `dns_rescan` signal the DNS
    /// server fires on a miss.
    dns_updated: Arc<Notify>,
    /// Atomic holder for the live-reloadable DNS first-hit policy. Shared with
    /// the DNS server task; updated by the config poller in the discovery loop.
    dns_first_hit_policy: Arc<AtomicU8>,
}

/// Startup phase reported by [`OverlayNetwork::start_with_progress`].
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayStartStep {
    LoadingLocalCa,
    InstallingTrust,
    CreatingTunDevice,
    BuildingTlsConfig,
    SpawningVirtualStack,
    StartingDnsServer,
    InstallingResolverConfig,
    Complete,
}

impl OverlayNetwork {
    /// Start the overlay (TUN + TCP stack + DNS).
    ///
    /// `dns_rescan` is notified by the embedded DNS server whenever it sees a
    /// query for an unknown `*.portzero.local` name, so the discovery loop can
    /// rescan immediately instead of waiting for its next poll.
    ///
    /// This must be called with sufficient privileges.
    pub async fn start(config: OverlayConfig, dns_rescan: Arc<Notify>) -> Result<Self> {
        Self::start_with_progress(config, dns_rescan, |_| {}).await
    }

    /// Start the overlay and report progress through the blocking/synchronous
    /// setup phases. Intended for diagnostics in privileged E2E tests.
    #[doc(hidden)]
    pub async fn start_with_progress(
        config: OverlayConfig,
        dns_rescan: Arc<Notify>,
        progress: impl Fn(OverlayStartStep),
    ) -> Result<Self> {
        // Generate (or load) the local CA and wildcard cert for *.portzero.local.
        // Fatal: without this, HTTPS termination cannot start.
        progress(OverlayStartStep::LoadingLocalCa);
        let ca = LocalCa::load_or_create()?;

        // Install the CA into OS trust stores so browsers accept the cert.
        // Non-fatal: the overlay still works for plain HTTP if this fails.
        if config.install_trust {
            progress(OverlayStartStep::InstallingTrust);
            if let Err(e) = trust::install(&LocalCa::ca_cert_path()?) {
                tracing::warn!(
                    "trust store installation failed (HTTPS may show cert warnings): {e:#}"
                );
            }
        } else {
            tracing::info!("skipping local CA trust installation by overlay configuration");
        }

        // Create the TUN device first (this may require root).
        progress(OverlayStartStep::CreatingTunDevice);
        let tun = TunDevice::create(&config.tun)?;
        // Capture the actual link name (the kernel may pick a different unit
        // number than requested) before the device is moved into the stack. The
        // scoped resolver is attached to THIS real link, not `lo`.
        let link_name = tun.name().to_string();

        // Build the rustls ServerConfig from the wildcard cert. Non-fatal: if
        // this fails the stack still runs, just without HTTPS on port 443.
        progress(OverlayStartStep::BuildingTlsConfig);
        let tls_config = match tls_stack::build_server_config(&ca) {
            Ok(c) => {
                tracing::info!("TLS termination enabled for *.portzero.local on port 443");
                Some(c)
            }
            Err(e) => {
                tracing::warn!("could not build TLS server config, port 443 disabled: {e:#}");
                None
            }
        };

        // Shared service table
        let services: Arc<RwLock<ServiceTable>> = Arc::new(RwLock::new(ServiceTable::new()));

        // Start the TCP stack
        let initial = ServiceTable::new();
        progress(OverlayStartStep::SpawningVirtualStack);
        let stack = VirtualStack::spawn(
            tun,
            initial,
            tls_config,
            config.https_policy,
            config.observations.clone(),
        )
        .await?;

        // Start DNS server. It pings `dns_rescan` on a miss and waits on
        // `dns_updated` for the table to refresh so the first query for a
        // just-started service can succeed instead of returning NXDOMAIN.
        progress(OverlayStartStep::StartingDnsServer);
        let dns_updated = Arc::new(Notify::new());
        let dns_server = OverlayDnsServer::new(services.clone(), config.dns_listen)
            .with_first_hit_policy(config.dns_first_hit_policy)
            .with_stack_updates(stack.command_sender())
            .with_rescan(dns_rescan, dns_updated.clone());
        let dns_first_hit_policy = dns_server.first_hit_policy_holder();
        #[cfg(target_os = "windows")]
        let runtime = tokio::runtime::Handle::current();
        #[cfg(target_os = "windows")]
        let dns_stop = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "windows")]
        let dns_stop_task = dns_stop.clone();
        #[cfg(target_os = "windows")]
        let dns_task = tokio::task::spawn_blocking(move || {
            if let Err(e) = dns_server.run_blocking_until(runtime, dns_stop_task) {
                tracing::error!("overlay DNS server exited: {}", e);
            }
        });
        #[cfg(not(target_os = "windows"))]
        let dns_task = tokio::spawn(async move {
            if let Err(e) = dns_server.run().await {
                tracing::error!("overlay DNS server exited: {}", e);
            }
        });

        // Install scoped OS resolver — routes *.portzero.local to our DNS server.
        // Attached to the TUN link (created above), so on Linux this works even
        // when systemd-networkd is absent. Log errors but do not abort startup;
        // the overlay still works for services that manually configure DNS.
        let resolver_addr = resolver_dns_addr(config.dns_listen);
        progress(OverlayStartStep::InstallingResolverConfig);
        if let Err(e) = resolver_config::install(resolver_addr, &link_name).await {
            tracing::warn!(
                "scoped resolver setup failed (may need elevated privileges): {:#}",
                e
            );
        }
        progress(OverlayStartStep::Complete);

        Ok(Self {
            services,
            stack,
            dns_task,
            #[cfg(target_os = "windows")]
            dns_stop,
            link_name,
            resolver_addr,
            ca,
            dns_updated,
            dns_first_hit_policy,
        })
    }

    /// Push an updated service table into the overlay (called by discovery).
    pub async fn update_services(&self, table: ServiceTable) -> Result<()> {
        let mut table = table;
        // Update DNS view
        {
            let mut guard = self.services.write().await;
            table.preserve_assignments_from(&guard);
            *guard = table.clone();
        }

        // Tell the TCP stack
        self.stack.update_services(table).await?;

        // Wake any DNS query currently held open waiting for a new service to
        // appear so it can re-resolve against the fresh table right away.
        self.dns_updated.notify_waiters();
        Ok(())
    }

    /// Apply an updated HTTPS policy to the running virtual stack.
    ///
    /// Safe to call while the daemon is serving traffic: only future
    /// connection *accept* decisions and the set of listening ports are
    /// affected. In-progress proxied sessions are not torn down.
    pub async fn update_https_policy(&self, policy: OverlayHttpsPolicy) -> Result<()> {
        self.stack.update_https_policy(policy).await
    }

    /// Apply an updated DNS first-hit policy live.
    ///
    /// Only affects how the DNS server reacts to queries for names that are
    /// not yet in the service table; has no effect on established resolutions
    /// or open connections.
    pub fn update_dns_first_hit_policy(&self, policy: DnsFirstHitPolicy) {
        self.dns_first_hit_policy
            .store(DnsFirstHitPolicy::to_u8(policy), Ordering::Relaxed);
    }

    /// Re-check that the scoped OS resolver for `*.portzero.local` is still in
    /// place and recreate it if it was removed out-of-band (e.g. a user or
    /// another tool deleted `/etc/resolver/portzero.local`). Best-effort and
    /// cheap enough to call periodically; see [`ResolverCheck`] for outcomes.
    pub async fn ensure_resolver_installed(&self) -> ResolverCheck {
        match resolver_config::status(self.resolver_addr) {
            resolver_config::ResolverStatus::Present | resolver_config::ResolverStatus::Unknown => {
                ResolverCheck::Ok
            }
            resolver_config::ResolverStatus::Missing => {
                tracing::warn!(
                    "scoped resolver for *.portzero.local was removed after startup; \
                     recreating it (until then, *.portzero.local names will not resolve)"
                );
                match resolver_config::install(self.resolver_addr, &self.link_name).await {
                    Ok(()) => {
                        tracing::info!("scoped resolver for *.portzero.local recreated");
                        ResolverCheck::Repaired
                    }
                    Err(e) => {
                        tracing::warn!(
                            "failed to recreate scoped resolver (may need elevated privileges): {e:#}"
                        );
                        ResolverCheck::RepairFailed
                    }
                }
            }
        }
    }

    /// Shutdown the overlay components.
    pub async fn shutdown(&self) {
        // Remove the scoped OS resolver before tearing down DNS.
        if let Err(e) = resolver_config::uninstall(&self.link_name).await {
            tracing::warn!("scoped resolver teardown failed: {:#}", e);
        }

        let _ = self.stack.shutdown().await;
        #[cfg(target_os = "windows")]
        self.dns_stop.store(true, Ordering::Relaxed);
        self.dns_task.abort();
    }

    /// Return the actual TUN link name chosen by the kernel/driver.
    ///
    /// The real E2E test uses this to install a host route on platforms that
    /// need an explicit per-VIP route to reach the overlay from the host OS.
    #[doc(hidden)]
    pub fn link_name(&self) -> &str {
        &self.link_name
    }
}

/// Outcome of [`OverlayNetwork::ensure_resolver_installed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverCheck {
    /// The scoped resolver is present, or its presence can't be determined on
    /// this platform — nothing to do.
    Ok,
    /// The scoped resolver had been removed and was successfully recreated.
    Repaired,
    /// The scoped resolver had been removed but recreation failed (e.g. the
    /// daemon no longer has the privileges it held at startup).
    RepairFailed,
}

/// Ensure the scoped OS resolver for `*.portzero.local` is present, for use by
/// `portzero setup` so name resolution works from install time rather than only
/// after the daemon's first privileged start.
///
/// On macOS this writes `/etc/resolver/portzero.local` pointing at the daemon's
/// loopback DNS listener. On Linux/Windows the scoped resolver is bound to the
/// running overlay's TUN link / NRPT rule and is installed by the daemon at
/// startup, so there is nothing a one-shot setup command can pre-create and this
/// is a no-op.
pub async fn ensure_scoped_resolver_for_setup() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let addr = resolver_dns_addr(default_dns_listen());
        resolver_config::install(addr, "").await
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On macOS the default DNS server must bind to loopback (not the overlay
    /// gateway), because the point-to-point utun has no local route for the
    /// gateway IP — so a query to `10.254.0.1:53` routes into the tunnel and the
    /// real `UdpSocket` never sees it (task-33).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_dns_listen_is_loopback() {
        let addr = default_dns_listen();
        assert_eq!(addr.ip(), IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        assert_ne!(
            addr.ip(),
            IpAddr::V4(gateway_ip_for_test()),
            "must not bind the DNS server to the unreachable overlay gateway"
        );
        assert_eq!(addr.port(), MACOS_DNS_LOOPBACK_PORT);
        assert_ne!(addr.port(), 53, "must not use privileged port 53 on macOS");
    }

    /// The scoped `/etc/resolver/portzero.local` file written from the macOS
    /// default must point the system resolver at that same loopback address,
    /// including the matching `port` line.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_resolver_file_points_at_loopback_default() {
        let addr = default_dns_listen();
        let content = crate::net::resolver_config::macos_resolver_file_content(addr);
        assert!(
            content.contains("nameserver 127.0.0.1\n"),
            "resolver file must point at loopback: {content}"
        );
        assert!(
            content.contains(&format!("port {MACOS_DNS_LOOPBACK_PORT}\n")),
            "resolver file must carry the loopback port: {content}"
        );
    }

    /// On Windows the default listener must use port 53: NRPT does not carry a
    /// port, and binding to the Wintun gateway address fails with WSAEADDRNOTAVAIL.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_default_dns_listen_is_wildcard_53() {
        let addr = default_dns_listen();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 53)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_resolver_points_at_loopback_for_wildcard_listener() {
        let listen = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 53);
        assert_eq!(
            resolver_dns_addr(listen),
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 53)
        );
    }

    /// On Linux (and other non-macOS/non-Windows targets) the default is unchanged: the
    /// overlay gateway on port 53, which the kernel makes locally reachable via
    /// the interface route.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn non_macos_non_windows_default_dns_listen_is_gateway_53() {
        let addr = default_dns_listen();
        assert_eq!(addr, SocketAddr::new(IpAddr::V4(gateway_ip()), 53));
    }

    /// The gateway IP, for asserting the macOS default does NOT use it.
    #[cfg(target_os = "macos")]
    fn gateway_ip_for_test() -> std::net::Ipv4Addr {
        crate::net::virtual_ip::gateway_ip()
    }
}
