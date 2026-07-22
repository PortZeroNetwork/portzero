//! rustls integration for the overlay stack.
//!
//! Provides:
//! - [`build_server_config`] — builds a `rustls::ServerConfig` from the local
//!   CA's wildcard cert and key PEM.  Called once at overlay startup.
//! - [`spawn_tls_backend`] — spawns the async TLS-terminating proxy task used
//!   by the smoltcp stack for port-443 connections.
//!
//! ## How TLS termination fits in the existing proxy
//!
//! The smoltcp `StackEngine` already proxies connections through two mpsc
//! channels:
//!
//! ```text
//! browser ←→ [smoltcp socket] ←→ [channels] ←→ backend task ←→ real service
//! ```
//!
//! For port 443, `spawn_tls_backend` replaces the plain `spawn_backend` task.
//! It bridges the byte channels to a `tokio::io::duplex` pipe so that
//! `tokio_rustls::TlsAcceptor` can run the TLS handshake, then forwards the
//! decrypted plaintext to the real backend over a normal `TcpStream`:
//!
//! ```text
//! browser ←→ [smoltcp socket]
//!              ↕ raw TLS bytes (mpsc channels)
//!            [duplex raw side]      ← to_backend_rx feeds in, raw read feeds out
//!              ↕ in-memory pipe
//!            [duplex TLS side]      ← TlsAcceptor handshakes here → TlsStream
//!              ↕ plaintext
//!            real backend (tokio::io::copy)
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

use super::ca::LocalCa;

const DUPLEX_BUF: usize = 128 * 1024;
const READ_BUF: usize = 16 * 1024;

/// Build a `rustls::ServerConfig` from the wildcard cert and key in `ca`.
///
/// Returns an `Arc` ready to be shared across connections.
pub fn build_server_config(ca: &LocalCa) -> Result<Arc<ServerConfig>> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut ca.wildcard_cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .context("parse wildcard cert PEM")?;

    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut ca.wildcard_key_pem.as_bytes())
            .context("parse wildcard key PEM")?
            .context("no private key found in wildcard PEM")?;

    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .context("configure TLS protocol versions")?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("build rustls ServerConfig")?;

    Ok(Arc::new(config))
}

/// Spawn a TLS-terminating proxy task for a port-443 connection.
///
/// Raw TLS bytes arrive from `to_backend_rx` (sent by the smoltcp socket),
/// are decrypted by rustls, and the plaintext is forwarded to `real_addr`.
/// The backend's response is encrypted and sent back via `from_backend_tx`.
///
/// Errors during the handshake or connection are logged as warnings; the
/// smoltcp socket is cleaned up by the stack loop when the channels close.
pub(crate) fn spawn_tls_backend(
    server_config: Arc<ServerConfig>,
    real_addr: SocketAddr,
    to_backend_rx: mpsc::Receiver<Vec<u8>>,
    from_backend_tx: mpsc::Sender<Vec<u8>>,
    http_tap: Option<crate::observations::HttpTap>,
) {
    tokio::spawn(async move {
        if let Err(e) = tls_proxy(
            server_config,
            real_addr,
            to_backend_rx,
            from_backend_tx,
            http_tap,
        )
        .await
        {
            tracing::warn!("TLS proxy for {real_addr}: {e:#}");
        }
    });
}

async fn tls_proxy(
    server_config: Arc<ServerConfig>,
    real_addr: SocketAddr,
    to_backend_rx: mpsc::Receiver<Vec<u8>>,
    from_backend_tx: mpsc::Sender<Vec<u8>>,
    mut http_tap: Option<crate::observations::HttpTap>,
) -> Result<()> {
    // `tls_io` is handed to TlsAcceptor; `raw_io` is the bridge to our channels.
    let (tls_io, raw_io) = tokio::io::duplex(DUPLEX_BUF);
    let (mut raw_rd, mut raw_wr) = tokio::io::split(raw_io);

    // Pump: channel → pipe (TLS ciphertext from client into the duplex).
    let to_pipe = tokio::spawn(async move {
        let mut rx = to_backend_rx;
        while let Some(chunk) = rx.recv().await {
            if raw_wr.write_all(&chunk).await.is_err() {
                break;
            }
        }
        // Dropping raw_wr signals EOF to the TLS layer.
    });

    // Pump: pipe → channel (TLS ciphertext produced by rustls back to smoltcp).
    let from_pipe = tokio::spawn(async move {
        let mut buf = vec![0u8; READ_BUF];
        loop {
            match raw_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if from_backend_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Run the TLS handshake on the `tls_io` end of the duplex.
    let acceptor = TlsAcceptor::from(server_config);
    let tls_stream = acceptor.accept(tls_io).await.context("TLS handshake")?;

    // Connect to the real backend service (plain TCP).
    let backend = tokio::net::TcpStream::connect(real_addr)
        .await
        .with_context(|| format!("backend connect to {real_addr}"))?;
    let _ = backend.set_nodelay(true);

    // Proxy plaintext between the TLS stream and the backend.
    let (mut tls_rd, mut tls_wr) = tokio::io::split(tls_stream);
    let (mut be_rd, mut be_wr) = backend.into_split();

    let client_to_backend = tokio::spawn(async move {
        // Manual copy loop (rather than `tokio::io::copy`) so the decrypted
        // client→backend plaintext can be sniffed for exercised HTTP routes
        // (task-91).
        let mut buf = vec![0u8; READ_BUF];
        loop {
            match tls_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(tap) = http_tap.as_mut() {
                        tap.inspect(&buf[..n]);
                    }
                    if be_wr.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = be_wr.shutdown().await;
    });
    let backend_to_client = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut be_rd, &mut tls_wr).await;
    });

    let _ = tokio::join!(client_to_backend, backend_to_client, to_pipe, from_pipe);
    Ok(())
}
