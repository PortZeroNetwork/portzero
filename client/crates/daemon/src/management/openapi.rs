//! Generates the OpenAPI document for the management API from the axum handlers
//! and their request/response types.
//!
//! `api/management-v1.yaml` at the repo root is generated from this module via
//! `just openapi` (see `src/bin/generate_openapi.rs`), and CI fails the build if
//! the checked-in file has drifted from the handlers.

use utoipa::openapi::extensions::Extensions;
use utoipa::openapi::path::Operation;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::{Modify, OpenApi};

use crate::management::handlers;
use crate::management::server::PortRegistration;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "portzero Management API",
        version = "1.0.0",
        description = "HTTP management API for the portzero overlay network daemon, reachable at \
            `http://portzero.local` through the virtual NIC.\n\n\
            ## Identity mechanism\n\n\
            This API uses **no authentication tokens**. The daemon identifies the \
            calling process by inspecting the TCP source port of each inbound \
            connection and resolving it to a PID via OS-level socket tables:\n\n\
            - **Linux** — `/proc/net/tcp` and `/proc/net/tcp6`\n\
            - **macOS** — `sysctl` / `libproc`\n\
            - **Windows** — `GetExtendedTcpTable`\n\n\
            Clients must never include a `pid` field in request bodies; the daemon \
            derives it from the connection. If the daemon cannot resolve the source \
            port to a PID it returns `500 caller_unidentifiable`.\n\n\
            ## Port verification\n\n\
            When a process registers port→domain mappings, the daemon independently \
            verifies that the resolved PID is actually listening on each claimed \
            `local_port`. Claims that fail verification are rejected with \
            `422 port_not_listening`.\n\n\
            ## Auto-deregistration\n\n\
            When the daemon detects that a registered PID has exited, it automatically \
            removes all mappings for that PID. Processes may also call \
            `DELETE /v1/register` to deregister explicitly (e.g., on graceful shutdown)."
    ),
    servers(
        (url = "http://portzero.local", description = "portzero overlay network — reachable via the virtual NIC")
    ),
    paths(
        handlers::register,
        handlers::deregister,
        handlers::status,
        handlers::daemon_status
    ),
    components(schemas(
        PortRegistration,
        handlers::ApiRegistrationsStatus,
        handlers::DaemonStatusResponse,
        handlers::HttpsStatus,
        handlers::RegisterRequest,
        handlers::RegisterResponse,
        handlers::DeregisterResponse,
        handlers::StatusResponse,
        handlers::ErrorResponse,
    )),
    tags(
        (name = "management", description = "Port/domain registration for local processes")
    ),
    modifiers(&ManagementApiModifier)
)]
pub struct ManagementApiDoc;

struct ManagementApiModifier;

impl Modify for ManagementApiModifier {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.extensions = Some(Extensions::from_iter([(
            "x-portzero-sdk-note",
            serde_json::json!(
                "SDK generation should use `http://portzero.local` as the base URL. \
                No auth headers are needed — the daemon identifies the caller by the \
                TCP source port of the connection. Do not inject Authorization or \
                X-Api-Key headers."
            ),
        )]));

        let components = openapi
            .components
            .as_mut()
            .expect("paths declare schemas, so components is always populated");
        components.add_security_scheme(
            "TcpSourcePortIdentity",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-Portzero-Tcp-Identity",
                "**Not a real header.** portzero does not use tokens or headers for \
                authentication.\n\n\
                Identity is established by the daemon resolving the TCP source port of \
                the connection to a PID using OS-level socket tables \
                (`/proc/net/tcp` on Linux, `libproc` on macOS, \
                `GetExtendedTcpTable` on Windows). Clients must send no auth header.\n\n\
                This entry exists only so that OpenAPI tooling can document the \
                security model. SDK generators should **not** inject this header.",
            ))),
        );

        openapi.security = Some(vec![SecurityRequirement::new(
            "TcpSourcePortIdentity",
            Vec::<String>::new(),
        )]);

        if let Some(path) = openapi.paths.paths.get_mut("/v1/register") {
            if let Some(operation) = path.post.as_mut() {
                mark_unstable_registration(operation);
            }
            if let Some(operation) = path.delete.as_mut() {
                mark_unstable_registration(operation);
            }
        }
        if let Some(path) = openapi.paths.paths.get_mut("/v1/status") {
            if let Some(operation) = path.get.as_mut() {
                mark_unstable_registration(operation);
            }
        }
    }
}

fn mark_unstable_registration(operation: &mut Operation) {
    operation.extensions = Some(Extensions::from_iter([(
        "x-portzero-feature-stability",
        serde_json::json!("unstable"),
    )]));
}
