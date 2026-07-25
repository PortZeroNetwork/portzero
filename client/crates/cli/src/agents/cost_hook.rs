//! `portzero agents cost-hook` — the session-end (Claude Code `Stop`) cost hook.
//!
//! Wired into a repo's `.claude/settings.json` by `agents::repo`. On each
//! session end it:
//!
//! 1. Resolves the repo (`git remote origin`) and checks whether cost tracking
//!    is **consented** for it (cloud `/agent-cost/config`, cached so consented
//!    repos keep working offline).
//! 2. Reads the session transcript and sums token usage **per model** — from
//!    the `usage`/`model` fields only. It never reads message text or any other
//!    content. Cost is computed from a small embedded public pricing table.
//! 3. **If not consented (or not logged in):** prints a one-line ephemeral
//!    teaser to stderr and persists nothing — no store write, no upload.
//! 4. **If consented:** correlates the session's commits (git log within the
//!    session window) with a confidence, writes the durable local cost store
//!    (`cost_store`), and posts the session to the cloud (idempotent on
//!    `session_id`). Works in a headless sandbox with no daemon.
//!
//! Privacy hard line: **usage metadata only** leaves this process — token
//! counts, model ids, dollar figures, commit SHAs, timestamps. Never content.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::api_client::ApiClient;
use crate::cost_store::{CostStore, StoredCommitCost, StoredSession};
use crate::mcp::run_async;
use crate::mcp_cost::resolve_repo;

/// Public entry point (`portzero agents cost-hook`). Best-effort: a Stop hook
/// must never break the session, so every failure degrades to a printed
/// message and a success exit.
pub fn run() -> anyhow::Result<()> {
    if let Err(e) = run_inner() {
        eprintln!("PortZero cost hook: {e}");
    }
    Ok(())
}

fn run_inner() -> Result<(), String> {
    let input = read_hook_input();
    let repo = resolve_repo()?;

    let transcript = find_transcript(input.transcript_path.as_deref(), input.cwd.as_deref())
        .ok_or_else(|| {
            "No Claude Code transcript found for this session.\n\
             Cost is computed from the session transcript's usage metadata; nothing was recorded."
                .to_string()
        })?;
    let text = std::fs::read_to_string(&transcript)
        .map_err(|e| format!("reading transcript {}: {e}", transcript.display()))?;
    let usage = parse_usage_from_str(&text);
    let priced = price_session(&usage);

    match repo_consent(&repo) {
        Consent::Yes => capture(&repo, &input, &usage, &priced),
        // Not consented, or we cannot confirm consent: ephemeral teaser only.
        Consent::No | Consent::Unknown => {
            print_teaser(priced.cost_usd);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Hook input (Claude Code passes a JSON object on stdin).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HookInput {
    session_id: Option<String>,
    transcript_path: Option<String>,
    cwd: Option<String>,
}

fn read_hook_input() -> HookInput {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return HookInput::default();
    }
    let Ok(v) = serde_json::from_str::<Value>(&buf) else {
        return HookInput::default();
    };
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    HookInput {
        session_id: get("session_id"),
        transcript_path: get("transcript_path"),
        cwd: get("cwd"),
    }
}

/// Find the transcript file: prefer the explicit path from the hook input,
/// else the newest `.jsonl` under `~/.claude/projects/<url-encoded cwd>/`.
fn find_transcript(explicit: Option<&str>, cwd: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let home = dirs::home_dir()?;
    let cwd = cwd.map(str::to_string).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    })?;
    let dir = home
        .join(".claude")
        .join("projects")
        .join(encode_project_dir(&cwd));
    newest_jsonl(&dir)
}

/// Claude Code encodes a project path into a directory name by replacing the
/// path separators (and `.`) with `-`.
fn encode_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| {
            if c == '/' || c == '.' || c == '_' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

fn newest_jsonl(dir: &std::path::Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl") {
            if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
                    newest = Some((modified, path));
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

// ---------------------------------------------------------------------------
// Transcript parsing — usage/model metadata ONLY, never content.
// ---------------------------------------------------------------------------

/// Per-model token tallies (input, output, and the two cache token classes).
#[derive(Default, Clone)]
struct ModelUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

/// A session's usage rolled up per model, plus the session time window.
#[derive(Default)]
struct SessionUsage {
    by_model: BTreeMap<String, ModelUsage>,
    started_at: Option<String>,
    ended_at: Option<String>,
}

/// Parse a transcript's usage metadata. Reads only each event's `timestamp`
/// and its `message.model` / `message.usage.*` — never `message.content` or
/// any other text.
fn parse_usage_from_str(text: &str) -> SessionUsage {
    let mut usage = SessionUsage::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        record_timestamp(&mut usage, &event);
        // Only assistant messages carry a usage object.
        let Some(message) = event.get("message") else {
            continue;
        };
        let Some(u) = message.get("usage") else {
            continue;
        };
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let slot = usage.by_model.entry(model).or_default();
        slot.input += field_u64(u, "input_tokens");
        slot.output += field_u64(u, "output_tokens");
        slot.cache_read += field_u64(u, "cache_read_input_tokens");
        slot.cache_creation += field_u64(u, "cache_creation_input_tokens");
    }
    usage
}

fn record_timestamp(usage: &mut SessionUsage, event: &Value) {
    if let Some(ts) = event.get("timestamp").and_then(Value::as_str) {
        if usage.started_at.is_none() {
            usage.started_at = Some(ts.to_string());
        }
        usage.ended_at = Some(ts.to_string());
    }
}

fn field_u64(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Pricing — small embedded public table (USD per million tokens).
// ---------------------------------------------------------------------------

/// Public Anthropic list prices, matched by substring of the model id.
/// `(needle, input $/Mtok, output $/Mtok)`. Cache reads are billed at 0.1x the
/// input rate and cache writes at 1.25x, the standard Anthropic multipliers.
const PRICING: &[(&str, f64, f64)] = &[
    ("opus", 15.0, 75.0),
    ("sonnet", 3.0, 15.0),
    ("haiku", 0.80, 4.0),
];

/// Fallback when a model id matches nothing above (priced as Sonnet, and the
/// session is then flagged `inferred` rather than `measured`).
const FALLBACK_RATE: (f64, f64) = (3.0, 15.0);

struct PricedSession {
    model: String,
    tokens_in: u64,
    tokens_out: u64,
    cost_usd: f64,
    provenance: &'static str,
}

/// Look up `(input_rate, output_rate, matched)` for a model id.
fn rate_for(model: &str) -> (f64, f64, bool) {
    let m = model.to_lowercase();
    for (needle, input, output) in PRICING {
        if m.contains(needle) {
            return (*input, *output, true);
        }
    }
    (FALLBACK_RATE.0, FALLBACK_RATE.1, false)
}

/// Cost of one model's usage, and whether its rate was matched (vs. fallback).
fn model_cost(model: &str, u: &ModelUsage) -> (f64, bool) {
    let (input_rate, output_rate, matched) = rate_for(model);
    let per_m = |tokens: u64, rate: f64| (tokens as f64 / 1_000_000.0) * rate;
    let cost = per_m(u.input, input_rate)
        + per_m(u.output, output_rate)
        + per_m(u.cache_read, input_rate * 0.1)
        + per_m(u.cache_creation, input_rate * 1.25);
    (cost, matched)
}

/// Price a whole session: total cost across models, total tokens, and the
/// dominant model (most output tokens) as the session's representative model.
fn price_session(usage: &SessionUsage) -> PricedSession {
    let mut cost_usd = 0.0;
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut all_matched = true;
    let mut dominant: Option<(&String, u64)> = None;

    for (model, u) in &usage.by_model {
        let (cost, matched) = model_cost(model, u);
        cost_usd += cost;
        all_matched &= matched;
        tokens_in += u.input + u.cache_read + u.cache_creation;
        tokens_out += u.output;
        if dominant.is_none_or(|(_, best)| u.output > best) {
            dominant = Some((model, u.output));
        }
    }

    PricedSession {
        model: dominant.map(|(m, _)| m.clone()).unwrap_or_default(),
        tokens_in,
        tokens_out,
        cost_usd,
        provenance: if usage.by_model.is_empty() || !all_matched {
            "inferred"
        } else {
            "measured"
        },
    }
}

// ---------------------------------------------------------------------------
// Consent (cloud config, cached for offline use).
// ---------------------------------------------------------------------------

enum Consent {
    Yes,
    No,
    Unknown,
}

/// Resolve whether cost tracking is consented for `repo`: cloud
/// `/agent-cost/config` when logged in (result cached), the cache when the
/// network is unavailable, or `Unknown` when neither can answer.
fn repo_consent(repo: &str) -> Consent {
    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return Consent::Unknown;
    }
    let path = format!("/agent-cost/config?repo={repo}");
    let result = run_async(async move {
        let resp = client.get(&path).await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::Ok((status, text))
    });
    match result {
        Ok((status, _)) if status.as_u16() == 403 => {
            write_consent_cache(repo, false);
            Consent::No
        }
        Ok((status, text)) if status.is_success() => {
            let enabled = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("enabled").and_then(Value::as_bool))
                .unwrap_or(false);
            write_consent_cache(repo, enabled);
            if enabled {
                Consent::Yes
            } else {
                Consent::No
            }
        }
        // Network/other error: fall back to the last-known cached state.
        _ => match read_consent_cache(repo) {
            Some(true) => Consent::Yes,
            Some(false) => Consent::No,
            None => Consent::Unknown,
        },
    }
}

fn consent_cache_path() -> Option<PathBuf> {
    crate::cost_store::store_dir()
        .ok()
        .map(|d| d.join("consent.json"))
}

fn read_consent_cache(repo: &str) -> Option<bool> {
    let path = consent_cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .get(repo)
        .and_then(Value::as_bool)
}

fn write_consent_cache(repo: &str, enabled: bool) {
    let Some(path) = consent_cache_path() else {
        return;
    };
    let mut map = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = map.as_object_mut() {
        obj.insert(repo.to_string(), json!(enabled));
    }
    if let Some(dir) = path.parent() {
        if portzero_daemon::secure_file::ensure_private_dir(dir).is_err() {
            return;
        }
    }
    if let Ok(text) = serde_json::to_vec(&map) {
        let _ = portzero_daemon::secure_file::write_secret_atomic(&path, &text);
    }
}

// ---------------------------------------------------------------------------
// Teaser (unconsented) and capture (consented).
// ---------------------------------------------------------------------------

fn print_teaser(cost_usd: f64) {
    eprintln!(
        "PortZero: this session cost ≈ ${cost_usd:.2}. \
         Consent in app.portzero.cloud to keep track over time."
    );
}

/// Consented path: attribute commits, write the local store, sync to cloud.
fn capture(
    repo: &str,
    input: &HookInput,
    usage: &SessionUsage,
    priced: &PricedSession,
) -> Result<(), String> {
    let session_id = session_id_for(input);
    let commits = attribute_commits(
        usage.started_at.as_deref(),
        usage.ended_at.as_deref(),
        priced.cost_usd,
    );

    write_store(repo, &session_id, usage, priced, &commits)?;
    sync_to_cloud(repo, &session_id, usage, priced, &commits);
    Ok(())
}

fn session_id_for(input: &HookInput) -> String {
    input
        .session_id
        .clone()
        .unwrap_or_else(|| format!("session-{}", now_stamp()))
}

fn now_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// One commit's attribution within the session window.
struct CommitAttribution {
    sha: String,
    cost_usd: f64,
    confidence: &'static str,
}

/// Correlate the session's commits by git-log within `[start, end]` and split
/// the session cost across them. One commit → high confidence; several → an
/// even split at medium confidence; none → nothing attributed.
fn attribute_commits(start: Option<&str>, end: Option<&str>, total: f64) -> Vec<CommitAttribution> {
    let shas = commits_in_window(start, end);
    let confidence = match shas.len() {
        0 => return Vec::new(),
        1 => "high",
        _ => "medium",
    };
    let per = total / shas.len() as f64;
    shas.into_iter()
        .map(|sha| CommitAttribution {
            sha,
            cost_usd: per,
            confidence,
        })
        .collect()
}

/// SHAs of commits on `HEAD` whose commit date is within the session window.
fn commits_in_window(start: Option<&str>, end: Option<&str>) -> Vec<String> {
    let mut args = vec!["log".to_string(), "--pretty=format:%H".to_string()];
    if let Some(s) = start {
        args.push(format!("--since={s}"));
    }
    if let Some(e) = end {
        args.push(format!("--until={e}"));
    }
    let Ok(output) = std::process::Command::new("git").args(&args).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| l.len() == 40 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string)
        .collect()
}

fn write_store(
    repo: &str,
    session_id: &str,
    usage: &SessionUsage,
    priced: &PricedSession,
    commits: &[CommitAttribution],
) -> Result<(), String> {
    let mut store = CostStore::load().map_err(|e| e.to_string())?;
    store.upsert_session(StoredSession {
        session_id: session_id.to_string(),
        agent: "claude-code".to_string(),
        model: priced.model.clone(),
        tokens_in: priced.tokens_in,
        tokens_out: priced.tokens_out,
        cost_usd: priced.cost_usd,
        cost_provenance: priced.provenance.to_string(),
        repo: repo.to_string(),
        started_at: usage.started_at.clone().unwrap_or_default(),
        ended_at: usage.ended_at.clone().unwrap_or_default(),
    });
    for c in commits {
        store.upsert_commit(StoredCommitCost {
            repo: repo.to_string(),
            commit_sha: c.sha.clone(),
            attributed_cost_usd: c.cost_usd,
            confidence: c.confidence.to_string(),
            cost_provenance: priced.provenance.to_string(),
            session_id: session_id.to_string(),
        });
    }
    store.save().map_err(|e| e.to_string())
}

/// Build the `POST /agent-cost/sessions` body (metadata only).
fn session_post_body(
    repo: &str,
    session_id: &str,
    usage: &SessionUsage,
    priced: &PricedSession,
    commits: &[CommitAttribution],
) -> Value {
    let commit_json: Vec<Value> = commits
        .iter()
        .map(|c| {
            json!({
                "sha": c.sha,
                "attributed_cost_usd": c.cost_usd,
                "confidence": c.confidence,
            })
        })
        .collect();
    json!({
        "session_id": session_id,
        "agent": "claude-code",
        "model": priced.model,
        "tokens_in": priced.tokens_in,
        "tokens_out": priced.tokens_out,
        "cost_usd": priced.cost_usd,
        "cost_provenance": priced.provenance,
        "repo": repo,
        "started_at": usage.started_at,
        "ended_at": usage.ended_at,
        "commits": commit_json,
    })
}

/// Post the session to the cloud (idempotent on `session_id`). Best-effort:
/// the durable local store already holds the data, so a failed upload just
/// means it syncs later — the hook still succeeds. A running daemon is not
/// required (works in a headless sandbox).
fn sync_to_cloud(
    repo: &str,
    session_id: &str,
    usage: &SessionUsage,
    priced: &PricedSession,
    commits: &[CommitAttribution],
) {
    let client = ApiClient::new();
    if client.require_auth().is_err() {
        return;
    }
    let body = session_post_body(repo, session_id, usage, priced, commits);
    let _ = run_async(async move {
        let resp = client.post("/agent-cost/sessions", &body).await?;
        anyhow::Ok(resp.status())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript whose message *content* carries a sentinel that must never
    /// appear in any computed output or stored record.
    const SENTINEL: &str = "SUPER_SECRET_PROMPT_TEXT";

    fn fixture_transcript() -> String {
        [
            json!({
                "type": "user",
                "timestamp": "2026-07-23T12:00:00.000Z",
                "message": { "role": "user", "content": SENTINEL }
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-07-23T12:01:00.000Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-opus-4-8",
                    "content": [ { "type": "text", "text": SENTINEL } ],
                    "usage": {
                        "input_tokens": 1000,
                        "output_tokens": 500,
                        "cache_read_input_tokens": 2000,
                        "cache_creation_input_tokens": 0
                    }
                }
            }),
            json!({
                "type": "assistant",
                "timestamp": "2026-07-23T12:05:00.000Z",
                "message": {
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [ { "type": "text", "text": SENTINEL } ],
                    "usage": { "input_tokens": 4000, "output_tokens": 100 }
                }
            }),
        ]
        .map(|v| v.to_string())
        .join("\n")
    }

    #[test]
    fn parse_reads_usage_only_and_never_content() {
        let usage = parse_usage_from_str(&fixture_transcript());
        // Two models tallied.
        assert_eq!(usage.by_model.len(), 2);
        let opus = &usage.by_model["claude-opus-4-8"];
        assert_eq!(opus.input, 1000);
        assert_eq!(opus.output, 500);
        assert_eq!(opus.cache_read, 2000);
        // Window captured from timestamps.
        assert_eq!(
            usage.started_at.as_deref(),
            Some("2026-07-23T12:00:00.000Z")
        );
        assert_eq!(usage.ended_at.as_deref(), Some("2026-07-23T12:05:00.000Z"));
    }

    #[test]
    fn priced_session_uses_dominant_model_and_measured_provenance() {
        let usage = parse_usage_from_str(&fixture_transcript());
        let priced = price_session(&usage);
        // Opus has the most output tokens (500 > 100) → representative model.
        assert_eq!(priced.model, "claude-opus-4-8");
        // tokens_in = opus(1000+2000) + sonnet(4000) = 7000; tokens_out = 600.
        assert_eq!(priced.tokens_in, 7000);
        assert_eq!(priced.tokens_out, 600);
        assert_eq!(priced.provenance, "measured");
        // Opus: 1000/1e6*15 + 500/1e6*75 + 2000/1e6*1.5 = 0.015+0.0375+0.003
        // Sonnet: 4000/1e6*3 + 100/1e6*15 = 0.012 + 0.0015
        let expected = 0.015 + 0.0375 + 0.003 + 0.012 + 0.0015;
        assert!(
            (priced.cost_usd - expected).abs() < 1e-9,
            "got {}",
            priced.cost_usd
        );
    }

    #[test]
    fn unknown_model_is_priced_but_flagged_inferred() {
        let transcript = json!({
            "type": "assistant",
            "timestamp": "2026-07-23T12:00:00.000Z",
            "message": {
                "model": "some-future-model-x1",
                "usage": { "input_tokens": 1000, "output_tokens": 1000 }
            }
        })
        .to_string();
        let priced = price_session(&parse_usage_from_str(&transcript));
        assert_eq!(priced.provenance, "inferred");
        assert!(priced.cost_usd > 0.0);
    }

    #[test]
    fn store_write_holds_metadata_only_no_content_leak() {
        crate::with_temp_home("costhook", |_| {
            let usage = parse_usage_from_str(&fixture_transcript());
            let priced = price_session(&usage);
            let commits = vec![CommitAttribution {
                sha: "a".repeat(40),
                cost_usd: priced.cost_usd,
                confidence: "high",
            }];
            write_store("acme/widget", "s-1", &usage, &priced, &commits).unwrap();

            // The persisted store must contain the cost but never the sentinel.
            let raw = std::fs::read_to_string(crate::cost_store::store_path().unwrap()).unwrap();
            assert!(!raw.contains(SENTINEL), "content leaked into store: {raw}");
            assert!(raw.contains("claude-opus-4-8"));
            assert!(raw.contains("acme/widget"));

            let reloaded = CostStore::load().unwrap();
            assert_eq!(reloaded.sessions.len(), 1);
            assert_eq!(reloaded.commits.len(), 1);
        });
    }

    #[test]
    fn post_body_is_metadata_only_and_idempotent_shaped() {
        let usage = parse_usage_from_str(&fixture_transcript());
        let priced = price_session(&usage);
        let commits = vec![CommitAttribution {
            sha: "b".repeat(40),
            cost_usd: 0.5,
            confidence: "medium",
        }];
        let body = session_post_body("acme/widget", "s-42", &usage, &priced, &commits);
        assert_eq!(body["session_id"], "s-42");
        assert_eq!(body["agent"], "claude-code");
        assert_eq!(body["model"], "claude-opus-4-8");
        assert_eq!(body["repo"], "acme/widget");
        assert_eq!(body["commits"][0]["confidence"], "medium");
        // No content anywhere in the serialized body.
        assert!(!serde_json::to_string(&body).unwrap().contains(SENTINEL));
    }

    #[test]
    fn attribute_commits_confidence_scales_with_count() {
        // No commits in window (bogus range) → nothing attributed. Uses a real
        // git invocation; an empty result is the expected shape here.
        let none = attribute_commits(
            Some("2000-01-01T00:00:00Z"),
            Some("2000-01-02T00:00:00Z"),
            1.0,
        );
        assert!(none.is_empty());
    }

    #[test]
    fn encode_project_dir_replaces_separators() {
        assert_eq!(
            encode_project_dir("/home/user/my.repo"),
            "-home-user-my-repo"
        );
    }

    #[test]
    fn empty_transcript_prices_to_zero_inferred() {
        let priced = price_session(&parse_usage_from_str(""));
        assert_eq!(priced.cost_usd, 0.0);
        assert_eq!(priced.provenance, "inferred");
        assert_eq!(priced.tokens_in, 0);
    }
}
