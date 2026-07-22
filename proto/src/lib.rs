//! Shared protocol types for the port-zero tunnel.
//!
//! These types define the wire format between the tunnel client (running on the
//! developer's machine) and the edge server (running in the cloud).
//!
//! ## Framing
//!
//! Messages are JSON-encoded and sent as WebSocket text frames. Each frame
//! contains exactly one JSON object with a `"type"` discriminator field.

use serde::{Deserialize, Serialize};

pub mod codec;
pub mod connection;

/// Current protocol version. Included in the `Hello` handshake so the server
/// can reject incompatible clients.
///
/// v2 adds the cloud-tunnel review flow: `RegisterRoute.metadata`,
/// `RouteAck.status`, and the `RouteStatusChanged` server message. All three
/// additions are backward-compatible via `#[serde(default)]`, so a v1 client
/// and a v2 server (or vice versa) still interoperate during rollout.
pub const PROTOCOL_VERSION: u32 = 2;

// Supporting types

/// Transport protocol for a registered route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteProtocol {
    Http,
    Tcp,
}

/// Review status of a cloud tunnel.
///
/// New cloud tunnels (`*.tunnel.portzero.cloud`) enter `PendingReview` and are
/// not reachable from the public internet until the account owner approves them
/// in app.portzero.cloud. `.portzero.local` overlay tunnels never carry a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteStatus {
    /// Awaiting owner approval; not publicly routable.
    PendingReview,
    /// Approved; publicly routable.
    Published,
    /// Rejected by the owner.
    Denied,
}

/// Identifying details about the process behind a cloud tunnel, shown in the
/// review UI so the owner can see exactly what is being exposed. Best-effort:
/// any field may be absent depending on OS permissions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMetadata {
    /// Working directory of the process, if known.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Resolved executable path, if known.
    #[serde(default)]
    pub exe_path: Option<String>,
    /// Short process name (e.g. "node", "python"), if known.
    #[serde(default)]
    pub process_name: Option<String>,
    /// Full command line, if known.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Hostname of the machine running the daemon, if known.
    #[serde(default)]
    pub hostname: Option<String>,
}

/// Strongly-typed error codes returned by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    AuthFailed,
    RateLimited,
    PlanLimitExceeded,
    DomainNotAllowed,
    InternalError,
}

// Client -> Server messages

/// Messages sent from the tunnel client to the edge server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Initial handshake: authenticate and declare the client.
    Hello {
        protocol_version: u32,
        auth_token: String,
        client_version: String,
        machine_id: String,
        os: String,
    },
    /// Register a route: make a domain reachable via this tunnel.
    RegisterRoute {
        domain: String,
        local_port: u16,
        protocol: RouteProtocol,
        /// Details about the process behind this route, used by the cloud
        /// review UI. Optional for backward compatibility with v1 clients.
        #[serde(default)]
        metadata: Option<RouteMetadata>,
    },
    /// Unregister a route: stop forwarding traffic for this domain.
    UnregisterRoute { domain: String },
    /// Response to an HTTP request forwarded by the edge.
    HttpResponse {
        request_id: u64,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Keep-alive reply.
    Pong { timestamp: u64 },
}

// Server -> Client messages

/// Messages sent from the edge server to the tunnel client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Handshake accepted.
    Welcome {
        session_id: String,
        account_id: String,
        plan: String,
        /// Whether the account can create cloud tunnels right now, considering
        /// both its own plan and any paid team it belongs to.
        can_use_cloud_tunnels: bool,
        /// The account's cloud username. Empty when the edge could not resolve it
        /// (older token / lookup failure). Lets the client pre-validate that a
        /// tunnel's `--<namespace>` scope is the user's own before registering.
        #[serde(default)]
        username: String,
        /// Slugs of every team the account belongs to. Together with `username`
        /// these are the namespaces the account may use on the shared apex, so
        /// the client can flag an unusable namespace locally instead of only as
        /// an edge rejection. Empty when unknown; the edge stays authoritative.
        #[serde(default)]
        team_slugs: Vec<String>,
    },
    /// Acknowledgement of a route registration.
    RouteAck {
        domain: String,
        success: bool,
        error: Option<String>,
        url: Option<String>,
        /// Review status assigned by the cloud. `PendingReview` means the route
        /// registered successfully but is awaiting owner approval and is not yet
        /// publicly reachable. `None` for v1 servers / non-cloud routes.
        #[serde(default)]
        status: Option<RouteStatus>,
    },
    /// An incoming HTTP request to be forwarded to the local service.
    HttpRequest {
        request_id: u64,
        method: String,
        path: String,
        host: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Keep-alive probe.
    Ping { timestamp: u64 },
    /// Server-side error.
    Error { code: ErrorCode, message: String },
    /// Notification that a route has expired or been revoked.
    RouteExpired { domain: String, reason: String },
    /// Notification that a route's review status changed (e.g. the owner
    /// approved a pending tunnel in app.portzero.cloud). Lets the daemon update
    /// its local state and dashboard without re-registering.
    RouteStatusChanged {
        domain: String,
        status: RouteStatus,
        /// Public URL, present once the route is `Published`.
        url: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ClientMessage roundtrips

    #[test]
    fn roundtrip_hello() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok_abc".into(),
            client_version: "0.1.0".into(),
            machine_id: "m-1234".into(),
            os: "linux".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::Hello {
                protocol_version,
                auth_token,
                os,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(auth_token, "tok_abc");
                assert_eq!(os, "linux");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_register_route() {
        let msg = ClientMessage::RegisterRoute {
            domain: "myapp.tunnel.portzero.cloud".into(),
            local_port: 3000,
            protocol: RouteProtocol::Http,
            metadata: Some(RouteMetadata {
                cwd: Some("/home/alice/src/myapp".into()),
                exe_path: Some("/usr/bin/node".into()),
                process_name: Some("node".into()),
                argv: vec!["node".into(), "server.js".into()],
                hostname: Some("alice-laptop".into()),
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::RegisterRoute {
                domain,
                local_port,
                protocol,
                metadata,
            } => {
                assert_eq!(domain, "myapp.tunnel.portzero.cloud");
                assert_eq!(local_port, 3000);
                assert_eq!(protocol, RouteProtocol::Http);
                let meta = metadata.unwrap();
                assert_eq!(meta.process_name.as_deref(), Some("node"));
                assert_eq!(meta.argv, vec!["node", "server.js"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn register_route_without_metadata_deserializes() {
        // A v1 client omits the `metadata` field entirely.
        let json = r#"{"type":"RegisterRoute","domain":"a.tunnel.portzero.cloud","local_port":80,"protocol":"Http"}"#;
        let decoded: ClientMessage = serde_json::from_str(json).unwrap();
        match decoded {
            ClientMessage::RegisterRoute { metadata, .. } => assert!(metadata.is_none()),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_unregister_route() {
        let msg = ClientMessage::UnregisterRoute {
            domain: "myapp.tunnel.portzero.cloud".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::UnregisterRoute { domain } => {
                assert_eq!(domain, "myapp.tunnel.portzero.cloud");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_http_response() {
        let msg = ClientMessage::HttpResponse {
            request_id: 99,
            status: 200,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: b"ok".to_vec(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::HttpResponse {
                request_id,
                status,
                body,
                ..
            } => {
                assert_eq!(request_id, 99);
                assert_eq!(status, 200);
                assert_eq!(body, b"ok");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_pong() {
        let msg = ClientMessage::Pong {
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ClientMessage::Pong { timestamp } => assert_eq!(timestamp, 1234567890),
            _ => panic!("wrong variant"),
        }
    }

    // ServerMessage roundtrips

    #[test]
    fn roundtrip_welcome() {
        let msg = ServerMessage::Welcome {
            session_id: "sess_1".into(),
            account_id: "acct_1".into(),
            plan: "pro".into(),
            can_use_cloud_tunnels: true,
            username: String::new(),
            team_slugs: Vec::new(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Welcome {
                session_id,
                account_id,
                plan,
                can_use_cloud_tunnels,
                username,
                team_slugs,
            } => {
                assert_eq!(session_id, "sess_1");
                assert_eq!(account_id, "acct_1");
                assert_eq!(plan, "pro");
                assert!(can_use_cloud_tunnels);
                assert!(username.is_empty());
                assert!(team_slugs.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_route_ack() {
        let msg = ServerMessage::RouteAck {
            domain: "myapp.tunnel.portzero.cloud".into(),
            success: true,
            error: None,
            url: Some("https://myapp.tunnel.portzero.cloud".into()),
            status: Some(RouteStatus::PendingReview),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::RouteAck {
                domain,
                success,
                error,
                url,
                status,
            } => {
                assert_eq!(domain, "myapp.tunnel.portzero.cloud");
                assert!(success);
                assert!(error.is_none());
                assert_eq!(url.unwrap(), "https://myapp.tunnel.portzero.cloud");
                assert_eq!(status, Some(RouteStatus::PendingReview));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_route_status_changed() {
        let msg = ServerMessage::RouteStatusChanged {
            domain: "myapp.tunnel.portzero.cloud".into(),
            status: RouteStatus::Published,
            url: Some("https://myapp.tunnel.portzero.cloud".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::RouteStatusChanged {
                domain,
                status,
                url,
            } => {
                assert_eq!(domain, "myapp.tunnel.portzero.cloud");
                assert_eq!(status, RouteStatus::Published);
                assert_eq!(url.unwrap(), "https://myapp.tunnel.portzero.cloud");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn route_status_roundtrip() {
        for status in [
            RouteStatus::PendingReview,
            RouteStatus::Published,
            RouteStatus::Denied,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let decoded: RouteStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn route_metadata_roundtrip_and_default() {
        let meta = RouteMetadata::default();
        let json = serde_json::to_string(&meta).unwrap();
        let decoded: RouteMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, meta);
        assert!(decoded.argv.is_empty());
    }

    #[test]
    fn roundtrip_http_request() {
        let msg = ServerMessage::HttpRequest {
            request_id: 42,
            method: "GET".into(),
            path: "/api/health".into(),
            host: "api.myapp.tunnel.portzero.cloud".into(),
            headers: vec![("Accept".into(), "application/json".into())],
            body: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::HttpRequest { request_id, .. } => assert_eq!(request_id, 42),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_ping() {
        let msg = ServerMessage::Ping { timestamp: 9999999 };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Ping { timestamp } => assert_eq!(timestamp, 9999999),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_error() {
        let msg = ServerMessage::Error {
            code: ErrorCode::AuthFailed,
            message: "invalid token".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::Error { code, message } => {
                assert_eq!(code, ErrorCode::AuthFailed);
                assert_eq!(message, "invalid token");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn roundtrip_route_expired() {
        let msg = ServerMessage::RouteExpired {
            domain: "myapp.tunnel.portzero.cloud".into(),
            reason: "idle timeout".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            ServerMessage::RouteExpired { domain, reason } => {
                assert_eq!(domain, "myapp.tunnel.portzero.cloud");
                assert_eq!(reason, "idle timeout");
            }
            _ => panic!("wrong variant"),
        }
    }

    // Supporting types

    #[test]
    fn route_protocol_roundtrip() {
        for proto in [RouteProtocol::Http, RouteProtocol::Tcp] {
            let json = serde_json::to_string(&proto).unwrap();
            let decoded: RouteProtocol = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, proto);
        }
    }

    #[test]
    fn error_code_roundtrip() {
        let codes = [
            ErrorCode::AuthFailed,
            ErrorCode::RateLimited,
            ErrorCode::PlanLimitExceeded,
            ErrorCode::DomainNotAllowed,
            ErrorCode::InternalError,
        ];
        for code in codes {
            let json = serde_json::to_string(&code).unwrap();
            let decoded: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, code);
        }
    }
}
