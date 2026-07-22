//! The user-space smoltcp engine: the dedicated task that owns the smoltcp
//! [`Interface`]/[`SocketSet`], accepts virtual connections, and proxies each to
//! its real backend. See the parent module for the overall concurrency model.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant as StdInstant;

use rustls::ServerConfig;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::Device;
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use tokio::sync::mpsc;

use crate::net::service_table::ServiceTable;
use crate::observations::{HttpTap, ObservationStore};
use crate::protocol_detect::Canonical;

use super::{OverlayHttpsPolicy, Pumpable, StackCommand};
use super::{PENDING_BACKEND_WAIT, POLL_INTERVAL, PROXY_CHUNK, SOCKET_BUF};

/// State for a single proxied connection living inside the stack loop.
struct Connection {
    /// Bytes received from the backend, waiting to be pushed into the smoltcp
    /// socket's tx buffer.
    to_client: VecDeque<u8>,
    /// Sender to the backend writer task (bytes the client sent us).
    to_backend_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Receiver of bytes the backend sent back.
    from_backend_rx: mpsc::Receiver<Vec<u8>>,
    /// Set when the backend side has closed (EOF or error).
    backend_eof: bool,
    /// Set once we've initiated a smoltcp close toward the client.
    client_closing: bool,
}

struct PendingConnection {
    vip: Ipv4Address,
    port: u16,
    accepted_at: StdInstant,
}

/// Listening socket bookkeeping: one smoltcp listener per service port.
struct Listener {
    handle: SocketHandle,
    port: u16,
}

pub(super) struct StackEngine<D: Device> {
    iface: Interface,
    sockets: SocketSet<'static>,
    device: D,
    services: ServiceTable,
    listeners: Vec<Listener>,
    connections: HashMap<SocketHandle, Connection>,
    pending_connections: HashMap<SocketHandle, PendingConnection>,
    cmd_rx: mpsc::Receiver<StackCommand>,
    /// When present, port-443 connections are TLS-terminated with this config
    /// and forwarded as plaintext to the service's real backend.
    tls_config: Option<Arc<ServerConfig>>,
    https_policy: OverlayHttpsPolicy,
    /// When present, every proxied overlay connection is recorded here:
    /// TCP-level who-talks-to-whom edges at accept time and, for plaintext
    /// HTTP, per-request exercised routes (task-91). `None` in tests.
    observations: Option<Arc<ObservationStore>>,
}

/// Everything needed to record one accepted overlay connection (task-91).
/// Assembled on the stack loop from cheap in-memory lookups; the possibly-slow
/// PID attribution runs on the blocking pool afterwards.
struct ConnObserve {
    store: Arc<ObservationStore>,
    /// Tunnel domain the client addressed (`<name>.portzero.local`).
    to_domain: String,
    /// `"http"` / `"https"` / `"tcp"` from the virtual port.
    protocol: &'static str,
    /// The client's ephemeral source port, used to find the calling PID.
    client_src_port: Option<u16>,
    /// PID → tunnel domain for every discovered process-backed service, so an
    /// attributed connection becomes a `web → db`-style edge.
    pid_services: Vec<(u32, String)>,
}

impl ConnObserve {
    /// Spawn the TCP-level edge recording for this connection and return the
    /// plaintext HTTP tap (for http/https connections) that records exercised
    /// routes from the byte stream.
    ///
    /// Edge policy: an edge is always recorded when the calling process maps to
    /// another tunnel (source port → PID → discovered service). Unattributed
    /// callers (curl, browsers) are recorded as `(external)` only for opaque
    /// TCP services — for HTTP the cloud path's semantics are kept, where
    /// external browser traffic shows up as exercised routes, not edges.
    fn record_edge_and_make_tap(self) -> Option<HttpTap> {
        let tap = matches!(self.protocol, "http" | "https")
            .then(|| HttpTap::new(self.store.clone(), self.to_domain.clone()));

        // PID attribution reads /proc (or shells out on other platforms), so
        // it must not run on the stack loop.
        tokio::task::spawn_blocking(move || {
            let pid = self
                .client_src_port
                .and_then(crate::management::pid_lookup::pid_for_source_port);
            tracing::debug!(
                to = %self.to_domain,
                src_port = ?self.client_src_port,
                ?pid,
                pid_services = ?self.pid_services,
                "overlay connection attribution"
            );
            let from = pid
                .and_then(|pid| {
                    self.pid_services
                        .iter()
                        .find(|(p, _)| *p == pid)
                        .map(|(_, domain)| domain.clone())
                })
                .filter(|from| *from != self.to_domain);
            if from.is_some() || self.protocol == "tcp" {
                self.store
                    .record_connection(&self.to_domain, self.protocol, from.as_deref());
            }
        });

        tap
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionAction {
    PlainProxy(std::net::SocketAddr),
    TlsTerminate(std::net::SocketAddr),
    RedirectToHttps,
}

impl<D> StackEngine<D>
where
    D: Device + Pumpable + Send + 'static,
{
    pub(super) fn spawn(
        mut device: D,
        initial: ServiceTable,
        cmd_rx: mpsc::Receiver<StackCommand>,
        tls_config: Option<Arc<ServerConfig>>,
        https_policy: OverlayHttpsPolicy,
        observations: Option<Arc<ObservationStore>>,
    ) {
        // The interface uses our virtual gateway IP and covers the entire
        // 10.254.0.0/16 block via a route so that AnyIP accepts every VIP.
        let gateway = crate::net::virtual_ip::gateway_ip();
        let config = Config::new(HardwareAddress::Ip);
        let mut iface = Interface::new(config, &mut device, Instant::now());

        let gw_addr = Ipv4Address::from_bytes(&gateway.octets());
        iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::Ipv4(Ipv4Cidr::new(gw_addr, 16)));
        });
        // Accept packets addressed to any IP inside our subnet, not just the
        // gateway address.
        iface.set_any_ip(true);
        // A route pointing the whole subnet back at ourselves so AnyIP engages.
        let _ = iface.routes_mut().add_default_ipv4_route(gw_addr);

        let mut engine = StackEngine {
            iface,
            sockets: SocketSet::new(Vec::new()),
            device,
            services: ServiceTable::new(),
            listeners: Vec::new(),
            connections: HashMap::new(),
            pending_connections: HashMap::new(),
            cmd_rx,
            tls_config,
            https_policy,
            observations,
        };
        engine.apply_services(initial);

        tokio::spawn(async move {
            engine.run().await;
        });
    }

    /// Replace the service table and (re)create listeners so there is one
    /// listening socket per distinct service port.
    fn apply_services(&mut self, table: ServiceTable) {
        let mut wanted_ports: Vec<u16> = table.all().map(|s| s.service_port).collect();
        wanted_ports.extend(table.pending_ports());
        // Hold a port-443 listener only when at least one HTTP service should
        // receive daemon-terminated HTTPS. Explicit 443 services already add
        // this listener through `service_port` and are passed through by default.
        if self.tls_config.is_some()
            && self.https_policy.enable_for_port_80
            && table.all().any(|s| s.service_port == 80)
        {
            wanted_ports.push(443);
        }
        wanted_ports.sort_unstable();
        wanted_ports.dedup();

        // Remove listeners whose port is no longer wanted.
        let mut keep: Vec<Listener> = Vec::new();
        for listener in self.listeners.drain(..) {
            if wanted_ports.contains(&listener.port) {
                keep.push(listener);
            } else {
                // Only remove if it's still purely a listener (not an accepted
                // connection that reused the handle — accepted sockets are moved
                // into `connections` and replaced with a fresh listener).
                let sock = self.sockets.get::<tcp::Socket>(listener.handle);
                if sock.is_listening() {
                    self.sockets.remove(listener.handle);
                }
            }
        }
        self.listeners = keep;

        // Add listeners for new ports.
        let existing: Vec<u16> = self.listeners.iter().map(|l| l.port).collect();
        for port in wanted_ports {
            if !existing.contains(&port) {
                if let Some(handle) = self.add_listener(port) {
                    self.listeners.push(Listener { handle, port });
                }
            }
        }

        self.services = table;
    }

    /// Create a new smoltcp listening socket on the given port across all VIPs.
    fn add_listener(&mut self, port: u16) -> Option<SocketHandle> {
        let socket = new_tcp_socket();
        let handle = self.sockets.add(socket);
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        // `addr: None` => listen on this port across every local address (every VIP).
        match sock.listen(port) {
            Ok(()) => {
                tracing::debug!("listening on virtual port {port}");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!("failed to listen on virtual port {port}: {e:?}");
                self.sockets.remove(handle);
                None
            }
        }
    }

    async fn run(mut self) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    match cmd {
                        Some(StackCommand::UpdateServices(table)) => {
                            self.apply_services(*table);
                        }
                        Some(StackCommand::UpdateHttpsPolicy(policy)) => {
                            self.https_policy = policy;
                            // Re-evaluate listeners (e.g. whether 443 should be
                            // present for http-80 services under the new policy).
                            // Safe: only listener sockets are added/removed; any
                            // accepted connections keep their original action.
                            let table = self.services.clone();
                            self.apply_services(table);
                        }
                        Some(StackCommand::Shutdown) | None => {
                            tracing::debug!("virtual stack shutting down");
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {}
            }

            self.poll_once();
        }

        // Best-effort: abort all sockets on shutdown.
        for (_, sock) in self.sockets.iter_mut() {
            let smoltcp::socket::Socket::Tcp(tcp) = sock;
            tcp.abort();
        }
        let _ = self
            .iface
            .poll(Instant::now(), &mut self.device, &mut self.sockets);
    }

    /// One iteration: drain inbound packets, poll smoltcp, service every socket.
    fn poll_once(&mut self) {
        // Move any inbound TUN packets into the device RX queue. We only do this
        // for the real ChannelDevice; generic devices manage their own queues.
        self.pump_device();

        let timestamp = Instant::now();
        let _ = self
            .iface
            .poll(timestamp, &mut self.device, &mut self.sockets);

        self.accept_new_connections();
        self.promote_pending_connections();
        self.service_connections();

        // Poll again so any bytes we just queued into sockets get flushed out.
        let _ = self
            .iface
            .poll(Instant::now(), &mut self.device, &mut self.sockets);
    }

    /// Detect listening sockets that just became connected, promote them to
    /// proxied connections, and replace them with a fresh listener so the port
    /// keeps accepting.
    fn accept_new_connections(&mut self) {
        let mut promotions: Vec<(usize, SocketHandle, u16)> = Vec::new();
        for (idx, listener) in self.listeners.iter().enumerate() {
            let sock = self.sockets.get::<tcp::Socket>(listener.handle);
            if sock.is_active() && !sock.is_listening() {
                promotions.push((idx, listener.handle, listener.port));
            }
        }

        for (idx, handle, port) in promotions {
            // Resolve which VIP the client connected to.
            let local = self.sockets.get::<tcp::Socket>(handle).local_endpoint();

            let (vip, action) = match local {
                Some(ep) => match ep.addr {
                    IpAddress::Ipv4(v4) => (
                        Some(v4),
                        select_connection_action(
                            &self.services,
                            self.tls_config.is_some(),
                            self.https_policy,
                            v4,
                            port,
                        ),
                    ),
                    #[allow(unreachable_patterns)]
                    _ => (None, None),
                },
                None => (None, None),
            };

            let Some(action) = action else {
                if let Some(vip) = vip {
                    if self.services.should_hold_pending_connection(vip) {
                        tracing::debug!(
                            "holding virtual connection on pending VIP {vip} port {port}"
                        );
                        self.pending_connections.insert(
                            handle,
                            PendingConnection {
                                vip,
                                port,
                                accepted_at: StdInstant::now(),
                            },
                        );
                        self.replace_listener(idx, port);
                        continue;
                    }
                }
                tracing::warn!(
                    "no backend for accepted connection on port {port} (local={local:?}); aborting"
                );
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
                // Replace listener so the port keeps working.
                self.replace_listener(idx, port);
                continue;
            };

            tracing::debug!("accepted virtual connection on port {port} -> {action:?}");
            let observe = vip.and_then(|vip| self.connection_observe(handle, vip, port));
            if !self.start_connection_backend(handle, action, observe) {
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
                self.replace_listener(idx, port);
                continue;
            }

            // The old handle is now a live connection; install a fresh listener
            // for the port.
            self.replace_listener(idx, port);
        }
    }

    fn start_connection_backend(
        &mut self,
        handle: SocketHandle,
        action: ConnectionAction,
        observe: Option<ConnObserve>,
    ) -> bool {
        let (to_backend_tx, to_backend_rx) = mpsc::channel::<Vec<u8>>(16);
        let (from_backend_tx, from_backend_rx) = mpsc::channel::<Vec<u8>>(16);
        // Record the who-talks-to-whom edge (and build the plaintext HTTP tap)
        // for proxied actions only — the 80→443 redirect never reaches a
        // backend and would double-count with the request re-sent over HTTPS.
        let http_tap = match (&action, observe) {
            (ConnectionAction::RedirectToHttps, _) | (_, None) => None,
            (_, Some(observe)) => observe.record_edge_and_make_tap(),
        };
        match action {
            ConnectionAction::PlainProxy(real_addr) => {
                spawn_backend(real_addr, to_backend_rx, from_backend_tx, http_tap);
            }
            ConnectionAction::TlsTerminate(real_addr) => {
                let Some(ref tls) = self.tls_config else {
                    return false;
                };
                crate::tls::stack::spawn_tls_backend(
                    tls.clone(),
                    real_addr,
                    to_backend_rx,
                    from_backend_tx,
                    http_tap,
                );
            }
            ConnectionAction::RedirectToHttps => {
                spawn_https_redirect(to_backend_rx, from_backend_tx);
            }
        }

        self.connections.insert(
            handle,
            Connection {
                to_client: VecDeque::new(),
                to_backend_tx: Some(to_backend_tx),
                from_backend_rx,
                backend_eof: false,
                client_closing: false,
            },
        );
        true
    }

    fn promote_pending_connections(&mut self) {
        let handles: Vec<SocketHandle> = self.pending_connections.keys().copied().collect();
        for handle in handles {
            let Some(pending) = self.pending_connections.get(&handle) else {
                continue;
            };
            let action = select_connection_action(
                &self.services,
                self.tls_config.is_some(),
                self.https_policy,
                pending.vip,
                pending.port,
            );
            if let Some(action) = action {
                tracing::debug!(
                    "attached pending virtual connection on {}:{} -> {action:?}",
                    pending.vip,
                    pending.port
                );
                let (pending_vip, pending_port) = (pending.vip, pending.port);
                self.pending_connections.remove(&handle);
                let observe = self.connection_observe(handle, pending_vip, pending_port);
                if !self.start_connection_backend(handle, action, observe) {
                    self.sockets.get_mut::<tcp::Socket>(handle).abort();
                }
                continue;
            }

            if pending.accepted_at.elapsed() >= PENDING_BACKEND_WAIT {
                tracing::warn!(
                    "timed out waiting for backend for pending virtual connection on {}:{}",
                    pending.vip,
                    pending.port
                );
                self.pending_connections.remove(&handle);
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
            }
        }
    }

    /// Build the observation context for a just-accepted connection (task-91):
    /// which tunnel domain it targets, the protocol label for the edge, the
    /// client's source port (for PID attribution off-loop), and a snapshot of
    /// PID→domain for every discovered service. `None` when observation is
    /// disabled or the VIP has no (non-management) service.
    fn connection_observe(
        &self,
        handle: SocketHandle,
        vip: Ipv4Address,
        port: u16,
    ) -> Option<ConnObserve> {
        let store = self.observations.as_ref()?.clone();
        let name = self.services.assigned_name_by_vip(vip)?;
        // The daemon's own dashboard/API tunnels are polled constantly by the
        // desktop app; recording them would drown real services in noise.
        if is_management_service(name) {
            return None;
        }
        let to_domain = format!("{name}.portzero.local");
        let protocol = match port {
            80 => "http",
            443 => "https",
            _ => "tcp",
        };
        let client_src_port = self
            .sockets
            .get::<tcp::Socket>(handle)
            .remote_endpoint()
            .map(|ep| ep.port);
        let pid_services = self
            .services
            .all()
            .filter(|s| s.pid != 0 && !is_management_service(&s.name))
            .map(|s| (s.pid, format!("{}.portzero.local", s.name)))
            .collect();
        Some(ConnObserve {
            store,
            to_domain,
            protocol,
            client_src_port,
            pid_services,
        })
    }

    /// Replace the listener at `idx` (whose socket was consumed by an accepted
    /// connection) with a brand new listening socket on the same port.
    fn replace_listener(&mut self, idx: usize, port: u16) {
        if let Some(handle) = self.add_listener(port) {
            if idx < self.listeners.len() {
                self.listeners[idx] = Listener { handle, port };
            } else {
                self.listeners.push(Listener { handle, port });
            }
        } else if idx < self.listeners.len() {
            self.listeners.remove(idx);
        }
    }

    /// Move bytes between each established smoltcp socket and its backend.
    fn service_connections(&mut self) {
        let handles: Vec<SocketHandle> = self.connections.keys().copied().collect();

        for handle in handles {
            // 1. Drain bytes the client sent into the backend channel.
            self.drain_client_to_backend(handle);
            // 2. Pull bytes the backend sent us into the to_client buffer.
            self.pull_backend_bytes(handle);
            // 3. Push queued backend bytes into the smoltcp tx buffer.
            self.push_to_client(handle);
            // 4. Handle close conditions and cleanup.
            self.handle_close(handle);
        }

        // Remove finished connections.
        self.connections.retain(|_, c| !c.is_finished());
    }

    fn drain_client_to_backend(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);

        while sock.can_recv() {
            let Some(tx) = conn.to_backend_tx.as_ref() else {
                break;
            };
            // Respect backend channel capacity to provide backpressure.
            let permit = match tx.try_reserve() {
                Ok(p) => p,
                Err(_) => break, // channel full or closed; try later
            };
            let mut chunk = vec![0u8; PROXY_CHUNK];
            match sock.recv_slice(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    chunk.truncate(n);
                    permit.send(chunk);
                }
                Err(_) => break,
            }
        }

        // If the client closed its send side, signal EOF to the backend by
        // dropping the sender. We only treat this as a half-close once the
        // connection has progressed past the handshake into a state where the
        // remote has actually sent (or will send) a FIN. `may_recv()` is false
        // both before establishment and after the peer's FIN, so we additionally
        // require that we have nothing left to read.
        let client_half_closed = matches!(
            sock.state(),
            tcp::State::CloseWait
                | tcp::State::LastAck
                | tcp::State::Closed
                | tcp::State::Closing
                | tcp::State::TimeWait
        );
        if client_half_closed && conn.to_backend_tx.is_some() && !sock.can_recv() {
            conn.to_backend_tx = None;
        }
    }

    fn pull_backend_bytes(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        loop {
            match conn.from_backend_rx.try_recv() {
                Ok(bytes) => conn.to_client.extend(bytes),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    conn.backend_eof = true;
                    break;
                }
            }
        }
    }

    fn push_to_client(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);
        while !conn.to_client.is_empty() && sock.can_send() {
            let (front, _) = conn.to_client.as_slices();
            if front.is_empty() {
                conn.to_client.make_contiguous();
                continue;
            }
            match sock.send_slice(front) {
                Ok(0) => break,
                Ok(n) => {
                    conn.to_client.drain(..n);
                }
                Err(_) => break,
            }
        }
    }

    fn handle_close(&mut self, handle: SocketHandle) {
        let Some(conn) = self.connections.get_mut(&handle) else {
            return;
        };
        let sock = self.sockets.get_mut::<tcp::Socket>(handle);

        // Backend finished and we've flushed everything to the client: close the
        // client-facing side.
        if conn.backend_eof && conn.to_client.is_empty() && !conn.client_closing && sock.may_send()
        {
            sock.close();
            conn.client_closing = true;
        }
    }

    /// Pump packets into the device. The real [`ChannelDevice`] moves queued
    /// inbound packets from its channel into its synchronous RX queue here;
    /// self-contained mock devices no-op.
    fn pump_device(&mut self) {
        self.device.pump();
    }
}

impl Connection {
    fn is_finished(&self) -> bool {
        // A connection is done once the backend is gone, everything is flushed,
        // and we've closed the client side.
        self.backend_eof
            && self.to_client.is_empty()
            && self.client_closing
            && self.to_backend_tx.is_none()
    }
}

/// Build a new TCP socket with our standard buffer sizes.
// test-support: exposed for integration tests; not part of the stable API
#[doc(hidden)]
pub fn new_tcp_socket() -> tcp::Socket<'static> {
    let rx = tcp::SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    let tx = tcp::SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    tcp::Socket::new(rx, tx)
}

pub(super) fn select_connection_action(
    services: &ServiceTable,
    tls_available: bool,
    policy: OverlayHttpsPolicy,
    vip: Ipv4Address,
    port: u16,
) -> Option<ConnectionAction> {
    let service = services.get_by_vip(vip)?;

    match port {
        80 if is_management_service(&service.name) => {
            Some(ConnectionAction::PlainProxy(service.real_addr))
        }
        443 if is_management_service(&service.name) => None,
        80 if service.service_port == 80
            && policy.enable_for_port_80
            && policy.redirect_port_80
            && tls_available =>
        {
            Some(ConnectionAction::RedirectToHttps)
        }
        443 if service.service_port == 443
            && service.backend_protocol == Some(Canonical::Tls)
            && policy.passthrough_port_443 =>
        {
            Some(ConnectionAction::PlainProxy(service.real_addr))
        }
        443 if service.service_port == 443 && tls_available => {
            Some(ConnectionAction::TlsTerminate(service.real_addr))
        }
        443 if service.service_port == 80 && policy.enable_for_port_80 && tls_available => {
            Some(ConnectionAction::TlsTerminate(service.real_addr))
        }
        443 if service.service_port == 443 && policy.passthrough_port_443 => {
            Some(ConnectionAction::PlainProxy(service.real_addr))
        }
        _ if service.service_port == port => Some(ConnectionAction::PlainProxy(service.real_addr)),
        _ => None,
    }
}

fn is_management_service(name: &str) -> bool {
    matches!(name, "portzero" | "portzero-api")
}

fn spawn_https_redirect(
    mut to_backend_rx: mpsc::Receiver<Vec<u8>>,
    from_backend_tx: mpsc::Sender<Vec<u8>>,
) {
    tokio::spawn(async move {
        let first = to_backend_rx.recv().await.unwrap_or_default();
        let location = redirect_location(&first);
        let response = format!(
            "HTTP/1.1 308 Permanent Redirect\r\n\
             Location: {location}\r\n\
             Connection: close\r\n\
             Content-Length: 0\r\n\r\n"
        );
        let _ = from_backend_tx.send(response.into_bytes()).await;
    });
}

pub(super) fn redirect_location(request: &[u8]) -> String {
    let request = String::from_utf8_lossy(request);
    let mut lines = request.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .filter(|path| path.starts_with('/'))
        .unwrap_or("/");
    let host = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("host").then(|| value.trim())
        })
        .filter(|host| !host.is_empty())
        .unwrap_or("portzero.local");
    let host = host.strip_suffix(":80").unwrap_or(host);

    format!("https://{host}{path}")
}

/// Spawn the async tasks that own the real backend `TcpStream`. One task reads
/// from the backend and forwards to the stack; the same task writes client bytes
/// to the backend.
fn spawn_backend(
    real_addr: std::net::SocketAddr,
    mut to_backend_rx: mpsc::Receiver<Vec<u8>>,
    from_backend_tx: mpsc::Sender<Vec<u8>>,
    mut http_tap: Option<HttpTap>,
) {
    tokio::spawn(async move {
        let stream = match tokio::net::TcpStream::connect(real_addr).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("backend connect to {real_addr} failed: {e}");
                // Dropping from_backend_tx signals EOF/abort to the stack.
                return;
            }
        };
        let _ = stream.set_nodelay(true);
        let (mut rd, mut wr) = stream.into_split();

        // Backend -> client
        let reader = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; PROXY_CHUNK];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if from_backend_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::trace!("backend read error from {real_addr}: {e}");
                        break;
                    }
                }
            }
            // Dropping from_backend_tx here signals backend EOF to the stack.
        });

        // Client -> backend
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(chunk) = to_backend_rx.recv().await {
                if let Some(tap) = http_tap.as_mut() {
                    tap.inspect(&chunk);
                }
                if wr.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = wr.shutdown().await;
        });

        let _ = reader.await;
        let _ = writer.await;
        tracing::trace!("backend proxy for {real_addr} finished");
    });
}
