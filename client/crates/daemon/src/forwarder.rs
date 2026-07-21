//! Local request forwarding: proxies HTTP requests from the edge to local services.
//!
//! When the edge server sends an HttpRequest through the tunnel, this module
//! builds a corresponding request to the local service (on localhost) and
//! collects the response to send back.

use std::collections::HashSet;
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
/// Builds a request to `http://127.0.0.1:{local_port}{path}`, sets the
/// forwarded headers, sends it, and returns a `ClientMessage::HttpResponse`.
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
    local_port: u16,
) -> Result<ClientMessage> {
    let url = format!("http://127.0.0.1:{}{}", local_port, path);
    debug!(request_id, %method, %url, %host, "Forwarding request to local service");

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
                format!("could not reach local process on port {local_port}: {e}"),
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
            19999, // unlikely to be in use
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
            19998,
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
