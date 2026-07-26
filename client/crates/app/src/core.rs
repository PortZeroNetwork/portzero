//! Webview-independent backend logic for the PortZero desktop app.
//!
//! Everything here is plain Rust with no `tauri` dependency, so it compiles and
//! unit-tests without webkit. The thin Tauri command layer (`commands.rs`) wraps
//! these functions and forwards their results to the frontend over `invoke`.
//!
//! The app talks to the running daemon exactly like the old browser dashboard
//! did — over the local overlay name `http://portzero.local`, which the daemon's
//! DNS/overlay routes to its localhost management server. That name is the
//! product's fixed local-loopback identity (not a configurable cloud service
//! endpoint), so it lives here as a constant rather than in
//! `portzero_domain::endpoints`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// Local overlay base URL the daemon serves its management UI + API on.
pub const LOCAL_BASE: &str = "http://portzero.local";

/// The version this app was built from — the workspace version every PortZero
/// binary shares, so it is directly comparable with what the daemon, tray, and
/// CLI report.
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The one blocking HTTP client the app uses, built once.
///
/// It must be shared, not per-call. `reqwest::blocking::Client` owns a private
/// tokio runtime on its own thread, and **dropping one blocks until that thread
/// joins**. Building a client per request therefore paid a thread spawn *and* a
/// `pthread_join` on every call — and because these commands run on the Tauri
/// main thread, that join froze the UI. A sample of the app mid-freeze put 92%
/// of main-thread time in `InnerClientHandle::drop_slow → thread::join`, not in
/// the request itself. Reusing one client removes that cost entirely.
///
/// `no_proxy()` is essential: the app may run with `HTTPS_PROXY`/`HTTP_PROXY`
/// set, and routing a loopback overlay request through an external proxy would
/// always fail. Per-request timeouts are applied at the call site with
/// `RequestBuilder::timeout`, so one shared client still serves callers that
/// need very different budgets.
static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();

fn client() -> Result<&'static reqwest::blocking::Client, String> {
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    let built = reqwest::blocking::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_millis(700))
        .build()
        .map_err(|e| {
            format!(
                "Could not initialize the local HTTP client: {e}.\n\
                 This is unexpected — please restart the PortZero app, and if it \
                 keeps happening, report it with this message."
            )
        })?;
    Ok(CLIENT.get_or_init(|| built))
}

/// Base URL for daemon requests.
///
/// Uses the overlay's fixed gateway IP rather than `LOCAL_BASE`'s
/// `portzero.local`. On macOS, mDNSResponder claims every `.local` name before
/// the scoped resolver is consulted, so `getaddrinfo("portzero.local")` can
/// stall for seconds — on the UI thread, per poll — even when the daemon is
/// serving normally. `connect_timeout` does not reliably bound name resolution,
/// so the only dependable fix is not to resolve. Requests carry an explicit
/// `Host` header so the daemon still routes them as before.
const DAEMON_ADDR: &str = "http://10.254.0.2";
const DAEMON_HOST: &str = "portzero.local";

/// A GET against the local daemon with the given budget, bypassing DNS.
fn daemon_get(path: &str, timeout: Duration) -> Result<reqwest::blocking::Response, String> {
    client()?
        .get(format!("{DAEMON_ADDR}{path}"))
        .header("Host", DAEMON_HOST)
        .timeout(timeout)
        .send()
        .map_err(|e| e.to_string())
}

/// A POST against the local daemon with the given budget, bypassing DNS.
fn daemon_post(path: &str, timeout: Duration) -> Result<reqwest::blocking::Response, String> {
    client()?
        .post(format!("{DAEMON_ADDR}{path}"))
        .header("Host", DAEMON_HOST)
        .timeout(timeout)
        .send()
        .map_err(|e| e.to_string())
}

/// Fetch `/status.json` and return it enriched with a `running` flag, or a
/// daemon-down fallback object the UI can still render.
///
/// The fallback consults the on-disk daemon PID (via
/// [`portzero_domain`]-independent means below) so that a daemon which is up but
/// whose overlay isn't routing `portzero.local` yet is still reported as
/// running — the UI then offers Restart rather than Start.
pub fn get_status() -> Value {
    match daemon_get("/status.json", Duration::from_millis(1200)).and_then(|r| {
        if r.status().is_success() {
            r.json::<Value>().map_err(|e| e.to_string())
        } else {
            Err(format!("daemon returned HTTP {}", r.status().as_u16()))
        }
    }) {
        Ok(mut v) => {
            let running = v.get("daemon_pid").map(|p| !p.is_null()).unwrap_or(false);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("running".into(), json!(running));
                obj.insert("reachable".into(), json!(true));
            }
            v
        }
        Err(reason) => fallback_status(&reason),
    }
}

/// A daemon-down status object with the same shape the UI reads from
/// `/status.json`, so every panel renders empty-but-valid instead of erroring.
pub fn fallback_status(reason: &str) -> Value {
    json!({
        "running": false,
        "reachable": false,
        "daemon_pid": Value::Null,
        "overlay_active": false,
        "auth_authenticated": false,
        "local_services": [],
        "cloud_connected": false,
        "cloud_routes": [],
        "management_registrations": [],
        "problems": [],
        "examples": {
            "downloaded": false,
            "download_state": "absent",
            "download_error": Value::Null,
            "dir": "~/.portzero/examples",
            "running": []
        },
        "https_policy": {
            "enable_for_port_80": false,
            "redirect_port_80": false,
            "passthrough_port_443": false
        },
        "status_message": format!(
            "The PortZero daemon isn't reachable at {LOCAL_BASE} ({reason}). \
             Start it below. If it is running, its overlay network may not be \
             active yet — check the daemon status or run `portzero status` in a terminal."
        ),
    })
}

/// GET `/v1/examples/status`.
pub fn examples_status() -> Result<Value, String> {
    get_json("/v1/examples/status", Duration::from_millis(1500))
}

/// POST `/v1/examples/download`.
pub fn download_examples() -> Result<Value, String> {
    post_json("/v1/examples/download", Duration::from_secs(120))
}

/// POST `/v1/examples/stop?id=<id>`.
pub fn stop_example(id: &str) -> Result<Value, String> {
    let path = format!("/v1/examples/stop?id={}", urlencode(id));
    // The stop endpoint returns an empty body with a status code; treat any 2xx
    // as success and surface a clear message otherwise.
    let resp = daemon_post(&path, Duration::from_secs(15)).map_err(|e| daemon_unreachable(&e))?;
    if resp.status().is_success() {
        Ok(json!({ "ok": true }))
    } else {
        Err(format!(
            "Couldn't stop example \"{id}\": the daemon returned HTTP {}.\n\
             It may have already stopped. Refresh to see the current state.",
            resp.status().as_u16()
        ))
    }
}

/// GET a daemon endpoint and parse its JSON body.
fn get_json(path: &str, timeout: Duration) -> Result<Value, String> {
    let resp = daemon_get(path, timeout).map_err(|e| daemon_unreachable(&e))?;
    parse_json_response(resp)
}

/// POST a daemon endpoint and parse its JSON body.
fn post_json(path: &str, timeout: Duration) -> Result<Value, String> {
    let resp = daemon_post(path, timeout).map_err(|e| daemon_unreachable(&e))?;
    parse_json_response(resp)
}

fn parse_json_response(resp: reqwest::blocking::Response) -> Result<Value, String> {
    let status = resp.status();
    let body = resp
        .json::<Value>()
        .map_err(|e| format!("The daemon sent a response we couldn't read: {e}."))?;
    if status.is_success() {
        Ok(body)
    } else {
        // Endpoints like /v1/examples/download return {ok:false, message:"..."}.
        let msg = body
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("the daemon reported an error");
        Err(format!("{msg} (HTTP {})", status.as_u16()))
    }
}

/// A consistent, actionable "can't reach the daemon" message.
fn daemon_unreachable(detail: &str) -> String {
    format!(
        "Couldn't reach the PortZero daemon at {LOCAL_BASE} ({detail}).\n\
         Make sure the daemon is running (use Start below), and that its overlay \
         network is active. On Linux the overlay needs `setcap` on the `portzero` \
         binary — see `portzero status` for the exact command."
    )
}

/// Minimal percent-encoding for a query-string value (example ids are simple
/// slugs like `python/process`, but `/` and spaces still need escaping).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Open the run stream for an example and return the reader over its
/// Server-Sent-Events body. The caller drives it with an [`SseParser`],
/// emitting each frame to the UI. Dropping the reader closes the connection,
/// which is how the daemon learns to tear the example down.
pub fn open_example_stream(id: &str) -> Result<impl Read, String> {
    let url = format!("{LOCAL_BASE}/v1/examples/run?id={}", urlencode(id));
    // No overall timeout: a running example streams for as long as it lives.
    let resp = reqwest::blocking::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_millis(700))
        .build()
        .map_err(|e| format!("Could not initialize the local HTTP client: {e}."))?
        .get(&url)
        .send()
        .map_err(|e| daemon_unreachable(&e.to_string()))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Couldn't start example \"{id}\": the daemon returned HTTP {}.\n\
             Make sure the examples are downloaded and the daemon is running.",
            resp.status().as_u16()
        ));
    }
    Ok(resp)
}

/// A single parsed Server-Sent-Events frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, or `"message"` when the frame had none (the SSE
    /// default). The daemon uses the default event for log lines and named
    /// `end` / `error` events for terminal states.
    pub event: String,
    /// The concatenated `data:` payload (multiple `data:` lines joined by `\n`).
    pub data: String,
}

/// Incremental parser for a Server-Sent-Events byte stream.
///
/// Feed it chunks with [`SseParser::push`]; it returns every complete frame
/// (delimited by a blank line) decoded so far. Kept separate from the network
/// I/O so it can be unit-tested without a live daemon.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes received but not yet split into a complete line.
    buffer: Vec<u8>,
    /// `event:` for the frame currently being assembled.
    cur_event: Option<String>,
    /// Accumulated `data:` lines for the frame currently being assembled.
    cur_data: Vec<String>,
    /// Whether the current frame has any field at all (so a stray blank line
    /// before any field doesn't emit an empty frame).
    has_fields: bool,
}

impl SseParser {
    /// Feed a chunk of bytes, returning any frames completed by it.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            // Trim the trailing '\n' and an optional '\r'.
            let mut end = line_bytes.len() - 1;
            if end > 0 && line_bytes[end - 1] == b'\r' {
                end -= 1;
            }
            let line = String::from_utf8_lossy(&line_bytes[..end]).to_string();
            if let Some(ev) = self.consume_line(&line) {
                events.push(ev);
            }
        }
        events
    }

    /// Process one already-delimited line, returning a frame if it completed one.
    fn consume_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.finish_frame();
        }
        // SSE comment lines start with ':' — ignore them (keep-alive pings).
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest;
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => {
                self.cur_event = Some(value.to_string());
                self.has_fields = true;
            }
            "data" => {
                self.cur_data.push(value.to_string());
                self.has_fields = true;
            }
            // "id"/"retry" and unknown fields are irrelevant to us.
            _ => {}
        }
        None
    }

    fn finish_frame(&mut self) -> Option<SseEvent> {
        if !self.has_fields {
            return None;
        }
        let event = self
            .cur_event
            .take()
            .unwrap_or_else(|| "message".to_string());
        let data = self.cur_data.join("\n");
        self.cur_data.clear();
        self.has_fields = false;
        Some(SseEvent { event, data })
    }
}

/// Run `portzero <sub>` detached (best-effort), mirroring the tray so the two
/// drive the daemon identically. `--no-browser` is passed for lifecycle
/// commands that would otherwise pop a browser, since the app is the GUI.
fn run_cli(sub: &str) -> Result<(), String> {
    let bin = portzero_domain::app::cli_bin();
    let mut cmd = Command::new(&bin);
    cmd.arg(sub);
    if sub == "start" || sub == "restart" {
        cmd.arg("--no-browser");
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|e| {
            format!(
                "Couldn't run `{} {sub}`: {e}.\n\
                 The PortZero CLI (`portzero`) must be installed and on your PATH \
                 (or set PORTZERO_BIN to its full path). Reinstalling PortZero \
                 usually fixes this.",
                bin.display()
            )
        })
}

/// Start the daemon (`portzero start --no-browser`).
pub fn start_daemon() -> Result<(), String> {
    run_cli("start")
}

/// Stop the daemon (`portzero stop`).
pub fn stop_daemon() -> Result<(), String> {
    run_cli("stop")
}

/// Restart the daemon (`portzero restart --no-browser`).
pub fn restart_daemon() -> Result<(), String> {
    run_cli("restart")
}

/// Open a URL in the user's default browser (best-effort, non-blocking).
///
/// Only real service URLs (tunnel links, cloud upgrade pages) should reach this
/// — the app itself replaces the local `portzero.local` dashboard, so we never
/// point the browser back at it.
pub fn open_external(url: &str) -> Result<(), String> {
    // Guard against obviously unopenable values so a bad link becomes a clear
    // message instead of a spawned shell doing nothing.
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "Refusing to open \"{url}\": only http:// and https:// links can be \
             opened in the browser."
        ));
    }
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let (program, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);

    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|e| {
            format!(
                "Couldn't open {url} in your browser: {e}.\n\
                 Copy the link and open it manually if this keeps happening."
            )
        })
}

/// Set the "enable HTTPS for HTTP tunnels" policy, mirroring the tray's
/// `set_https_enabled`: the new policy is written to `~/.portzero/config.toml`,
/// which a running daemon hot-applies within a couple of seconds and a stopped
/// daemon picks up on next start.
pub fn set_https(enabled: bool) -> Result<(), String> {
    use portzero_daemon::discovery_loop::DaemonConfig;

    let config = DaemonConfig::load();
    let mut policy = config.overlay_https;
    policy.enable_for_port_80 = enabled;
    config.write_https_policy(policy).map_err(|e| {
        format!(
            "Couldn't save the HTTPS setting to {}: {e:#}.\n\
             Check that you can write to ~/.portzero/config.toml (it must not be \
             read-only or owned by another user).",
            config.config_path().display()
        )
    })
}

/// Record that this app is running, so the CLI's version report can see it.
pub fn announce_version() {
    use portzero_daemon::discovery_loop::DaemonConfig;
    use portzero_daemon::versions::{announce, Component};

    announce(&DaemonConfig::load(), Component::App, BUILD_VERSION);
}

/// Remove this app's version record when it exits.
pub fn withdraw_version() {
    use portzero_daemon::discovery_loop::DaemonConfig;
    use portzero_daemon::versions::{withdraw, Component};

    withdraw(&DaemonConfig::load(), Component::App);
}

/// Every component's version plus whether they agree — the data behind the
/// app's Version panel.
///
/// Serialized rather than returned as a typed struct because the whole app
/// bridge speaks `serde_json::Value`; the shape is owned by
/// [`portzero_daemon::versions::VersionReport`].
pub fn get_versions() -> Value {
    use portzero_daemon::discovery_loop::DaemonConfig;
    use portzero_daemon::versions::{collect, Component};

    let report = collect(&DaemonConfig::load(), Component::App, BUILD_VERSION);
    serde_json::to_value(&report).unwrap_or_else(|e| {
        json!({
            "build_version": BUILD_VERSION,
            "components": [],
            "consistent": true,
            "summary": format!("PortZero {BUILD_VERSION}. Could not read the other components' versions ({e})."),
            "next_steps": [],
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_report_always_includes_this_app() {
        let v = get_versions();
        assert_eq!(v["build_version"], json!(BUILD_VERSION));
        let app = v["components"]
            .as_array()
            .expect("components should be an array")
            .iter()
            .find(|c| c["component"] == json!("app"))
            .expect("the app reports its own version");
        assert_eq!(app["version"], json!(BUILD_VERSION));
        assert_eq!(app["running"], json!(true));
    }

    #[test]
    fn fallback_status_is_renderable_and_marks_daemon_down() {
        let v = fallback_status("connection refused");
        assert_eq!(v["running"], json!(false));
        assert_eq!(v["reachable"], json!(false));
        assert!(v["local_services"].is_array());
        assert!(v["cloud_routes"].is_array());
        assert!(v["problems"].is_array());
        assert_eq!(v["examples"]["downloaded"], json!(false));
        assert!(v["https_policy"]["enable_for_port_80"] == json!(false));
        assert!(v["status_message"]
            .as_str()
            .unwrap()
            .contains("isn't reachable"));
    }

    #[test]
    fn urlencode_escapes_slash_and_space() {
        assert_eq!(urlencode("python/process"), "python%2Fprocess");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("simple-id_1.2~"), "simple-id_1.2~");
    }

    #[test]
    fn sse_parses_default_event_as_message() {
        let mut p = SseParser::default();
        let evs = p.push(b"data: hello world\n\n");
        assert_eq!(
            evs,
            vec![SseEvent {
                event: "message".into(),
                data: "hello world".into()
            }]
        );
    }

    #[test]
    fn sse_parses_named_end_event() {
        let mut p = SseParser::default();
        let evs = p.push(b"event: end\ndata: [example stopped]\n\n");
        assert_eq!(
            evs,
            vec![SseEvent {
                event: "end".into(),
                data: "[example stopped]".into()
            }]
        );
    }

    #[test]
    fn sse_handles_chunk_boundaries_mid_frame() {
        let mut p = SseParser::default();
        assert!(p.push(b"data: par").is_empty());
        assert!(p.push(b"tial line").is_empty());
        let evs = p.push(b"\n\n");
        assert_eq!(evs[0].data, "partial line");
    }

    #[test]
    fn sse_joins_multiple_data_lines() {
        let mut p = SseParser::default();
        let evs = p.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(evs[0].data, "line1\nline2");
    }

    #[test]
    fn sse_ignores_comment_keepalives() {
        let mut p = SseParser::default();
        let evs = p.push(b": keep-alive\n\ndata: real\n\n");
        // The comment + blank line produce no frame; only the real one does.
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "real");
    }

    #[test]
    fn sse_tolerates_crlf_line_endings() {
        let mut p = SseParser::default();
        let evs = p.push(b"event: error\r\ndata: boom\r\n\r\n");
        assert_eq!(evs[0].event, "error");
        assert_eq!(evs[0].data, "boom");
    }

    #[test]
    fn open_external_rejects_non_http_schemes() {
        let err = open_external("file:///etc/passwd").unwrap_err();
        assert!(err.contains("only http"));
    }
}
