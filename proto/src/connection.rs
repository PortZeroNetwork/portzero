//! Connection state machine for the tunnel protocol.
//!
//! Tracks the lifecycle of a tunnel connection from initial WebSocket connect
//! through authentication and into the ready state.

use crate::{ClientMessage, ServerMessage};

/// The current state of a tunnel connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// WebSocket connection established, no messages exchanged yet.
    Connecting,
    /// `Hello` has been sent, waiting for `Welcome` or `Error` from the server.
    Authenticating,
    /// Authenticated and ready to register routes and forward traffic.
    Ready,
    /// Connection has been closed or lost.
    Disconnected,
}

/// Error returned when a state transition is invalid.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("unexpected message in state {state:?}: expected to be in {expected:?}")]
    UnexpectedState {
        state: ConnectionState,
        expected: ConnectionState,
    },
    #[error("authentication failed: {message}")]
    AuthFailed { message: String },
    #[error("connection is disconnected")]
    Disconnected,
}

/// Tracks connection state and validates transitions.
#[derive(Debug)]
pub struct ConnectionStateMachine {
    state: ConnectionState,
}

impl ConnectionStateMachine {
    /// Create a new state machine in the `Connecting` state.
    pub fn new() -> Self {
        Self {
            state: ConnectionState::Connecting,
        }
    }

    /// Returns the current connection state.
    pub fn state(&self) -> &ConnectionState {
        &self.state
    }

    /// Record that a `Hello` message was sent. Transitions from `Connecting`
    /// to `Authenticating`.
    pub fn on_hello_sent(&mut self) -> Result<(), StateError> {
        match &self.state {
            ConnectionState::Connecting => {
                self.state = ConnectionState::Authenticating;
                Ok(())
            }
            ConnectionState::Disconnected => Err(StateError::Disconnected),
            other => Err(StateError::UnexpectedState {
                state: other.clone(),
                expected: ConnectionState::Connecting,
            }),
        }
    }

    /// Record that a `Welcome` was received from the server. Transitions from
    /// `Authenticating` to `Ready`.
    pub fn on_welcome_received(&mut self) -> Result<(), StateError> {
        match &self.state {
            ConnectionState::Authenticating => {
                self.state = ConnectionState::Ready;
                Ok(())
            }
            ConnectionState::Disconnected => Err(StateError::Disconnected),
            other => Err(StateError::UnexpectedState {
                state: other.clone(),
                expected: ConnectionState::Authenticating,
            }),
        }
    }

    /// Record that an authentication error was received. Transitions to
    /// `Disconnected`.
    pub fn on_auth_error(&mut self, message: String) -> StateError {
        self.state = ConnectionState::Disconnected;
        StateError::AuthFailed { message }
    }

    /// Record that the connection has been lost or closed. Transitions to
    /// `Disconnected` from any state.
    pub fn on_disconnect(&mut self) {
        self.state = ConnectionState::Disconnected;
    }

    /// Returns true if the connection is in the `Ready` state and can process
    /// route registrations and traffic.
    pub fn is_ready(&self) -> bool {
        self.state == ConnectionState::Ready
    }

    /// Validate that a client message is appropriate for the current state.
    pub fn validate_send(&self, msg: &ClientMessage) -> Result<(), StateError> {
        if self.state == ConnectionState::Disconnected {
            return Err(StateError::Disconnected);
        }

        match msg {
            ClientMessage::Hello { .. } => {
                if self.state != ConnectionState::Connecting {
                    return Err(StateError::UnexpectedState {
                        state: self.state.clone(),
                        expected: ConnectionState::Connecting,
                    });
                }
            }
            ClientMessage::RegisterRoute { .. }
            | ClientMessage::UnregisterRoute { .. }
            | ClientMessage::HttpResponse { .. } => {
                if self.state != ConnectionState::Ready {
                    return Err(StateError::UnexpectedState {
                        state: self.state.clone(),
                        expected: ConnectionState::Ready,
                    });
                }
            }
            // Pong can be sent in any non-disconnected state.
            ClientMessage::Pong { .. } => {}
        }

        Ok(())
    }

    /// Validate that a server message is appropriate for the current state.
    pub fn validate_receive(&self, msg: &ServerMessage) -> Result<(), StateError> {
        if self.state == ConnectionState::Disconnected {
            return Err(StateError::Disconnected);
        }

        match msg {
            ServerMessage::Welcome { .. } => {
                if self.state != ConnectionState::Authenticating {
                    return Err(StateError::UnexpectedState {
                        state: self.state.clone(),
                        expected: ConnectionState::Authenticating,
                    });
                }
            }
            ServerMessage::RouteAck { .. }
            | ServerMessage::HttpRequest { .. }
            | ServerMessage::RouteExpired { .. }
            | ServerMessage::RouteStatusChanged { .. } => {
                if self.state != ConnectionState::Ready {
                    return Err(StateError::UnexpectedState {
                        state: self.state.clone(),
                        expected: ConnectionState::Ready,
                    });
                }
            }
            // Ping and Error can arrive in any non-disconnected state.
            ServerMessage::Ping { .. } | ServerMessage::Error { .. } => {}
        }

        Ok(())
    }
}

impl Default for ConnectionStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, RouteProtocol, PROTOCOL_VERSION};

    #[test]
    fn happy_path_lifecycle() {
        let mut sm = ConnectionStateMachine::new();
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        sm.on_hello_sent().unwrap();
        assert_eq!(*sm.state(), ConnectionState::Authenticating);

        sm.on_welcome_received().unwrap();
        assert_eq!(*sm.state(), ConnectionState::Ready);
        assert!(sm.is_ready());

        sm.on_disconnect();
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        assert!(!sm.is_ready());
    }

    #[test]
    fn cannot_send_hello_twice() {
        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        assert!(sm.on_hello_sent().is_err());
    }

    #[test]
    fn cannot_receive_welcome_before_hello() {
        let mut sm = ConnectionStateMachine::new();
        assert!(sm.on_welcome_received().is_err());
    }

    #[test]
    fn auth_error_disconnects() {
        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        let err = sm.on_auth_error("bad token".into());
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        assert!(err.to_string().contains("bad token"));
    }

    #[test]
    fn validate_send_hello_only_in_connecting() {
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok".into(),
            client_version: "0.1".into(),
            machine_id: "m".into(),
            os: "linux".into(),
        };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_send(&hello).is_ok());

        let mut sm2 = ConnectionStateMachine::new();
        sm2.on_hello_sent().unwrap();
        assert!(sm2.validate_send(&hello).is_err());
    }

    #[test]
    fn validate_send_register_route_only_when_ready() {
        let reg = ClientMessage::RegisterRoute {
            domain: "test.portzero.cloud".into(),
            local_port: 3000,
            protocol: RouteProtocol::Http,
            metadata: None,
        };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_send(&reg).is_err());

        let mut sm2 = ConnectionStateMachine::new();
        sm2.on_hello_sent().unwrap();
        sm2.on_welcome_received().unwrap();
        assert!(sm2.validate_send(&reg).is_ok());
    }

    #[test]
    fn validate_pong_allowed_in_any_active_state() {
        let pong = ClientMessage::Pong { timestamp: 123 };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_send(&pong).is_ok());

        let mut sm2 = ConnectionStateMachine::new();
        sm2.on_hello_sent().unwrap();
        assert!(sm2.validate_send(&pong).is_ok());

        sm2.on_welcome_received().unwrap();
        assert!(sm2.validate_send(&pong).is_ok());
    }

    #[test]
    fn validate_pong_rejected_when_disconnected() {
        let pong = ClientMessage::Pong { timestamp: 123 };
        let mut sm = ConnectionStateMachine::new();
        sm.on_disconnect();
        assert!(sm.validate_send(&pong).is_err());
    }

    #[test]
    fn validate_receive_welcome_only_in_authenticating() {
        let welcome = ServerMessage::Welcome {
            session_id: "s".into(),
            account_id: "a".into(),
            plan: "free".into(),
            can_use_cloud_tunnels: true,
            username: String::new(),
            team_slugs: Vec::new(),
        };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_receive(&welcome).is_err());

        let mut sm2 = ConnectionStateMachine::new();
        sm2.on_hello_sent().unwrap();
        assert!(sm2.validate_receive(&welcome).is_ok());
    }

    #[test]
    fn validate_receive_http_request_only_when_ready() {
        let req = ServerMessage::HttpRequest {
            request_id: 1,
            method: "GET".into(),
            path: "/".into(),
            host: "test.portzero.cloud".into(),
            headers: vec![],
            body: vec![],
        };

        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        assert!(sm.validate_receive(&req).is_err());

        sm.on_welcome_received().unwrap();
        assert!(sm.validate_receive(&req).is_ok());
    }

    #[test]
    fn validate_receive_ping_allowed_in_any_active_state() {
        let ping = ServerMessage::Ping { timestamp: 456 };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_receive(&ping).is_ok());

        let mut sm2 = ConnectionStateMachine::new();
        sm2.on_hello_sent().unwrap();
        sm2.on_welcome_received().unwrap();
        assert!(sm2.validate_receive(&ping).is_ok());
    }

    #[test]
    fn validate_receive_error_allowed_in_any_active_state() {
        let err = ServerMessage::Error {
            code: ErrorCode::InternalError,
            message: "oops".into(),
        };

        let sm = ConnectionStateMachine::new();
        assert!(sm.validate_receive(&err).is_ok());
    }

    #[test]
    fn disconnected_rejects_everything() {
        let mut sm = ConnectionStateMachine::new();
        sm.on_disconnect();

        assert!(sm.on_hello_sent().is_err());
        assert!(sm.on_welcome_received().is_err());

        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "t".into(),
            client_version: "0".into(),
            machine_id: "m".into(),
            os: "linux".into(),
        };
        assert!(sm.validate_send(&hello).is_err());

        let welcome = ServerMessage::Welcome {
            session_id: "s".into(),
            account_id: "a".into(),
            plan: "f".into(),
            can_use_cloud_tunnels: true,
            username: String::new(),
            team_slugs: Vec::new(),
        };
        assert!(sm.validate_receive(&welcome).is_err());
    }

    // Dropped connection / reconnect state transitions.
    //
    // `ConnectionStateMachine` has no socket I/O of its own (the actual
    // transport lives elsewhere), so a "dropped connection" is modeled
    // here as a call to `on_disconnect()` at some point in the lifecycle,
    // and a "reconnect attempt" is modeled as constructing a fresh state
    // machine (as the real client would do when redialing) and driving it
    // through the handshake again.

    fn hello_msg() -> ClientMessage {
        ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            auth_token: "tok".into(),
            client_version: "0.1".into(),
            machine_id: "m".into(),
            os: "linux".into(),
        }
    }

    fn welcome_msg() -> ServerMessage {
        ServerMessage::Welcome {
            session_id: "s".into(),
            account_id: "a".into(),
            plan: "free".into(),
            can_use_cloud_tunnels: true,
            username: String::new(),
            team_slugs: Vec::new(),
        }
    }

    #[test]
    fn dropped_connection_before_handshake_transitions_to_disconnected() {
        // Connection closes/drops immediately after connect, before any
        // message was ever sent.
        let mut sm = ConnectionStateMachine::new();
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        sm.on_disconnect();

        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        assert!(!sm.is_ready());
        assert!(matches!(
            sm.validate_send(&hello_msg()),
            Err(StateError::Disconnected)
        ));
    }

    #[test]
    fn dropped_connection_mid_handshake_transitions_to_disconnected() {
        // Hello was sent (mid-frame from the client's perspective -- the
        // request is in flight) but the socket drops before a Welcome or
        // Error ever arrives.
        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        assert_eq!(*sm.state(), ConnectionState::Authenticating);

        sm.on_disconnect();

        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        // Any message that arrives late, or is attempted afterwards, must
        // report a clean Disconnected error rather than panicking.
        assert!(matches!(
            sm.validate_receive(&welcome_msg()),
            Err(StateError::Disconnected)
        ));
        assert!(matches!(
            sm.on_welcome_received(),
            Err(StateError::Disconnected)
        ));
    }

    #[test]
    fn dropped_connection_mid_ready_session_transitions_to_disconnected() {
        // Connection drops while fully established and mid-traffic (e.g.
        // the writer half closed while a partial frame was in flight).
        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        sm.on_welcome_received().unwrap();
        assert!(sm.is_ready());

        sm.on_disconnect();

        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        assert!(!sm.is_ready());

        let reg = ClientMessage::RegisterRoute {
            domain: "test.portzero.cloud".into(),
            local_port: 3000,
            protocol: RouteProtocol::Http,
            metadata: None,
        };
        assert!(matches!(
            sm.validate_send(&reg),
            Err(StateError::Disconnected)
        ));
    }

    #[test]
    fn disconnect_is_idempotent_and_does_not_panic() {
        let mut sm = ConnectionStateMachine::new();
        sm.on_hello_sent().unwrap();
        sm.on_disconnect();
        // Calling disconnect again (e.g. duplicate EOF + explicit close)
        // must not panic and must leave the state machine disconnected.
        sm.on_disconnect();
        sm.on_disconnect();
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
    }

    #[test]
    fn reconnect_after_drop_succeeds_on_fresh_state_machine() {
        // First connection attempt: fully authenticates, then the socket
        // is dropped.
        let mut first = ConnectionStateMachine::new();
        first.on_hello_sent().unwrap();
        first.on_welcome_received().unwrap();
        first.on_disconnect();
        assert_eq!(*first.state(), ConnectionState::Disconnected);
        assert!(!first.is_ready());
        // The old, disconnected state machine must never come back to
        // life on its own.
        assert!(matches!(
            first.on_hello_sent(),
            Err(StateError::Disconnected)
        ));

        // Reconnect attempt: a brand-new state machine (as the real
        // client constructs on redial) drives through the handshake
        // again and reaches Ready independently of the old one.
        let mut reconnected = ConnectionStateMachine::new();
        assert_eq!(*reconnected.state(), ConnectionState::Connecting);
        reconnected.on_hello_sent().unwrap();
        reconnected.on_welcome_received().unwrap();
        assert!(reconnected.is_ready());

        // The first (dropped) machine is unaffected by the second.
        assert_eq!(*first.state(), ConnectionState::Disconnected);
    }

    #[test]
    fn reconnect_loop_retries_after_auth_failures_then_succeeds() {
        // Simulate a client retry loop: two failed auth attempts (bad
        // token rejected by server), each on a fresh redial, followed by
        // a third attempt that succeeds.
        let mut attempts = Vec::new();

        for i in 0..3 {
            let mut sm = ConnectionStateMachine::new();
            sm.on_hello_sent().unwrap();
            if i < 2 {
                let err = sm.on_auth_error(format!("bad token attempt {i}"));
                assert_eq!(*sm.state(), ConnectionState::Disconnected);
                assert!(err.to_string().contains("bad token"));
            } else {
                sm.on_welcome_received().unwrap();
                assert!(sm.is_ready());
            }
            attempts.push(sm);
        }

        assert_eq!(*attempts[0].state(), ConnectionState::Disconnected);
        assert_eq!(*attempts[1].state(), ConnectionState::Disconnected);
        assert_eq!(*attempts[2].state(), ConnectionState::Ready);
    }

    #[test]
    fn state_error_display_reports_states_cleanly() {
        // Sanity check that error rendering for the reconnect/disconnect
        // paths is well-formed and doesn't panic, since callers surface
        // this text in logs/CLI output.
        let mut sm = ConnectionStateMachine::new();
        sm.on_disconnect();
        let err = sm.validate_send(&hello_msg()).unwrap_err();
        assert_eq!(err.to_string(), "connection is disconnected");

        let mut sm2 = ConnectionStateMachine::new();
        let err2 = sm2.on_welcome_received().unwrap_err();
        let msg = err2.to_string();
        assert!(msg.contains("Connecting"));
        assert!(msg.contains("Authenticating"));
    }
}
