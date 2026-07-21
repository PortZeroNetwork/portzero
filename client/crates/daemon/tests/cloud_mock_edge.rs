//! Hermetic end-to-end test for the cloud tunnel client against a MOCK edge.
//!
//! This closes the biggest client-side cloud gap: [`CloudConnector`] had only
//! unit tests of individual message handlers, but nothing exercised the full
//! wire cycle. This test stands up a real `tokio-tungstenite` WebSocket server
//! that speaks the actual proto and a tiny local HTTP backend, then drives a
//! real `CloudConnector` (pointed at the mock via `PZ_TUNNEL_EDGE_URL`) through:
//!
//!   connect → Hello handshake → Welcome → RegisterRoute → RouteAck →
//!   HttpRequest → forward-to-local-backend → HttpResponse → Ping → Pong
//!
//! No root, no real network, no staging — only loopback sockets on ephemeral
//! ports. Every wait is bounded by a timeout so a stuck exchange never hangs CI.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use portzero_daemon::cloud::CloudConnector;
use portzero_proto::{ClientMessage, RouteStatus, ServerMessage};
use portzero_tunnel_client::domain_router::DomainRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Fixed body served by the mock local backend, asserted end to end.
const BACKEND_BODY: &[u8] = b"mock-backend-ok";

/// What the mock edge captured off the wire, handed back to the test body so
/// the assertions live next to the orchestration rather than buried in a task.
#[derive(Debug)]
struct EdgeOutcome {
    hello_protocol: u32,
    hello_auth: String,
    hello_client_version: String,
    register_domain: String,
    register_port: u16,
    response_status: u16,
    response_body: Vec<u8>,
    pong_ts: u64,
}

/// Spawn a minimal local HTTP/1.1 backend on an ephemeral loopback port that
/// answers every request with `200 OK` and [`BACKEND_BODY`]. This stands in for
/// the developer's real local service the tunnel forwards to.
async fn spawn_mock_backend() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            tokio::spawn(async move {
                // Consume the request head (a forwarded GET has no body). We do
                // not care about its contents, only that we reply afterwards.
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    BACKEND_BODY.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(BACKEND_BODY).await;
                let _ = sock.flush().await;
            });
        }
    });
    port
}

/// Read the next `ClientMessage` text frame from the tunnel, skipping any
/// transport-level ping/pong/binary frames.
async fn recv_client(ws: &mut WebSocketStream<TcpStream>) -> ClientMessage {
    loop {
        let frame = ws
            .next()
            .await
            .expect("edge: client stream ended early")
            .expect("edge: websocket read error");
        match frame {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("edge: malformed client json");
            }
            Message::Close(_) => panic!("edge: client closed the tunnel unexpectedly"),
            _ => continue,
        }
    }
}

/// Serialize and send a `ServerMessage` as a text frame.
async fn send_server(ws: &mut WebSocketStream<TcpStream>, msg: &ServerMessage) {
    let json = serde_json::to_string(msg).expect("edge: serialize server message");
    ws.send(Message::Text(json.into()))
        .await
        .expect("edge: websocket send error");
}

/// The scripted mock edge: accept one WebSocket connection and walk the real
/// protocol handshake and forwarding cycle, capturing what the client sent.
async fn mock_edge_server(
    listener: TcpListener,
    domain: String,
    path: String,
    ping_ts: u64,
) -> EdgeOutcome {
    let (stream, _) = listener
        .accept()
        .await
        .expect("edge: accept tcp connection");
    let mut ws = tokio_tungstenite::accept_async(stream)
        .await
        .expect("edge: websocket handshake failed");

    // 1. Hello handshake.
    let (hello_protocol, hello_auth, hello_client_version) = match recv_client(&mut ws).await {
        ClientMessage::Hello {
            protocol_version,
            auth_token,
            client_version,
            machine_id,
            os,
        } => {
            assert!(!auth_token.is_empty(), "Hello.auth_token must be present");
            assert!(
                !client_version.is_empty(),
                "Hello.client_version must be present"
            );
            assert!(!machine_id.is_empty(), "Hello.machine_id must be present");
            assert!(!os.is_empty(), "Hello.os must be present");
            (protocol_version, auth_token, client_version)
        }
        other => panic!("edge: expected Hello, got {other:?}"),
    };

    // 2. Accept the handshake.
    send_server(
        &mut ws,
        &ServerMessage::Welcome {
            session_id: "sess_mock".into(),
            account_id: "acct_mock".into(),
            plan: "pro".into(),
            can_use_cloud_tunnels: true,
        },
    )
    .await;

    // 3. RegisterRoute for our test domain.
    let (register_domain, register_port) = match recv_client(&mut ws).await {
        ClientMessage::RegisterRoute {
            domain,
            local_port,
            protocol,
            ..
        } => {
            assert_eq!(
                protocol,
                portzero_proto::RouteProtocol::Http,
                "cloud HTTP tunnel must register as Http"
            );
            (domain, local_port)
        }
        other => panic!("edge: expected RegisterRoute, got {other:?}"),
    };

    // 4. Acknowledge it as published (also drives the client's route-status map).
    send_server(
        &mut ws,
        &ServerMessage::RouteAck {
            domain: register_domain.clone(),
            success: true,
            error: None,
            url: Some(format!("https://{register_domain}")),
            status: Some(RouteStatus::Published),
        },
    )
    .await;

    // 5. Forward an HTTP request and expect the backend's response back.
    send_server(
        &mut ws,
        &ServerMessage::HttpRequest {
            request_id: 7,
            method: "GET".into(),
            path: path.clone(),
            host: domain.clone(),
            headers: vec![("Host".into(), domain.clone())],
            body: vec![],
        },
    )
    .await;
    let (response_status, response_body) = match recv_client(&mut ws).await {
        ClientMessage::HttpResponse {
            request_id,
            status,
            body,
            ..
        } => {
            assert_eq!(request_id, 7, "HttpResponse must echo the request_id");
            (status, body)
        }
        other => panic!("edge: expected HttpResponse, got {other:?}"),
    };

    // 6. Keep-alive: Ping must be answered with a matching Pong.
    send_server(&mut ws, &ServerMessage::Ping { timestamp: ping_ts }).await;
    let pong_ts = match recv_client(&mut ws).await {
        ClientMessage::Pong { timestamp } => timestamp,
        other => panic!("edge: expected Pong, got {other:?}"),
    };

    EdgeOutcome {
        hello_protocol,
        hello_auth,
        hello_client_version,
        register_domain,
        register_port,
        response_status,
        response_body,
        pong_ts,
    }
}

/// Full hermetic cloud tunnel flow against a mock edge and a mock local backend.
#[tokio::test]
async fn cloud_connector_full_flow_against_mock_edge() {
    // Client TLS users ask rustls for the process-default provider; the cloud
    // path expects it initialized before first use.
    portzero_daemon::install_default_crypto_provider();

    // 1. Local backend the tunnel forwards to.
    let backend_port = spawn_mock_backend().await;

    // 2. Mock edge on an ephemeral port; point the connector at it via the env
    //    override. This is the only test in this binary, so the process-global
    //    env var cannot race a sibling test.
    let edge_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let edge_port = edge_listener.local_addr().unwrap().port();
    std::env::set_var(
        "PZ_TUNNEL_EDGE_URL",
        format!("ws://127.0.0.1:{edge_port}/tunnel"),
    );

    let domain = "mock-app.testuser.portzero.cloud".to_string();
    let path = "/hello".to_string();
    let ping_ts = 987_654u64;

    let edge = tokio::spawn(mock_edge_server(
        edge_listener,
        domain.clone(),
        path.clone(),
        ping_ts,
    ));

    // 3. The domain router the reader uses to resolve host -> local port. In
    //    production the discovery loop populates this alongside register_route.
    let router = DomainRouter::new();
    router.add_route(domain.clone(), backend_port);

    // 4. Drive a real CloudConnector: connect (sends Hello) then register.
    let auth_token = "tok_mock_abc123".to_string();
    let mut connector = CloudConnector::new(auth_token.clone());
    connector
        .connect(router)
        .await
        .expect("connect to mock edge");
    connector
        .register_route(&domain, backend_port, None)
        .await
        .expect("register route with mock edge");

    // 5. Wait for the scripted exchange to complete (bounded).
    let outcome = tokio::time::timeout(Duration::from_secs(15), edge)
        .await
        .expect("mock edge did not complete the exchange in time")
        .expect("mock edge task panicked");

    // Hello handshake.
    assert_eq!(
        outcome.hello_protocol,
        portzero_proto::PROTOCOL_VERSION,
        "client must announce the current protocol version"
    );
    assert_eq!(outcome.hello_auth, auth_token);
    assert!(!outcome.hello_client_version.is_empty());

    // RegisterRoute.
    assert_eq!(outcome.register_domain, domain);
    assert_eq!(outcome.register_port, backend_port);

    // HttpRequest was forwarded to the local backend and the response relayed.
    assert_eq!(outcome.response_status, 200);
    assert_eq!(outcome.response_body, BACKEND_BODY);

    // Ping/Pong keep-alive.
    assert_eq!(outcome.pong_ts, ping_ts);

    // The RouteAck (Published) was recorded for the local dashboard, and the
    // connector still reports a live, fully-wired connection.
    assert_eq!(
        connector.route_statuses().get(&domain).map(String::as_str),
        Some("published"),
        "RouteAck status should be recorded for the dashboard"
    );
    assert!(
        connector.is_connected(),
        "connector should report a live connection after the full exchange"
    );

    std::env::remove_var("PZ_TUNNEL_EDGE_URL");
}
