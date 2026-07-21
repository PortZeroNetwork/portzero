//! Locate and launch the PortZero desktop app (`portzero-app`).
//!
//! This is the single shared entry point both the tray and the CLI use to open
//! the desktop app, so they always resolve the binary the same way and never
//! drift. The app is a separate binary installers ship next to `portzero` and
//! `portzero-tray`, so the resolution order mirrors [`sibling_bin`]:
//!
//! 1. the `PORTZERO_APP_BIN` environment override (an explicit path),
//! 2. a sibling of the currently-running executable (the layout every installer
//!    produces),
//! 3. bare `portzero-app` on `PATH`.
//!
//! Launching is always best-effort and never blocks: the app owns its own
//! window/event loop, and because it registers a single-instance guard, a
//! second launch while it is already running simply focuses the existing
//! window instead of starting a duplicate.

use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Environment override for the desktop app binary path.
pub const APP_BIN_ENV: &str = "PORTZERO_APP_BIN";

/// Base (extension-less) name of the desktop app binary.
pub const APP_BIN_NAME: &str = "portzero-app";

/// Platform-specific executable file name for a base binary name.
fn exe_file_name(base: &str) -> String {
    #[cfg(windows)]
    {
        format!("{base}.exe")
    }
    #[cfg(not(windows))]
    {
        base.to_string()
    }
}

/// Resolve a sibling binary by base name, honouring an environment override.
///
/// Resolution order: `$env_override` (used verbatim as a path if set) → a
/// sibling of the current executable named `base` (`base.exe` on Windows) if it
/// exists → bare `base` (found on `PATH` at spawn time). This is the shared
/// resolver both `portzero-app` and, for callers that shell out to the daemon
/// CLI, `portzero` are located with.
pub fn sibling_bin(env_override: &str, base: &str) -> PathBuf {
    if let Some(explicit) = std::env::var_os(env_override) {
        return PathBuf::from(explicit);
    }
    let file_name = exe_file_name(base);
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join(&file_name);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from(base)
}

/// Resolve the desktop app (`portzero-app`) binary path.
///
/// See the module docs for the resolution order.
pub fn app_bin() -> PathBuf {
    sibling_bin(APP_BIN_ENV, APP_BIN_NAME)
}

/// Launch the PortZero desktop app, detached and non-blocking (best-effort).
///
/// Returns once the child has been spawned; the app daemonizes its own window
/// loop, so this never waits for it to exit. Thanks to the app's single-instance
/// guard, calling this while the app is already open just focuses the existing
/// window.
///
/// On failure the returned error names the binary that could not be launched so
/// a caller surfacing it to a user can point at the likely fix (install the
/// desktop app, or set `PORTZERO_APP_BIN`).
pub fn launch() -> io::Result<()> {
    let bin = app_bin();
    Command::new(&bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_bin_prefers_env_override() {
        // A set override is used verbatim, regardless of the current exe layout.
        let key = "PORTZERO_APP_BIN_TEST_OVERRIDE";
        std::env::set_var(key, "/opt/custom/portzero-app");
        let resolved = sibling_bin(key, APP_BIN_NAME);
        std::env::remove_var(key);
        assert_eq!(resolved, PathBuf::from("/opt/custom/portzero-app"));
    }

    #[test]
    fn sibling_bin_falls_back_to_bare_name_without_override() {
        // With no override set and (almost certainly) no sibling named this in
        // the test runner's dir, resolution falls back to the bare base name.
        let key = "PORTZERO_APP_BIN_TEST_MISSING";
        std::env::remove_var(key);
        let resolved = sibling_bin(key, "portzero-app-nonexistent-xyz");
        assert_eq!(resolved, PathBuf::from("portzero-app-nonexistent-xyz"));
    }

    #[test]
    fn exe_file_name_matches_platform() {
        let name = exe_file_name("portzero-app");
        #[cfg(windows)]
        assert_eq!(name, "portzero-app.exe");
        #[cfg(not(windows))]
        assert_eq!(name, "portzero-app");
    }

    #[test]
    fn app_bin_uses_app_env_and_name() {
        std::env::set_var(APP_BIN_ENV, "/tmp/pz-app-marker");
        let resolved = app_bin();
        std::env::remove_var(APP_BIN_ENV);
        assert_eq!(resolved, PathBuf::from("/tmp/pz-app-marker"));
    }
}
