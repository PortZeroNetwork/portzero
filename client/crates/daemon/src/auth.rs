//! Auth token management for cloud connectivity.
//!
//! The daemon reads an auth token from `~/.portzero/auth.json`. If no
//! token is present, the daemon runs in local-only mode: it still discovers
//! routes and writes `routes.json`, but does not connect to the cloud edge.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use portzero_domain::endpoints;

/// Stored authentication configuration (written by the CLI after login).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthFile {
    #[serde(default)]
    email: Option<String>,
    token: Option<String>,
    account_id: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

/// Credentials returned by the cloud dashboard's browser login callback.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BrowserLoginCredentials {
    pub email: String,
    pub token: String,
    pub account_id: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Deserialize)]
struct BrowserLoginCallback {
    code: String,
    email: String,
    token: String,
    account_id: String,
    #[serde(default)]
    username: String,
}

/// A started browser-login session. Open `auth_url` in a browser to continue.
#[derive(Debug, Clone)]
pub struct BrowserLoginSession {
    pub auth_url: String,
    pub callback_port: u16,
}

/// Runtime authentication state.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Bearer token, if present.
    pub token: Option<String>,
    /// Account UUID from the auth server — used for `{uid}` template substitution.
    pub account_id: Option<String>,
    /// Cloud username — used for `{cloud-username}` template substitution.
    pub username: Option<String>,
    /// Directory where auth.json lives (defaults to `~/.portzero/`).
    pub config_dir: PathBuf,
}

/// Decode the `exp` claim from a JWT payload without verifying the signature.
/// Returns `None` if the token is malformed, the payload can't be decoded,
/// or the `exp` field is missing.
fn decode_jwt_exp(token: &str) -> Option<u64> {
    let payload_b64 = token.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload.get("exp")?.as_u64()
}

fn generate_session_code() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Start the same browser login callback flow used by `portzero login`.
///
/// The returned URL should be opened or redirected to by the caller. A background
/// task waits up to two minutes for the dashboard callback, then saves
/// `~/.portzero/auth.json`.
pub async fn start_browser_login_session() -> Result<BrowserLoginSession> {
    let listener = TcpListener::bind("127.0.0.1:0").await.with_context(|| {
        "Failed to bind a local TCP listener for the login callback.\n\n\
         Ensure that localhost networking is available."
    })?;
    let callback_port = listener.local_addr()?.port();
    let code = generate_session_code();
    let auth_url = format!(
        "{}/#/auth/cli?port={callback_port}&code={code}",
        endpoints::dashboard_url()
    );

    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            accept_browser_login_callback(listener, &code).await
        })
        .await;

        match result {
            Ok(Ok(credentials)) => {
                if let Err(error) = save_browser_login_credentials(&credentials) {
                    tracing::warn!(?error, "failed to save browser login credentials");
                } else {
                    tracing::info!(
                        email = credentials.email,
                        "browser login completed and credentials saved"
                    );
                }
            }
            Ok(Err(error)) => tracing::warn!(?error, "browser login failed"),
            Err(_) => tracing::warn!("browser login timed out"),
        }
    });

    Ok(BrowserLoginSession {
        auth_url,
        callback_port,
    })
}

pub fn save_browser_login_credentials(credentials: &BrowserLoginCredentials) -> Result<()> {
    let config_dir = dirs::home_dir()
        .map(|h| h.join(".portzero"))
        .unwrap_or_else(|| PathBuf::from(".portzero"));
    crate::secure_file::ensure_private_dir(&config_dir)?;

    let auth_file = AuthFile {
        email: Some(credentials.email.clone()),
        token: Some(credentials.token.clone()),
        account_id: Some(credentials.account_id.clone()),
        username: Some(credentials.username.clone()).filter(|value| !value.is_empty()),
    };
    let json = serde_json::to_string_pretty(&auth_file)?;
    let auth_path = config_dir.join("auth.json");
    crate::secure_file::write_secret_atomic(&auth_path, json.as_bytes())?;

    Ok(())
}

async fn accept_browser_login_callback(
    listener: TcpListener,
    expected_code: &str,
) -> Result<BrowserLoginCredentials> {
    loop {
        let (mut stream, _addr) = listener.accept().await?;

        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");

        if first_line.starts_with("OPTIONS ") {
            let response = "HTTP/1.1 204 No Content\r\n\
Access-Control-Allow-Origin: *\r\n\
Access-Control-Allow-Methods: POST, OPTIONS\r\n\
Access-Control-Allow-Headers: Content-Type\r\n\
Access-Control-Max-Age: 86400\r\n\
Content-Length: 0\r\n\
\r\n";
            stream.write_all(response.as_bytes()).await.ok();
            stream.flush().await.ok();
            continue;
        }

        if !first_line.starts_with("POST /callback") {
            let response = "HTTP/1.1 404 Not Found\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: text/plain\r\n\
Content-Length: 9\r\n\
\r\n\
Not Found";
            stream.write_all(response.as_bytes()).await.ok();
            stream.flush().await.ok();
            continue;
        }

        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .or_else(|| request.split("\n\n").nth(1))
            .unwrap_or("");

        let payload: BrowserLoginCallback = match serde_json::from_str(body) {
            Ok(payload) => payload,
            Err(error) => {
                write_callback_error(&mut stream, 400, &format!("Invalid request body: {error}"))
                    .await;
                continue;
            }
        };

        if payload.code != expected_code {
            write_callback_error(
                &mut stream,
                403,
                "Session code mismatch. This request may be from a stale login attempt.",
            )
            .await;
            continue;
        }

        let success_body = r#"{"ok":true}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
\r\n\
{}",
            success_body.len(),
            success_body
        );
        stream.write_all(response.as_bytes()).await.ok();
        stream.flush().await.ok();

        return Ok(BrowserLoginCredentials {
            email: payload.email,
            token: payload.token,
            account_id: payload.account_id,
            username: payload.username,
        });
    }
}

async fn write_callback_error(stream: &mut TcpStream, status: u16, message: &str) {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
Access-Control-Allow-Origin: *\r\n\
Content-Type: text/plain\r\n\
Content-Length: {}\r\n\
\r\n\
{}",
        message.len(),
        message
    );
    stream.write_all(response.as_bytes()).await.ok();
    stream.flush().await.ok();
}

impl AuthConfig {
    /// Load auth config from disk.
    ///
    /// Reads `~/.portzero/auth.json`. Returns an unauthenticated config
    /// (token = None) if the file is missing or unreadable.
    pub fn load() -> Self {
        let config_dir = dirs::home_dir()
            .map(|h| h.join(".portzero"))
            .unwrap_or_else(|| PathBuf::from(".portzero"));

        Self::load_from(&config_dir)
    }

    /// Load auth config from a specific directory.
    pub fn load_from(config_dir: &Path) -> Self {
        let auth_path = config_dir.join("auth.json");

        let (token, account_id, username) = std::fs::read_to_string(&auth_path)
            .ok()
            .and_then(|content| serde_json::from_str::<AuthFile>(&content).ok())
            .map(|f| {
                let tok = f.token.filter(|t| !t.is_empty());
                let aid = f.account_id.filter(|a| !a.is_empty());
                let uname = f.username.filter(|u| !u.is_empty());
                (tok, aid, uname)
            })
            .unwrap_or((None, None, None));

        Self {
            token,
            account_id,
            username,
            config_dir: config_dir.to_path_buf(),
        }
    }

    /// Save an auth token to disk.
    pub fn save_token(&mut self, token: &str) -> Result<()> {
        crate::secure_file::ensure_private_dir(&self.config_dir)?;
        let auth_file = AuthFile {
            email: None,
            token: Some(token.to_string()),
            account_id: self.account_id.clone(),
            username: self.username.clone(),
        };
        let json = serde_json::to_string_pretty(&auth_file)?;
        crate::secure_file::write_secret_atomic(
            &self.config_dir.join("auth.json"),
            json.as_bytes(),
        )?;
        self.token = Some(token.to_string());
        Ok(())
    }

    /// Whether the daemon has a valid auth token for cloud connectivity.
    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    /// Check if the stored JWT is near expiry (within `threshold` seconds).
    /// Returns `Some(true)` if near expiry, `Some(false)` if far from expiry,
    /// `None` if no token is stored or the token can't be decoded.
    pub fn is_token_near_expiry(&self, threshold_secs: u64) -> Option<bool> {
        let token = self.token.as_ref()?;
        let exp = decode_jwt_exp(token)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(exp.saturating_sub(now) <= threshold_secs)
    }

    /// Refresh the JWT by calling the cloud API's /auth/refresh endpoint.
    /// Updates auth.json on success.
    pub async fn refresh_token(&mut self) -> Result<()> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No token to refresh"))?;

        let api_url = endpoints::api_url();

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/auth/refresh", api_url))
            .bearer_auth(token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("Failed to reach the API for token refresh")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Token refresh failed (HTTP {status}): {body}");
        }

        let response: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse refresh response")?;
        let new_token = response["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Refresh response missing 'token' field"))?
            .to_string();

        self.save_token(&new_token)?;
        tracing::info!("JWT token refreshed successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_missing_file() {
        let config = AuthConfig::load_from(&PathBuf::from("/nonexistent/path"));
        assert!(!config.is_authenticated());
        assert!(config.token.is_none());
    }

    #[test]
    fn test_load_valid_token() {
        let dir = std::env::temp_dir().join(format!("portzero-auth-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let auth_file = r#"{"token": "tok_test123"}"#;
        std::fs::write(dir.join("auth.json"), auth_file).unwrap();

        let config = AuthConfig::load_from(&dir);
        assert!(config.is_authenticated());
        assert_eq!(config.token.as_deref(), Some("tok_test123"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_empty_token() {
        let dir =
            std::env::temp_dir().join(format!("portzero-auth-test-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let auth_file = r#"{"token": ""}"#;
        std::fs::write(dir.join("auth.json"), auth_file).unwrap();

        let config = AuthConfig::load_from(&dir);
        assert!(!config.is_authenticated());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_null_token() {
        let dir =
            std::env::temp_dir().join(format!("portzero-auth-test-null-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let auth_file = r#"{"token": null}"#;
        std::fs::write(dir.join("auth.json"), auth_file).unwrap();

        let config = AuthConfig::load_from(&dir);
        assert!(!config.is_authenticated());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_save_and_reload() {
        let dir =
            std::env::temp_dir().join(format!("portzero-auth-test-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut config = AuthConfig {
            token: None,
            account_id: None,
            username: None,
            config_dir: dir.clone(),
        };

        config.save_token("tok_new").unwrap();
        assert!(config.is_authenticated());

        let reloaded = AuthConfig::load_from(&dir);
        assert_eq!(reloaded.token.as_deref(), Some("tok_new"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_decode_jwt_exp_valid() {
        // A JWT with exp=2000000000 (roughly 2033)
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhY2N0XzEiLCJleHAiOjIwMDAwMDAwMDB9.dummy";
        let exp = decode_jwt_exp(token);
        assert_eq!(exp, Some(2_000_000_000));
    }

    #[test]
    fn test_decode_jwt_exp_missing_field() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhY2N0XzEifQ.dummy";
        let exp = decode_jwt_exp(token);
        assert_eq!(exp, None);
    }

    #[test]
    fn test_decode_jwt_exp_malformed() {
        let token = "not.a.jwt";
        let exp = decode_jwt_exp(token);
        assert_eq!(exp, None);
    }

    #[test]
    fn test_is_token_near_expiry_far() {
        let config = AuthConfig {
            token: Some("eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjk5OTk5OTk5OTk5fQ.dummy".to_string()),
            account_id: None,
            username: None,
            config_dir: PathBuf::from("/tmp"),
        };
        // exp far in future -> not near expiry
        assert_eq!(config.is_token_near_expiry(3600), Some(false));
    }

    #[test]
    fn test_is_token_near_expiry_no_token() {
        let config = AuthConfig {
            token: None,
            account_id: None,
            username: None,
            config_dir: PathBuf::from("/tmp"),
        };
        assert_eq!(config.is_token_near_expiry(3600), None);
    }

    #[test]
    fn test_default_api_url_matches_the_dashboard_api() {
        assert_eq!(endpoints::DEFAULT_API_URL, "https://app.portzero.net/api");
    }
}
