//! `portzero mcp`: a Model Context Protocol server (JSON-RPC 2.0 over stdio)
//! that exposes the daemon's observed runtime truth to AI coding agents.
//!
//! It speaks newline-delimited JSON-RPC on stdin/stdout — the MCP stdio
//! transport. An agent launches `portzero mcp` as a subprocess and calls the
//! tools below to learn what is actually running:
//!
//! - `overview`          — everything at once (services, tunnels, edges, routes)
//! - `list_services`     — discovered processes/containers, their ports, images
//! - `list_tunnels`      — tunnel domains (local + cloud), URLs, health paths
//! - `observed_edges`    — who-talks-to-whom dependency edges
//! - `exercised_routes`  — HTTP routes actually hit per tunnel (smoke-test list)
//! - `list_feedback`     — reviewer feedback threads from portzero.cloud
//! - `propose_fix`       — mark a feedback thread as fixed by a commit
//! - `submit_bug_report`      — report a bug in Port Zero to the team
//! - `submit_feature_request` — request a Port Zero feature (see `mcp_feedback`)
//!
//! Local data is read fresh from the daemon's state files on each call, so it
//! reflects current truth; the cloud-backed tools (feedback, bug/feature
//! reports) call the portzero.cloud API and require `portzero login`. The two
//! `submit_*` tools live in `mcp_feedback` to keep this file within the client
//! file-size budget. See `portzero inspect` for the human-readable view.

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use portzero_daemon::discovery::ServiceSource;
use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};
use portzero_daemon::observations::Observations;
use portzero_daemon::route_table::{OverlayState, RouteTable};

use crate::api_client::ApiClient;
use crate::export::tunnel_url;

/// MCP protocol revision we implement (a widely-supported stable revision).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Run the MCP stdio server until stdin closes.
pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                // Parse error — id unknown, per JSON-RPC use null.
                write_message(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                )?;
                continue;
            }
        };

        // Notifications have no `id` and get no response.
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");

        if id.is_none() {
            // e.g. notifications/initialized — nothing to reply.
            continue;
        }
        let id = id.unwrap();

        let response = match handle_method(method, request.get("params")) {
            Ok(result) => success_response(id, result),
            Err(err) => error_response(id, -32601, &err),
        };
        write_message(&mut stdout, &response)?;
    }

    Ok(())
}

/// Dispatch a JSON-RPC method to its handler. `Err(message)` becomes a
/// JSON-RPC error response.
fn handle_method(method: &str, params: Option<&Value>) -> Result<Value, String> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "portzero", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Runtime truth from the portzero daemon: discovered \
                services/containers, tunnel domains, health paths, observed \
                dependency edges, and exercised HTTP routes. Only traffic addressed \
                via tunnel names is observed."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => {
            let params = params.ok_or_else(|| "missing params".to_string())?;
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing tool name".to_string())?;
            call_tool(name, params.get("arguments"))
        }
        other => Err(format!("method not found: {other}")),
    }
}

/// The advertised tool catalog. The daemon-state tools take no arguments;
/// `list_feedback` and `propose_fix` take typed arguments and call the
/// portzero.cloud API.
fn tool_list() -> Value {
    let no_args = json!({ "type": "object", "properties": {} });
    let mut tools = json!([
        {
            "name": "overview",
            "description": "Everything the daemon knows at once: discovered services, \
                tunnel domains + health paths, observed dependency edges, and exercised routes.",
            "inputSchema": no_args,
        },
        {
            "name": "list_services",
            "description": "Discovered processes and Docker containers with PZ_TUNNEL: \
                their tunnel domain, listening/forwarded port, source (process pid or \
                container image/name), and health path.",
            "inputSchema": no_args,
        },
        {
            "name": "list_tunnels",
            "description": "Tunnel domains (local overlay and cloud), their resolved URLs, \
                health paths, and cloud review status.",
            "inputSchema": no_args,
        },
        {
            "name": "observed_edges",
            "description": "Observed who-talks-to-whom dependency edges between tunnels \
                (protocol, request count, last seen).",
            "inputSchema": no_args,
        },
        {
            "name": "exercised_routes",
            "description": "HTTP routes actually exercised per tunnel (method, path, count, \
                and X-PZ-Test attributions) — a ready-made smoke-test inventory.",
            "inputSchema": no_args,
        },
        {
            "name": "list_feedback",
            "description": "List feedback threads from portzero.cloud. These are reviewer \
                comments pinned on your tunneled app; each thread has a ref like PZ-7. \
                To mark one fixed, either call propose_fix, or include \"Fixes PZ-<n>\" in \
                the commit message of the fixing commit and upload a review record with \
                `portzero review` — the cloud advances the thread automatically. \
                Requires `portzero login`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["open", "fix_proposed", "resolved"],
                        "description": "Filter threads by status (default: open).",
                    },
                },
            },
        },
        {
            "name": "propose_fix",
            "description": "Mark a feedback thread as fixed by a specific commit \
                (status becomes fix_proposed). The thread then awaits human confirmation: \
                the commenter or a team member resolves it. Alternative: include \
                \"Fixes PZ-<n>\" in the fixing commit's message and upload a review record \
                with `portzero review`, which advances the thread the same way. \
                Requires `portzero login`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "thread_id": {
                        "type": "string",
                        "description": "Thread id (the `id` field from list_feedback, \
                            not the PZ-<n> ref).",
                    },
                    "fix_commit": {
                        "type": "string",
                        "description": "Git commit SHA containing the fix.",
                    },
                    "fix_summary": {
                        "type": "string",
                        "description": "One-line description of the fix.",
                    },
                    "agent": {
                        "type": "string",
                        "description": "Optional: the coding agent that produced the fix \
                            (e.g. claude-code). Recorded as cost metadata only.",
                    },
                    "model": {
                        "type": "string",
                        "description": "Optional: the model used (e.g. claude-opus-4-8).",
                    },
                    "tokens_in": {
                        "type": "integer",
                        "description": "Optional: input tokens spent producing the fix.",
                    },
                    "tokens_out": {
                        "type": "integer",
                        "description": "Optional: output tokens spent producing the fix.",
                    },
                    "cost_usd": {
                        "type": "number",
                        "description": "Optional: measured USD cost attributed to the fix.",
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Optional: the agent session id this fix belongs to.",
                    },
                },
                "required": ["thread_id", "fix_commit", "fix_summary"],
            },
        },
    ]);
    // The bug-report / feature-request tools live in `mcp_feedback` (keeps this
    // file within the client file-size budget).
    if let Value::Array(arr) = &mut tools {
        arr.extend(crate::mcp_feedback::report_tools());
        arr.extend(crate::mcp_cost::cost_tools());
    }
    tools
}

/// Execute a tool by name, returning a `tools/call` result envelope.
/// `arguments` is the raw `params.arguments` object from the client, if any;
/// the daemon-state tools ignore it.
fn call_tool(name: &str, arguments: Option<&Value>) -> Result<Value, String> {
    // Cloud-backed tools handle their own result envelope (they report
    // argument/auth/HTTP failures as isError tool results, not JSON-RPC errors).
    match name {
        "list_feedback" => return Ok(list_feedback(arguments)),
        "propose_fix" => return Ok(propose_fix(arguments)),
        "submit_bug_report" => return Ok(crate::mcp_feedback::submit_report("bug", arguments)),
        "submit_feature_request" => {
            return Ok(crate::mcp_feedback::submit_report("feature", arguments))
        }
        _ => {}
    }

    if let Some(result) = crate::mcp_cost::call_cost_tool(name, arguments) {
        return Ok(result);
    }

    let config = DaemonConfig::load();
    let value = match name {
        "overview" => overview(&config),
        "list_services" => json!({ "services": services(&config) }),
        "list_tunnels" => json!({ "tunnels": tunnels(&config) }),
        "observed_edges" => json!({ "edges": edges(&config) }),
        "exercised_routes" => json!({ "routes": routes(&config) }),
        other => return Err(format!("unknown tool: {other}")),
    };

    Ok(tool_ok(&value))
}

/// Wrap a JSON value as a successful `tools/call` result (text content block).
pub(crate) fn tool_ok(value: &Value) -> Value {
    // MCP tools return content blocks; we return the JSON as a text block.
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false,
    })
}

/// Wrap a message as a failed `tools/call` result (`isError: true`).
pub(crate) fn tool_error(message: &str) -> Value {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true,
    })
}

/// Message returned when the cloud tools are used without `portzero login`.
pub(crate) const NOT_LOGGED_IN: &str = "Not logged in. Run `portzero login` to authenticate.";

/// Run a future to completion on a dedicated thread with its own runtime.
///
/// The MCP server loop is synchronous, but it may itself be running inside
/// the CLI's tokio runtime — blocking that runtime's worker (or nesting a
/// second `block_on`) panics. A scoped thread with a fresh current-thread
/// runtime works from any calling context.
pub(crate) fn run_async<T: Send>(fut: impl std::future::Future<Output = T> + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build tokio runtime for MCP cloud call")
                    .block_on(fut)
            })
            .join()
            .expect("MCP cloud call thread panicked")
    })
}

/// `list_feedback` — GET /feedback/comments?status=… from portzero.cloud.
fn list_feedback(arguments: Option<&Value>) -> Value {
    let status = arguments
        .and_then(|a| a.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("open");
    if !["open", "fix_proposed", "resolved"].contains(&status) {
        return tool_error(&format!(
            "invalid status '{status}': expected one of open, fix_proposed, resolved"
        ));
    }

    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return tool_error(NOT_LOGGED_IN);
    }

    let path = format!("/feedback/comments?status={status}");
    let result = run_async(async move {
        let resp = client.get(&path).await?;
        let http_status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((http_status, text))
    });

    match result {
        Ok((http_status, text)) if http_status.is_success() => {
            match serde_json::from_str::<Value>(&text) {
                Ok(body) => tool_ok(&shape_feedback_threads(&body)),
                Err(e) => tool_error(&format!("failed to parse cloud response: {e}")),
            }
        }
        Ok((http_status, text)) if http_status.as_u16() == 401 => tool_error(&format!(
            "Authentication expired or invalid (HTTP 401). Run `portzero logout` then \
             `portzero login` to re-authenticate.\n\n{text}"
        )),
        Ok((http_status, text)) => tool_error(&format!(
            "failed to list feedback threads (HTTP {http_status}): {text}"
        )),
        Err(e) => tool_error(&format!("{e:#}")),
    }
}

/// `propose_fix` — POST /feedback/comments/{thread_id}/propose-fix.
fn propose_fix(arguments: Option<&Value>) -> Value {
    let thread_id = match required_string_arg(arguments, "thread_id") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let fix_commit = match required_string_arg(arguments, "fix_commit") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let fix_summary = match required_string_arg(arguments, "fix_summary") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };

    // The thread id is interpolated into a URL path — reject anything that
    // could escape the /feedback/comments/{id}/ segment.
    if thread_id
        .chars()
        .any(|c| c == '/' || c == '?' || c == '#' || c.is_whitespace())
    {
        return tool_error(&format!(
            "invalid thread_id '{thread_id}': use the `id` field from list_feedback"
        ));
    }

    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return tool_error(NOT_LOGGED_IN);
    }

    let path = format!("/feedback/comments/{thread_id}/propose-fix");
    let mut body = json!({ "fix_commit": fix_commit, "fix_summary": fix_summary });
    attach_optional_cost_args(&mut body, arguments);
    let result = run_async(async move {
        let resp = client.post(&path, &body).await?;
        let http_status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((http_status, text))
    });

    match result {
        Ok((http_status, text)) if http_status.is_success() => {
            let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
            tool_ok(&json!({
                "proposed": true,
                "thread_id": thread_id,
                "note": "The thread now awaits human confirmation: the commenter or a \
                         team member resolves it.",
                "thread": body,
            }))
        }
        Ok((http_status, text)) => tool_error(&format!(
            "failed to propose fix for thread {thread_id} (HTTP {http_status}): {text}"
        )),
        Err(e) => tool_error(&format!("{e:#}")),
    }
}

/// Copy the optional cost-metadata arguments (`agent`, `model`, `tokens_in`,
/// `tokens_out`, `cost_usd`, `session_id`) onto a propose-fix POST body, only
/// when the caller supplied them. Metadata only — never message content.
fn attach_optional_cost_args(body: &mut Value, arguments: Option<&Value>) {
    let Some(args) = arguments else { return };
    let Some(map) = body.as_object_mut() else {
        return;
    };
    for key in [
        "agent",
        "model",
        "tokens_in",
        "tokens_out",
        "cost_usd",
        "session_id",
    ] {
        if let Some(v) = args.get(key) {
            if !v.is_null() {
                map.insert(key.to_string(), v.clone());
            }
        }
    }
}

/// Extract a required non-empty string argument, with a descriptive error.
pub(crate) fn required_string_arg(arguments: Option<&Value>, key: &str) -> Result<String, String> {
    arguments
        .and_then(|a| a.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("missing required argument `{key}` (a non-empty string)"))
}

/// Reduce the cloud's thread list to the fields an agent needs: ref, id,
/// status, domain, route, guest_name, created_at, comment bodies, and the
/// fix_commit/fix_summary provenance when present.
fn shape_feedback_threads(body: &Value) -> Value {
    let threads = body
        .as_array()
        .or_else(|| body.get("threads").and_then(Value::as_array))
        .or_else(|| body.get("comments").and_then(Value::as_array));

    match threads {
        Some(list) => json!({
            "threads": list.iter().map(shape_feedback_thread).collect::<Vec<Value>>(),
        }),
        // Unknown envelope — pass the body through rather than dropping data.
        None => body.clone(),
    }
}

fn shape_feedback_thread(thread: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "ref",
        "id",
        "status",
        "domain",
        "route",
        "guest_name",
        "created_at",
    ] {
        if let Some(v) = thread.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    for key in ["fix_commit", "fix_summary"] {
        if let Some(v) = thread.get(key) {
            if !v.is_null() {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    if let Some(comments) = thread.get("comments").and_then(Value::as_array) {
        let bodies: Vec<Value> = comments
            .iter()
            .map(|c| c.get("body").cloned().unwrap_or(Value::Null))
            .collect();
        out.insert("comments".to_string(), Value::Array(bodies));
    } else if let Some(body) = thread.get("body") {
        out.insert("body".to_string(), body.clone());
    }
    Value::Object(out)
}

fn overview(config: &DaemonConfig) -> Value {
    json!({
        "daemon_running": read_daemon_pid(config).is_some(),
        "services": services(config),
        "tunnels": tunnels(config),
        "observed_edges": edges(config),
        "exercised_routes": routes(config),
        "observability_caveat":
            "Only traffic addressed via tunnel names is observed. Container-to-container \
             traffic over compose-internal DNS bypasses the daemon and is not recorded.",
    })
}

fn source_json(source: &ServiceSource, pid: u32) -> Value {
    match source {
        ServiceSource::Process { cwd } => json!({
            "type": "process",
            "pid": pid,
            "cwd": cwd.as_ref().map(|c| c.display().to_string()),
        }),
        ServiceSource::Container { id, name } => json!({
            "type": "container",
            "id": id,
            "name": name,
        }),
    }
}

fn services(config: &DaemonConfig) -> Vec<Value> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();

    let mut out = Vec::new();
    for r in table.routes.values() {
        out.push(json!({
            "domain": r.domain,
            "kind": "cloud",
            "port": r.port,
            "health_path": r.health_path,
            "source": source_json(&r.source, r.pid),
        }));
    }
    for r in &overlay.routes {
        out.push(json!({
            "domain": r.domain,
            "kind": "local",
            "port": r.service_port,
            "real_addr": r.real_addr,
            "health_path": r.health_path,
            "source": source_json(&r.source, r.pid),
        }));
    }
    out.sort_by(|a, b| domain_of(a).cmp(domain_of(b)));
    out
}

fn tunnels(config: &DaemonConfig) -> Vec<Value> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
    let statuses = read_cloud_route_statuses(config);

    let mut out = Vec::new();
    for r in table.routes.values() {
        out.push(json!({
            "domain": r.domain,
            "kind": "cloud",
            "url": tunnel_url(&r.domain, r.port),
            "health_path": r.health_path,
            "status": statuses.get(&r.domain).cloned().unwrap_or_else(|| "published".to_string()),
        }));
    }
    for r in &overlay.routes {
        out.push(json!({
            "domain": r.domain,
            "kind": "local",
            "url": tunnel_url(&r.domain, r.service_port),
            "health_path": r.health_path,
            "status": "local",
        }));
    }
    out.sort_by(|a, b| domain_of(a).cmp(domain_of(b)));
    out.dedup_by(|a, b| domain_of(a).eq_ignore_ascii_case(domain_of(b)));
    out
}

fn edges(config: &DaemonConfig) -> Vec<Value> {
    Observations::load(&config.observations_path())
        .edges
        .into_iter()
        .map(|e| {
            json!({
                "from": e.from,
                "to": e.to,
                "protocol": e.protocol,
                "request_count": e.request_count,
                "last_seen": e.last_seen,
            })
        })
        .collect()
}

fn routes(config: &DaemonConfig) -> Vec<Value> {
    Observations::load(&config.observations_path())
        .routes
        .into_iter()
        .map(|r| {
            json!({
                "domain": r.domain,
                "method": r.method,
                "path": r.path,
                "count": r.count,
                "tests": r.tests,
                "last_seen": r.last_seen,
            })
        })
        .collect()
}

fn domain_of(v: &Value) -> &str {
    v.get("domain").and_then(Value::as_str).unwrap_or("")
}

fn read_cloud_route_statuses(config: &DaemonConfig) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(config.cloud_route_status_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_message(out: &mut impl Write, msg: &Value) -> Result<()> {
    // Newline-delimited JSON: one compact object per line.
    let line = serde_json::to_string(msg)?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initialize_reports_tools_capability() {
        let result = handle_method("initialize", None).unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "portzero");
    }

    #[test]
    fn test_tools_list_advertises_expected_tools() {
        let result = handle_method("tools/list", None).unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "overview",
            "list_services",
            "list_tunnels",
            "observed_edges",
            "exercised_routes",
            "list_feedback",
            "propose_fix",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    /// Find a tool object in the advertised catalog by name.
    fn tool_named(name: &str) -> Value {
        let result = handle_method("tools/list", None).unwrap();
        result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} not advertised"))
            .clone()
    }

    #[test]
    fn test_list_feedback_schema_has_typed_status_enum() {
        let tool = tool_named("list_feedback");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["status"]["type"], "string");
        let enum_values: Vec<&str> = schema["properties"]["status"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(enum_values, vec!["open", "fix_proposed", "resolved"]);
        // status is optional (defaults to "open").
        assert!(schema.get("required").is_none());
        // The description must teach the Fixes PZ-<n> commit convention.
        let description = tool["description"].as_str().unwrap();
        assert!(description.contains("Fixes PZ-<n>"), "got: {description}");
        assert!(
            description.contains("portzero review"),
            "got: {description}"
        );
    }

    #[test]
    fn test_propose_fix_schema_requires_all_three_args() {
        let tool = tool_named("propose_fix");
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        for property in ["thread_id", "fix_commit", "fix_summary"] {
            assert_eq!(
                schema["properties"][property]["type"], "string",
                "property {property} must be a typed string"
            );
        }
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["thread_id", "fix_commit", "fix_summary"]);
        // The description must state the human-confirmation flow and the
        // commit-message alternative.
        let description = tool["description"].as_str().unwrap();
        assert!(
            description.contains("human confirmation"),
            "got: {description}"
        );
        assert!(description.contains("Fixes PZ-<n>"), "got: {description}");
    }

    #[test]
    fn test_tools_list_advertises_cost_tools() {
        let result = handle_method("tools/list", None).unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in ["cost_of_commit", "cost_of_pr", "cost_of_lines"] {
            assert!(names.contains(&expected), "missing cost tool {expected}");
        }
    }

    #[test]
    fn test_propose_fix_schema_has_optional_cost_args_not_required() {
        let tool = tool_named("propose_fix");
        let props = &tool["inputSchema"]["properties"];
        // The optional cost-metadata args are advertised as typed properties.
        assert_eq!(props["agent"]["type"], "string");
        assert_eq!(props["model"]["type"], "string");
        assert_eq!(props["tokens_in"]["type"], "integer");
        assert_eq!(props["tokens_out"]["type"], "integer");
        assert_eq!(props["cost_usd"]["type"], "number");
        assert_eq!(props["session_id"]["type"], "string");
        // But `required` is still only the original three.
        let required: Vec<&str> = tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["thread_id", "fix_commit", "fix_summary"]);
    }

    #[test]
    fn test_attach_optional_cost_args_copies_only_present_values() {
        let mut body = json!({ "fix_commit": "abc", "fix_summary": "s" });
        let args = json!({
            "agent": "claude-code",
            "cost_usd": 0.42,
            "tokens_in": 100,
            "session_id": null,
        });
        attach_optional_cost_args(&mut body, Some(&args));
        assert_eq!(body["agent"], "claude-code");
        assert_eq!(body["cost_usd"], 0.42);
        assert_eq!(body["tokens_in"], 100);
        // Null values and absent keys are not copied.
        assert!(body.get("session_id").is_none());
        assert!(body.get("model").is_none());
    }

    #[test]
    fn test_propose_fix_missing_args_is_tool_error() {
        let params = json!({ "name": "propose_fix", "arguments": {} });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("thread_id"), "got: {text}");
    }

    #[test]
    fn test_propose_fix_missing_one_arg_names_it() {
        let params = json!({
            "name": "propose_fix",
            "arguments": { "thread_id": "t-1", "fix_commit": "abc123" },
        });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("fix_summary"), "got: {text}");
    }

    #[test]
    fn test_propose_fix_no_arguments_key_is_tool_error() {
        // A client may omit `arguments` entirely; that must not be a JSON-RPC
        // error, just an isError tool result.
        let params = json!({ "name": "propose_fix" });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn test_propose_fix_rejects_path_escaping_thread_id() {
        let params = json!({
            "name": "propose_fix",
            "arguments": {
                "thread_id": "../other",
                "fix_commit": "abc",
                "fix_summary": "s",
            },
        });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("invalid thread_id"), "got: {text}");
    }

    #[test]
    fn test_list_feedback_rejects_invalid_status() {
        let params = json!({ "name": "list_feedback", "arguments": { "status": "bogus" } });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("invalid status"), "got: {text}");
    }

    #[test]
    fn test_list_feedback_requires_login() {
        // Point the home dir at an empty temp dir so no ~/.portzero/auth.json
        // exists; the tool must fail with the not-logged-in message before any
        // HTTP.
        let result = crate::with_temp_home("mcp", |_| {
            let params = json!({ "name": "list_feedback", "arguments": {} });
            handle_method("tools/call", Some(&params)).unwrap()
        });

        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Not logged in"), "got: {text}");
    }

    #[test]
    fn test_shape_feedback_thread_extracts_agent_fields() {
        let thread = json!({
            "ref": "PZ-42",
            "ref_num": 42,
            "id": "t-1",
            "status": "fix_proposed",
            "domain": "web.alice.tunnel.portzero.cloud",
            "route": "/checkout",
            "guest_name": "Reviewer",
            "created_at": "2026-07-11 00:00:00",
            "fix_commit": "abc123",
            "fix_summary": "fix the button",
            "resolved_at": null,
            "comments": [
                { "id": "c-1", "body": "button is broken" },
                { "id": "c-2", "body": "still broken on mobile" },
            ],
        });
        let shaped = shape_feedback_thread(&thread);
        assert_eq!(shaped["ref"], "PZ-42");
        assert_eq!(shaped["id"], "t-1");
        assert_eq!(shaped["status"], "fix_proposed");
        assert_eq!(shaped["route"], "/checkout");
        assert_eq!(shaped["guest_name"], "Reviewer");
        assert_eq!(shaped["fix_commit"], "abc123");
        assert_eq!(shaped["fix_summary"], "fix the button");
        assert_eq!(
            shaped["comments"],
            json!(["button is broken", "still broken on mobile"])
        );
        // Internal fields not in the agent contract are dropped.
        assert!(shaped.get("ref_num").is_none());
    }

    #[test]
    fn test_shape_feedback_threads_handles_envelope_and_bare_array() {
        let bare = json!([{ "id": "t-1", "status": "open", "body": "hi" }]);
        let shaped = shape_feedback_threads(&bare);
        assert_eq!(shaped["threads"][0]["id"], "t-1");
        assert_eq!(shaped["threads"][0]["body"], "hi");

        let enveloped = json!({ "threads": [{ "id": "t-2", "status": "open" }] });
        let shaped = shape_feedback_threads(&enveloped);
        assert_eq!(shaped["threads"][0]["id"], "t-2");
    }

    #[test]
    fn test_tools_call_returns_text_content() {
        let params = json!({ "name": "observed_edges", "arguments": {} });
        let result = handle_method("tools/call", Some(&params)).unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert!(result["content"][0]["text"].is_string());
    }

    #[test]
    fn test_unknown_method_is_error() {
        assert!(handle_method("does/not/exist", None).is_err());
    }

    #[test]
    fn test_unknown_tool_is_error() {
        let params = json!({ "name": "bogus" });
        assert!(handle_method("tools/call", Some(&params)).is_err());
    }
}
