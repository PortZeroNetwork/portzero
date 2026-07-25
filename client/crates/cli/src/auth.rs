//! Authentication commands: login, logout, whoami.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::api_client::ApiClient;
use crate::browser::open_browser;

/// Stored auth credentials at ~/.portzero/auth.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub email: String,
    pub token: String,
    pub account_id: String,
    #[serde(default)]
    pub username: String,
}

impl AuthConfig {
    /// Path to the auth config file.
    pub fn path() -> Result<PathBuf> {
        let home = crate::client_home().ok_or_else(|| {
            anyhow::anyhow!(
                "Could not determine your home directory, so PortZero cannot locate \
                 your credentials (~/.portzero/auth.json).\n\n\
                 On Linux and macOS, set the HOME environment variable and try again. \
                 On Windows this means the user profile folder could not be resolved, \
                 which usually indicates a damaged profile or a service account with \
                 no profile loaded."
            )
        })?;
        Ok(home.join(".portzero").join("auth.json"))
    }

    /// Load auth config from disk.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            anyhow::bail!("Not logged in. Run `portzero login` to authenticate.");
        }

        let content = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read auth config at {}.\n\n\
                 The file may be corrupted. Try `portzero logout` then `portzero login`.",
                path.display()
            )
        })?;

        let config: AuthConfig = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse auth config at {}.\n\n\
                 The file may be corrupted. Try `portzero logout` then `portzero login`.",
                path.display()
            )
        })?;

        Ok(config)
    }

    /// Save auth config to disk.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            portzero_daemon::secure_file::ensure_private_dir(parent).with_context(|| {
                format!(
                    "Failed to create config directory: {}\n\n\
                     Check that you have write permissions to your home directory.",
                    parent.display()
                )
            })?;
        }

        let json = serde_json::to_string_pretty(self).context("Failed to serialize auth config")?;
        portzero_daemon::secure_file::write_secret_atomic(&path, json.as_bytes())
            .with_context(|| format!("Failed to write auth config to {}", path.display()))?;

        Ok(())
    }

    /// Remove auth config from disk.
    pub fn remove() -> Result<()> {
        let path = Self::path()?;
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove auth config at {}", path.display()))?;
        }
        Ok(())
    }
}

/// Request body for POST /auth/login.
#[derive(Serialize)]
struct LoginRequest {
    email: String,
}

/// Response from POST /auth/login.
#[derive(Deserialize)]
struct LoginResponse {
    message: String,
}

/// Request body for POST /auth/verify.
#[derive(Serialize)]
struct VerifyRequest {
    email: String,
    code: String,
}

/// Response from POST /auth/verify.
#[derive(Deserialize)]
struct VerifyResponse {
    token: String,
    account_id: String,
    email: String,
    #[allow(dead_code)]
    name: String,
    #[serde(default)]
    username: String,
}

/// Derive the dashboard URL from the API URL.
///
/// - If `PZ_TUNNEL_DASHBOARD_URL` is set, use it directly.
/// - If `PZ_TUNNEL_API_URL` looks like `localhost:3001`, use `localhost:3003`.
/// - Otherwise default to `https://app.portzero.cloud`.
pub(crate) fn dashboard_url() -> String {
    portzero_domain::endpoints::dashboard_url()
}

/// Generate a random session code for the browser login flow.
fn generate_session_code() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.gen();
    hex::encode(bytes)
}

/// Run the browser-based login flow (default).
///
/// 1. Print a URL containing a fresh, high-entropy session code
/// 2. Try to open it in a local browser (works when there is one)
/// 3. Poll the API for that code to be completed, regardless of whether the
///    browser opened here or the URL was opened on a different device
/// 4. Save credentials and exit
///
/// Deliberately does NOT bind a local callback port: earlier versions did,
/// and had the dashboard page's JS POST directly to
/// `http://localhost:<port>`. That only works when the browser completing
/// login and the CLI waiting for it share a loopback interface — never true
/// when the CLI runs somewhere remote (e.g. a Claude Code cloud session),
/// even if the URL is opened in a browser on a different machine. Polling
/// the API instead means the URL can be opened anywhere.
async fn login_browser() -> Result<()> {
    let code = generate_session_code();
    let dash_url = dashboard_url();
    let auth_url = format!("{dash_url}/#/auth/cli?code={code}");

    let browser_opened = open_browser(&auth_url);

    if browser_opened {
        println!("Opening browser for authentication...");
    } else {
        println!("Could not open a browser automatically.");
    }
    println!();
    println!("Visit this URL on any device to log in:");
    println!();
    println!("  {auth_url}");
    println!();
    println!("Waiting for login... (timeout: 10 minutes)");

    let result = tokio::time::timeout(Duration::from_secs(600), poll_for_cli_login(&code)).await;

    match result {
        Ok(Ok(config)) => {
            config.save()?;
            println!();
            println!("Logged in as {}.", config.email);
            println!("Credentials saved to ~/.portzero/auth.json");
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            anyhow::bail!(
                "Login timed out after 10 minutes.\n\n\
                 The login link was not completed in time. Try again with:\n\
                 \n\
                   portzero login\n\
                 \n\
                 If you don't have access to a browser at all, use:\n\
                 \n\
                   portzero login --interactive"
            );
        }
    }
}

/// Response from GET /auth/cli/poll/:code.
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CliPollResponse {
    Pending,
    Completed {
        token: String,
        account_id: String,
        email: String,
        #[serde(default)]
        username: String,
    },
}

/// Poll `GET /auth/cli/poll/:code` every 2 seconds until the dashboard has
/// linked this code to an account.
async fn poll_for_cli_login(code: &str) -> Result<AuthConfig> {
    let client = ApiClient::new();

    loop {
        let resp = client.get(&format!("/auth/cli/poll/{code}")).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to check login status (HTTP {status}).\n\n{body}");
        }

        let poll: CliPollResponse = resp.json().await.with_context(|| {
            "Received an unexpected response from the server while polling for login."
        })?;

        if let CliPollResponse::Completed {
            token,
            account_id,
            email,
            username,
        } = poll
        {
            return Ok(AuthConfig {
                email,
                token,
                account_id,
                username,
            });
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Run the interactive (terminal) login flow.
///
/// This is the fallback for headless servers where a browser is not available.
async fn login_interactive(email: Option<String>) -> Result<()> {
    let email = match email {
        Some(e) => e,
        None => {
            print!("Email: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                anyhow::bail!("Email cannot be empty.");
            }
            trimmed
        }
    };

    println!("Sending verification code to {}...", email);

    let client = ApiClient::new();
    let resp = client
        .post(
            "/auth/login",
            &LoginRequest {
                email: email.clone(),
            },
        )
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to send verification code (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             If you believe this is an error, visit {}/support for help.",
            portzero_domain::endpoints::web_url()
        );
    }

    let login_resp: LoginResponse = resp.json().await.with_context(|| {
        "Received an unexpected response from the server.\n\n\
         This may indicate an API version mismatch. Try updating portzero:\n\
         curl -fsSL https://portzero.net/install.sh | sh"
    })?;

    println!("{}", login_resp.message);
    println!();
    print!("Enter verification code: ");
    io::stdout().flush()?;
    let mut code_input = String::new();
    io::stdin().read_line(&mut code_input)?;
    let code = code_input.trim().to_string();

    if code.is_empty() {
        anyhow::bail!("Verification code cannot be empty.");
    }

    println!("Verifying...");

    let verify_resp = client
        .post(
            "/auth/verify",
            &VerifyRequest {
                email: email.clone(),
                code,
            },
        )
        .await?;

    if !verify_resp.status().is_success() {
        let status = verify_resp.status();
        let body = verify_resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Verification failed (HTTP {status}).\n\n\
             Server response: {body}\n\n\
             The code may have expired. Run `portzero login --interactive` to try again."
        );
    }

    let verified: VerifyResponse = verify_resp
        .json()
        .await
        .with_context(|| "Received an unexpected response from the server during verification.")?;

    let config = AuthConfig {
        email: verified.email.clone(),
        token: verified.token,
        account_id: verified.account_id,
        username: verified.username,
    };
    config.save()?;

    println!("Logged in as {}.", verified.email);
    println!("Credentials saved to ~/.portzero/auth.json");

    Ok(())
}

/// Run the login flow.
///
/// By default opens a browser for authentication. Use `--interactive` to fall
/// back to terminal-based email+code flow (useful on headless servers).
pub async fn login(interactive: bool, email: Option<String>, _name: Option<String>) -> Result<()> {
    if interactive {
        login_interactive(email).await
    } else {
        login_browser().await
    }
}

/// Remove stored credentials.
pub fn logout() -> Result<()> {
    AuthConfig::remove()?;
    println!("Logged out. Credentials removed (~/.portzero/auth.json).");
    println!(
        "To also erase local cost metadata, observations, logs, and daemon \
         state, run `portzero purge`."
    );
    Ok(())
}

/// Response from GET /auth/me.
#[derive(Deserialize)]
struct WhoamiResponse {
    email: String,
    name: Option<String>,
    plan: Option<String>,
    team: Option<String>,
}

/// Show current user info.
pub async fn whoami() -> Result<()> {
    let client = ApiClient::new();
    client.require_auth()?;

    let resp = client.get("/auth/me").await?;

    if !resp.status().is_success() {
        let status = resp.status();
        if status.as_u16() == 401 {
            anyhow::bail!(
                "Authentication expired or invalid.\n\n\
                 Run `portzero logout` then `portzero login` to re-authenticate."
            );
        }
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to fetch account info (HTTP {status}).\n\nServer response: {body}");
    }

    let info: WhoamiResponse = resp
        .json()
        .await
        .with_context(|| "Failed to parse account info from server response.")?;

    println!("Email: {}", info.email);
    if let Some(name) = &info.name {
        println!("Name:  {name}");
    }
    if let Some(plan) = &info.plan {
        println!("Plan:  {plan}");
    }
    if let Some(team) = &info.team {
        println!("Team:  {team}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    // Dashboard-URL derivation is centralised in and tested by
    // `portzero_domain::endpoints`.
    use portzero_domain::endpoints::dashboard_url_from_api_url;

    #[test]
    fn dashboard_url_tracks_the_app_domain_for_the_production_api() {
        assert_eq!(
            dashboard_url_from_api_url("https://app.portzero.net/api"),
            "https://app.portzero.net",
        );
    }

    #[test]
    fn dashboard_url_rewrites_local_api_ports_for_dev() {
        assert_eq!(
            dashboard_url_from_api_url("http://127.0.0.1:3001"),
            "http://127.0.0.1:3003",
        );
    }
}
