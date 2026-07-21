//! Cloud edge connection lifecycle: initial connect, token re-check and
//! refresh, auth-failure handling, and connect/reconnect with route re-registration.

use std::sync::Arc;

use portzero_tunnel_client::domain_router::DomainRouter;

use crate::auth::AuthConfig;
use crate::cloud::CloudConnector;
use crate::route_table::RouteTable;

use super::cloud_state::{read_cloud_error, write_cloud_state};
use super::config::DaemonConfig;
use super::{ReconnectBackoff, TOKEN_REFRESH_THRESHOLD_SECS};

/// Attempt the daemon's first cloud connection from the loaded auth token,
/// writing the resulting state to `cloud_state.json`. Returns the live connector,
/// or `None` when there is no token or the connect failed (local-only mode).
pub(super) async fn initial_cloud_connect(
    config: &DaemonConfig,
    current_token: &Option<String>,
    domain_router: &DomainRouter,
    observations: &Arc<crate::observations::ObservationStore>,
) -> Option<CloudConnector> {
    let Some(token) = current_token else {
        tracing::info!("No auth token found, running in local-only mode");
        write_cloud_state(config, false, None, None, None, None);
        return None;
    };
    tracing::info!("Auth token found, connecting to cloud edge");
    let mut connector = CloudConnector::new(token.clone());
    connector.set_status_path(config.cloud_route_status_path());
    connector.set_observations(observations.clone());
    match connector.connect(domain_router.clone()).await {
        Ok(()) => {
            let p = connector.plan();
            let c = connector.can_use_cloud_tunnels();
            let m = connector.status_message();
            write_cloud_state(config, true, None, p, c, m);
            Some(connector)
        }
        Err(e) => {
            let err_msg = e.root_cause().to_string();
            tracing::warn!(
                "Failed to connect to cloud edge: {}. Running in local-only mode.",
                e
            );
            write_cloud_state(config, false, Some(err_msg), None, None, None);
            None
        }
    }
}

/// Register routes persisted from a previous daemon session with a freshly
/// connected cloud edge, so they go live immediately instead of waiting for the
/// next scan cycle to detect a "change".
pub(super) async fn register_persisted_routes(
    cloud: &Option<CloudConnector>,
    route_table: &RouteTable,
) {
    let Some(connector) = cloud else { return };
    for (domain, route) in &route_table.routes {
        if let Err(e) = connector
            .register_route(
                domain,
                route.port,
                Some(crate::cloud::build_route_metadata(route)),
            )
            .await
        {
            tracing::warn!(
                "Failed to register pre-existing route {} on startup: {}",
                domain,
                e
            );
        }
    }
}

/// Pick up a login/logout by reloading auth. When the token changed, reset the
/// cloud connection and backoff so the connect step re-establishes with the new
/// identity (or tears down on logout).
pub(super) fn maybe_recheck_auth(
    config: &DaemonConfig,
    current_token: &mut Option<String>,
    account_id: &mut Option<String>,
    username: &mut Option<String>,
    auth_failed: &mut bool,
    cloud: &mut Option<CloudConnector>,
    reconnect_backoff: &mut ReconnectBackoff,
) {
    let new_auth = AuthConfig::load();
    let new_token = new_auth.token.clone();
    if new_token == *current_token {
        return;
    }
    let had_cloud = cloud.is_some();
    *current_token = new_token;
    *account_id = new_auth.account_id.clone();
    *username = new_auth.username.clone();
    *auth_failed = false;
    *cloud = None; // drop the existing connection; connect step re-establishes
    *reconnect_backoff = ReconnectBackoff::new();
    match current_token {
        Some(_) => tracing::info!("Auth token changed, will reconnect to cloud"),
        None if had_cloud => {
            tracing::info!("Logged out, disconnecting from cloud");
            write_cloud_state(config, false, None, None, None, None);
        }
        None => {}
    }
}

/// Renew the JWT before it expires so a long-running daemon doesn't silently lose
/// cloud connectivity. On a successful refresh, drop the connection so the connect
/// step reconnects with the fresh token.
pub(super) async fn maybe_refresh_token(
    current_token: &mut Option<String>,
    cloud: &mut Option<CloudConnector>,
    reconnect_backoff: &mut ReconnectBackoff,
    auth_failed: &mut bool,
) {
    if current_token.is_none() {
        return;
    }
    let current_auth = AuthConfig::load();
    if current_auth.is_token_near_expiry(TOKEN_REFRESH_THRESHOLD_SECS) != Some(true) {
        return;
    }
    tracing::info!("JWT token is near expiry, attempting auto-refresh");
    let mut refresh_auth = AuthConfig::load();
    match refresh_auth.refresh_token().await {
        Ok(()) => {
            *current_token = refresh_auth.token.clone();
            if current_token.is_some() {
                tracing::info!("JWT token refreshed, will reconnect to cloud");
                *cloud = None;
                *reconnect_backoff = ReconnectBackoff::new();
                *auth_failed = false;
            }
        }
        // Don't block future attempts; the token may still be valid.
        Err(e) => tracing::warn!("Failed to auto-refresh JWT token: {}", e),
    }
}

/// Snapshot a live connector's plan / status message into `cloud_state.json` so
/// `portzero status` and the web UI see fresh upsell info without waiting for a
/// reconnect. Only rewrites when there is a plan or message to report.
pub(super) fn snapshot_cloud_state(config: &DaemonConfig, cloud: &Option<CloudConnector>) {
    let Some(connector) = cloud else { return };
    if !connector.is_connected() {
        return;
    }
    let p = connector.plan();
    let c = connector.can_use_cloud_tunnels();
    let m = connector.status_message();
    if p.is_some() || m.is_some() {
        let prev_err = read_cloud_error(config);
        write_cloud_state(config, true, prev_err, p, c, m);
    }
}

/// Detect when the edge rejected our token (expired or revoked), record the
/// failure, and drop the connection so we stop retrying until re-authentication.
pub(super) fn handle_cloud_auth_failure(
    config: &DaemonConfig,
    cloud: &mut Option<CloudConnector>,
    auth_failed: &mut bool,
) {
    if !cloud.as_ref().is_some_and(|c| c.is_auth_failed()) {
        return;
    }
    tracing::warn!(
        "Edge server rejected our token (expired or revoked). \
         Run `portzero login` to re-authenticate."
    );
    write_cloud_state(
        config,
        false,
        Some("Authentication failed. Run `portzero login` to re-authenticate.".to_string()),
        None,
        None,
        None,
    );
    *cloud = None;
    *auth_failed = true;
}

/// Connect (or reconnect) to the cloud edge when we hold a token, aren't blocked
/// by an auth failure, and the reconnect backoff is due. On success, re-registers
/// the current route table so persisted routes go live immediately.
#[allow(clippy::too_many_arguments)]
pub(super) async fn maybe_connect_cloud(
    config: &DaemonConfig,
    domain_router: &DomainRouter,
    observations: &Arc<crate::observations::ObservationStore>,
    route_table: &RouteTable,
    current_token: &Option<String>,
    auth_failed: bool,
    cloud: &mut Option<CloudConnector>,
    reconnect_backoff: &mut ReconnectBackoff,
) {
    let Some(token_ref) = current_token else {
        return;
    };
    if auth_failed {
        return;
    }
    let needs_connect = cloud.as_ref().is_none_or(|c| !c.is_connected());
    if !needs_connect || !reconnect_backoff.is_due() {
        return;
    }

    let token = token_ref.clone();
    let is_reconnect = cloud.is_some();
    *cloud = None; // drop any stale connector before creating the new one
    tracing::info!(
        "{}connecting to cloud edge...",
        if is_reconnect { "Re" } else { "C" }
    );
    let mut connector = CloudConnector::new(token);
    connector.set_status_path(config.cloud_route_status_path());
    connector.set_observations(observations.clone());
    match connector.connect(domain_router.clone()).await {
        Ok(()) => {
            reconnect_backoff.on_success();
            let p = connector.plan();
            let c = connector.can_use_cloud_tunnels();
            let m = connector.status_message();
            write_cloud_state(config, true, None, p, c, m);
            for (domain, route) in &route_table.routes {
                if let Err(e) = connector
                    .register_route(
                        domain,
                        route.port,
                        Some(crate::cloud::build_route_metadata(route)),
                    )
                    .await
                {
                    tracing::warn!("Failed to re-register route {}: {}", domain, e);
                }
            }
            *cloud = Some(connector);
        }
        Err(e) => {
            let err_msg = e.root_cause().to_string();
            tracing::warn!("Cloud connect failed: {}", e);
            reconnect_backoff.on_failure();
            write_cloud_state(config, false, Some(err_msg), None, None, None);
        }
    }
}
