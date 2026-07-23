//! MCP cost tools for portzero.cloud: `cost_of_commit`, `cost_of_pr`, and
//! `cost_of_lines`.
//!
//! These answer "what did the agent labor cost?" at three granularities. They
//! are **consent-gated and local-first**: each reads the durable local cost
//! store (`cost_store`, populated by the session-end hook) first, then — when
//! logged in and the repo has cost tracking consented — merges cloud data and
//! labels every result with its `source` (`local`/`cloud`).
//!
//! Contract (implemented server-side by the cloud `agent_cost` routes):
//! - `GET  /agent-cost/commit/{sha}?repo=` → `{attributed, cost_usd, provenance, confidence, breakdown[]}`
//! - `GET  /agent-cost/pr/{number}?repo=`  → same shape
//! - `POST /agent-cost/commits {repo, shas[]}` → per-SHA costs (server can't blame repos it doesn't host)
//! - `GET  /agent-cost/config?repo=` → `{enabled, ...}` (enabled = consent active)
//!
//! When the repo isn't consented (cloud 403 or `config.enabled == false`) and
//! there's no local data, the tools return a readable `consent_required`
//! payload (via `tool_ok`, not `tool_error`) so agents can act on it. An
//! attributed-but-unknown answer is `{"attributed":false,"cost_usd":null,
//! "note":"unknown"}` — never a misleading `$0`.

use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::cost_store::{CostStore, StoredCommitCost};
use crate::mcp::{required_string_arg, run_async, tool_error, tool_ok};

/// The three cost-tool definitions, spread into the main `tools/list` catalog.
pub fn cost_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "cost_of_commit",
            "description": "Report the measured agent-labor cost attributed to a specific commit \
                (dollars, model, provenance, confidence). Local-first: reads the durable local \
                cost store, then merges portzero.cloud data when you are logged in and this repo \
                has cost tracking (data sharing) turned on. Returns a `consent_required` result \
                if it is off. Cost tracking captures usage metadata only — never prompt or code \
                content.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "sha": {
                        "type": "string",
                        "description": "Full or abbreviated git commit SHA.",
                    },
                },
                "required": ["sha"],
            },
        }),
        json!({
            "name": "cost_of_pr",
            "description": "Report the aggregate measured agent-labor cost of a pull request \
                (sum over its commits). Local-first, cloud-merged, consent-gated exactly like \
                cost_of_commit. Returns `consent_required` when cost tracking is off for this repo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "number": {
                        "type": "integer",
                        "description": "Pull request number.",
                    },
                },
                "required": ["number"],
            },
        }),
        json!({
            "name": "cost_of_lines",
            "description": "Report the agent-labor cost attributed to a range of lines in a file. \
                Blames the lines locally (git blame) to the commits that last touched them, then \
                resolves each commit's cost (local store first, then portzero.cloud). Aggregates \
                the total. Consent-gated: returns `consent_required` when cost tracking is off.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Path to the file, relative to the repo root.",
                    },
                    "start": {
                        "type": "integer",
                        "description": "First line of the range (1-based, inclusive).",
                    },
                    "end": {
                        "type": "integer",
                        "description": "Last line of the range (1-based, inclusive).",
                    },
                },
                "required": ["file", "start", "end"],
            },
        }),
    ]
}

/// Dispatch a cost tool by name. Returns `None` if `name` is not a cost tool,
/// so the main dispatcher can fall through to its other tools.
pub fn call_cost_tool(name: &str, arguments: Option<&Value>) -> Option<Value> {
    match name {
        "cost_of_commit" => Some(cost_of_commit(arguments)),
        "cost_of_pr" => Some(cost_of_pr(arguments)),
        "cost_of_lines" => Some(cost_of_lines(arguments)),
        _ => None,
    }
}

/// The `consent_required` result body (readable by agents; not an error).
pub(crate) fn consent_required() -> Value {
    json!({
        "error": "consent_required",
        "note": "Turn on cost tracking (data sharing) for this repo in app.portzero.cloud",
    })
}

/// The "attributed cost is unknown" body (never a misleading `$0`).
fn unknown() -> Value {
    json!({ "attributed": false, "cost_usd": null, "note": "unknown" })
}

/// Resolve the current repository to `owner/name` via `git remote get-url
/// origin`. A clear next-step error is returned when that cannot be done.
pub(crate) fn resolve_repo() -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| {
            format!(
                "Could not run git to resolve this repository ({e}).\n\
                 Make sure git is installed and you are inside the repository."
            )
        })?;
    if !output.status.success() {
        return Err("This directory has no git remote named `origin`.\n\
             Cost tools resolve the repo from `git remote get-url origin` — run this inside a \
             cloned repository, or add an origin remote with `git remote add origin <url>`."
            .to_string());
    }
    let url = String::from_utf8_lossy(&output.stdout);
    normalize_remote(&url).ok_or_else(|| {
        format!(
            "Could not parse `owner/name` from the origin remote URL `{}`.\n\
             Expected a GitHub-style remote such as git@github.com:owner/name.git or \
             https://github.com/owner/name.git.",
            url.trim()
        )
    })
}

/// Normalize a git remote URL to `owner/name` (drops protocol, host, `.git`).
pub(crate) fn normalize_remote(url: &str) -> Option<String> {
    let cleaned = url.trim();
    let cleaned = cleaned.strip_suffix(".git").unwrap_or(cleaned);
    let parts: Vec<&str> = cleaned
        .split(['/', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() >= 2 {
        let name = parts[parts.len() - 1];
        let owner = parts[parts.len() - 2];
        if !owner.is_empty() && !name.is_empty() && !owner.contains('@') {
            return Some(format!("{owner}/{name}"));
        }
        // owner may still carry a leading `git@host` when there was no ':'
        // separator we could split on; fall through to reject.
    }
    None
}

/// Outcome of a cloud cost lookup, normalized so tool handlers stay small.
enum CloudOutcome {
    /// An attributed cost, carrying the full cloud body.
    Found(Value),
    /// The cloud answered but has no attribution for this target.
    Unattributed,
    /// Repo isn't consented (HTTP 403) — cost tracking is off.
    ConsentRequired,
    /// Not logged in — no cloud call was made.
    NotLoggedIn,
    /// A transport/HTTP error with a message.
    Error(String),
}

/// GET a cloud cost endpoint and classify the outcome.
fn cloud_get(path: String) -> CloudOutcome {
    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return CloudOutcome::NotLoggedIn;
    }
    let result = run_async(async move {
        let resp = client.get(&path).await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((status, text))
    });
    classify_cloud(result)
}

/// POST to a cloud cost endpoint and classify the outcome.
fn cloud_post(path: &str, body: &Value) -> CloudOutcome {
    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return CloudOutcome::NotLoggedIn;
    }
    let path = path.to_string();
    let body = body.clone();
    let result = run_async(async move {
        let resp = client.post(&path, &body).await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((status, text))
    });
    classify_cloud(result)
}

/// Turn a `(status, text)` HTTP result into a `CloudOutcome`.
fn classify_cloud(result: anyhow::Result<(reqwest::StatusCode, String)>) -> CloudOutcome {
    match result {
        Ok((status, _)) if status.as_u16() == 403 => CloudOutcome::ConsentRequired,
        Ok((status, text)) if status.is_success() => match serde_json::from_str::<Value>(&text) {
            Ok(body) if body.get("attributed").and_then(Value::as_bool) == Some(false) => {
                CloudOutcome::Unattributed
            }
            Ok(body) => CloudOutcome::Found(body),
            Err(e) => CloudOutcome::Error(format!("failed to parse cloud response: {e}")),
        },
        Ok((status, text)) => {
            CloudOutcome::Error(format!("cloud request failed (HTTP {status}): {text}"))
        }
        Err(e) => CloudOutcome::Error(format!("{e:#}")),
    }
}

/// Build a result payload from a single stored commit cost (source: local).
fn local_commit_payload(c: &StoredCommitCost) -> Value {
    json!({
        "attributed": true,
        "cost_usd": c.attributed_cost_usd,
        "provenance": c.cost_provenance,
        "confidence": c.confidence,
        "source": "local",
    })
}

/// Merge a cloud body with an optional local fallback into a final result.
/// Cloud wins when present; otherwise fall back to local; otherwise unknown or
/// consent_required depending on why the cloud had nothing.
fn finalize(outcome: CloudOutcome, local: Option<&StoredCommitCost>) -> Value {
    match outcome {
        CloudOutcome::Found(mut body) => {
            if let Value::Object(map) = &mut body {
                map.entry("source").or_insert_with(|| json!("cloud"));
            }
            tool_ok(&body)
        }
        CloudOutcome::Unattributed => match local {
            Some(c) => tool_ok(&local_commit_payload(c)),
            None => tool_ok(&unknown()),
        },
        CloudOutcome::ConsentRequired | CloudOutcome::NotLoggedIn => match local {
            // We have consented data cached offline — keep answering.
            Some(c) => tool_ok(&local_commit_payload(c)),
            None => tool_ok(&consent_required()),
        },
        CloudOutcome::Error(msg) => match local {
            Some(c) => tool_ok(&local_commit_payload(c)),
            None => tool_error(&msg),
        },
    }
}

/// `cost_of_commit` — attributed cost of one commit.
fn cost_of_commit(arguments: Option<&Value>) -> Value {
    let sha = match required_string_arg(arguments, "sha") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    if !is_safe_sha(&sha) {
        return tool_error(&format!(
            "invalid sha `{sha}`: expected a hex git commit SHA (no slashes or spaces)."
        ));
    }
    let repo = match resolve_repo() {
        Ok(r) => r,
        Err(e) => return tool_error(&e),
    };
    let store = CostStore::load().unwrap_or_default();
    let local = store.lookup_commit(&repo, &sha).cloned();
    let outcome = cloud_get(format!("/agent-cost/commit/{sha}?repo={repo}"));
    finalize(outcome, local.as_ref())
}

/// `cost_of_pr` — aggregate cost of a pull request.
fn cost_of_pr(arguments: Option<&Value>) -> Value {
    let number = match required_integer_arg(arguments, "number") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let repo = match resolve_repo() {
        Ok(r) => r,
        Err(e) => return tool_error(&e),
    };
    // A PR spans many commits; the local store cannot know a PR's commit set
    // without cloud data, so PR lookups are cloud-authoritative (no local
    // fallback), still consent-gated.
    let outcome = cloud_get(format!("/agent-cost/pr/{number}?repo={repo}"));
    finalize(outcome, None)
}

/// `cost_of_lines` — cost attributed to a line range, via local blame.
fn cost_of_lines(arguments: Option<&Value>) -> Value {
    let file = match required_string_arg(arguments, "file") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let start = match required_integer_arg(arguments, "start") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    let end = match required_integer_arg(arguments, "end") {
        Ok(v) => v,
        Err(e) => return tool_error(&e),
    };
    if start < 1 || end < start {
        return tool_error(
            "invalid line range: `start` must be >= 1 and `end` must be >= `start`.",
        );
    }
    let repo = match resolve_repo() {
        Ok(r) => r,
        Err(e) => return tool_error(&e),
    };
    let shas = match blame_shas(&file, start, end) {
        Ok(s) => s,
        Err(e) => return tool_error(&e),
    };
    if shas.is_empty() {
        return tool_ok(&unknown());
    }
    aggregate_line_cost(&repo, &shas)
}

/// Resolve a set of commit SHAs to an aggregate cost (local + cloud).
fn aggregate_line_cost(repo: &str, shas: &[String]) -> Value {
    let store = CostStore::load().unwrap_or_default();
    let local: Vec<StoredCommitCost> = store
        .lookup_commits(repo, shas)
        .into_iter()
        .cloned()
        .collect();

    let outcome = cloud_post(
        "/agent-cost/commits",
        &json!({ "repo": repo, "shas": shas }),
    );
    match outcome {
        CloudOutcome::Found(body) => tool_ok(&shape_line_cost_from_cloud(&body, shas)),
        CloudOutcome::Unattributed => aggregate_local_or_unknown(&local, shas),
        CloudOutcome::ConsentRequired | CloudOutcome::NotLoggedIn => {
            if local.is_empty() {
                tool_ok(&consent_required())
            } else {
                aggregate_local_or_unknown(&local, shas)
            }
        }
        CloudOutcome::Error(msg) => {
            if local.is_empty() {
                tool_error(&msg)
            } else {
                aggregate_local_or_unknown(&local, shas)
            }
        }
    }
}

/// Aggregate matched local commit costs, or `unknown` when none matched.
fn aggregate_local_or_unknown(local: &[StoredCommitCost], shas: &[String]) -> Value {
    if local.is_empty() {
        return tool_ok(&unknown());
    }
    let total: f64 = local.iter().map(|c| c.attributed_cost_usd).sum();
    let confidence = weakest_confidence(local.iter().map(|c| c.confidence.as_str()));
    let breakdown: Vec<Value> = local
        .iter()
        .map(|c| {
            json!({
                "sha": c.commit_sha,
                "cost_usd": c.attributed_cost_usd,
                "confidence": c.confidence,
                "provenance": c.cost_provenance,
            })
        })
        .collect();
    tool_ok(&json!({
        "attributed": true,
        "cost_usd": total,
        "provenance": "measured",
        "confidence": confidence,
        "source": "local",
        "commits": shas.len(),
        "breakdown": breakdown,
    }))
}

/// Shape the cloud `/agent-cost/commits` response into an aggregate line cost.
fn shape_line_cost_from_cloud(body: &Value, shas: &[String]) -> Value {
    let items = body
        .as_array()
        .or_else(|| body.get("costs").and_then(Value::as_array))
        .or_else(|| body.get("breakdown").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();

    let total: f64 = items
        .iter()
        .filter_map(|it| it.get("cost_usd").and_then(Value::as_f64))
        .sum();
    let any_attributed = items
        .iter()
        .any(|it| it.get("cost_usd").and_then(Value::as_f64).is_some());
    if !any_attributed {
        return unknown();
    }
    let confidence = weakest_confidence(
        items
            .iter()
            .filter_map(|it| it.get("confidence").and_then(Value::as_str)),
    );
    json!({
        "attributed": true,
        "cost_usd": total,
        "provenance": "measured",
        "confidence": confidence,
        "source": "cloud",
        "commits": shas.len(),
        "breakdown": items,
    })
}

/// The most conservative (weakest) confidence in a set: low < medium < high.
fn weakest_confidence<'a>(levels: impl Iterator<Item = &'a str>) -> String {
    let rank = |c: &str| match c {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    };
    levels.min_by_key(|c| rank(c)).unwrap_or("low").to_string()
}

/// Blame `file` over `[start, end]` and collect the distinct commit SHAs.
fn blame_shas(file: &str, start: i64, end: i64) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .args([
            "blame",
            "-L",
            &format!("{start},{end}"),
            "--porcelain",
            "--",
            file,
        ])
        .output()
        .map_err(|e| {
            format!(
                "Could not run git blame ({e}).\n\
                 Make sure git is installed and you are inside the repository."
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git blame failed for {file}:{start},{end}: {}\n\
             Check that the file path is relative to the repo root and the line range exists.",
            stderr.trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut shas: Vec<String> = Vec::new();
    // In porcelain output, each hunk header starts with the 40-hex SHA.
    for line in text.lines() {
        let sha = line.split_whitespace().next().unwrap_or("");
        if sha.len() == 40
            && sha.chars().all(|c| c.is_ascii_hexdigit())
            && !shas.contains(&sha.to_string())
        {
            shas.push(sha.to_string());
        }
    }
    Ok(shas)
}

/// True if `sha` is a plausible git SHA (hex, no path-escaping characters).
fn is_safe_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Extract a required integer argument, with a descriptive error.
fn required_integer_arg(arguments: Option<&Value>, key: &str) -> Result<i64, String> {
    arguments
        .and_then(|a| a.get(key))
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("missing required argument `{key}` (an integer)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_named(name: &str) -> Value {
        cost_tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} not advertised"))
    }

    #[test]
    fn cost_tools_advertise_expected_names_and_required_args() {
        let commit = tool_named("cost_of_commit");
        assert_eq!(commit["inputSchema"]["properties"]["sha"]["type"], "string");
        assert_eq!(
            commit["inputSchema"]["required"],
            json!(["sha"]),
            "cost_of_commit requires sha"
        );

        let pr = tool_named("cost_of_pr");
        assert_eq!(pr["inputSchema"]["properties"]["number"]["type"], "integer");
        assert_eq!(pr["inputSchema"]["required"], json!(["number"]));

        let lines = tool_named("cost_of_lines");
        let props = &lines["inputSchema"]["properties"];
        assert_eq!(props["file"]["type"], "string");
        assert_eq!(props["start"]["type"], "integer");
        assert_eq!(props["end"]["type"], "integer");
        assert_eq!(
            lines["inputSchema"]["required"],
            json!(["file", "start", "end"])
        );
    }

    #[test]
    fn normalize_remote_handles_ssh_https_and_git_suffix() {
        assert_eq!(
            normalize_remote("git@github.com:acme/widget.git"),
            Some("acme/widget".to_string())
        );
        assert_eq!(
            normalize_remote("https://github.com/acme/widget.git"),
            Some("acme/widget".to_string())
        );
        assert_eq!(
            normalize_remote("https://github.com/acme/widget"),
            Some("acme/widget".to_string())
        );
        assert_eq!(
            normalize_remote("  ssh://git@github.com/acme/widget.git\n"),
            Some("acme/widget".to_string())
        );
        assert_eq!(normalize_remote("not-a-url"), None);
    }

    #[test]
    fn cost_of_commit_rejects_bad_sha() {
        let result = cost_of_commit(Some(&json!({ "sha": "../etc/passwd" })));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("invalid sha"), "got: {text}");
    }

    #[test]
    fn cost_of_commit_missing_sha_is_tool_error() {
        let result = cost_of_commit(Some(&json!({})));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("sha"), "got: {text}");
    }

    #[test]
    fn cost_of_pr_missing_number_is_tool_error() {
        let result = cost_of_pr(Some(&json!({})));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("number"), "got: {text}");
    }

    #[test]
    fn cost_of_lines_rejects_bad_range() {
        let result = cost_of_lines(Some(&json!({ "file": "a.rs", "start": 5, "end": 2 })));
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("line range"), "got: {text}");
    }

    #[test]
    fn consent_required_shape_is_stable() {
        let v = consent_required();
        assert_eq!(v["error"], "consent_required");
        assert!(v["note"].as_str().unwrap().contains("app.portzero.cloud"));
    }

    #[test]
    fn finalize_consent_required_without_local_returns_consent_payload() {
        let result = finalize(CloudOutcome::ConsentRequired, None);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("consent_required"), "got: {text}");
    }

    #[test]
    fn finalize_prefers_local_when_cloud_is_off() {
        let local = StoredCommitCost {
            repo: "acme/widget".to_string(),
            commit_sha: "abc".to_string(),
            attributed_cost_usd: 0.5,
            confidence: "high".to_string(),
            cost_provenance: "measured".to_string(),
            session_id: "s-1".to_string(),
        };
        let result = finalize(CloudOutcome::NotLoggedIn, Some(&local));
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"source\": \"local\""), "got: {text}");
        assert!(text.contains("0.5"), "got: {text}");
    }

    #[test]
    fn finalize_cloud_found_labels_source_cloud() {
        let body = json!({
            "attributed": true, "cost_usd": 1.25,
            "provenance": "measured", "confidence": "medium"
        });
        let result = finalize(CloudOutcome::Found(body), None);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"source\": \"cloud\""), "got: {text}");
    }

    #[test]
    fn shape_line_cost_sums_and_reports_weakest_confidence() {
        let body = json!({
            "costs": [
                { "sha": "a", "cost_usd": 1.0, "confidence": "high" },
                { "sha": "b", "cost_usd": 0.5, "confidence": "low" },
            ]
        });
        let shaped = shape_line_cost_from_cloud(&body, &["a".into(), "b".into()]);
        assert_eq!(shaped["attributed"], true);
        assert_eq!(shaped["cost_usd"], 1.5);
        assert_eq!(shaped["confidence"], "low");
        assert_eq!(shaped["source"], "cloud");
    }

    #[test]
    fn shape_line_cost_unattributed_is_unknown() {
        let body = json!({ "costs": [ { "sha": "a", "cost_usd": null } ] });
        let shaped = shape_line_cost_from_cloud(&body, &["a".into()]);
        assert_eq!(shaped["attributed"], false);
        assert_eq!(shaped["note"], "unknown");
    }

    #[test]
    fn weakest_confidence_picks_lowest() {
        assert_eq!(
            weakest_confidence(["high", "medium", "high"].into_iter()),
            "medium"
        );
        assert_eq!(weakest_confidence(["high", "high"].into_iter()), "high");
        assert_eq!(weakest_confidence(std::iter::empty()), "low");
    }
}
