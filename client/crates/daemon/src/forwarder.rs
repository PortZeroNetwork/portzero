//! Local request forwarding: proxies HTTP requests from the edge to local services.
//!
//! When the edge server sends an HttpRequest through the tunnel, this module
//! builds a corresponding request to the local service (on localhost) and
//! collects the response to send back.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::LazyLock;
use std::time::Duration;

use anyhow::Result;
use portzero_proto::ClientMessage;
use tracing::{debug, warn};

const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

static FORWARD_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("static local forwarding client configuration is valid")
});

/// Forward an HTTP request from the edge to a local service.
///
/// Builds a request to `http://{local_addr}{path}`, sets the forwarded
/// headers, sends it, and returns a `ClientMessage::HttpResponse`.
///
/// `local_addr` carries the address family the service is actually listening
/// on, so an IPv6-only process is dialed on `[::1]` rather than an empty
/// `127.0.0.1`. `SocketAddr`'s `Display` already brackets IPv6, which is what
/// a URL authority needs.
///
/// On connection failure, returns a 502 Bad Gateway response rather than
/// an error, so the tunnel can relay a meaningful status to the end user.
pub async fn forward_request(
    request_id: u64,
    method: &str,
    path: &str,
    host: &str,
    headers: &[(String, String)],
    body: &[u8],
    local_addr: SocketAddr,
) -> Result<ClientMessage> {
    let url = format!("http://{}{}", local_addr, path);
    debug!(request_id, %method, %url, %host, "Forwarding request to local service");

    // The edge protocol carries whole requests and responses, so there is no
    // way to hand a Cloud tunnel's connection over to a WebSocket. The hop-by-hop
    // filter below would strip `Upgrade`/`Connection` and forward the handshake
    // as an ordinary GET, which the local service answers 200 — leaving the
    // client waiting on a socket that will never open. Say so instead.
    if is_websocket_upgrade(headers) {
        warn!(request_id, %host, "Rejected WebSocket upgrade over a Cloud tunnel");
        return Ok(not_implemented(
            request_id,
            format!(
                "Cloud tunnels ({host}) forward HTTP requests and responses only, so this \
                 WebSocket upgrade cannot be completed.\n\n\
                 Local tunnels do support WebSockets: a *.portzero.local name proxies raw \
                 TCP, so live reload, HMR, and any other WebSocket traffic work over it. \
                 Point the WebSocket at a Local tunnel, or reach the service directly on \
                 {local_addr} while keeping the Cloud tunnel for HTTP."
            ),
        ));
    }

    let req_method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);

    let mut builder = FORWARD_CLIENT.request(req_method, &url);

    // reqwest owns framing. Never relay hop-by-hop or caller-supplied forwarding
    // metadata into the local process.
    let connection_headers = connection_header_names(headers);
    for (key, value) in headers {
        if is_filtered_request_header(key, &connection_headers) {
            continue;
        }
        builder = builder.header(key.as_str(), value.as_str());
    }

    // Add forwarding metadata
    builder = builder.header("X-Forwarded-For", "tunnel");
    builder = builder.header("X-Forwarded-Proto", "https");
    builder = builder.header("X-Forwarded-Host", host);

    if !body.is_empty() {
        builder = builder.body(body.to_vec());
    }

    let response = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(request_id, "Local service error: {e}");
            return Ok(bad_gateway(
                request_id,
                format!("could not reach the local process at {local_addr}: {e}"),
            ));
        }
    };

    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BODY_BYTES as u64)
    {
        return Ok(bad_gateway(
            request_id,
            format!("local response exceeds the {MAX_RESPONSE_BODY_BYTES}-byte limit"),
        ));
    }

    let status = response.status().as_u16();
    let response_connection_headers = connection_header_names(
        &response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>(),
    );
    let resp_headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name.as_str(), &response_connection_headers))
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let mut response = response;
    let mut resp_body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if resp_body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            return Ok(bad_gateway(
                request_id,
                format!("local response exceeds the {MAX_RESPONSE_BODY_BYTES}-byte limit"),
            ));
        }
        resp_body.extend_from_slice(&chunk);
    }

    debug!(
        request_id,
        status,
        body_len = resp_body.len(),
        "Local service responded"
    );

    Ok(ClientMessage::HttpResponse {
        request_id,
        status,
        headers: resp_headers,
        body: resp_body,
    })
}

fn connection_header_names(headers: &[(String, String)]) -> HashSet<String> {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
        .flat_map(|(_, value)| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect()
}

fn is_hop_by_hop(key: &str, connection_headers: &HashSet<String>) -> bool {
    let key = key.to_ascii_lowercase();
    connection_headers.contains(&key)
        || matches!(
            key.as_str(),
            "connection"
                | "proxy-connection"
                | "keep-alive"
                | "transfer-encoding"
                | "upgrade"
                | "te"
                | "trailer"
        )
}

fn is_filtered_request_header(key: &str, connection_headers: &HashSet<String>) -> bool {
    is_hop_by_hop(key, connection_headers)
        || matches!(
            key.to_ascii_lowercase().as_str(),
            "host" | "x-forwarded-for" | "x-forwarded-host" | "x-forwarded-proto"
        )
}

/// Whether these request headers are a WebSocket handshake.
///
/// Both halves are required by RFC 6455 (`Connection: Upgrade` plus
/// `Upgrade: websocket`), and `Connection` is a comma-separated list, so match
/// on tokens rather than the whole value.
fn is_websocket_upgrade(headers: &[(String, String)]) -> bool {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    let upgrades_to_websocket = header("upgrade").is_some_and(|value| {
        value
            .split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
    });
    let connection_upgrade = header("connection").is_some_and(|value| {
        value
            .split(',')
            .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
    });

    upgrades_to_websocket && connection_upgrade
}

fn not_implemented(request_id: u64, detail: String) -> ClientMessage {
    ClientMessage::HttpResponse {
        request_id,
        status: 501,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: detail.into_bytes(),
    }
}

fn bad_gateway(request_id: u64, detail: String) -> ClientMessage {
    ClientMessage::HttpResponse {
        request_id,
        status: 502,
        headers: vec![("Content-Type".into(), "text/plain".into())],
        body: format!("Bad Gateway: {detail}").into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The IPv4-loopback address these tests forward to. Nothing listens on
    /// these ports; the point is what the forwarder does when the dial fails.
    fn local(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn filters_hop_by_hop_untrusted_forwarding_and_connection_named_headers() {
        let headers = vec![("Connection".to_string(), "X-Remove".to_string())];
        let connection_headers = connection_header_names(&headers);
        for name in [
            "Host",
            "Connection",
            "Transfer-Encoding",
            "X-Forwarded-For",
            "X-Forwarded-Host",
            "X-Forwarded-Proto",
            "X-Remove",
        ] {
            assert!(
                is_filtered_request_header(name, &connection_headers),
                "{name}"
            );
        }
        assert!(!is_filtered_request_header(
            "authorization",
            &connection_headers
        ));
    }

    #[test]
    fn detects_websocket_handshakes_and_nothing_else() {
        let headers = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };

        assert!(is_websocket_upgrade(&headers(&[
            ("Upgrade", "websocket"),
            ("Connection", "Upgrade"),
        ])));
        // Real clients send mixed case and multi-token Connection values.
        assert!(is_websocket_upgrade(&headers(&[
            ("upgrade", "WebSocket"),
            ("connection", "keep-alive, Upgrade"),
        ])));
        // Half a handshake is not a handshake — forward it as ordinary HTTP.
        assert!(!is_websocket_upgrade(&headers(&[("Upgrade", "websocket")])));
        assert!(!is_websocket_upgrade(&headers(&[(
            "Connection",
            "Upgrade"
        )])));
        assert!(!is_websocket_upgrade(&headers(&[(
            "Connection",
            "keep-alive"
        )])));
        assert!(!is_websocket_upgrade(&[]));
    }

    #[tokio::test]
    async fn websocket_upgrade_over_a_cloud_tunnel_is_refused_with_guidance() {
        // Nothing listens on this port: reaching the 501 proves the handshake
        // is rejected before any forwarding is attempted, rather than being
        // stripped and answered as a plain GET.
        let result = forward_request(
            7,
            "GET",
            "/socket",
            "api.alice.tunnel.portzero.cloud",
            &[
                ("Upgrade".to_string(), "websocket".to_string()),
                ("Connection".to_string(), "Upgrade".to_string()),
            ],
            &[],
            local(19997),
        )
        .await
        .unwrap();

        match result {
            ClientMessage::HttpResponse {
                request_id,
                status,
                body,
                ..
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(status, 501);
                let body = String::from_utf8_lossy(&body);
                // The message must say where WebSockets *do* work, not just
                // that this failed.
                assert!(body.contains("portzero.local"), "{body}");
                assert!(body.contains("19997"), "{body}");
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    /// Answer exactly one request with a canned 200 and return the address to
    /// forward to. `bind` decides the address family under test.
    async fn one_shot_backend(bind: &str) -> Option<SocketAddr> {
        let listener = tokio::net::TcpListener::bind(bind).await.ok()?;
        let addr = listener.local_addr().ok()?;
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                    .await;
                let _ = stream.flush().await;
            }
        });
        Some(addr)
    }

    #[tokio::test]
    async fn forwards_to_an_ipv6_only_backend() {
        // The regression this exists for: a dev server bound to [::1] alone
        // (Vite/Node >= 17 resolving "localhost" to ::1 first) used to get a
        // hardcoded 127.0.0.1 dial and answer nothing at all.
        let Some(addr) = one_shot_backend("[::1]:0").await else {
            eprintln!("skipping: no IPv6 loopback on this host");
            return;
        };
        assert!(addr.is_ipv6());

        let result = forward_request(1, "GET", "/", "vite.test.portzero.cloud", &[], &[], addr)
            .await
            .unwrap();

        match result {
            ClientMessage::HttpResponse { status, body, .. } => {
                assert_eq!(status, 200);
                assert_eq!(&body[..], b"hi");
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn forwards_to_an_ipv4_backend() {
        let addr = one_shot_backend("127.0.0.1:0")
            .await
            .expect("IPv4 loopback is always available");

        let result = forward_request(2, "GET", "/", "api.test.portzero.cloud", &[], &[], addr)
            .await
            .unwrap();

        match result {
            ClientMessage::HttpResponse { status, .. } => assert_eq!(status, 200),
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unreachable_ipv6_backend_names_the_bracketed_address() {
        // The 502 has to say which address failed — "port 5173" would not
        // distinguish the family, which is the whole diagnosis here.
        let addr = SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), 19996);
        let result = forward_request(3, "GET", "/", "vite.test.portzero.cloud", &[], &[], addr)
            .await
            .unwrap();

        match result {
            ClientMessage::HttpResponse { status, body, .. } => {
                assert_eq!(status, 502);
                let body = String::from_utf8_lossy(&body);
                assert!(body.contains("[::1]:19996"), "{body}");
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forward_to_unreachable_port() {
        // Forwarding to a port with nothing listening should return 502
        let result = forward_request(
            1,
            "GET",
            "/health",
            "api.test.portzero.cloud",
            &[],
            &[],
            local(19999), // unlikely to be in use
        )
        .await
        .unwrap();

        match result {
            ClientMessage::HttpResponse {
                request_id,
                status,
                body,
                ..
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(status, 502);
                let body_str = String::from_utf8_lossy(&body);
                assert!(body_str.contains("Bad Gateway"));
                assert!(body_str.contains("19999"));
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_forward_preserves_request_id() {
        let result = forward_request(
            42,
            "POST",
            "/api/data",
            "api.test.portzero.cloud",
            &[("Content-Type".to_string(), "application/json".to_string())],
            b"{}",
            local(19998),
        )
        .await
        .unwrap();

        match result {
            ClientMessage::HttpResponse { request_id, .. } => {
                assert_eq!(request_id, 42);
            }
            other => panic!("Expected HttpResponse, got {:?}", other),
        }
    }
}
