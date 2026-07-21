//! Windows / macOS backend: drive the tray from a winit event loop.
//!
//! winit owns the native message pump (Windows) / run loop (macOS) that the tray
//! icon and its menu need in order to receive clicks. The tray icon must be
//! created *after* the event loop starts running, so we build the controller in
//! response to `StartCause::Init`. Periodic refresh is scheduled with
//! `ControlFlow::WaitUntil`, and menu clicks are drained from the muda channel in
//! `about_to_wait`.

use std::time::Instant;

use anyhow::{Context, Result};
use muda::MenuEvent;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::controller::Controller;
use crate::engine::{Dispatch, REFRESH_INTERVAL};

#[derive(Default)]
struct TrayApp {
    controller: Option<Controller>,
}

impl TrayApp {
    /// Schedule the next periodic refresh.
    fn arm_refresh(&self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + REFRESH_INTERVAL));
    }
}

impl ApplicationHandler for TrayApp {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        match cause {
            // The event loop is running now — safe to create the tray icon.
            StartCause::Init => match Controller::new() {
                Ok(mut controller) => {
                    controller.maybe_autostart_daemon();
                    controller.maybe_notify_first_run();
                    self.controller = Some(controller);
                    self.arm_refresh(event_loop);
                }
                Err(e) => {
                    tracing::error!("failed to create the system tray: {e:#}");
                    event_loop.exit();
                }
            },
            // A refresh timer fired.
            StartCause::ResumeTimeReached { .. } => {
                if let Some(controller) = self.controller.as_mut() {
                    controller.refresh();
                }
                self.arm_refresh(event_loop);
            }
            _ => {}
        }
    }

    // The tray owns no windows, so these are intentionally empty.
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}
    fn window_event(&mut self, _e: &ActiveEventLoop, _id: WindowId, _ev: WindowEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(controller) = self.controller.as_mut() else {
            return;
        };
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if controller.handle_menu(&event.id.0) == Dispatch::Quit {
                event_loop.exit();
                return;
            }
        }
    }
}

pub fn run() -> Result<()> {
    let event_loop = EventLoop::new().context("failed to create the event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = TrayApp::default();
    event_loop
        .run_app(&mut app)
        .context("tray event loop exited with an error")?;
    Ok(())
}
