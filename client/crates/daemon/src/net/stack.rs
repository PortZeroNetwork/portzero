//! User-space TCP stack (smoltcp over TUN).
//!
//! This module implements the real async packet-routing loop for the virtual
//! overlay network:
//!
//! 1. Raw L3 (IP) packets are read from the TUN device and fed into a smoltcp
//!    [`Interface`] + [`SocketSet`].
//! 2. We listen on every registered service port across *all* virtual IPs
//!    (`AnyIP` + wildcard listen endpoints). When a client connects to
//!    `VIP:service_port` the smoltcp socket completes the TCP handshake entirely
//!    in user space (we never use OS sockets for the client side).
//! 3. Once a connection is established, we look up the [`NetworkService`] by the
//!    VIP the client targeted and open a `tokio::net::TcpStream` to the real
//!    ephemeral backend. Payload is then proxied bidirectionally between the
//!    smoltcp socket and the backend stream.
//!
//! ## Concurrency model
//!
//! smoltcp is synchronous and its sockets are not `Send`-friendly across `await`
//! points, so the entire smoltcp engine runs inside a single dedicated task (the
//! "stack loop"). That task owns the [`Interface`], the [`SocketSet`] and the
//! [`phy::Device`]. The loop:
//!
//! - drains inbound IP packets (from TUN, or a mock device in tests) into the
//!   device's RX queue,
//! - calls [`Interface::poll`] to advance every TCP socket,
//! - moves bytes between each established socket and its backend connection via
//!   per-connection mpsc channels (the actual blocking backend I/O lives in
//!   small async helper tasks so the loop never blocks on the network),
//! - flushes the device's TX queue back out to TUN.
//!
//! The public API ([`VirtualStack::spawn`], [`update_services`], [`shutdown`],
//! [`StackCommand`]) is unchanged so the rest of the system keeps compiling.

use std::sync::Arc;

use anyhow::Result;
use rustls::ServerConfig;
use smoltcp::phy::Device;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::net::service_table::ServiceTable;

mod device;
mod engine;
mod testsupport;

#[cfg(test)]
#[path = "stack/tests.rs"]
mod tests;

pub use device::{ChannelDevice, ChannelRxToken, ChannelTxToken, Pumpable};
#[doc(hidden)]
pub use engine::new_tcp_socket;
#[doc(hidden)]
pub use testsupport::{
    client_iface, test_tcp, MockDevice, MockRxToken, MockTxToken, TestInstant, TestIpAddress,
    TestIpv4Address, TestSocketSet,
};

use engine::StackEngine;

/// MTU used for the virtual interface. Matches the default TUN MTU.
const STACK_MTU: usize = 1500;
/// Per-socket smoltcp send/recv buffer size.
const SOCKET_BUF: usize = 64 * 1024;
/// Chunk size used when shuttling bytes to/from the backend channels.
const PROXY_CHUNK: usize = 16 * 1024;
/// How often the stack loop wakes up even with no I/O, to drive smoltcp timers.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const PENDING_BACKEND_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Configurable HTTPS behavior for `.portzero.local` overlay services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayHttpsPolicy {
    /// When a service is exposed on virtual port 80, also expose HTTPS on 443
    /// with daemon-side TLS termination to the same plaintext backend.
    pub enable_for_port_80: bool,
    /// Redirect HTTP requests on virtual port 80 to HTTPS.
    pub redirect_port_80: bool,
    /// When a service is exposed on virtual port 443 and its backend is
    /// confirmed to speak TLS, pass TLS through to that backend.
    pub passthrough_port_443: bool,
}

impl Default for OverlayHttpsPolicy {
    fn default() -> Self {
        Self {
            enable_for_port_80: false,
            redirect_port_80: true,
            passthrough_port_443: true,
        }
    }
}

pub enum StackCommand {
    UpdateServices(Box<ServiceTable>),
    /// Apply a new HTTPS policy without restarting the stack. This may add or
    /// remove the synthetic 443 listener (for 80->443 redirect/term) but will
    /// not terminate any established proxied connections.
    UpdateHttpsPolicy(OverlayHttpsPolicy),
    Shutdown,
}

/// Handle to the running virtual stack. Cloneable senders are used to talk to
/// the dedicated stack task.
pub struct VirtualStack {
    cmd_tx: mpsc::Sender<StackCommand>,
    /// Handles to the TUN reader/writer tasks (only present for the real
    /// [`VirtualStack::spawn`] path; the test [`spawn_with_device`] path creates
    /// no TUN tasks and leaves these `None`).
    ///
    /// These must be aborted on [`shutdown`] so the reader task — which would
    /// otherwise stay parked forever in `tun_reader.read().await` — drops its
    /// half of the device. Once both halves drop, the TUN fd closes, the
    /// [`RouteGuard`] on the writer half runs, and the OS interface (`deven0`)
    /// disappears on a normal stop.
    reader_task: Option<JoinHandle<()>>,
    writer_task: Option<JoinHandle<()>>,
}

impl VirtualStack {
    /// Spawn the virtual stack driving the given TUN device.
    ///
    /// `tls_config` enables TLS termination on port 443. For plaintext HTTP
    /// backends, connections to `*.portzero.local` VIPs on port 443 are
    /// decrypted with the wildcard cert and forwarded to the real backend.
    pub async fn spawn(
        tun: crate::net::tun_device::TunDevice,
        initial: ServiceTable,
        tls_config: Option<Arc<ServerConfig>>,
        https_policy: OverlayHttpsPolicy,
        observations: Option<Arc<crate::observations::ObservationStore>>,
    ) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<StackCommand>(16);

        // Bridge the async TUN device into the synchronous smoltcp device via two
        // channels: inbound IP packets (TUN -> stack) and outbound IP packets
        // (stack -> TUN).
        let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(256);

        let (tun_reader, tun_writer) = tun.split();

        // TUN reader task: read raw L3 packets and push them to the stack.
        let inbound_tx_reader = inbound_tx.clone();
        let reader_task = tokio::spawn(async move {
            let mut buf = vec![0u8; STACK_MTU + 4];
            loop {
                match tun_reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if inbound_tx_reader.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("TUN read error: {e}");
                        break;
                    }
                }
            }
            tracing::debug!("TUN reader task exiting");
        });

        // TUN writer task: drain outbound packets and write them to TUN.
        let writer_task = tokio::spawn(async move {
            while let Some(pkt) = outbound_rx.recv().await {
                if let Err(e) = tun_writer.write(&pkt).await {
                    tracing::warn!("TUN write error: {e}");
                    break;
                }
            }
            tracing::debug!("TUN writer task exiting");
        });

        let device = ChannelDevice::new(inbound_rx, outbound_tx);
        StackEngine::spawn(
            device,
            initial,
            cmd_rx,
            tls_config,
            https_policy,
            observations,
        );

        Ok(Self {
            cmd_tx,
            reader_task: Some(reader_task),
            writer_task: Some(writer_task),
        })
    }

    /// Spawn a stack over an arbitrary smoltcp device (used by tests with a mock
    /// in-memory device).
    // test-support: exposed for integration tests; not part of the stable API
    #[doc(hidden)]
    pub fn spawn_with_device<D>(
        device: D,
        initial: ServiceTable,
        tls_config: Option<Arc<ServerConfig>>,
        https_policy: OverlayHttpsPolicy,
    ) -> Self
    where
        D: Device + Pumpable + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel::<StackCommand>(16);
        // The mock-device test path records no observations.
        StackEngine::spawn(device, initial, cmd_rx, tls_config, https_policy, None);
        // The mock-device test path drives no real TUN, so there are no
        // reader/writer tasks to track or abort.
        Self {
            cmd_tx,
            reader_task: None,
            writer_task: None,
        }
    }

    pub async fn update_services(&self, table: ServiceTable) -> Result<()> {
        let _ = self
            .cmd_tx
            .send(StackCommand::UpdateServices(Box::new(table)))
            .await;
        Ok(())
    }

    /// Update HTTPS policy live. Affects decisions for *new* connections only
    /// (and whether a 443 listener exists); established TCP sessions continue
    /// with the action chosen at their accept time. No connections are dropped.
    pub async fn update_https_policy(&self, policy: OverlayHttpsPolicy) -> Result<()> {
        let _ = self
            .cmd_tx
            .send(StackCommand::UpdateHttpsPolicy(policy))
            .await;
        Ok(())
    }

    pub fn command_sender(&self) -> mpsc::Sender<StackCommand> {
        self.cmd_tx.clone()
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.cmd_tx.send(StackCommand::Shutdown).await;
        // Abort the TUN reader/writer tasks so their halves of the device drop.
        // The reader task is otherwise parked forever in `read().await`, holding
        // the reader half (and, via the writer half's `RouteGuard`, the route and
        // fd). Aborting both lets the device fd close and the OS interface
        // (`deven0`) go away on a normal stop. `JoinHandle::abort` takes `&self`,
        // so aborting from `&self` here is fine. No-op on the test path where the
        // handles are `None`.
        if let Some(task) = self.reader_task.as_ref() {
            task.abort();
        }
        if let Some(task) = self.writer_task.as_ref() {
            task.abort();
        }
        Ok(())
    }
}
