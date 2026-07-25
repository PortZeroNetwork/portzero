//! Tests for the virtual stack. Split out of stack.rs to keep the module within
//! the per-file line budget; pure code motion.

use smoltcp::iface::{Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{IpAddress, Ipv4Address};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::net::service_table::ServiceTable;
use crate::protocol_detect::Canonical;

use super::engine::{redirect_location, select_connection_action, ConnectionAction};
use super::{client_iface, new_tcp_socket, MockDevice, OverlayHttpsPolicy, VirtualStack};

/// End-to-end: a smoltcp "client" connects to VIP:port, the stack accepts via
/// smoltcp, proxies to a real tokio echo backend, and data flows both ways.
#[tokio::test]
async fn vip_connect_proxies_to_backend() {
    tracing_subscriber_try_init();

    // 1. Start a real backend echo server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });

    // 2. Build the service table mapping a VIP:5432 -> backend.
    let mut table = ServiceTable::new();
    let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
    let vip = svc.vip;

    // 3. Spawn the stack on one half of a mock device pair.
    let (stack_dev, mut client_dev) = MockDevice::pair();
    let stack =
        VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

    // 4. Build a client smoltcp interface on the other half and connect to
    //    VIP:5432 from a client IP in the same subnet.
    let client_ip = Ipv4Address::new(10, 254, 9, 9);
    let mut client_iface = client_iface(&mut client_dev, client_ip);
    let mut client_sockets = SocketSet::new(Vec::new());
    let client_handle = client_sockets.add(new_tcp_socket());

    {
        let sock = client_sockets.get_mut::<tcp::Socket>(client_handle);
        let cx = client_iface.context();
        sock.connect(cx, (IpAddress::Ipv4(vip), 5432u16), (client_ip, 49000u16))
            .unwrap();
    }

    // 5. Drive both sides until the connection is established and echo works.
    let payload = b"hello virtual overlay";
    let mut sent = false;
    let mut received = Vec::new();

    for _ in 0..2000 {
        client_iface.poll(Instant::now(), &mut client_dev, &mut client_sockets);

        let sock = client_sockets.get_mut::<tcp::Socket>(client_handle);
        if sock.may_send() && sock.can_send() && !sent {
            sock.send_slice(payload).unwrap();
            sent = true;
        }
        if sock.can_recv() {
            let mut buf = vec![0u8; 4096];
            if let Ok(n) = sock.recv_slice(&mut buf) {
                received.extend_from_slice(&buf[..n]);
            }
        }
        if received.len() >= payload.len() {
            break;
        }

        // Give the stack task time to run its poll loop and the backend to
        // echo.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    assert!(
        sent,
        "client never reached a sendable state (handshake failed)"
    );
    assert_eq!(
        received, payload,
        "echoed payload did not match what was sent"
    );

    stack.shutdown().await.unwrap();
}

/// A finished proxied connection must have its smoltcp socket removed from the
/// engine's `SocketSet`, not just its `Connection` bookkeeping. Regression test
/// for the ~128 KiB-per-connection leak: opening and cleanly closing many
/// connections in sequence must return the engine's socket count to its
/// listeners-only baseline, never accumulate one dead socket per connection.
#[tokio::test]
async fn finished_connection_socket_is_reaped() {
    tracing_subscriber_try_init();

    let backend = echo_backend().await;

    let mut table = ServiceTable::new();
    let svc = table.register("leaky".to_string(), backend, 5432, 0);
    let vip = svc.vip;

    let (stack_dev, mut client_dev) = MockDevice::pair();
    let stack =
        VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

    // Baseline: one listener socket for the single service port, no connections.
    let baseline = stack.debug_socket_count().await;
    assert_eq!(baseline, 1, "expected exactly one listener socket at rest");

    let client_ip = Ipv4Address::new(10, 254, 9, 9);
    let mut client_iface = client_iface(&mut client_dev, client_ip);

    for i in 0..4u16 {
        let mut client_sockets = SocketSet::new(Vec::new());
        let handle = client_sockets.add(new_tcp_socket());
        {
            let sock = client_sockets.get_mut::<tcp::Socket>(handle);
            let cx = client_iface.context();
            sock.connect(cx, (IpAddress::Ipv4(vip), 5432u16), (client_ip, 49000 + i))
                .unwrap();
        }

        // Drive to established, then close the client side. The client FINs
        // first, so the stack's socket completes its close via LAST-ACK and
        // reaches `Closed` (no lingering TIME_WAIT to slow the test).
        let mut closed = false;
        for _ in 0..2000 {
            client_iface.poll(Instant::now(), &mut client_dev, &mut client_sockets);
            let sock = client_sockets.get_mut::<tcp::Socket>(handle);
            if !closed && sock.may_send() {
                sock.close();
                closed = true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            // Stop once the client has closed and the stack has reaped the
            // finished connection back to the listeners-only baseline.
            if closed && stack.debug_socket_count().await == baseline {
                break;
            }
        }
        client_sockets.remove(handle);

        // Give the stack task time to reap, then require it is back to baseline.
        let mut count = stack.debug_socket_count().await;
        for _ in 0..100 {
            if count == baseline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            count = stack.debug_socket_count().await;
        }
        assert_eq!(
            count, baseline,
            "connection {i} was not reaped from the SocketSet (socket leak)"
        );
    }

    stack.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_service_port_routes_by_destination_vip() {
    tracing_subscriber_try_init();

    let backend_a = fixed_response_backend(b"backend-a").await;
    let backend_b = fixed_response_backend(b"backend-b").await;

    let mut table = ServiceTable::new();
    let svc_a = table.register("rust-demo1".to_string(), backend_a, 8080, 0);
    let svc_b = table.register("rust-demo2".to_string(), backend_b, 8080, 0);

    assert_ne!(
        svc_a.vip, svc_b.vip,
        "distinct names must receive distinct virtual IPs"
    );
    assert_eq!(svc_a.service_port, svc_b.service_port);

    let (stack_dev, mut client_dev) = MockDevice::pair();
    let stack =
        VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

    let client_ip = Ipv4Address::new(10, 254, 9, 9);
    let mut client_iface = client_iface(&mut client_dev, client_ip);
    let mut client_sockets = SocketSet::new(Vec::new());

    let got_a = connect_and_read(
        &mut client_iface,
        &mut client_dev,
        &mut client_sockets,
        svc_a.vip,
        8080,
        client_ip,
        49101,
    )
    .await;
    let got_b = connect_and_read(
        &mut client_iface,
        &mut client_dev,
        &mut client_sockets,
        svc_b.vip,
        8080,
        client_ip,
        49102,
    )
    .await;

    assert_eq!(got_a, b"backend-a");
    assert_eq!(got_b, b"backend-b");

    stack.shutdown().await.unwrap();
}

/// Shutdown should stop the stack task cleanly without panicking and reject
/// further use gracefully.
#[tokio::test]
async fn shutdown_is_graceful() {
    let (stack_dev, _client_dev) = MockDevice::pair();
    let table = ServiceTable::new();
    let stack =
        VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

    stack.shutdown().await.unwrap();
    // Give the task a moment to exit.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    // Sending another command after shutdown must not panic; it may simply be
    // dropped if the receiver is gone.
    let _ = stack.update_services(ServiceTable::new()).await;
}

/// Updating services adds listeners for new ports without disrupting the
/// stack.
#[tokio::test]
async fn update_services_adds_ports() {
    let (stack_dev, _client_dev) = MockDevice::pair();
    let stack = VirtualStack::spawn_with_device(
        stack_dev,
        ServiceTable::new(),
        None,
        OverlayHttpsPolicy::default(),
    );

    let mut table = ServiceTable::new();
    table.register("svc-a".to_string(), "127.0.0.1:1".parse().unwrap(), 8080, 0);
    table.register("svc-b".to_string(), "127.0.0.1:2".parse().unwrap(), 9090, 0);
    stack.update_services(table).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    stack.shutdown().await.unwrap();
}

#[test]
fn port_80_service_redirects_http_and_terminates_https() {
    let mut table = ServiceTable::new();
    let svc = table.register("web".to_string(), "127.0.0.1:49152".parse().unwrap(), 80, 0);

    let policy = OverlayHttpsPolicy {
        enable_for_port_80: true,
        redirect_port_80: true,
        passthrough_port_443: true,
    };
    assert_eq!(
        select_connection_action(&table, true, policy, svc.vip, 80),
        Some(ConnectionAction::RedirectToHttps)
    );
    assert_eq!(
        select_connection_action(&table, true, policy, svc.vip, 443),
        Some(ConnectionAction::TlsTerminate(svc.real_addr))
    );
}

#[test]
fn port_443_service_passes_https_through() {
    let mut table = ServiceTable::new();
    let svc = table.register_with_backend_protocol(
        "secure".to_string(),
        "127.0.0.1:49153".parse().unwrap(),
        443,
        0,
        Some(Canonical::Tls),
    );

    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 443),
        Some(ConnectionAction::PlainProxy(svc.real_addr))
    );
}

#[test]
fn port_443_plain_http_backend_terminates_tls() {
    let mut table = ServiceTable::new();
    let svc = table.register_with_backend_protocol(
        "staging".to_string(),
        "127.0.0.1:49154".parse().unwrap(),
        443,
        0,
        Some(Canonical::Http),
    );

    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 443),
        Some(ConnectionAction::TlsTerminate(svc.real_addr))
    );
}

#[test]
fn port_443_unknown_backend_terminates_tls_when_available() {
    let mut table = ServiceTable::new();
    let svc = table.register(
        "staging".to_string(),
        "127.0.0.1:49155".parse().unwrap(),
        443,
        0,
    );

    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 443),
        Some(ConnectionAction::TlsTerminate(svc.real_addr))
    );
}

#[test]
fn management_dashboard_remains_http_only() {
    let mut table = ServiceTable::new();
    let svc = table.register(
        "portzero".to_string(),
        "127.0.0.1:49156".parse().unwrap(),
        80,
        0,
    );

    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 80),
        Some(ConnectionAction::PlainProxy(svc.real_addr))
    );
    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 443),
        None
    );
}

#[test]
fn management_api_remains_http_only() {
    let mut table = ServiceTable::new();
    let svc = table.register(
        "portzero-api".to_string(),
        "127.0.0.1:49157".parse().unwrap(),
        80,
        0,
    );

    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 80),
        Some(ConnectionAction::PlainProxy(svc.real_addr))
    );
    assert_eq!(
        select_connection_action(&table, true, OverlayHttpsPolicy::default(), svc.vip, 443),
        None
    );
}

#[test]
fn disabled_port_80_https_keeps_http_plain() {
    let mut table = ServiceTable::new();
    let svc = table.register("web".to_string(), "127.0.0.1:49152".parse().unwrap(), 80, 0);
    let policy = OverlayHttpsPolicy {
        enable_for_port_80: false,
        ..OverlayHttpsPolicy::default()
    };

    assert_eq!(
        select_connection_action(&table, true, policy, svc.vip, 80),
        Some(ConnectionAction::PlainProxy(svc.real_addr))
    );
    assert_eq!(
        select_connection_action(&table, true, policy, svc.vip, 443),
        None
    );
}

#[test]
fn redirect_location_uses_host_and_path() {
    let req = b"GET /docs?q=1 HTTP/1.1\r\nHost: app.portzero.local:80\r\n\r\n";
    assert_eq!(
        redirect_location(req),
        "https://app.portzero.local/docs?q=1"
    );
}

fn tracing_subscriber_try_init() {}

async fn fixed_response_backend(response: &'static [u8]) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _ = sock.write_all(response).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    backend_addr
}

/// A backend that echoes whatever it receives and closes when the peer's write
/// half closes. Used to drive a full open→use→close connection lifecycle.
async fn echo_backend() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(p) => p,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }
    });
    backend_addr
}

async fn connect_and_read(
    client_iface: &mut Interface,
    client_dev: &mut MockDevice,
    client_sockets: &mut SocketSet<'_>,
    vip: Ipv4Address,
    dst_port: u16,
    client_ip: Ipv4Address,
    src_port: u16,
) -> Vec<u8> {
    let handle = client_sockets.add(new_tcp_socket());

    {
        let sock = client_sockets.get_mut::<tcp::Socket>(handle);
        let cx = client_iface.context();
        sock.connect(cx, (IpAddress::Ipv4(vip), dst_port), (client_ip, src_port))
            .unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..2000 {
        client_iface.poll(Instant::now(), client_dev, client_sockets);

        let sock = client_sockets.get_mut::<tcp::Socket>(handle);
        if sock.can_recv() {
            let mut buf = vec![0u8; 4096];
            if let Ok(n) = sock.recv_slice(&mut buf) {
                received.extend_from_slice(&buf[..n]);
            }
        }
        if !received.is_empty() {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    client_sockets.remove(handle);
    received
}
