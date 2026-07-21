//! Getting-started examples: download the `portzero-examples` repo and run a
//! single example, streaming its output to the dashboard's terminal console.
//!
//! The set of runnable examples is fixed at compile time by the checked-in
//! `installer/getting-started.json` manifest — the dashboard sends an example
//! **id**, never a command, so a local caller can only launch one of the
//! known-good examples in its known directory (no arbitrary command execution).
//!
//! Output is streamed over Server-Sent Events. Closing the `EventSource` (the
//! "Stop" button, or navigating away) drops the response stream; the supervisor
//! notices and terminates the example's whole process group, so a `docker
//! compose up` or dev server doesn't outlive the console that started it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, Mutex, Notify};

use crate::management::server::AppState;

const GETTING_STARTED_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../installer/getting-started.json"
));

/// Registry of currently-running examples, keyed by example id. Shared via
/// [`AppState`] so `/status.json` can report what's running and `stop` can find
/// a handle to cancel.
pub type RunningExamples = Arc<Mutex<HashMap<String, RunningExample>>>;

/// A live example process. `cancel` asks the supervisor (which owns the
/// actual process/compose teardown) to stop.
pub struct RunningExample {
    cancel: Arc<Notify>,
}

/// Absolute path to the examples checkout (`~/.portzero/examples`).
///
/// Lives under `~/.portzero/` — the directory PortZero already owns (alongside
/// `auth.json` and `daemon/`) — so the getting-started examples are fetched
/// automatically without dropping a visible folder in the user's home dir that
/// they'd have to reason about.
pub fn examples_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".portzero")
        .join("examples")
}

/// Display form of the examples directory, kept short (`~/.portzero/examples`).
pub fn examples_dir_display() -> String {
    "~/.portzero/examples".to_string()
}

/// Coarse state of the automatic examples download, surfaced to the UI so it can
/// show "setting up…" / retry without the user ever clicking a download button.
#[derive(Debug, Clone, Default)]
pub enum DownloadState {
    /// Not yet started (nothing checked out and no fetch in flight).
    #[default]
    Absent,
    /// A clone/update is currently running.
    Downloading,
    /// The examples checkout is present and usable.
    Ready,
    /// The last attempt failed; carries a user-facing reason.
    Error(String),
}

impl DownloadState {
    /// Short machine tag for `/status.json` (`absent`/`downloading`/`ready`/`error`).
    fn tag(&self) -> &'static str {
        match self {
            DownloadState::Absent => "absent",
            DownloadState::Downloading => "downloading",
            DownloadState::Ready => "ready",
            DownloadState::Error(_) => "error",
        }
    }
}

/// Shared handle to the automatic-download state, updated by the background
/// fetch task and read by the status handlers.
pub type ExamplesDownload = Arc<Mutex<DownloadState>>;

/// Kick off the one-time background fetch of the examples repo, if needed.
///
/// Called once at daemon startup. Never blocks the caller: the actual git work
/// runs on a blocking thread. If the checkout is already present it records
/// `Ready` immediately; if `git` is missing or the network is down it records a
/// clear `Error` the UI can show with a retry, rather than failing loudly.
pub fn spawn_auto_download(download: ExamplesDownload) {
    tokio::spawn(async move {
        run_download(&download).await;
    });
}

/// Perform (or refresh) the examples checkout, updating `download` as it goes.
/// Shared by the startup auto-fetch and the manual retry endpoint.
async fn run_download(download: &ExamplesDownload) {
    if is_downloaded() {
        *download.lock().await = DownloadState::Ready;
        return;
    }
    if !command_on_path("git") {
        *download.lock().await = DownloadState::Error(
            "Git isn't installed, so the examples can't be fetched automatically. \
             Install Git and reopen PortZero, or clone \
             https://github.com/PortZeroNetwork/portzero-examples into \
             ~/.portzero/examples yourself."
                .to_string(),
        );
        return;
    }
    *download.lock().await = DownloadState::Downloading;
    let dir = examples_dir();
    let repo = source_repo();
    let result = tokio::task::spawn_blocking(move || clone_or_update(&dir, &repo)).await;
    *download.lock().await = match result {
        Ok(Ok(_)) => DownloadState::Ready,
        Ok(Err(msg)) => DownloadState::Error(msg),
        Err(e) => DownloadState::Error(format!("The examples download task failed to run: {e}")),
    };
}

/// Whether the examples repo appears to be present (a `.git` dir, i.e. a clone).
pub fn is_downloaded() -> bool {
    examples_dir().join(".git").is_dir()
}

/// Parsed manifest (`installer/getting-started.json`).
fn manifest() -> serde_json::Value {
    serde_json::from_str(GETTING_STARTED_JSON).unwrap_or_else(|_| serde_json::json!({}))
}

/// The git URL the examples repo is cloned from (from the manifest's `source`).
fn source_repo() -> String {
    manifest()
        .get("source")
        .and_then(|s| s.get("repo"))
        .and_then(|r| r.as_str())
        .unwrap_or("https://github.com/PortZeroNetwork/portzero-examples.git")
        .to_string()
}

/// Look up an example entry by its manifest id (e.g. `"python/process"`).
fn find_example(id: &str) -> Option<serde_json::Value> {
    manifest()
        .get("examples")?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id))
        .cloned()
}

/// The OS-appropriate run command string for an example.
fn os_command(example: &serde_json::Value) -> Option<String> {
    let commands = example.get("commands")?;
    let key = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    commands
        .get(key)
        .or_else(|| commands.get("linux"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        if dir.join(program).is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            dir.join(format!("{program}.exe")).is_file()
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

/// Whether this example can run on this machine: at least one of its language
/// tools is on PATH, and Docker is present if the example needs it.
fn example_runnable(example: &serde_json::Value) -> bool {
    let requires_docker = example
        .get("requires_docker")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if requires_docker && !command_on_path("docker") {
        return false;
    }
    example
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|tools| tools.iter().filter_map(|t| t.as_str()).any(command_on_path))
        .unwrap_or(false)
}

/// GET /v1/examples/status — download state + list of running example ids. Also
/// folded into `/status.json`; this endpoint exists for direct polling.
pub async fn examples_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(status_value(&state).await)
}

/// Shared shape used by both `examples_status` and `/status.json`.
pub async fn status_value(state: &AppState) -> serde_json::Value {
    let running: Vec<String> = state
        .running_examples
        .lock()
        .await
        .keys()
        .cloned()
        .collect();
    let download = state.examples_download.lock().await.clone();
    let download_error = match &download {
        DownloadState::Error(msg) => Some(msg.clone()),
        _ => None,
    };
    serde_json::json!({
        "downloaded": is_downloaded(),
        "download_state": download.tag(),
        "download_error": download_error,
        "dir": examples_dir_display(),
        "path": examples_dir().to_string_lossy(),
        "running": running,
    })
}

/// POST /v1/examples/download — clone (or fast-forward) the examples repo into
/// `~/.portzero/examples`. Runs git on a blocking thread so the async server
/// isn't stalled by the network.
///
/// The examples are normally fetched automatically at daemon startup; this
/// endpoint is the manual retry path (e.g. after fixing an offline / missing-git
/// error), and it keeps the shared download state in sync so the UI reflects the
/// retry the same way it reflects the automatic fetch.
pub async fn download_examples(State(state): State<AppState>) -> Response {
    let dir = examples_dir();
    let repo = source_repo();

    *state.examples_download.lock().await = DownloadState::Downloading;
    let result = tokio::task::spawn_blocking(move || clone_or_update(&dir, &repo)).await;
    *state.examples_download.lock().await = match &result {
        Ok(Ok(_)) => DownloadState::Ready,
        Ok(Err(msg)) => DownloadState::Error(msg.clone()),
        Err(e) => DownloadState::Error(format!("The examples download task failed to run: {e}")),
    };

    match result {
        Ok(Ok(msg)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "path": examples_dir().to_string_lossy(),
                "message": msg,
            })),
        )
            .into_response(),
        Ok(Err(msg)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "path": examples_dir().to_string_lossy(),
                "message": msg,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("download task failed: {e}"),
            })),
        )
            .into_response(),
    }
}

/// Blocking git clone/pull. Returns a short human message on success.
fn clone_or_update(dir: &Path, repo: &str) -> Result<String, String> {
    if !command_on_path("git") {
        return Err("git is not installed. Install git, then try again.".to_string());
    }

    if dir.join(".git").is_dir() {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["pull", "--ff-only"])
            .output()
            .map_err(|e| format!("git pull failed to start: {e}"))?;
        if out.status.success() {
            return Ok("Updated ~/.portzero/examples.".to_string());
        }
        // A non-fast-forwardable or offline checkout is still usable.
        return Ok("~/.portzero/examples is already present.".to_string());
    }

    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {parent:?}: {e}"))?;
    }
    let out = std::process::Command::new("git")
        .args(["clone", "--depth", "1", repo])
        .arg(dir)
        .output()
        .map_err(|e| format!("git clone failed to start: {e}"))?;
    if out.status.success() {
        Ok("Downloaded examples to ~/.portzero/examples.".to_string())
    } else {
        Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct RunQuery {
    id: String,
}

/// GET /v1/examples/run?id=<example-id> — run one example and stream its output
/// as Server-Sent Events. The browser drives this with an `EventSource`; closing
/// it terminates the example.
pub async fn run_example(State(state): State<AppState>, Query(q): Query<RunQuery>) -> Response {
    let Some(example) = find_example(&q.id) else {
        return (StatusCode::NOT_FOUND, format!("unknown example: {}", q.id)).into_response();
    };
    if !is_downloaded() {
        return sse_error("Examples aren't downloaded yet. Click \"Download examples\" first.");
    }
    if !example_runnable(&example) {
        return sse_error(
            "This example can't run here — the required language (or Docker) isn't installed.",
        );
    }
    let Some(command) = os_command(&example) else {
        return sse_error("This example has no command for your OS.");
    };
    let rel_path = example
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let workdir = examples_dir().join(&rel_path);
    if !workdir.is_dir() {
        return sse_error(&format!(
            "Example directory {rel_path} is missing. Re-download the examples."
        ));
    }
    let uses_docker_compose = example
        .get("uses_docker_compose")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    spawn_streaming_example(state, q.id, rel_path, command, workdir, uses_docker_compose).await
}

/// One SSE output line.
fn line_event(text: impl Into<String>) -> Event {
    Event::default().data(text.into())
}

/// Build a one-shot SSE response carrying a single error line then closing.
fn sse_error(message: &str) -> Response {
    let (tx, rx) = mpsc::channel::<Event>(4);
    let message = message.to_string();
    tokio::spawn(async move {
        let _ = tx.send(Event::default().event("error").data(message)).await;
    });
    sse_from_receiver(rx)
}

/// Turn a receiver of events into an SSE response.
fn sse_from_receiver(rx: mpsc::Receiver<Event>) -> Response {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|ev| (Ok::<Event, Infallible>(ev), rx))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Spawn the example, register it, and return the SSE stream of its output.
async fn spawn_streaming_example(
    state: AppState,
    id: String,
    rel_path: String,
    command: String,
    workdir: PathBuf,
    uses_docker_compose: bool,
) -> Response {
    // Restart semantics: if this example is already running, stop the old one.
    stop_running(&state, &id).await;

    let (tx, rx) = mpsc::channel::<Event>(256);

    let mut cmd = build_command(&command, &workdir);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return sse_error(&format!("Failed to start example: {e}")),
    };

    let pgid = child.id().map(|p| p as i32);
    let compose_workdir = uses_docker_compose.then(|| workdir.clone());

    let cancel = Arc::new(Notify::new());
    state.running_examples.lock().await.insert(
        id.clone(),
        RunningExample {
            cancel: cancel.clone(),
        },
    );

    // Echo the commands the user is running, so the console reads like a shell.
    let _ = tx
        .send(line_event(format!(
            "$ cd {}",
            examples_display_path(&rel_path)
        )))
        .await;
    let _ = tx.send(line_event(format!("$ {command}"))).await;

    // Pump stdout and stderr into the same channel.
    if let Some(out) = child.stdout.take() {
        spawn_pump(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_pump(err, tx.clone());
    }

    let running = state.running_examples.clone();
    tokio::spawn(async move {
        supervise(&mut child, &tx, &cancel, pgid, compose_workdir.as_deref()).await;
        running.lock().await.remove(&id);
    });

    sse_from_receiver(rx)
}

fn examples_display_path(rel_path: &str) -> String {
    format!("{}/{rel_path}", examples_dir_display())
}

/// Build the platform shell command that runs `command` inside `workdir`, in its
/// own process group (Unix) so the whole tree can be terminated together.
fn build_command(command: &str, workdir: &Path) -> tokio::process::Command {
    // The manifest's Windows commands are PowerShell (`$env:PZ_TUNNEL = ...; ...`),
    // so run them through PowerShell rather than cmd.exe.
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-NoProfile", "-Command", command]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };

    cmd.current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(unix)]
    cmd.process_group(0);

    cmd
}

/// Read `reader` line by line, forwarding each line as an SSE event until the
/// stream ends or the receiver is gone.
fn spawn_pump<R>(reader: R, tx: mpsc::Sender<Event>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line_event(line)).await.is_err() {
                break;
            }
        }
    });
}

/// Grace period between SIGTERM and SIGKILL, so `docker compose up` (and other
/// well-behaved CLIs) gets a chance to run its own shutdown before we force-kill
/// it out from under any teardown it's doing.
const TERMINATE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait for the example to finish, the client to disconnect, or a stop request —
/// whichever comes first — then make sure the process tree (and, for `docker
/// compose` examples, the containers it started) is gone.
async fn supervise(
    child: &mut tokio::process::Child,
    tx: &mpsc::Sender<Event>,
    cancel: &Notify,
    pgid: Option<i32>,
    compose_workdir: Option<&Path>,
) {
    tokio::select! {
        status = child.wait() => {
            let code = status.ok().and_then(|s| s.code());
            let msg = match code {
                Some(0) => "[example exited]".to_string(),
                Some(c) => format!("[example exited with code {c}]"),
                None => "[example stopped]".to_string(),
            };
            let _ = tx.send(Event::default().event("end").data(msg)).await;
            return;
        }
        _ = tx.closed() => {}       // client closed the console
        _ = cancel.notified() => {} // Stop button / stop endpoint
    }

    // Client-disconnect or explicit stop: tear the whole tree down.
    //
    // `docker compose down` is run *before* force-killing the CLI process:
    // the containers `docker compose up` starts are owned by the Docker
    // daemon, not by the `sh`/`docker compose` process tree, so SIGTERM/
    // SIGKILL to the process group never reaches them. Compose is the only
    // thing that knows which containers belong to this run.
    if let Some(workdir) = compose_workdir {
        let _ = tx.send(line_event("$ docker compose down")).await;
        match tokio::process::Command::new("docker")
            .args(["compose", "down"])
            .current_dir(workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(mut down) => {
                if let Some(out) = down.stdout.take() {
                    spawn_pump(out, tx.clone());
                }
                if let Some(err) = down.stderr.take() {
                    spawn_pump(err, tx.clone());
                }
                let _ = down.wait().await;
            }
            Err(e) => {
                let _ = tx
                    .send(line_event(format!(
                        "$ docker compose down failed to start: {e}"
                    )))
                    .await;
            }
        }
    }

    terminate_group(pgid, libc_sigterm());
    if tokio::time::timeout(TERMINATE_GRACE, child.wait())
        .await
        .is_err()
    {
        terminate_group(pgid, libc_sigkill());
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    let _ = tx
        .send(Event::default().event("end").data("[example stopped]"))
        .await;
}

#[cfg(unix)]
fn libc_sigterm() -> i32 {
    libc::SIGTERM
}

#[cfg(unix)]
fn libc_sigkill() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn libc_sigterm() -> i32 {
    0
}

#[cfg(not(unix))]
fn libc_sigkill() -> i32 {
    0
}

/// Send `signal` to the process group so children (dev servers, non-compose
/// containers) stop too rather than being orphaned. No-op on non-Unix
/// platforms.
#[cfg(unix)]
fn terminate_group(pgid: Option<i32>, signal: i32) {
    if let Some(pgid) = pgid {
        // Negative pid targets the whole group. Best-effort.
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
}

#[cfg(not(unix))]
fn terminate_group(_pgid: Option<i32>, _signal: i32) {}

/// POST /v1/examples/stop?id=<example-id> — stop a running example. Returns
/// 404 if it isn't (or is no longer) running, so a stale UI or a race with
/// the natural exit path is visible to the caller rather than a silent
/// success.
pub async fn stop_example(State(state): State<AppState>, Query(q): Query<RunQuery>) -> StatusCode {
    if stop_running(&state, &q.id).await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

/// Signal a running example to stop, if one is running under `id`. Only sends
/// the initial SIGTERM here — `supervise` (woken by `cancel`) owns the rest
/// of teardown (compose down, grace period, SIGKILL) so it isn't duplicated
/// between this and the disconnect path. Returns whether an example was
/// found to stop.
async fn stop_running(state: &AppState, id: &str) -> bool {
    if let Some(handle) = state.running_examples.lock().await.get(id) {
        handle.cancel.notify_waiters();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_known_example_and_rejects_unknown() {
        assert!(find_example("python/process").is_some());
        assert!(find_example("does/not-exist").is_none());
    }

    #[test]
    fn known_example_command_keeps_pz_tunnel_visible() {
        // The command we run (and echo to the console) must be the raw
        // PZ_TUNNEL command, never a wrapper like `just` — that's what teaches
        // the user how PortZero works.
        let ex = find_example("python/process").expect("python/process example");
        let cmd = os_command(&ex).expect("has an OS command");
        assert!(cmd.contains("PZ_TUNNEL"), "command was: {cmd}");
        assert!(
            !cmd.contains("just "),
            "command should not hide behind just: {cmd}"
        );
    }

    #[test]
    fn source_repo_points_at_examples_repo() {
        assert!(source_repo().contains("portzero-examples"));
    }

    #[test]
    fn examples_live_under_the_owned_dot_portzero_dir() {
        // The checkout lives under ~/.portzero (which PortZero already owns) so it
        // can be fetched automatically without dropping a visible folder in $HOME.
        assert!(examples_dir().ends_with(".portzero/examples"));
        assert_eq!(examples_dir_display(), "~/.portzero/examples");
    }

    #[test]
    fn download_state_tags_are_stable() {
        assert_eq!(DownloadState::Absent.tag(), "absent");
        assert_eq!(DownloadState::Downloading.tag(), "downloading");
        assert_eq!(DownloadState::Ready.tag(), "ready");
        assert_eq!(DownloadState::Error("boom".into()).tag(), "error");
    }
}
