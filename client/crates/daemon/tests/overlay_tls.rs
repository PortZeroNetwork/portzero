//! Unprivileged TLS e2e for the virtual overlay network.
//!
//! A smoltcp client connects to VIP:443 through the real `StackEngine`
//! (in-memory [`MockDevice`]) and completes a full TLS handshake backed by our
//! locally-generated wildcard cert. The TLS payload is forwarded in plaintext
//! to a real tokio echo backend; the echo comes back encrypted and the client
//! decrypts it via the same [`rustls::ClientConnection`].
//!
//! No root, no TUN, no disk I/O — cert material is generated in-memory via
//! [`LocalCa::generate_ephemeral`]. Platform-neutral: runs on Linux, macOS,
//! and Windows without any `cfg` guards.

mod common;

use std::sync::Arc;
use std::time::Duration;

use portzero_daemon::net::service_table::ServiceTable;

use common::{spawn_echo_backend, TEST_TIMEOUT};

#[tokio::test]
async fn unprivileged_tls_vip_proxy() {
    use std::io::Read as _;
    use std::io::Write as _;

    use portzero_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, OverlayHttpsPolicy, TestInstant,
        TestIpAddress, TestIpv4Address, TestSocketSet, VirtualStack,
    };
    use portzero_daemon::tls::{ca::LocalCa, stack::build_server_config};
    use rustls::pki_types::ServerName;

    // Both ring and aws-lc-rs are compiled in (via reqwest/hyper-rustls), so
    // rustls cannot auto-select a provider — install ring explicitly.
    // .ok() silently ignores "already installed" when tests share a process.
    let _ = rustls::crypto::ring::default_provider().install_default();

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Generate a fresh CA + wildcard cert entirely in memory (no disk I/O).
        let ca = LocalCa::generate_ephemeral().unwrap();
        let server_config = build_server_config(&ca).unwrap();

        // 2. Build a rustls ClientConfig that trusts our in-memory CA.
        let mut root_store = rustls::RootCertStore::empty();
        let ca_cert_der = rustls_pemfile::certs(&mut ca.ca_cert_pem.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        root_store.add(ca_cert_der).unwrap();
        let client_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        let server_name = ServerName::try_from("hello.portzero.local")
            .unwrap()
            .to_owned();
        let mut tls_conn = rustls::ClientConnection::new(client_config, server_name).unwrap();

        // 3. Plain TCP echo backend — receives cleartext bytes from the TLS
        //    terminator, so it never sees ciphertext.
        let backend_addr = spawn_echo_backend().await;

        // 4. Register the service on its HTTP port (80). The stack accepts
        //    VIP:443, terminates TLS, and forwards plaintext to this backend
        //    via VIP lookup (not port matching).
        let mut table = ServiceTable::new();
        let svc = table.register("hello".to_string(), backend_addr, 80, 0);
        let vip = svc.vip;
        let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

        // 5. Spawn the stack engine with TLS enabled.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack = VirtualStack::spawn_with_device(
            stack_dev,
            table,
            Some(server_config),
            OverlayHttpsPolicy {
                enable_for_port_80: true,
                redirect_port_80: true,
                passthrough_port_443: true,
            },
        );

        // 6. Build a smoltcp client interface and open a TCP connection to VIP:443.
        let client_ip = TestIpv4Address::new(10, 254, 9, 9);
        let mut client_iface = client_iface(&mut client_dev, client_ip);
        let mut client_sockets = TestSocketSet::new(Vec::new());
        let client_handle = client_sockets.add(new_tcp_socket());
        {
            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
            let cx = client_iface.context();
            sock.connect(
                cx,
                (TestIpAddress::Ipv4(vip_v4), 443u16),
                (client_ip, 49000u16),
            )
            .unwrap();
        }

        // 7. Drive the smoltcp poll loop interleaved with the rustls
        //    ClientConnection state machine.
        //
        //    Each iteration:
        //      a) poll smoltcp to move packets between client and stack;
        //      b) feed received ciphertext bytes into rustls;
        //      c) drain any decrypted application data;
        //      d) queue our payload once the handshake is done;
        //      e) write any pending TLS output into the smoltcp send buffer.
        let payload = b"hello tls overlay";
        let mut plaintext_sent = false;
        let mut received = Vec::new();

        for _ in 0..3000 {
            client_iface.poll(TestInstant::now(), &mut client_dev, &mut client_sockets);

            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);

            // (b) Feed incoming TLS ciphertext to rustls.
            if sock.can_recv() {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = sock.recv_slice(&mut buf) {
                    if n > 0 {
                        buf.truncate(n);
                        tls_conn.read_tls(&mut buf.as_slice()).unwrap();
                        tls_conn
                            .process_new_packets()
                            .expect("TLS processing error");
                    }
                }
            }

            // (c) Drain any decrypted application data (the backend echo).
            {
                let mut tmp = [0u8; 4096];
                loop {
                    match tls_conn.reader().read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => received.extend_from_slice(&tmp[..n]),
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    }
                }
            }

            // (d) Once the handshake completes, queue our payload for encryption.
            if !tls_conn.is_handshaking() && !plaintext_sent {
                tls_conn.writer().write_all(payload).unwrap();
                plaintext_sent = true;
            }

            // (e) Write any pending TLS output bytes into the smoltcp socket.
            if sock.may_send() && sock.can_send() {
                let mut tls_out = Vec::new();
                let _ = tls_conn.write_tls(&mut tls_out);
                if !tls_out.is_empty() {
                    let _ = sock.send_slice(&tls_out);
                }
            }

            if received.len() >= payload.len() {
                break;
            }

            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert!(
            plaintext_sent,
            "TLS handshake never completed (plaintext was never queued)"
        );
        assert_eq!(
            &received[..payload.len().min(received.len())],
            payload,
            "TLS-proxied echo did not match payload"
        );

        stack.shutdown().await.unwrap();
    })
    .await
    .expect("unprivileged TLS vip proxy timed out");
}
