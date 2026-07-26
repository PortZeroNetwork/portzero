//! Thin Tauri command layer.
//!
//! Each command is a small wrapper that forwards to [`crate::core`] (which holds
//! all the webview-independent logic and its unit tests). The commands are
//! synchronous `fn`s: Tauri runs them off the async runtime, so the blocking
//! `reqwest` client in `core` never runs inside a tokio context (which would
//! panic). The only exception is [`run_example`], which returns immediately
//! after handing the long-lived output stream to a background thread that emits
//! frames to the UI over the event bus.

use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::core;

/// Event carrying one line of an example's output to the frontend console.
const EVENT_LOG: &str = "example://log";
/// Event signalling an example run has ended (finished, stopped, or errored).
const EVENT_END: &str = "example://end";

/// Fetch daemon status (or a daemon-down fallback). Never errors — the UI always
/// gets a renderable object.
#[tauri::command]
pub async fn get_status() -> Value {
    // Daemon I/O must never run on the Tauri main thread: a blocking
    // command holds the UI for the whole round trip.
    tauri::async_runtime::spawn_blocking(core::get_status)
        .await
        .unwrap_or_else(|e| panic!("get_status task panicked: {e}"))
}

/// Download-state + running-example ids.
#[tauri::command]
pub async fn examples_status() -> Result<Value, String> {
    // Daemon I/O must never run on the Tauri main thread: a blocking
    // command holds the UI for the whole round trip.
    tauri::async_runtime::spawn_blocking(core::examples_status)
        .await
        .unwrap_or_else(|e| panic!("examples_status task panicked: {e}"))
}

/// Clone/update the examples repo.
#[tauri::command]
pub async fn download_examples() -> Result<Value, String> {
    // Daemon I/O must never run on the Tauri main thread: a blocking
    // command holds the UI for the whole round trip.
    tauri::async_runtime::spawn_blocking(core::download_examples)
        .await
        .unwrap_or_else(|e| panic!("download_examples task panicked: {e}"))
}

/// Stop a running example.
#[tauri::command]
pub async fn stop_example(id: String) -> Result<Value, String> {
    // Daemon I/O must never run on the Tauri main thread: a blocking
    // command holds the UI for the whole round trip.
    tauri::async_runtime::spawn_blocking(move || core::stop_example(&id))
        .await
        .unwrap_or_else(|e| panic!("stop_example task panicked: {e}"))
}

/// Start an example and stream its output to the UI.
///
/// Returns as soon as the stream is open; a background thread then reads the
/// example's Server-Sent-Events and emits an [`EVENT_LOG`] per line and a final
/// [`EVENT_END`]. The frontend correlates events by the example `id` in the
/// payload. Dropping the reader (when the run ends or the daemon tears it down)
/// ends the thread.
#[tauri::command]
pub fn run_example(app: AppHandle, id: String) -> Result<(), String> {
    use std::io::Read;

    let reader = core::open_example_stream(&id)?;
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut parser = core::SseParser::default();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // stream closed
                Ok(n) => {
                    for ev in parser.push(&buf[..n]) {
                        emit_frame(&app, &id, ev);
                    }
                }
                Err(_) => break,
            }
        }
        // The connection closed without an explicit `end`; make sure the UI
        // stops showing a spinner.
        let _ = app.emit(
            EVENT_END,
            serde_json::json!({ "id": id, "message": "[stream closed]" }),
        );
    });
    Ok(())
}

/// Translate one SSE frame into a UI event.
fn emit_frame(app: &AppHandle, id: &str, ev: core::SseEvent) {
    match ev.event.as_str() {
        "end" => {
            let _ = app.emit(
                EVENT_END,
                serde_json::json!({ "id": id, "message": ev.data }),
            );
        }
        "error" => {
            let _ = app.emit(
                EVENT_LOG,
                serde_json::json!({ "id": id, "line": ev.data, "kind": "error" }),
            );
            let _ = app.emit(EVENT_END, serde_json::json!({ "id": id, "message": "" }));
        }
        _ => {
            // Lines starting with '$' are the echoed commands; tag them so the
            // console can style them like the old dashboard did.
            let kind = if ev.data.starts_with('$') {
                "cmd"
            } else {
                "out"
            };
            let _ = app.emit(
                EVENT_LOG,
                serde_json::json!({ "id": id, "line": ev.data, "kind": kind }),
            );
        }
    }
}

/// Start the daemon.
#[tauri::command]
pub fn start_daemon() -> Result<(), String> {
    core::start_daemon()
}

/// Stop the daemon.
#[tauri::command]
pub fn stop_daemon() -> Result<(), String> {
    core::stop_daemon()
}

/// Restart the daemon.
#[tauri::command]
pub fn restart_daemon() -> Result<(), String> {
    core::restart_daemon()
}

/// Toggle "enable HTTPS for HTTP tunnels".
#[tauri::command]
pub fn set_https(enabled: bool) -> Result<(), String> {
    core::set_https(enabled)
}

/// Open a real service URL (a tunnel link, a cloud page) in the browser.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    core::open_external(&url)
}

/// This app's version, every other component's version, and whether they agree.
/// Never errors — a component whose version can't be read is reported as
/// unknown rather than failing the whole panel.
#[tauri::command]
pub async fn get_versions() -> Value {
    // Daemon I/O must never run on the Tauri main thread: a blocking
    // command holds the UI for the whole round trip.
    tauri::async_runtime::spawn_blocking(core::get_versions)
        .await
        .unwrap_or_else(|e| panic!("get_versions task panicked: {e}"))
}
