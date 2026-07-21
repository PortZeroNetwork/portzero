//! `portzero review`: upload a review record (branch commits + diff) to
//! portzero.cloud so reviewer feedback pinned on your tunneled app can be
//! tied to the code that changed.
//!
//! A review record captures the current branch relative to a base ref:
//! the commit list, the unified diff, and the tunnel domain where the live
//! app is running. The cloud scans commit messages for `Fixes PZ-<n>`
//! references and advances the matching feedback threads to `fix_proposed`.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

use portzero_daemon::discovery::is_local_overlay_domain;
use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_domain::{sanitize_for_dns, DomainContext};

use crate::api_client::ApiClient;
use crate::auth::dashboard_url;
use crate::browser::open_browser;
use crate::export::{discovered_tunnels, TunnelUrl};

/// Maximum accepted unified diff size in bytes. The cloud rejects anything
/// larger with 413, so fail fast locally with a better message.
const MAX_DIFF_BYTES: usize = 5_000_000;

/// One commit on the branch under review, as sent to the cloud.
#[derive(Debug, PartialEq, serde::Serialize)]
struct ReviewCommit {
    sha: String,
    author: String,
    timestamp: String,
    message: String,
}

/// A feedback thread the cloud advanced to `fix_proposed` because a commit
/// message on this branch referenced it (`Fixes PZ-<n>`).
#[derive(Debug, Deserialize)]
struct ThreadAdvanced {
    #[serde(default)]
    #[allow(dead_code)]
    thread_id: String,
    #[serde(rename = "ref")]
    thread_ref: String,
    #[serde(default)]
    fix_commit: String,
}

/// Response from POST /review-records/.
#[derive(Debug, Deserialize)]
struct ReviewRecordResponse {
    id: String,
    branch: String,
    #[serde(default)]
    threads_advanced: Vec<ThreadAdvanced>,
}

/// `portzero review [--base <ref>] [--domain <domain>] [--project <name>] [--open]`
pub async fn run(
    base: Option<String>,
    domain: Option<String>,
    project: Option<String>,
    open: bool,
) -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let cwd = std::env::current_dir().context("Failed to determine the current directory")?;

    let base_ref = match base {
        Some(b) => b,
        None => detect_default_base(&cwd),
    };

    let (project_name, branch) = detect_project_and_branch(&cwd, project)?;

    let base_sha = git_output(&cwd, &["merge-base", "HEAD", &base_ref]).with_context(|| {
        format!(
            "Failed to find a merge base between HEAD and '{base_ref}'.\n\n\
             Pass an explicit base ref with `portzero review --base <ref>`."
        )
    })?;
    let head_sha = git_output(&cwd, &["rev-parse", "HEAD"])
        .context("Failed to resolve HEAD. Is this a git repository?")?;

    let range = format!("{base_sha}..HEAD");
    let log = git_output(
        &cwd,
        &[
            "log",
            "--reverse",
            "--format=%H%x1f%an%x1f%aI%x1f%s",
            &range,
        ],
    )
    .context("Failed to list commits for the branch under review")?;
    let commits = parse_commit_log(&log);

    let diff =
        git_output_raw(&cwd, &["diff", &range]).context("Failed to compute the branch diff")?;
    check_diff(&diff, commits.len())?;

    let domain = match domain {
        Some(d) => d,
        None => {
            let config = DaemonConfig::load();
            let tunnels = discovered_tunnels(&config);
            pick_domain(&tunnels, &project_name, &branch)?
        }
    };

    let body = json!({
        "project": project_name,
        "branch": branch,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "head_sha": head_sha,
        "domain": domain,
        "commits": commits,
        "diff": diff,
    });

    let resp = client.post("/review-records/", &body).await?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            401 => bail!(
                "Authentication expired or invalid.\n\n\
                 Run `portzero logout` then `portzero login` to re-authenticate."
            ),
            402 => bail!(
                "Your plan does not include review records (HTTP 402).\n\n\
                 Server response: {text}\n\n\
                 Upgrade your plan at {}/#/billing to upload review records.",
                dashboard_url()
            ),
            _ => {
                bail!("Failed to upload review record (HTTP {status}).\n\nServer response: {text}")
            }
        }
    }

    let record: ReviewRecordResponse = resp
        .json()
        .await
        .context("Failed to parse the review record from the server response.")?;

    println!("Review record {} uploaded.", record.id);
    println!("  branch:  {} (base {base_ref})", record.branch);
    println!(
        "  commits: {} commit(s), diff {} bytes",
        commits.len(),
        diff.len()
    );
    println!("  domain:  {domain}");
    for advanced in &record.threads_advanced {
        println!(
            "  {}: fix proposed ({})",
            format_thread_ref(&advanced.thread_ref),
            short_sha(&advanced.fix_commit)
        );
    }

    let url = review_record_url(&dashboard_url(), &record.id);
    println!();
    println!("View it at: {url}");
    if open {
        open_browser(&url);
    }

    Ok(())
}

/// Run `git -C <cwd> <args…>` and return trimmed stdout, or a descriptive error.
fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    Ok(git_output_raw(cwd, args)?.trim().to_string())
}

/// Run `git -C <cwd> <args…>` and return stdout untouched (diffs are
/// byte-sensitive), or a descriptive error including git's stderr.
fn git_output_raw(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run `git {}`. Is git installed?", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`git {}` failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Base ref when `--base` is not given: origin's default branch
/// (`refs/remotes/origin/HEAD`, with the `origin/` prefix stripped), falling
/// back to `main`.
fn detect_default_base(cwd: &Path) -> String {
    match git_output(
        cwd,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        Ok(sym) if !sym.is_empty() => strip_origin(&sym).to_string(),
        _ => "main".to_string(),
    }
}

/// Strip a leading `origin/` remote prefix from a symbolic ref like
/// `origin/main`.
fn strip_origin(symbolic_ref: &str) -> &str {
    symbolic_ref.strip_prefix("origin/").unwrap_or(symbolic_ref)
}

/// Resolve the project name and branch, preferring `DomainContext` detection
/// (which understands worktrees and reads `.git` directly) with git-command
/// fallbacks.
fn detect_project_and_branch(cwd: &Path, project: Option<String>) -> Result<(String, String)> {
    let ctx = DomainContext::from_environment("review", cwd, None, None);

    let project_name = match project {
        Some(p) => p,
        None if ctx.project != "unknown" => ctx.project.clone(),
        None => {
            let toplevel = git_output(cwd, &["rev-parse", "--show-toplevel"])
                .context("Failed to detect the project name. Is this a git repository?")?;
            Path::new(&toplevel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_string())
        }
    };

    let branch = if ctx.branch != "unknown" {
        ctx.branch
    } else {
        git_output(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
            .context("Failed to detect the current branch. Is this a git repository?")?
    };

    Ok((project_name, branch))
}

/// Parse `git log --format=%H%x1f%an%x1f%aI%x1f%s` output (one commit per
/// line, fields separated by the ASCII unit separator 0x1f).
fn parse_commit_log(log: &str) -> Vec<ReviewCommit> {
    log.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\u{1f}');
            let sha = fields.next()?.to_string();
            let author = fields.next()?.to_string();
            let timestamp = fields.next()?.to_string();
            let message = fields.next().unwrap_or("").to_string();
            Some(ReviewCommit {
                sha,
                author,
                timestamp,
                message,
            })
        })
        .collect()
}

/// Guard the diff: bail when it exceeds the cloud's 5 MB limit, or when
/// there is nothing to review at all.
fn check_diff(diff: &str, commit_count: usize) -> Result<()> {
    if diff.len() > MAX_DIFF_BYTES {
        bail!(
            "The branch diff is {} bytes, which exceeds the {MAX_DIFF_BYTES}-byte limit for a \
             review record.\n\n\
             Upload a smaller branch: split the work, or pass a nearer base with \
             `portzero review --base <ref>`.",
            diff.len()
        );
    }
    if diff.trim().is_empty() && commit_count == 0 {
        bail!(
            "Nothing to review: no commits and an empty diff against the base ref.\n\n\
             Commit your changes first, or pass a different base with \
             `portzero review --base <ref>`."
        );
    }
    Ok(())
}

/// Pick the cloud tunnel domain hosting the live app for this review.
///
/// - Prefer a cloud tunnel whose domain contains the DNS-sanitized project or
///   branch token.
/// - Otherwise, if exactly one cloud tunnel exists, use it.
/// - Otherwise bail and ask for `--domain`.
fn pick_domain(tunnels: &[TunnelUrl], project: &str, branch: &str) -> Result<String> {
    let cloud: Vec<&TunnelUrl> = tunnels
        .iter()
        .filter(|t| !is_local_overlay_domain(&t.domain))
        .collect();

    if cloud.is_empty() {
        bail!(
            "No cloud tunnels are currently discovered, so the review domain cannot be \
             auto-detected.\n\n\
             Start the app with a `*.tunnel.portzero.cloud` PZ_TUNNEL, or pass the domain \
             explicitly with `portzero review --domain <domain>`."
        );
    }

    for token in [sanitize_for_dns(project), sanitize_for_dns(branch)] {
        if token.is_empty() {
            continue;
        }
        if let Some(t) = cloud.iter().find(|t| t.domain.contains(&token)) {
            return Ok(t.domain.clone());
        }
    }

    if cloud.len() == 1 {
        return Ok(cloud[0].domain.clone());
    }

    let known: Vec<&str> = cloud.iter().map(|t| t.domain.as_str()).collect();
    bail!(
        "Multiple cloud tunnels are discovered and none matches this project or branch: {}.\n\n\
         Pass the one hosting the app under review with `portzero review --domain <domain>`.",
        known.join(", ")
    );
}

/// Dashboard URL for a review record.
fn review_record_url(dashboard: &str, record_id: &str) -> String {
    format!("{dashboard}/#/review-records/{record_id}")
}

/// Display form of a thread ref: the API sends "PZ-42" already, but guard
/// against a bare number so the output always reads "PZ-<n>".
fn format_thread_ref(thread_ref: &str) -> String {
    if thread_ref.starts_with("PZ-") {
        thread_ref.to_string()
    } else {
        format!("PZ-{thread_ref}")
    }
}

/// First 8 characters of a git SHA (or the whole string when shorter).
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commit_log_parses_unit_separated_fields() {
        let log = "abc123\u{1f}Alice\u{1f}2026-07-01T12:00:00+00:00\u{1f}fix: things\n\
                   def456\u{1f}Bob\u{1f}2026-07-02T13:00:00+00:00\u{1f}feat: stuff (Fixes PZ-7)\n";
        let commits = parse_commit_log(log);
        assert_eq!(commits.len(), 2);
        assert_eq!(
            commits[0],
            ReviewCommit {
                sha: "abc123".to_string(),
                author: "Alice".to_string(),
                timestamp: "2026-07-01T12:00:00+00:00".to_string(),
                message: "fix: things".to_string(),
            }
        );
        assert_eq!(commits[1].message, "feat: stuff (Fixes PZ-7)");
    }

    #[test]
    fn parse_commit_log_empty_input_yields_no_commits() {
        assert!(parse_commit_log("").is_empty());
        assert!(parse_commit_log("\n\n").is_empty());
    }

    #[test]
    fn parse_commit_log_skips_malformed_lines() {
        // A line with fewer than 3 separators cannot be a commit record.
        let log = "only-a-sha\nabc\u{1f}Alice\u{1f}2026-07-01T00:00:00Z\u{1f}msg\n";
        let commits = parse_commit_log(log);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha, "abc");
    }

    #[test]
    fn parse_commit_log_message_keeps_extra_separators() {
        // splitn(4) means a stray 0x1f inside the subject stays in `message`.
        let log = "abc\u{1f}Alice\u{1f}t\u{1f}weird\u{1f}subject";
        let commits = parse_commit_log(log);
        assert_eq!(commits[0].message, "weird\u{1f}subject");
    }

    #[test]
    fn check_diff_rejects_oversized_diff() {
        let big = "a".repeat(MAX_DIFF_BYTES + 1);
        let err = check_diff(&big, 3).unwrap_err();
        assert!(err.to_string().contains("smaller branch"), "got: {err}");
    }

    #[test]
    fn check_diff_accepts_diff_at_the_limit() {
        let at_limit = "a".repeat(MAX_DIFF_BYTES);
        assert!(check_diff(&at_limit, 1).is_ok());
    }

    #[test]
    fn check_diff_rejects_empty_diff_with_no_commits() {
        let err = check_diff("", 0).unwrap_err();
        assert!(err.to_string().contains("Nothing to review"), "got: {err}");
    }

    #[test]
    fn check_diff_allows_empty_diff_when_commits_exist() {
        // e.g. a branch of empty commits, or a revert pair — still reviewable.
        assert!(check_diff("", 2).is_ok());
    }

    #[test]
    fn review_record_url_appends_hash_route() {
        assert_eq!(
            review_record_url("https://app.portzero.cloud", "rec-123"),
            "https://app.portzero.cloud/#/review-records/rec-123"
        );
    }

    #[test]
    fn strip_origin_removes_remote_prefix_only() {
        assert_eq!(strip_origin("origin/main"), "main");
        assert_eq!(strip_origin("origin/release/v2"), "release/v2");
        assert_eq!(strip_origin("main"), "main");
    }

    #[test]
    fn format_thread_ref_never_doubles_the_prefix() {
        assert_eq!(format_thread_ref("PZ-42"), "PZ-42");
        assert_eq!(format_thread_ref("42"), "PZ-42");
    }

    #[test]
    fn short_sha_truncates_to_eight_chars() {
        assert_eq!(short_sha("0123456789abcdef"), "01234567");
        assert_eq!(short_sha("abc"), "abc");
    }

    fn tunnel(domain: &str) -> TunnelUrl {
        TunnelUrl {
            domain: domain.to_string(),
            url: format!("https://{domain}"),
            health_path: None,
        }
    }

    #[test]
    fn pick_domain_prefers_project_token_match() {
        let tunnels = vec![
            tunnel("other.alice.tunnel.portzero.cloud"),
            tunnel("web-myapp-main.alice.tunnel.portzero.cloud"),
        ];
        let picked = pick_domain(&tunnels, "MyApp", "feature/x").unwrap();
        assert_eq!(picked, "web-myapp-main.alice.tunnel.portzero.cloud");
    }

    #[test]
    fn pick_domain_falls_back_to_branch_token_match() {
        let tunnels = vec![
            tunnel("a.alice.tunnel.portzero.cloud"),
            tunnel("web-feat-login.alice.tunnel.portzero.cloud"),
        ];
        let picked = pick_domain(&tunnels, "unrelated", "feat/login").unwrap();
        assert_eq!(picked, "web-feat-login.alice.tunnel.portzero.cloud");
    }

    #[test]
    fn pick_domain_uses_single_cloud_tunnel_when_no_token_matches() {
        let tunnels = vec![
            tunnel("solo.alice.tunnel.portzero.cloud"),
            tunnel("web.portzero.local"), // local: ignored
        ];
        let picked = pick_domain(&tunnels, "proj", "branch").unwrap();
        assert_eq!(picked, "solo.alice.tunnel.portzero.cloud");
    }

    #[test]
    fn pick_domain_bails_when_ambiguous() {
        let tunnels = vec![
            tunnel("a.alice.tunnel.portzero.cloud"),
            tunnel("b.alice.tunnel.portzero.cloud"),
        ];
        let err = pick_domain(&tunnels, "proj", "branch").unwrap_err();
        assert!(err.to_string().contains("--domain"), "got: {err}");
    }

    #[test]
    fn pick_domain_bails_when_only_local_tunnels_exist() {
        let tunnels = vec![tunnel("web.portzero.local")];
        let err = pick_domain(&tunnels, "proj", "branch").unwrap_err();
        assert!(err.to_string().contains("--domain"), "got: {err}");
    }
}
