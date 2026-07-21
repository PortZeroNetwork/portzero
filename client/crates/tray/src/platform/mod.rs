//! Platform event-loop glue. Each backend only owns the event loop and the
//! periodic-refresh timer; all menu structure and state logic lives in the
//! shared [`crate::menu`], [`crate::state`], and [`crate::controller`] modules,
//! so the tray behaves identically on every platform.
//!
//! - Linux drives the tray from a pure-Rust ksni StatusNotifierItem service
//!   (SNI over D-Bus, no GTK); ksni owns its own background service thread.
//! - Windows and macOS drive it from a winit event loop, which owns the native
//!   message pump / run loop the tray icon needs.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::run;

#[cfg(not(target_os = "linux"))]
mod winit_loop;
#[cfg(not(target_os = "linux"))]
pub use winit_loop::run;
