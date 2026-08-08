//! Propagating discovered `RouteChanges` to the domain router, the cloud
//! connector, and the log.

use portzero_tunnel_client::domain_router::DomainRouter;

use crate::cloud::CloudConnector;
use crate::route_table::{Route, RouteChanges};

/// The address to forward this route's traffic to.
///
/// `Route::host` is persisted as text, so a `routes.json` written by an older
/// daemon (or hand-edited) may not parse; IPv4 loopback is the historical
/// value and the right fallback.
pub(super) fn route_backend_addr(route: &Route) -> std::net::SocketAddr {
    let host = route
        .host
        .parse()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    std::net::SocketAddr::new(host, route.port)
}

/// Update the domain router with route changes.
pub(super) fn update_domain_router(router: &DomainRouter, changes: &RouteChanges) {
    for route in &changes.added {
        router.add_route(route.domain.clone(), route_backend_addr(route));
    }
    for route in &changes.removed {
        router.remove_route(&route.domain);
    }
    for route in &changes.changed {
        router.add_route(route.domain.clone(), route_backend_addr(route));
    }
}

/// Sync route changes with the cloud connector.
pub(super) async fn sync_cloud_routes(connector: &CloudConnector, changes: &RouteChanges) {
    for route in &changes.added {
        if let Err(e) = connector
            .register_route(
                &route.domain,
                route.port,
                Some(crate::cloud::build_route_metadata(route)),
            )
            .await
        {
            tracing::warn!(
                "Failed to register route {} with cloud: {}",
                route.domain,
                e
            );
        }
    }
    for route in &changes.removed {
        if let Err(e) = connector.unregister_route(&route.domain).await {
            tracing::warn!(
                "Failed to unregister route {} from cloud: {}",
                route.domain,
                e
            );
        }
    }
    for route in &changes.changed {
        // Re-register with updated port
        if let Err(e) = connector
            .register_route(
                &route.domain,
                route.port,
                Some(crate::cloud::build_route_metadata(route)),
            )
            .await
        {
            tracing::warn!("Failed to update route {} with cloud: {}", route.domain, e);
        }
    }
}

/// Log route changes to tracing.
pub(super) fn log_changes(changes: &RouteChanges) {
    for route in &changes.added {
        tracing::info!(
            "Route added: {} -> {}:{} ({})",
            route.domain,
            route.host,
            route.port,
            route.source
        );
    }
    for route in &changes.removed {
        tracing::info!(
            "Route removed: {} (was {}:{})",
            route.domain,
            route.host,
            route.port
        );
    }
    for route in &changes.changed {
        tracing::info!(
            "Route changed: {} -> {}:{} ({})",
            route.domain,
            route.host,
            route.port,
            route.source
        );
    }
}
