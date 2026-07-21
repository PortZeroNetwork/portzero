//! PortZero desktop app (Tauri v2).
//!
//! The primary local GUI for the PortZero daemon — it replaces the old
//! browser-based dashboard at `http://portzero.local`. The daemon still serves
//! that page (nothing here removes it), but every user-facing surface (tray,
//! CLI) now opens this app instead.
//!
//! This binary is deliberately thin: all logic lives in [`core`] (plain,
//! unit-tested, no webkit) and [`commands`] (small Tauri wrappers). `main` only
//! wires up the single-instance guard and the invoke handlers.

// On Windows, don't spawn an extra console window for the GUI binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod core;

use tauri::{Emitter, Manager};

fn main() {
    tauri::Builder::default()
        // Single-instance must be registered first: a second launch (e.g. the
        // tray or CLI opening the app again) is routed to the already-running
        // instance, which focuses its window instead of starting a duplicate.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
                // Nudge the UI to refresh now that it's been re-focused.
                let _ = app.emit("app://focus", ());
            }
        }))
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::examples_status,
            commands::download_examples,
            commands::run_example,
            commands::stop_example,
            commands::start_daemon,
            commands::stop_daemon,
            commands::restart_daemon,
            commands::set_https,
            commands::open_external,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the PortZero desktop app");
}
