//! MCP tools for submitting bug reports and feature requests to portzero.cloud.
//!
//! `submit_bug_report` and `submit_feature_request` both POST to
//! `/feedback/reports` on the cloud API (requires `portzero login`). They are
//! split out of `mcp.rs` to keep that file within the client file-size budget.
//!
//! The two descriptions are deliberately asymmetric. A bug report has no gate:
//! if the product misbehaves, file it. A feature request carries a high bar —
//! submit only when the missing feature would make things *much* easier for the
//! user — and the agent must confirm with the user before submitting, never on
//! its own initiative. That guidance lives in the tool `description` text,
//! which is what an agent reads when deciding whether to call the tool.

use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::mcp::{required_string_arg, run_async, tool_error, tool_ok, NOT_LOGGED_IN};

/// The two feedback-submission tool definitions, spread into the main
/// `tools/list` catalog.
pub fn report_tools() -> Vec<Value> {
    let common_props = |title_desc: &str, body_desc: &str, context_desc: &str| {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": title_desc },
                "body": { "type": "string", "description": body_desc },
                "context": { "type": "string", "description": context_desc },
            },
            "required": ["title", "body"],
        })
    };

    vec![
        json!({
            "name": "submit_bug_report",
            "description": "Report a bug in Port Zero itself — the `portzero` CLI/daemon, tunnels, \
                the dashboard, or the cloud API — to the Port Zero team. Use this for defects in \
                the product you are using, NOT for bugs in the user's own application. Give clear, \
                step-by-step reproduction in `body`, and put environment details (OS, \
                `portzero --version`, relevant tunnel domains, log excerpts) in `context`. \
                Requires `portzero login`.",
            "inputSchema": common_props(
                "One-line summary of the bug.",
                "What happened, what you expected instead, and exact steps to reproduce it.",
                "Optional environment details: OS, portzero version, tunnel domains, relevant logs.",
            ),
        }),
        json!({
            "name": "submit_feature_request",
            "description": "Request a new Port Zero feature on the user's behalf. \
                STRICT BAR — submit ONLY if the missing feature would make things MUCH, MUCH easier \
                for the user; a minor convenience or nice-to-have does NOT qualify. And ALWAYS ask \
                the user first and get their explicit confirmation BEFORE calling this tool — never \
                submit a feature request on your own initiative or without checking. In `body`, \
                describe the concrete problem the user is hitting and why this feature would be a \
                major improvement, not merely a restated wish. Requires `portzero login`.",
            "inputSchema": common_props(
                "One-line summary of the requested feature.",
                "The problem the user is hitting and why this feature would make things much \
                 easier for them.",
                "Optional context: the user's workflow, what they tried, relevant repo/version details.",
            ),
        }),
    ]
}

/// Handle a `submit_bug_report` / `submit_feature_request` tools/call. `kind`
/// is "bug" or "feature". Returns a `tools/call` result envelope (reporting
/// argument/auth/HTTP failures as `isError` results, not JSON-RPC errors).
pub fn submit_report(kind: &str, arguments: Option<&Value>) -> Value {
    let title = match required_string_arg(arguments, "title") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let body = match required_string_arg(arguments, "body") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let context = arguments
        .and_then(|a| a.get("context"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return tool_error(NOT_LOGGED_IN);
    }

    let mut payload = json!({ "kind": kind, "title": title, "body": body });
    if let Some(context) = context {
        payload["context"] = Value::String(context);
    }

    let result = run_async(async move {
        let resp = client.post("/feedback/reports", &payload).await?;
        let http_status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((http_status, text))
    });

    match result {
        Ok((http_status, text)) if http_status.is_success() => {
            let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
            tool_ok(&body)
        }
        Ok((http_status, text)) if http_status.as_u16() == 401 => tool_error(&format!(
            "Authentication expired or invalid (HTTP 401). Run `portzero logout` then \
             `portzero login` to re-authenticate.\n\n{text}"
        )),
        Ok((http_status, text)) => tool_error(&format!(
            "failed to submit {kind} report (HTTP {http_status}): {text}"
        )),
        Err(e) => tool_error(&format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_named(name: &str) -> Value {
        report_tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} not advertised"))
    }

    #[test]
    fn test_report_tools_advertises_both() {
        let names: Vec<String> = report_tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"submit_bug_report".to_string()));
        assert!(names.contains(&"submit_feature_request".to_string()));
    }

    #[test]
    fn test_both_tools_require_title_and_body() {
        for name in ["submit_bug_report", "submit_feature_request"] {
            let tool = tool_named(name);
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object");
            assert_eq!(schema["properties"]["title"]["type"], "string");
            assert_eq!(schema["properties"]["body"]["type"], "string");
            assert_eq!(schema["properties"]["context"]["type"], "string");
            let required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(required, vec!["title", "body"]);
        }
    }

    #[test]
    fn test_feature_request_description_states_the_bar_and_confirmation() {
        let desc = tool_named("submit_feature_request")["description"]
            .as_str()
            .unwrap()
            .to_string();
        // The high bar: only if it makes things much easier.
        assert!(desc.contains("MUCH, MUCH easier"), "got: {desc}");
        // Must confirm with the user before submitting.
        let lower = desc.to_lowercase();
        assert!(lower.contains("confirm"), "got: {desc}");
        assert!(lower.contains("before"), "got: {desc}");
    }

    #[test]
    fn test_bug_report_targets_the_product_not_the_users_app() {
        let desc = tool_named("submit_bug_report")["description"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(desc.contains("Port Zero"), "got: {desc}");
        assert!(desc.to_lowercase().contains("not for"), "got: {desc}");
    }

    #[test]
    fn test_submit_report_missing_title_is_tool_error() {
        let result = submit_report("bug", Some(&json!({ "body": "b" })));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("title"), "got: {text}");
    }

    #[test]
    fn test_submit_report_missing_body_is_tool_error() {
        let result = submit_report("feature", Some(&json!({ "title": "t" })));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("body"), "got: {text}");
    }
}
