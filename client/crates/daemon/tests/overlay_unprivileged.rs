//! Unprivileged, in-process e2e tests for the virtual overlay network.
//!
//! * [`unprivileged_overlay_round_trip`] runs in plain `cargo test` with NO
//!   root. It exercises the overlay components that are reachable through the
//!   daemon's public API: VIP allocation, the [`ServiceTable`], the embedded
//!   authoritative DNS server (`name.portzero.local` -> VIP), and a real tokio
//!   backend that the overlay would proxy to. This asserts the *wiring*
//!   (name -> VIP -> real backend).
//!
//! * [`unprivileged_vip_byte_proxy`] also unprivileged. It drives REAL bytes
//!   through the smoltcp user-space `StackEngine`: a client smoltcp interface
//!   connects to `VIP:port` across an in-memory `MockDevice`, and the payload is
//!   proxied to a real tokio backend and echoed back. This asserts the actual
//!   byte path (mirroring the in-crate unit test
//!   `net::stack::tests::vip_connect_proxies_to_backend`) using the
//!   `#[doc(hidden)]` test-support helpers exposed from `net::stack`.
//!
//! * [`unprivileged_contested_name_serves_exactly_one_backend`] covers the case
//!   where several live backends claim one `PZ_TUNNEL` name — leaked processes
//!   from repeated test runs, the shape that made a stale backend answer
//!   requests while `portzero status` reported the name healthy. It drives real
//!   bytes to the VIP through the production overlay table and asserts *which*
//!   backend answered, that the answer does not depend on scan order, and that
//!   the ambiguity is reported rather than silently resolved.
//!
//! TLS termination over the same byte path is covered separately in
//! `overlay_tls.rs`; the privileged real-TUN/trust-store scenarios live in
//! `overlay_e2e.rs`. Every wait here is bounded by a timeout — there is no
//! unbounded blocking.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use portzero_daemon::net::dns::OverlayDnsServer;
use portzero_daemon::net::service_table::ServiceTable;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use common::{dns_query_a, spawn_echo_backend, spawn_identifying_backend, TEST_TIMEOUT};

/// Unprivileged, in-process e2e: wires together the public overlay surface and
/// asserts name -> VIP resolution plus a reachable real backend. No root, no
/// real TUN, no network/cloud login. All waits are bounded.
#[tokio::test]
async fn unprivileged_overlay_round_trip() {
    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. A real backend bound to an ephemeral ("port 0") address.
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.portzero.local` service in the shared table the DNS
        //    server reads from. The service is reachable on VIP:5432 and proxies
        //    to the real ephemeral backend.
        let services: Arc<RwLock<ServiceTable>> = Arc::new(RwLock::new(ServiceTable::new()));
        let vip = {
            let mut table = services.write().await;
            let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
            // VIP must come from the 10.254.0.0/16 overlay block.
            let o = svc.vip.0;
            assert_eq!([o[0], o[1]], [10, 254], "VIP not in overlay subnet");
            Ipv4Addr::new(o[0], o[1], o[2], o[3])
        };

        // Re-registering the same name must keep the same VIP (stable identity).
        {
            let mut table = services.write().await;
            let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
            assert_eq!(svc.vip.0, vip.octets(), "VIP changed on re-register");
        }

        // 3. Stand up the *real* embedded authoritative DNS server on an
        //    ephemeral UDP port and confirm name -> VIP resolution end to end.
        let dns_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        // Bind ourselves to learn the chosen port, then hand the bound addr to
        // the server (it rebinds; both use SO_REUSEADDR-free fresh sockets, so
        // we instead let the server bind 0 and discover via a probe loop).
        let probe = UdpSocket::bind(dns_addr).await.unwrap();
        let server_addr = probe.local_addr().unwrap();
        drop(probe); // free the port for the server to claim immediately

        let dns = OverlayDnsServer::new(services.clone(), server_addr);
        let dns_task = tokio::spawn(async move {
            let _ = dns.run().await;
        });

        // Resolve the registered name; retry briefly while the server binds.
        let mut resolved = None;
        for _ in 0..50 {
            if let Some(ip) = dns_query_a(server_addr, "my-db.portzero.local").await {
                resolved = Some(ip);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            resolved,
            Some(vip),
            "DNS did not resolve my-db.portzero.local to its VIP"
        );

        // An unknown name under our zone must NOT resolve to an address.
        let unknown = dns_query_a(server_addr, "nope.portzero.local").await;
        assert_eq!(unknown, None, "unknown name unexpectedly resolved");

        // 4. The real backend the overlay proxies to is reachable and echoes.
        //    (The smoltcp byte-proxy across the VIP is asserted separately by
        //    `unprivileged_vip_byte_proxy`.)
        let mut client = tokio::net::TcpStream::connect(backend_addr).await.unwrap();
        let payload = b"hello virtual overlay";
        client.write_all(payload).await.unwrap();
        let mut got = vec![0u8; payload.len()];
        client.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, payload, "backend did not echo payload");

        dns_task.abort();
    })
    .await
    .expect("unprivileged overlay round-trip timed out");
}

/// Unprivileged, in-process e2e that drives REAL bytes through the smoltcp
/// `StackEngine`. Unlike [`unprivileged_overlay_round_trip`] (which asserts the
/// public wiring + a directly-reachable backend), this test connects a client
/// smoltcp interface to `VIP:port` across an in-memory [`MockDevice`], so the
/// payload actually traverses the user-space TCP stack and is proxied to the
/// real tokio backend and echoed back. No root, no real TUN. All waits bounded.
#[tokio::test]
async fn unprivileged_vip_byte_proxy() {
    use portzero_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, OverlayHttpsPolicy, TestInstant,
        TestIpAddress, TestIpv4Address, TestSocketSet, VirtualStack,
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Start a real backend echo server (the "port 0" backend).
        let backend_addr = spawn_echo_backend().await;

        // 2. Register a `*.portzero.local` service mapping VIP:5432 -> backend.
        let mut table = ServiceTable::new();
        let svc = table.register("my-db".to_string(), backend_addr, 5432, 0);
        let vip = svc.vip;
        let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

        // 3. Spawn the real stack engine on one half of an in-memory device pair.
        let (stack_dev, mut client_dev) = MockDevice::pair();
        let stack =
            VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

        // 4. Build a client smoltcp interface on the other half and open a TCP
        //    connection to VIP:5432 from a client IP in the same subnet.
        let client_ip = TestIpv4Address::new(10, 254, 9, 9);
        let mut client_iface = client_iface(&mut client_dev, client_ip);
        let mut client_sockets = TestSocketSet::new(Vec::new());
        let client_handle = client_sockets.add(new_tcp_socket());
        {
            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
            let cx = client_iface.context();
            sock.connect(
                cx,
                (TestIpAddress::Ipv4(vip_v4), 5432u16),
                (client_ip, 49000u16),
            )
            .unwrap();
        }

        // 5. Drive both sides: send a payload through the stack and assert the
        //    backend echo comes back across the user-space byte-proxy.
        let payload = b"hello virtual overlay";
        let mut sent = false;
        let mut received = Vec::new();

        for _ in 0..2000 {
            client_iface.poll(TestInstant::now(), &mut client_dev, &mut client_sockets);

            let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
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

            // Let the stack task run its poll loop and the backend echo.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert!(
            sent,
            "client never reached a sendable state (handshake failed)"
        );
        assert_eq!(
            received, payload,
            "byte-proxy did not echo payload through the smoltcp stack"
        );

        stack.shutdown().await.unwrap();
    })
    .await
    .expect("unprivileged vip byte-proxy timed out");
}

/// Unprivileged, in-process system test for a tunnel name claimed by more than
/// one live backend.
///
/// This is the failure the claim-ranking exists for: backends left running by
/// repeated test runs all set `PZ_TUNNEL` to one name, an arbitrary one of them
/// answered requests — including one wired to a deleted database volume — and
/// nothing reported the ambiguity, so its 500s read as application bugs.
///
/// Two real backends that name themselves are registered under one name through
/// the *production* table builder, then real bytes are driven to the VIP across
/// the smoltcp stack. This asserts three things the unit tests cannot: the reply
/// comes from the lowest-pid backend, the answer is identical when discovery
/// reports the claimants in the opposite order, and the ambiguity is raised as
/// an issue rather than silently resolved. No root, no real TUN, all waits
/// bounded.
#[tokio::test]
async fn unprivileged_contested_name_serves_exactly_one_backend() {
    use portzero_daemon::discovery::{DiscoveredNetworkService, ServiceSource};
    use portzero_daemon::discovery_loop::build_overlay_table;

    const SERVING: &str = "backend-pid-100";
    const SHADOWED: &str = "backend-pid-900";
    const VIRTUAL_PORT: u16 = 8080;

    tokio::time::timeout(TEST_TIMEOUT, async {
        // 1. Two real backends, each identifying itself so the reply says which
        //    one answered. Both claim "api" from the same working directory —
        //    the leaked-repeated-run shape, not two worktrees.
        let serving_addr = spawn_identifying_backend(SERVING).await;
        let shadowed_addr = spawn_identifying_backend(SHADOWED).await;

        let claimant = |pid: u32, real_addr: SocketAddr| DiscoveredNetworkService {
            name: "api".to_string(),
            domain_template: "api.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr,
            service_port: VIRTUAL_PORT,
            backend_protocol: None,
            pid,
            source: ServiceSource::Process {
                cwd: Some(std::path::PathBuf::from("/work/app")),
            },
            health_path: None,
        };

        let discovered = vec![claimant(900, shadowed_addr), claimant(100, serving_addr)];
        let reversed: Vec<_> = discovered.iter().cloned().rev().collect();

        // 2. The same backend must answer regardless of the order discovery
        //    happened to report the claimants in. A winner that depends on scan
        //    order is what made this look like an intermittent application bug.
        for (pass, services) in [("as-discovered", &discovered), ("reversed", &reversed)] {
            let table = build_overlay_table(services, 0);
            assert_eq!(
                table.len(),
                1,
                "{pass}: one name must resolve to one backend, got {}",
                table.len()
            );

            let answered = banner_through_overlay(table, "api", VIRTUAL_PORT).await;
            assert_eq!(
                answered, SERVING,
                "{pass}: the lowest-pid backend must serve, but {answered} answered"
            );
        }

        // 3. Routing to one backend is only half of it — the daemon must also
        //    say the name is contested and name both claimants. Silence here is
        //    what turned a stale backend into hours of debugging.
        let issues = portzero_daemon::notify::detect_duplicate_names(&discovered);
        assert_eq!(
            issues.len(),
            1,
            "contested name raised no issue: {issues:?}"
        );
        let summary = issues[0].summary();
        assert!(summary.contains("api"), "{summary}");
        assert!(
            summary.contains("pid 100") && summary.contains("[serving]"),
            "summary must name the serving backend: {summary}"
        );
        assert!(
            summary.contains("pid 900") && summary.contains("[shadowed]"),
            "summary must name the shadowed backend: {summary}"
        );
    })
    .await
    .expect("contested-name system test timed out");
}

/// Connect to `name`'s VIP on `port` across the real smoltcp stack driving
/// `table`, and return the banner the backend sent back.
///
/// Both passes of the contested-name test go through here, so the byte path is
/// identical between them and only the claimant ordering differs. The backend
/// speaks first (the engine dials it when the connection is established), so
/// nothing is sent; the read ends when the backend closes.
async fn banner_through_overlay(table: ServiceTable, name: &str, port: u16) -> String {
    use portzero_daemon::net::stack::{
        client_iface, new_tcp_socket, test_tcp, MockDevice, OverlayHttpsPolicy, TestInstant,
        TestIpAddress, TestIpv4Address, TestSocketSet, VirtualStack,
    };

    let vip = table.get(name).expect("name registered").vip;
    let vip_v4 = TestIpv4Address::from_bytes(&vip.0);

    let (stack_dev, mut client_dev) = MockDevice::pair();
    let stack =
        VirtualStack::spawn_with_device(stack_dev, table, None, OverlayHttpsPolicy::default());

    let client_ip = TestIpv4Address::new(10, 254, 9, 9);
    let mut client_iface = client_iface(&mut client_dev, client_ip);
    let mut client_sockets = TestSocketSet::new(Vec::new());
    let client_handle = client_sockets.add(new_tcp_socket());
    {
        let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
        let cx = client_iface.context();
        sock.connect(
            cx,
            (TestIpAddress::Ipv4(vip_v4), port),
            (client_ip, 49001u16),
        )
        .unwrap();
    }

    let mut received = Vec::new();
    for _ in 0..2000 {
        client_iface.poll(TestInstant::now(), &mut client_dev, &mut client_sockets);

        let sock = client_sockets.get_mut::<test_tcp::Socket>(client_handle);
        if sock.can_recv() {
            let mut buf = vec![0u8; 4096];
            if let Ok(n) = sock.recv_slice(&mut buf) {
                received.extend_from_slice(&buf[..n]);
            }
        }
        // The backend writes its banner then closes, so a drained socket that
        // can no longer receive means the whole reply has arrived.
        if !received.is_empty() && !sock.may_recv() {
            break;
        }

        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    stack.shutdown().await.unwrap();

    assert!(
        !received.is_empty(),
        "no backend answered {name}:{port} through the overlay"
    );
    String::from_utf8_lossy(&received).into_owned()
}
