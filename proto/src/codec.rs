//! Codec for encoding and decoding tunnel protocol messages over WebSocket
//! text frames.
//!
//! Each WebSocket text frame contains exactly one JSON-encoded message.

use crate::{ClientMessage, ServerMessage};

/// Error type for codec operations.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("failed to serialize message: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("failed to deserialize message: {source}\n\nReceived text: {text}")]
    Deserialize {
        source: serde_json::Error,
        text: String,
    },
}

pub type Result<T> = std::result::Result<T, CodecError>;

/// Encode a [`ClientMessage`] into a JSON string suitable for a WebSocket text
/// frame.
pub fn encode(msg: &ClientMessage) -> Result<String> {
    Ok(serde_json::to_string(msg)?)
}

/// Decode a WebSocket text frame into a [`ServerMessage`].
pub fn decode_server(text: &str) -> Result<ServerMessage> {
    serde_json::from_str(text).map_err(|e| CodecError::Deserialize {
        source: e,
        text: text.to_owned(),
    })
}

/// Encode a [`ServerMessage`] into a JSON string suitable for a WebSocket text
/// frame.
pub fn encode_server(msg: &ServerMessage) -> Result<String> {
    Ok(serde_json::to_string(msg)?)
}

/// Decode a WebSocket text frame into a [`ClientMessage`].
pub fn decode_client(text: &str) -> Result<ClientMessage> {
    serde_json::from_str(text).map_err(|e| CodecError::Deserialize {
        source: e,
        text: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, RouteProtocol, PROTOCOL_VERSION};

    #[test]
    fn encode_decode_client_hello() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok_test".into(),
            client_version: "0.1.0".into(),
            machine_id: "m-test".into(),
            os: "linux".into(),
        };
        let text = encode(&msg).unwrap();
        let decoded = decode_client(&text).unwrap();
        match decoded {
            ClientMessage::Hello { auth_token, .. } => assert_eq!(auth_token, "tok_test"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_server_welcome() {
        let msg = ServerMessage::Welcome {
            session_id: "sess_1".into(),
            account_id: "acct_1".into(),
            plan: "free".into(),
            can_use_cloud_tunnels: true,
        };
        let text = encode_server(&msg).unwrap();
        let decoded = decode_server(&text).unwrap();
        match decoded {
            ServerMessage::Welcome { session_id, .. } => assert_eq!(session_id, "sess_1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_register_route() {
        let msg = ClientMessage::RegisterRoute {
            domain: "test.portzero.cloud".into(),
            local_port: 8080,
            protocol: RouteProtocol::Tcp,
            metadata: None,
        };
        let text = encode(&msg).unwrap();
        let decoded = decode_client(&text).unwrap();
        match decoded {
            ClientMessage::RegisterRoute { protocol, .. } => {
                assert_eq!(protocol, RouteProtocol::Tcp);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn encode_decode_error() {
        let msg = ServerMessage::Error {
            code: ErrorCode::RateLimited,
            message: "slow down".into(),
        };
        let text = encode_server(&msg).unwrap();
        let decoded = decode_server(&text).unwrap();
        match decoded {
            ServerMessage::Error { code, .. } => assert_eq!(code, ErrorCode::RateLimited),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_invalid_json_gives_helpful_error() {
        let result = decode_server("not json at all");
        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("not json at all"),
            "error should include the received text"
        );
    }

    #[test]
    fn decode_wrong_type_tag() {
        let result = decode_server(r#"{"type":"Bogus"}"#);
        assert!(result.is_err());
    }

    // Fragmented / partial reads.
    //
    // The codec itself has no length-prefixed byte framing -- each
    // WebSocket text frame already contains exactly one JSON message, so
    // "fragmentation" at this layer means a caller handing decode_* a
    // string that is a truncated prefix of a full frame (e.g. because the
    // transport delivered the frame in pieces and an over-eager caller
    // tried to decode before the frame was fully reassembled). These
    // tests assert that partial prefixes fail cleanly (no panic) and that
    // the full, reassembled text decodes correctly afterwards.

    #[test]
    fn decode_truncated_mid_header_errors_then_full_text_succeeds() {
        let msg = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok_test".into(),
            client_version: "0.1.0".into(),
            machine_id: "m-test".into(),
            os: "linux".into(),
        };
        let full = encode(&msg).unwrap();

        // Truncated right after the opening brace / "type" key -- stops
        // mid "header" (the discriminant field).
        let cut = full.find("\"type\"").unwrap() + 3;
        let prefix = &full[..cut];
        assert!(
            decode_client(prefix).is_err(),
            "truncated prefix should not decode"
        );

        // Now the fully reassembled frame decodes fine.
        let decoded = decode_client(&full).unwrap();
        match decoded {
            ClientMessage::Hello { auth_token, .. } => assert_eq!(auth_token, "tok_test"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_truncated_mid_payload_errors_then_full_text_succeeds() {
        let msg = ServerMessage::Welcome {
            session_id: "sess_1".into(),
            account_id: "acct_1".into(),
            plan: "free".into(),
            can_use_cloud_tunnels: true,
        };
        let full = encode_server(&msg).unwrap();

        // Truncated partway through the payload (the account_id value),
        // well past the type tag / header.
        let cut = full.find("acct_1").unwrap() + 2;
        let prefix = &full[..cut];
        assert!(
            decode_server(prefix).is_err(),
            "payload cut mid-value should not decode"
        );

        let decoded = decode_server(&full).unwrap();
        match decoded {
            ServerMessage::Welcome { session_id, .. } => assert_eq!(session_id, "sess_1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_missing_closing_brace_errors_then_full_text_succeeds() {
        let msg = ClientMessage::Pong { timestamp: 42 };
        let full = encode(&msg).unwrap();

        // Drop the trailing closing brace -- stops right before the frame
        // is complete.
        let prefix = &full[..full.len() - 1];
        assert!(
            decode_client(prefix).is_err(),
            "frame missing its closing brace should not decode"
        );

        let decoded = decode_client(&full).unwrap();
        match decoded {
            ClientMessage::Pong { timestamp } => assert_eq!(timestamp, 42),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_accumulated_one_byte_at_a_time() {
        // Simulate worst-case fragmentation: a transport that only ever
        // hands the caller one extra byte at a time. Every prefix shorter
        // than the full frame must fail to decode (never panic); only the
        // fully accumulated buffer succeeds.
        let msg = ServerMessage::RouteExpired {
            domain: "test.portzero.cloud".into(),
            reason: "ttl elapsed".into(),
        };
        let full = encode_server(&msg).unwrap();
        let bytes = full.as_bytes();

        let mut acc = String::new();
        for (i, chunk) in bytes.iter().enumerate() {
            acc.push(*chunk as char);
            let is_last = i == bytes.len() - 1;
            let result = decode_server(&acc);
            if is_last {
                assert!(result.is_ok(), "full buffer should decode: {result:?}");
            } else {
                assert!(
                    result.is_err(),
                    "partial buffer of {} bytes should not decode",
                    acc.len()
                );
            }
        }

        match decode_server(&acc).unwrap() {
            ServerMessage::RouteExpired { domain, reason } => {
                assert_eq!(domain, "test.portzero.cloud");
                assert_eq!(reason, "ttl elapsed");
            }
            _ => panic!("wrong variant"),
        }
    }

    // Malformed / oversized frames.

    #[test]
    fn decode_empty_string_errors_cleanly() {
        let result = decode_server("");
        assert!(result.is_err());
    }

    #[test]
    fn decode_empty_object_errors_cleanly() {
        // Valid JSON, but missing the required "type" discriminant.
        let result = decode_server("{}");
        assert!(result.is_err());
    }

    #[test]
    fn decode_json_null_errors_cleanly() {
        let result = decode_server("null");
        assert!(result.is_err());
    }

    #[test]
    fn decode_garbage_bytes_errors_cleanly() {
        // Control characters / binary-looking garbage embedded in the
        // text: should be rejected, not panic.
        let garbage = "\u{0}\u{1}\u{2}not-json\u{7f}";
        let result = decode_client(garbage);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("not-json"));
    }

    #[test]
    fn decode_field_value_out_of_range_errors_cleanly() {
        // `local_port` is a u16; a value that overflows it is effectively
        // a frame claiming an out-of-bounds size for that field. The
        // decoder must return a clean error rather than panicking or
        // wrapping/truncating.
        let text = r#"{"type":"RegisterRoute","domain":"test.portzero.cloud","local_port":99999999,"protocol":"Tcp"}"#;
        let result = decode_client(text);
        assert!(result.is_err());
    }

    #[test]
    fn decode_huge_string_payload_does_not_panic() {
        // A frame with an extremely large (but well-formed) payload should
        // still decode successfully -- no arbitrary max-size enforcement
        // exists at this layer, so this should simply succeed without
        // panicking or hanging.
        let huge_reason = "x".repeat(5_000_000);
        let msg = ServerMessage::RouteExpired {
            domain: "test.portzero.cloud".into(),
            reason: huge_reason.clone(),
        };
        let text = encode_server(&msg).unwrap();
        let decoded = decode_server(&text).unwrap();
        match decoded {
            ServerMessage::RouteExpired { reason, .. } => {
                assert_eq!(reason.len(), huge_reason.len())
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn decode_deeply_malformed_json_errors_cleanly() {
        let malformed = r#"{"type":"Welcome","session_id": [[[[[[["#;
        let result = decode_server(malformed);
        assert!(result.is_err());
    }
}
