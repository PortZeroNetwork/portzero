//! Cloud connector: manages the WebSocket tunnel to the edge server.
//!
//! When authenticated, the daemon connects to `wss://edge.portzero.cloud/tunnel`
//! and registers discovered routes so they become reachable from the internet.
//! Incoming HTTP requests are forwarded to local services.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use portzero_proto::{ClientMessage, ServerMessage};
use portzero_tunnel_client::domain_router::DomainRouter;
use tokio::sync::{mpsc, Semaphore};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::forwarder;

const MAX_INCOMING_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONCURRENT_FORWARDS: usize = 32;

/// Cloud connector state.
pub struct CloudConnector {
    /// Outbound message sender.
    tx: Option<mpsc::Sender<ClientMessage>>,
    /// Auth token for the edge server.
    auth_token: String,
    /// Machine ID (derived from hostname + random suffix).
    machine_id: String,
    /// Edge server URL.
    edge_url: String,
    /// Session ID from the server Welcome message.
    session_id: Option<String>,
    /// Plan reported by the edge on Welcome (e.g. "free", "pro").
    /// Used to drive upsell prompts for non-paying users attempting cloud tunnels.
    plan: Arc<Mutex<Option<String>>>,
    /// Whether the account can create cloud tunnels right now (own plan or a
    /// paid team it belongs to), reported by the edge on Welcome.
    can_use_cloud_tunnels: Arc<Mutex<Option<bool>>>,
    /// Latest user-facing status message from the edge (e.g. plan limit explanation).
    /// This is surfaced in `portzero status` and the web dashboard when present.
    status_message: Arc<Mutex<Option<String>>>,
    /// True while the inbound WebSocket reader task is alive.
    /// The outbound writer task only fails on write, so it can stay alive after
    /// the connection drops if nothing is being sent — causing is_connected() to
    /// return true on a dead connection. This flag is set by the reader task.
    inbound_alive: Arc<AtomicBool>,
    /// Set by the inbound reader when the server rejects our token (AuthFailed).
    /// Signals the discovery loop to stop retrying and wait for a new token.
    auth_failed: Arc<AtomicBool>,
    /// Review status per cloud domain ("pending_review" | "published" | "denied"),
    /// updated from RouteAck and RouteStatusChanged. Surfaced in the local
    /// dashboard so the user sees whether a cloud tunnel is In Review or live.
    route_statuses: Arc<Mutex<HashMap<String, String>>>,
    /// Where to persist `route_statuses` for the dashboard to read. Set by the
    /// discovery loop once the state dir is known.
    status_path: Arc<Mutex<Option<PathBuf>>>,
    /// Records observed edges + exercised routes from forwarded traffic (task-63).
    /// Set by the discovery loop; `None` disables observation recording.
    observations: Arc<Mutex<Option<Arc<crate::observations::ObservationStore>>>>,
}

impl CloudConnector {
    /// Create a new cloud connector.
    ///
    /// Reads the edge server URL from `PZ_TUNNEL_EDGE_URL` (useful for
    /// local development) or falls back to the default production URL.
    pub fn new(auth_token: String) -> Self {
        let machine_id = generate_machine_id();
        Self {
            tx: None,
            auth_token,
            machine_id,
            edge_url: portzero_domain::endpoints::edge_url(),
            session_id: None,
            plan: Arc::new(Mutex::new(None)),
            can_use_cloud_tunnels: Arc::new(Mutex::new(None)),
            status_message: Arc::new(Mutex::new(None)),
            inbound_alive: Arc::new(AtomicBool::new(false)),
            auth_failed: Arc::new(AtomicBool::new(false)),
            route_statuses: Arc::new(Mutex::new(HashMap::new())),
            status_path: Arc::new(Mutex::new(None)),
            observations: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a cloud connector with a custom edge URL (useful for testing).
    #[cfg(test)]
    pub fn with_url(auth_token: String, edge_url: String) -> Self {
        let machine_id = generate_machine_id();
        Self {
            tx: None,
            auth_token,
            machine_id,
            edge_url,
            session_id: None,
            plan: Arc::new(Mutex::new(None)),
            can_use_cloud_tunnels: Arc::new(Mutex::new(None)),
            status_message: Arc::new(Mutex::new(None)),
            inbound_alive: Arc::new(AtomicBool::new(false)),
            auth_failed: Arc::new(AtomicBool::new(false)),
            route_statuses: Arc::new(Mutex::new(HashMap::new())),
            status_path: Arc::new(Mutex::new(None)),
            observations: Arc::new(Mutex::new(None)),
        }
    }

    /// Tell the connector where to persist per-route review status so the local
    /// dashboard can display it. Called once by the discovery loop.
    pub fn set_status_path(&self, path: PathBuf) {
        if let Ok(mut g) = self.status_path.lock() {
            *g = Some(path);
        }
    }

    /// Give the connector an observation store so forwarded requests record
    /// observed edges and exercised routes. Called once by the discovery loop.
    pub fn set_observations(&self, store: Arc<crate::observations::ObservationStore>) {
        if let Ok(mut g) = self.observations.lock() {
            *g = Some(store);
        }
    }

    /// Snapshot of review status per cloud domain.
    pub fn route_statuses(&self) -> HashMap<String, String> {
        self.route_statuses
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Connect to the edge server via WebSocket.
    ///
    /// Spawns a background task that reads from the WebSocket and forwards
    /// incoming requests to local services via the domain router.
    pub async fn connect(&mut self, domain_router: DomainRouter) -> Result<()> {
        tracing::info!("Connecting to edge server at {}", self.edge_url);

        let mut websocket_config = WebSocketConfig::default();
        websocket_config.max_message_size = Some(MAX_INCOMING_MESSAGE_BYTES);
        websocket_config.max_frame_size = Some(MAX_INCOMING_MESSAGE_BYTES);
        let (ws_stream, _) = tokio_tungstenite::connect_async_with_config(
            &self.edge_url,
            Some(websocket_config),
            false,
        )
        .await
        .with_context(|| {
            format!(
                "Failed to connect to edge server at {}.\n\n\
                     Check your internet connection and that the edge server is running.\n\
                     The daemon will continue in local-only mode.",
                self.edge_url
            )
        })?;

        let (mut ws_sink, mut ws_stream_rx) = ws_stream.split();

        // Channel for outbound messages
        let (tx, mut rx) = mpsc::channel::<ClientMessage>(64);
        self.tx = Some(tx.clone());
        self.inbound_alive.store(true, Ordering::Relaxed);

        // Send Hello
        let hello = ClientMessage::Hello {
            protocol_version: portzero_proto::PROTOCOL_VERSION,
            auth_token: self.auth_token.clone(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            machine_id: self.machine_id.clone(),
            os: std::env::consts::OS.to_string(),
        };
        let hello_json = serde_json::to_string(&hello)?;
        ws_sink.send(Message::Text(hello_json.into())).await?;

        // Spawn outbound writer task
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::error!("Failed to serialize outbound message: {}", e);
                        continue;
                    }
                };
                if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
                    tracing::error!("Failed to send message to edge: {}", e);
                    break;
                }
            }
            tracing::debug!("Outbound writer task exiting");
        });

        // Spawn inbound reader task
        let tx_for_reader = tx.clone();
        let inbound_alive = Arc::clone(&self.inbound_alive);
        let auth_failed = Arc::clone(&self.auth_failed);
        let plan_for_reader = Arc::clone(&self.plan);
        let can_use_cloud_tunnels_for_reader = Arc::clone(&self.can_use_cloud_tunnels);
        let status_msg_for_reader = Arc::clone(&self.status_message);
        let route_statuses_for_reader = Arc::clone(&self.route_statuses);
        let status_path_for_reader = Arc::clone(&self.status_path);
        let observations_for_reader = self.observations.lock().ok().and_then(|g| g.clone());
        let forward_slots = Arc::new(Semaphore::new(MAX_CONCURRENT_FORWARDS));
        tokio::spawn(async move {
            while let Some(msg_result) = ws_stream_rx.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("WebSocket read error: {}", e);
                        break;
                    }
                };

                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => {
                        tracing::info!("Edge server closed connection");
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => continue,
                };

                let server_msg: ServerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("Failed to parse server message: {}", e);
                        continue;
                    }
                };

                if let ServerMessage::HttpRequest { request_id, .. } = &server_msg {
                    let request_id = *request_id;
                    let permit = match Arc::clone(&forward_slots).try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            let _ = tx_for_reader.try_send(overloaded_response(request_id));
                            continue;
                        }
                    };
                    let router = domain_router.clone();
                    let observations = observations_for_reader.clone();
                    let response_tx = tx_for_reader.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        match handle_incoming(server_msg, &router, observations.as_ref()).await {
                            Ok(Some(response)) => {
                                if let Err(error) = response_tx.send(response).await {
                                    tracing::error!("Failed to queue response: {error}");
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                tracing::error!("Error handling incoming request: {error}");
                                let _ = response_tx
                                    .send(forwarding_error_response(request_id))
                                    .await;
                            }
                        }
                    });
                    continue;
                }

                // Capture plan (from Welcome) and friendly upsell / plan messages for the client UI.
                // These drive "you need a paid plan" prompts when users try *.portzero.cloud on free.
                match &server_msg {
                    ServerMessage::Welcome {
                        plan,
                        can_use_cloud_tunnels,
                        ..
                    } => {
                        if let Ok(mut g) = plan_for_reader.lock() {
                            *g = Some(plan.clone());
                        }
                        if let Ok(mut g) = can_use_cloud_tunnels_for_reader.lock() {
                            *g = Some(*can_use_cloud_tunnels);
                        }
                        tracing::info!("Edge session established (plan: {})", plan);
                    }
                    ServerMessage::Error { code, message } => {
                        if *code == portzero_proto::ErrorCode::PlanLimitExceeded {
                            let friendly = "Your plan does not include cloud tunnels (portzero.cloud). \
                                Upgrade at https://app.portzero.cloud to use *.portzero.cloud domains.".to_string();
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(friendly);
                            }
                            tracing::warn!("Edge reported plan limit: {}", message);
                        } else if message.to_lowercase().contains("plan")
                            || message.to_lowercase().contains("limit")
                            || message.to_lowercase().contains("upgrade")
                            || message.to_lowercase().contains("paid")
                            || message.to_lowercase().contains("subscription")
                        {
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(message.clone());
                            }
                        }
                        if message
                            .starts_with("Route accepted by edge, but dashboard persistence failed")
                        {
                            tracing::warn!("{}", message);
                        } else if *code != portzero_proto::ErrorCode::PlanLimitExceeded {
                            tracing::error!("Edge server error ({:?}): {}", code, message);
                        }
                        if *code == portzero_proto::ErrorCode::AuthFailed {
                            auth_failed.store(true, Ordering::Relaxed);
                            inbound_alive.store(false, Ordering::Relaxed);
                            break;
                        }
                        continue;
                    }
                    ServerMessage::RouteAck {
                        success: false,
                        error: Some(err),
                        domain,
                        ..
                    } => {
                        let low = err.to_lowercase();
                        if low.contains("plan")
                            || low.contains("limit")
                            || low.contains("upgrade")
                            || low.contains("paid")
                            || low.contains("subscription")
                        {
                            let friendly = format!(
                                "Cloud route {} rejected: {}. Upgrade at https://app.portzero.cloud for portzero.cloud tunnels.",
                                domain, err
                            );
                            if let Ok(mut g) = status_msg_for_reader.lock() {
                                *g = Some(friendly);
                            }
                        }
                        // General warn happens in handle_incoming
                    }
                    // Record review status so the local dashboard can show
                    // whether a cloud tunnel is In Review or Published.
                    ServerMessage::RouteAck {
                        success: true,
                        domain,
                        status: Some(status),
                        ..
                    } => {
                        update_route_status(
                            &route_statuses_for_reader,
                            &status_path_for_reader,
                            domain,
                            *status,
                        );
                    }
                    ServerMessage::RouteStatusChanged { domain, status, .. } => {
                        update_route_status(
                            &route_statuses_for_reader,
                            &status_path_for_reader,
                            domain,
                            *status,
                        );
                    }
                    _ => {}
                }

                match handle_incoming(server_msg, &domain_router, observations_for_reader.as_ref())
                    .await
                {
                    Ok(Some(response)) => {
                        if let Err(e) = tx_for_reader.send(response).await {
                            tracing::error!("Failed to queue response: {}", e);
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!("Error handling incoming message: {}", e);
                    }
                }
            }
            inbound_alive.store(false, Ordering::Relaxed);
            tracing::info!("Inbound reader task exiting");
        });

        tracing::info!("Connected to edge server");
        Ok(())
    }

    /// Register a discovered route with the cloud edge.
    ///
    /// `metadata` (process cwd/exe/argv) is shown in the cloud review UI so the
    /// account owner can see what is being exposed before approving the tunnel.
    pub async fn register_route(
        &self,
        domain: &str,
        local_port: u16,
        metadata: Option<portzero_proto::RouteMetadata>,
    ) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected to edge server"))?;

        let msg = ClientMessage::RegisterRoute {
            domain: domain.to_string(),
            local_port,
            protocol: portzero_proto::RouteProtocol::Http,
            metadata,
        };

        tx.send(msg)
            .await
            .context("Failed to send RegisterRoute to edge")?;

        tracing::info!("Registered route with cloud: {} -> :{}", domain, local_port);
        Ok(())
    }

    /// Unregister a route from the cloud edge.
    pub async fn unregister_route(&self, domain: &str) -> Result<()> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Not connected to edge server"))?;

        let msg = ClientMessage::UnregisterRoute {
            domain: domain.to_string(),
        };

        tx.send(msg)
            .await
            .context("Failed to send UnregisterRoute to edge")?;

        tracing::info!("Unregistered route from cloud: {}", domain);
        Ok(())
    }

    /// Check if the tunnel connection is fully alive (outbound channel open AND
    /// inbound reader running). Checking only the outbound tx is insufficient:
    /// the writer task idles on recv() and won't notice a dead socket until it
    /// tries to write, so a dropped inbound reader goes undetected indefinitely.
    pub fn is_connected(&self) -> bool {
        self.inbound_alive.load(Ordering::Relaxed)
            && self.tx.as_ref().map(|tx| !tx.is_closed()).unwrap_or(false)
    }

    /// Whether the server rejected our token with an AuthFailed error.
    /// When true the discovery loop drops this connector and waits for a new token.
    pub fn is_auth_failed(&self) -> bool {
        self.auth_failed.load(Ordering::Relaxed)
    }

    /// Plan reported on successful Welcome (e.g. "free", "pro").
    /// None until the first Welcome is received.
    pub fn plan(&self) -> Option<String> {
        self.plan.lock().ok().and_then(|g| g.clone())
    }

    /// Whether the account can create cloud tunnels right now (own plan or a
    /// paid team it belongs to). None until the first Welcome is received.
    pub fn can_use_cloud_tunnels(&self) -> Option<bool> {
        self.can_use_cloud_tunnels.lock().ok().and_then(|g| *g)
    }

    /// Latest user-facing status message (plan limits, quota, etc.).
    /// Set by the inbound reader on relevant ServerMessages.
    pub fn status_message(&self) -> Option<String> {
        self.status_message.lock().ok().and_then(|g| g.clone())
    }

    /// Reconnect to the edge server.
    pub async fn reconnect(&mut self, domain_router: DomainRouter) -> Result<()> {
        tracing::info!("Reconnecting to edge server...");
        self.tx = None;
        self.session_id = None;
        if let Ok(mut g) = self.plan.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.can_use_cloud_tunnels.lock() {
            *g = None;
        }
        if let Ok(mut g) = self.status_message.lock() {
            *g = None;
        }
        self.inbound_alive.store(false, Ordering::Relaxed);
        self.auth_failed.store(false, Ordering::Relaxed);
        self.connect(domain_router).await
    }
}

fn overloaded_response(request_id: u64) -> ClientMessage {
    ClientMessage::HttpResponse {
        request_id,
        status: 503,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: b"Tunnel forwarding concurrency limit reached".to_vec(),
    }
}

fn forwarding_error_response(request_id: u64) -> ClientMessage {
    ClientMessage::HttpResponse {
        request_id,
        status: 502,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: b"No local process is available for this tunnel".to_vec(),
    }
}

/// Handle an incoming server message.
///
/// Returns an optional response to send back through the tunnel.
pub async fn handle_incoming(
    msg: ServerMessage,
    domain_router: &DomainRouter,
    observations: Option<&Arc<crate::observations::ObservationStore>>,
) -> Result<Option<ClientMessage>> {
    match msg {
        ServerMessage::Welcome {
            session_id, plan, ..
        } => {
            // Plan is captured in the reader loop before calling handle_incoming for upsell logic.
            tracing::debug!(
                "Welcome received for session {} (plan {})",
                session_id,
                plan
            );
            Ok(None)
        }
        ServerMessage::RouteAck {
            domain,
            success,
            error,
            url,
            status,
        } => {
            if success {
                match status {
                    Some(portzero_proto::RouteStatus::PendingReview) => {
                        tracing::info!(
                            "Cloud tunnel {} is In Review — approve it at https://app.portzero.cloud to make it public",
                            domain
                        );
                    }
                    _ => {
                        let public_url = url.as_deref().unwrap_or(&domain);
                        tracing::info!("Route accepted by edge: {} → {}", domain, public_url);
                    }
                }
            } else {
                tracing::warn!(
                    "Route rejected by edge: {} - {}",
                    domain,
                    error.as_deref().unwrap_or("unknown error")
                );
            }
            Ok(None)
        }
        ServerMessage::RouteStatusChanged {
            domain,
            status,
            url,
        } => {
            // Status persistence happens in the reader loop; here we just log.
            match status {
                portzero_proto::RouteStatus::Published => tracing::info!(
                    "Cloud tunnel {} approved → {}",
                    domain,
                    url.as_deref().unwrap_or(&domain)
                ),
                portzero_proto::RouteStatus::Denied => {
                    tracing::warn!("Cloud tunnel {} was denied by the owner", domain)
                }
                portzero_proto::RouteStatus::PendingReview => {
                    tracing::info!("Cloud tunnel {} is In Review", domain)
                }
            }
            Ok(None)
        }
        ServerMessage::RouteExpired { domain, reason } => {
            tracing::warn!("Route expired: {} ({})", domain, reason);
            Ok(None)
        }
        ServerMessage::HttpRequest {
            request_id,
            method,
            path,
            host,
            headers,
            body,
        } => {
            // Record observed runtime truth: this request was addressed to the
            // `host` tunnel; a Referer/Origin identifies a caller edge, and an
            // X-PZ-Test header attributes the exercised route to a test.
            if let Some(store) = observations {
                let header = |name: &str| {
                    headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(name))
                        .map(|(_, v)| v.as_str())
                };
                let referer = header("referer").or_else(|| header("origin"));
                let x_pz_test = header("x-pz-test");
                store.record_http(&host, &method, &path, referer, x_pz_test);
            }

            let local_port = domain_router.resolve(&host).ok_or_else(|| {
                anyhow::anyhow!(
                    "No local service for host '{}' — route may have been removed",
                    host
                )
            })?;

            let response = forwarder::forward_request(
                request_id, &method, &path, &host, &headers, &body, local_port,
            )
            .await?;

            Ok(Some(response))
        }
        ServerMessage::Ping { timestamp } => Ok(Some(ClientMessage::Pong { timestamp })),
        ServerMessage::Error { code, message } => {
            tracing::error!("Edge server error ({:?}): {}", code, message);
            Ok(None)
        }
    }
}

/// Wire status → the string persisted for the dashboard.
fn route_status_to_str(status: portzero_proto::RouteStatus) -> &'static str {
    match status {
        portzero_proto::RouteStatus::PendingReview => "pending_review",
        portzero_proto::RouteStatus::Published => "published",
        portzero_proto::RouteStatus::Denied => "denied",
    }
}

/// Update the in-memory status map and persist it to `cloud_route_status.json`
/// so the local dashboard (which reads files, not the connector) can show it.
fn update_route_status(
    statuses: &Arc<Mutex<HashMap<String, String>>>,
    status_path: &Arc<Mutex<Option<PathBuf>>>,
    domain: &str,
    status: portzero_proto::RouteStatus,
) {
    let snapshot = {
        let Ok(mut map) = statuses.lock() else {
            return;
        };
        map.insert(domain.to_string(), route_status_to_str(status).to_string());
        map.clone()
    };
    let path = status_path.lock().ok().and_then(|g| g.clone());
    if let Some(path) = path {
        match serde_json::to_string(&snapshot) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to write cloud route status: {}", e);
                }
            }
            Err(e) => tracing::warn!("Failed to serialize cloud route status: {}", e),
        }
    }
}

/// Build best-effort process metadata for a route, shown in the cloud review
/// UI. `cwd` comes from discovery; `exe_path`/`process_name`/`argv` are looked
/// up from the owning PID via sysinfo. Any field may be absent.
pub fn build_route_metadata(route: &crate::route_table::Route) -> portzero_proto::RouteMetadata {
    use crate::discovery::ServiceSource;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let cwd = match &route.source {
        ServiceSource::Process { cwd: Some(cwd) } => Some(cwd.display().to_string()),
        _ => None,
    };

    let (mut exe_path, mut process_name, mut argv) = (None, None, Vec::new());
    let mut sys = System::new();
    let pid = Pid::from_u32(route.pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .with_exe(sysinfo::UpdateKind::Always)
            .with_cmd(sysinfo::UpdateKind::Always),
    );
    if let Some(proc) = sys.process(pid) {
        exe_path = proc.exe().map(|p| p.display().to_string());
        process_name = Some(proc.name().to_string_lossy().into_owned());
        argv = proc
            .cmd()
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
    }

    let hostname = hostname::get().ok().and_then(|h| h.into_string().ok());

    portzero_proto::RouteMetadata {
        cwd,
        exe_path,
        process_name,
        argv,
        hostname,
    }
}

/// Generate a machine ID from hostname + random suffix.
fn generate_machine_id() -> String {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());
    let suffix = &uuid::Uuid::new_v4().to_string()[..8];
    format!("{}-{}", host, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_machine_id() {
        let id = generate_machine_id();
        assert!(!id.is_empty());
        assert!(id.contains('-'));
    }

    #[test]
    fn test_cloud_connector_new() {
        let conn = CloudConnector::new("tok_test".to_string());
        assert!(!conn.is_connected());
        assert!(!conn.is_auth_failed());
        assert!(conn.session_id.is_none());
        assert_eq!(conn.auth_token, "tok_test");
        assert!(conn.plan().is_none());
        assert!(conn.status_message().is_none());
    }

    #[test]
    fn test_is_auth_failed_flag() {
        let conn = CloudConnector::new("tok_test".to_string());
        assert!(!conn.is_auth_failed());
        conn.auth_failed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(conn.is_auth_failed());
    }

    #[tokio::test]
    async fn test_handle_incoming_welcome() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Welcome {
            session_id: "sess_123".to_string(),
            account_id: "acct_1".to_string(),
            plan: "free".to_string(),
            can_use_cloud_tunnels: false,
        };
        let result = handle_incoming(msg, &router, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_ping() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Ping { timestamp: 42 };
        let result = handle_incoming(msg, &router, None).await.unwrap();
        match result {
            Some(ClientMessage::Pong { timestamp }) => assert_eq!(timestamp, 42),
            other => panic!("Expected Pong, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_handle_incoming_route_ack_success() {
        let router = DomainRouter::new();
        let msg = ServerMessage::RouteAck {
            domain: "api-test.alice.portzero.cloud".to_string(),
            success: true,
            error: None,
            url: Some("https://api-test.alice.portzero.cloud".to_string()),
            status: Some(portzero_proto::RouteStatus::Published),
        };
        let result = handle_incoming(msg, &router, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_route_ack_failure() {
        let router = DomainRouter::new();
        let msg = ServerMessage::RouteAck {
            domain: "api-test.alice.portzero.cloud".to_string(),
            success: false,
            error: Some("domain not authorized".to_string()),
            url: None,
            status: None,
        };
        let result = handle_incoming(msg, &router, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_error() {
        let router = DomainRouter::new();
        let msg = ServerMessage::Error {
            code: portzero_proto::ErrorCode::InternalError,
            message: "service unavailable".to_string(),
        };
        let result = handle_incoming(msg, &router, None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_handle_incoming_http_request_no_route() {
        let router = DomainRouter::new();
        let msg = ServerMessage::HttpRequest {
            request_id: 1,
            method: "GET".to_string(),
            path: "/".to_string(),
            host: "unknown-svc.alice.portzero.cloud".to_string(),
            headers: vec![],
            body: vec![],
        };
        let result = handle_incoming(msg, &router, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_route_not_connected() {
        let conn = CloudConnector::new("tok_test".to_string());
        let result = conn
            .register_route("api-test.alice.portzero.cloud", 8080, None)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not connected"));
    }

    #[tokio::test]
    async fn test_unregister_route_not_connected() {
        let conn = CloudConnector::new("tok_test".to_string());
        let result = conn.unregister_route("api-test.alice.portzero.cloud").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not connected"));
    }
}
