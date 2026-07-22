//! Happy-path integration test for `portzero login --github-repo`.
//!
//! Runs the real `portzero` binary against a hand-rolled in-process HTTP mock
//! of the two cloud endpoints (`/auth/github-repo/start` and
//! `/auth/github-repo/exchange`), with a fake `git` on PATH that records the
//! pushes it receives. Unix-only: the fake git is a shell script.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const NONCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Create a unique scratch directory for this test run.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pz-github-repo-login-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Install a fake `git` script that answers `remote get-url origin` and
/// records every other invocation (one line of args per call) to `log_path`.
fn install_fake_git(bin_dir: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"remote\" ] && [ \"$2\" = \"get-url\" ]; then\n\
           echo 'https://github.com/acme/widgets.git'\n\
           exit 0\n\
         fi\n\
         echo \"$@\" >> '{}'\n\
         exit 0\n",
        log_path.display()
    );
    let git_path = bin_dir.join("git");
    std::fs::write(&git_path, script).unwrap();
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Read one HTTP request (headers + content-length body) from the stream and
/// return (request-line, body).
fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut tmp).unwrap();
        assert!(n > 0, "client closed connection mid-request");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let request_line = headers.lines().next().unwrap_or_default().to_string();
    let content_length: usize = headers
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp).unwrap();
        assert!(n > 0, "client closed connection mid-body");
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length]).to_string();
    (request_line, body)
}

fn respond_json(stream: &mut std::net::TcpStream, json: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
        json.len()
    );
    stream.write_all(resp.as_bytes()).unwrap();
}

/// Serve the two auth endpoints, asserting the request bodies match the
/// server contract. Returns after both requests were answered.
fn serve_mock_api(listener: TcpListener) {
    // Request 1: start.
    let (mut stream, _) = listener.accept().unwrap();
    let (line, body) = read_request(&mut stream);
    assert!(
        line.starts_with("POST /api/auth/github-repo/start "),
        "unexpected first request: {line}"
    );
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["repository"], "acme/widgets");
    assert_eq!(body["team"], "myteam");
    respond_json(
        &mut stream,
        &format!(
            r#"{{"nonce":"{NONCE}","ref":"refs/portzero/auth/{NONCE}","expires_at":"2099-01-01T00:00:00Z"}}"#
        ),
    );

    // Request 2: exchange.
    let (mut stream, _) = listener.accept().unwrap();
    let (line, body) = read_request(&mut stream);
    assert!(
        line.starts_with("POST /api/auth/github-repo/exchange "),
        "unexpected second request: {line}"
    );
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["nonce"], NONCE);
    assert_eq!(body["ttl_secs"], 3600);
    respond_json(
        &mut stream,
        r#"{"token":"test-token","expires_at":"2099-01-01T01:00:00Z","expires_in":3600,"team_slug":"myteam","allowed_templates":["{name}.myteam.tunnel.portzero.cloud"]}"#,
    );
}

#[test]
fn github_repo_login_happy_path_saves_credentials_and_cleans_up() {
    let home = scratch_dir("home");
    let bin_dir = scratch_dir("bin");
    let git_log = scratch_dir("log").join("git.log");
    install_fake_git(&bin_dir, &git_log);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api_url = format!(
        "http://127.0.0.1:{}/api",
        listener.local_addr().unwrap().port()
    );
    let server = std::thread::spawn(move || serve_mock_api(listener));

    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_portzero"))
        .args(["login", "--github-repo", "--team", "myteam"])
        .env("HOME", &home)
        .env("PATH", &path)
        .env("PZ_TUNNEL_API_URL", &api_url)
        .env("PZ_TUNNEL_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "login failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    server.join().unwrap();

    // Credentials were saved with the repo-proof identity.
    let auth: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".portzero").join("auth.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(auth["token"], "test-token");
    assert_eq!(auth["email"], "github-ci@ci.portzero.cloud");
    assert_eq!(auth["account_id"], "github-repo-proof");

    // The proof ref was pushed and then deleted (best-effort cleanup).
    let log = std::fs::read_to_string(&git_log).unwrap();
    let pushes: Vec<&str> = log.lines().collect();
    assert_eq!(
        pushes,
        vec![
            format!("push origin HEAD:refs/portzero/auth/{NONCE}"),
            format!("push origin :refs/portzero/auth/{NONCE}"),
        ],
        "unexpected git invocations:\n{log}"
    );

    // Success output mentions the team, templates, and the template hint.
    assert!(stdout.contains("myteam"), "stdout:\n{stdout}");
    assert!(
        stdout.contains("{name}.myteam.tunnel.portzero.cloud"),
        "stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("must match these templates"),
        "stdout:\n{stdout}"
    );
}
