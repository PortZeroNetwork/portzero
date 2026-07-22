//! `portzero login --github-repo`: autonomous cloud auth for CI jobs and
//! agent sandboxes by proving push access to a GitHub repository.
//!
//! Flow:
//! 1. `POST /auth/github-repo/start` with the repository and team slug to get
//!    a one-time nonce and proof ref.
//! 2. Push the current HEAD to that ref using ambient git credentials —
//!    first the plain ref `refs/portzero/auth/<nonce>`, then the branch
//!    `refs/heads/portzero/auth-<nonce>` as a fallback for sandbox git
//!    proxies that only allow branch pushes.
//! 3. `POST /auth/github-repo/exchange` to trade the proven nonce for an API
//!    token, then save it like any other login.
//! 4. Best-effort: delete the proof ref again.
//!
//! Entirely non-interactive; never prompts.

use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api_client::ApiClient;
use crate::auth::AuthConfig;

/// Email recorded in the saved credentials for repo-proof logins.
const PROOF_EMAIL: &str = "github-ci@ci.portzero.cloud";
/// Account id recorded in the saved credentials for repo-proof logins.
const PROOF_ACCOUNT_ID: &str = "github-repo-proof";
/// Token lifetime requested from the exchange endpoint, in seconds.
const TOKEN_TTL_SECS: u64 = 3600;

/// Request body for POST /auth/github-repo/start.
#[derive(Serialize)]
pub(crate) struct StartRequest {
    pub repository: String,
    pub team: String,
}

/// Response from POST /auth/github-repo/start.
#[derive(Deserialize)]
pub(crate) struct StartResponse {
    pub nonce: String,
    /// The plain proof ref the server expects (`refs/portzero/auth/<nonce>`).
    #[serde(rename = "ref")]
    #[allow(dead_code)]
    pub proof_ref: String,
}

/// Request body for POST /auth/github-repo/exchange.
#[derive(Serialize)]
pub(crate) struct ExchangeRequest {
    pub nonce: String,
    pub ttl_secs: u64,
}

/// Response from POST /auth/github-repo/exchange (same shape as the OIDC
/// exchange).
#[derive(Deserialize)]
pub(crate) struct ExchangeResponse {
    pub token: String,
    pub expires_in: u64,
    pub team_slug: String,
    #[serde(default)]
    pub allowed_templates: Vec<String>,
}

/// Parse a GitHub remote URL into `owner/repo`.
///
/// Supports `https://github.com/owner/repo(.git)`,
/// `git@github.com:owner/repo(.git)`, and `ssh://git@github.com/owner/repo`.
/// Returns `None` for non-GitHub remotes or malformed URLs.
pub(crate) fn parse_github_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let rest = rest.trim_end_matches('/');

    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Run `git` with the given args in the current directory, capturing output.
fn git(args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .output()
        .context("Failed to run `git`. Is git installed and on your PATH?")
}

/// Determine `owner/repo` from the `origin` remote of the current directory.
fn detect_repository() -> Result<String> {
    let out = git(&["remote", "get-url", "origin"])?;
    if !out.status.success() {
        anyhow::bail!(
            "Could not read the `origin` remote in the current directory.\n\n\
             git said: {}\n\
             `portzero login --github-repo` needs to know which GitHub repository\n\
             to prove push access to. Either run it from inside a git clone with a\n\
             GitHub `origin` remote, or pass the repository explicitly:\n\
             \n\
               portzero login --github-repo --team <slug> --repo owner/repo",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_github_remote(&url).ok_or_else(|| {
        anyhow::anyhow!(
            "The `origin` remote ({url}) does not look like a GitHub repository.\n\n\
             Supported forms: https://github.com/owner/repo(.git) and\n\
             git@github.com:owner/repo(.git).\n\
             Pass the repository explicitly instead:\n\
             \n\
               portzero login --github-repo --team <slug> --repo owner/repo"
        )
    })
}

/// Which proof ref we managed to push (needed again for cleanup).
enum PushedRef {
    Plain(String),
    Branch(String),
}

impl PushedRef {
    fn refspec(&self) -> String {
        match self {
            PushedRef::Plain(nonce) => format!("refs/portzero/auth/{nonce}"),
            PushedRef::Branch(nonce) => format!("refs/heads/portzero/auth-{nonce}"),
        }
    }
}

/// Push HEAD to the proof ref, falling back to a branch ref for restricted
/// git proxies that only allow branch pushes.
fn push_proof(nonce: &str) -> Result<PushedRef> {
    let plain = PushedRef::Plain(nonce.to_string());
    let plain_spec = format!("HEAD:{}", plain.refspec());
    let plain_out = git(&["push", "origin", &plain_spec])?;
    if plain_out.status.success() {
        return Ok(plain);
    }

    let branch = PushedRef::Branch(nonce.to_string());
    let branch_spec = format!("HEAD:{}", branch.refspec());
    let branch_out = git(&["push", "origin", &branch_spec])?;
    if branch_out.status.success() {
        return Ok(branch);
    }

    anyhow::bail!(
        "Could not push the proof ref to `origin`.\n\n\
         Attempted:\n\
         \n\
           git push origin {plain_spec}\n\
             {}\n\
           git push origin {branch_spec}\n\
             {}\n\
         \n\
         This login method proves your identity by pushing a short-lived ref,\n\
         so it requires push access to the repository with the git credentials\n\
         available in this environment. If this sandbox cannot push, fall back\n\
         to the device flow instead:\n\
         \n\
           portzero login",
        String::from_utf8_lossy(&plain_out.stderr).trim(),
        String::from_utf8_lossy(&branch_out.stderr).trim(),
    )
}

/// Best-effort: delete the proof ref from `origin`. Failures are ignored —
/// the nonce expires server-side regardless.
fn cleanup_proof(pushed: &PushedRef) {
    let spec = format!(":{}", pushed.refspec());
    let _ = git(&["push", "origin", &spec]);
}

/// Call POST /auth/github-repo/start.
async fn start(client: &ApiClient, repository: &str, team: &str) -> Result<StartResponse> {
    let resp = client
        .post(
            "/auth/github-repo/start",
            &StartRequest {
                repository: repository.to_string(),
                team: team.to_string(),
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Could not start GitHub repo authentication for {repository} \
             (team `{team}`, HTTP {status}).\n\n\
             Server response: {body}\n\n\
             Common causes:\n\
             - The team slug is wrong — check your teams at {dash}/teams\n\
             - The team has no trust rule for {repository} — add one in the\n\
               team's settings on {dash}\n\
             - The Port Zero GitHub App is not installed on the repository or\n\
               not linked to the team — install it on {repository} and link it\n\
               to `{team}` in the team's settings on {dash}",
            dash = portzero_domain::endpoints::dashboard_url(),
        );
    }

    resp.json()
        .await
        .context("Received an unexpected response from /auth/github-repo/start.")
}

/// Call POST /auth/github-repo/exchange.
async fn exchange(client: &ApiClient, nonce: &str) -> Result<ExchangeResponse> {
    let resp = client
        .post(
            "/auth/github-repo/exchange",
            &ExchangeRequest {
                nonce: nonce.to_string(),
                ttl_secs: TOKEN_TTL_SECS,
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "The server did not accept the pushed proof ref (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             The nonce is valid for about 10 minutes — if the push took longer,\n\
             simply re-run `portzero login --github-repo --team <slug>`. If it\n\
             keeps failing, verify the GitHub App is installed on the repository\n\
             and can see the pushed ref, or fall back to `portzero login`."
        );
    }

    resp.json()
        .await
        .context("Received an unexpected response from /auth/github-repo/exchange.")
}

/// Restart the daemon so it picks up the new credentials — but only when it
/// is already running (agent sandboxes often cannot start it).
async fn restart_daemon_if_running() -> Result<()> {
    use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};

    let config = DaemonConfig::load();
    if read_daemon_pid(&config).is_some() {
        crate::daemon::restart().await?;
    }
    Ok(())
}

/// Run the GitHub repo-proof login flow. Non-interactive: never prompts.
pub async fn run(team: Option<String>, repo: Option<String>) -> Result<()> {
    let Some(team) = team else {
        anyhow::bail!(
            "--github-repo requires --team.\n\n\
             The team slug tells portzero.cloud which team's trust rules to\n\
             check the repository against. Run:\n\
             \n\
               portzero login --github-repo --team <slug>\n\
             \n\
             You can find your team slugs at {}/teams",
            portzero_domain::endpoints::dashboard_url()
        );
    };

    let repository = match repo {
        Some(r) => r,
        None => detect_repository()?,
    };

    println!("Authenticating as team `{team}` by proving push access to {repository}...");

    let client = ApiClient::new();
    let started = start(&client, &repository, &team).await?;

    let pushed = push_proof(&started.nonce)?;
    println!("Pushed proof ref {}.", pushed.refspec());

    // Exchange while the ref still exists, then clean up regardless of outcome.
    let exchanged = exchange(&client, &started.nonce).await;
    cleanup_proof(&pushed);
    let exchanged = exchanged?;

    AuthConfig {
        email: PROOF_EMAIL.to_string(),
        token: exchanged.token.clone(),
        account_id: PROOF_ACCOUNT_ID.to_string(),
        username: String::new(),
    }
    .save()?;

    restart_daemon_if_running().await?;

    println!();
    println!(
        "Logged in to team `{}` via GitHub repo proof.",
        exchanged.team_slug
    );
    if exchanged.allowed_templates.is_empty() {
        println!("Allowed tunnel templates: (none reported)");
    } else {
        println!(
            "Allowed tunnel templates: {}",
            exchanged.allowed_templates.join(", ")
        );
    }
    println!(
        "Token expires in {} seconds. Tunnels you publish must match these templates.",
        exchanged.expires_in
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_remote() {
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_https_remote_with_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_ssh_scp_remote() {
        assert_eq!(
            parse_github_remote("git@github.com:owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn parses_ssh_url_remote() {
        assert_eq!(
            parse_github_remote("ssh://git@github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace_and_trailing_slash() {
        assert_eq!(
            parse_github_remote("  https://github.com/owner/repo/\n"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn rejects_non_github_remotes() {
        assert_eq!(parse_github_remote("https://gitlab.com/owner/repo"), None);
        assert_eq!(
            parse_github_remote("git@bitbucket.org:owner/repo.git"),
            None
        );
    }

    #[test]
    fn rejects_malformed_paths() {
        assert_eq!(parse_github_remote("https://github.com/owner"), None);
        assert_eq!(parse_github_remote("https://github.com/"), None);
        assert_eq!(parse_github_remote("https://github.com//repo"), None);
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo/extra"),
            None
        );
    }

    #[test]
    fn start_request_serializes_to_the_server_contract() {
        let json = serde_json::to_value(StartRequest {
            repository: "owner/repo".into(),
            team: "myteam".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"repository": "owner/repo", "team": "myteam"})
        );
    }

    #[test]
    fn start_response_reads_nonce_and_ref() {
        let resp: StartResponse = serde_json::from_str(
            r#"{"nonce":"abc123","ref":"refs/portzero/auth/abc123","expires_at":"2026-07-22T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(resp.nonce, "abc123");
        assert_eq!(resp.proof_ref, "refs/portzero/auth/abc123");
    }

    #[test]
    fn exchange_request_serializes_to_the_server_contract() {
        let json = serde_json::to_value(ExchangeRequest {
            nonce: "abc".into(),
            ttl_secs: 3600,
        })
        .unwrap();
        assert_eq!(json, serde_json::json!({"nonce": "abc", "ttl_secs": 3600}));
    }

    #[test]
    fn exchange_response_reads_token_team_and_templates() {
        let resp: ExchangeResponse = serde_json::from_str(
            r#"{"token":"t","expires_at":"2026-07-22T01:00:00Z","expires_in":3600,
                "team_slug":"myteam","allowed_templates":["{name}.myteam.tunnel.portzero.cloud"]}"#,
        )
        .unwrap();
        assert_eq!(resp.token, "t");
        assert_eq!(resp.expires_in, 3600);
        assert_eq!(resp.team_slug, "myteam");
        assert_eq!(
            resp.allowed_templates,
            vec!["{name}.myteam.tunnel.portzero.cloud".to_string()]
        );
    }

    #[test]
    fn exchange_response_defaults_missing_templates_to_empty() {
        let resp: ExchangeResponse =
            serde_json::from_str(r#"{"token":"t","expires_in":60,"team_slug":"s"}"#).unwrap();
        assert!(resp.allowed_templates.is_empty());
    }

    #[test]
    fn proof_refspecs_match_the_server_contract() {
        assert_eq!(
            PushedRef::Plain("n0".into()).refspec(),
            "refs/portzero/auth/n0"
        );
        assert_eq!(
            PushedRef::Branch("n0".into()).refspec(),
            "refs/heads/portzero/auth-n0"
        );
    }
}
