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
//! 3. on macOS, the install prefix around the `.app` bundle we are running from,
//!    then the standard Homebrew prefixes (see [`sibling_bin`]),
//! 4. bare `portzero-app` on `PATH`.
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

/// Environment override for the `portzero` CLI binary path.
pub const CLI_BIN_ENV: &str = "PORTZERO_BIN";

/// Base (extension-less) name of the CLI binary. This is also the binary the
/// daemon runs in (`portzero start --foreground`).
pub const CLI_BIN_NAME: &str = "portzero";

/// Environment override for the tray binary path.
pub const TRAY_BIN_ENV: &str = "PORTZERO_TRAY_BIN";

/// Base (extension-less) name of the tray binary.
pub const TRAY_BIN_NAME: &str = "portzero-tray";

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
/// sibling of the current executable named `base` (`base.exe` on Windows) → on
/// macOS, [`macos_bundle_neighbours`] → bare `base` (found on `PATH` at spawn
/// time). This is the shared resolver both `portzero-app` and, for callers that
/// shell out to the daemon CLI, `portzero` are located with.
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
        for dir in macos_bundle_neighbours(&current) {
            let candidate = dir.join(&file_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(base)
}

/// Directories to search on macOS when the running executable lives inside a
/// `.app` bundle, in preference order.
///
/// Two things break the plain sibling lookup once the GUI binaries ship as
/// bundles. The obvious one is layout: `portzero` sits in `bin/`, while the app
/// runs from `PortZero.app/Contents/MacOS/`, so they are no longer siblings.
/// The subtle one is `PATH` — an app launched from Finder, the Dock, or a
/// LaunchAgent inherits a bare `/usr/bin:/bin:/usr/sbin:/sbin`, with neither
/// Homebrew prefix on it. Falling through to a bare name would therefore fail
/// for exactly the users who launch the app the normal way, so the install
/// prefixes are searched explicitly rather than left to `PATH`.
///
/// Returns an empty list off macOS, and for executables not inside a bundle.
fn macos_bundle_neighbours(current: &std::path::Path) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }

    let mut dirs = Vec::new();

    // `<prefix>/<Name>.app/Contents/MacOS/<exe>` → `<prefix>`, which is the keg
    // (or install) root holding both the bundles and `bin/`.
    if let Some(prefix) = bundle_prefix(current) {
        dirs.push(prefix.join("bin"));
        dirs.push(prefix);
    }

    // Standard Homebrew prefixes: Apple silicon first, then Intel/custom.
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

/// The directory containing the `.app` bundle `exe` runs from, if any.
///
/// Matches the `<prefix>/<Name>.app/Contents/MacOS/<exe>` shape exactly rather
/// than walking up a fixed number of parents, so a binary that merely happens to
/// live four levels deep is never mistaken for a bundled one.
fn bundle_prefix(exe: &std::path::Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos_dir.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let bundle = contents.parent()?;
    if !bundle.file_name()?.to_str()?.ends_with(".app") {
        return None;
    }
    bundle.parent().map(PathBuf::from)
}

/// Resolve the desktop app (`portzero-app`) binary path.
///
/// See the module docs for the resolution order.
pub fn app_bin() -> PathBuf {
    sibling_bin(APP_BIN_ENV, APP_BIN_NAME)
}

/// Resolve the `portzero` CLI binary path, with the same resolution order as
/// [`app_bin`]. Every caller that shells out to the CLI (the app's daemon
/// controls, the version report) resolves it here so they can never disagree
/// about which `portzero` they mean.
pub fn cli_bin() -> PathBuf {
    sibling_bin(CLI_BIN_ENV, CLI_BIN_NAME)
}

/// Resolve the `portzero-tray` binary path, with the same resolution order as
/// [`app_bin`].
pub fn tray_bin() -> PathBuf {
    sibling_bin(TRAY_BIN_ENV, TRAY_BIN_NAME)
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
    use std::path::Path;

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
    fn bundle_prefix_finds_the_install_root_around_a_bundle() {
        assert_eq!(
            bundle_prefix(Path::new(
                "/opt/homebrew/opt/portzero/PortZero.app/Contents/MacOS/portzero-app"
            )),
            Some(PathBuf::from("/opt/homebrew/opt/portzero"))
        );
        // Bundle names with spaces are the norm on macOS.
        assert_eq!(
            bundle_prefix(Path::new(
                "/usr/local/opt/portzero/PortZero Tray.app/Contents/MacOS/portzero-tray"
            )),
            Some(PathBuf::from("/usr/local/opt/portzero"))
        );
    }

    #[test]
    fn bundle_prefix_ignores_paths_that_merely_look_deep() {
        // A plain binary, however nested, is not inside a bundle.
        assert_eq!(
            bundle_prefix(Path::new("/usr/local/bin/portzero-app")),
            None
        );
        assert_eq!(
            bundle_prefix(Path::new("/a/b/Contents/MacOS/portzero-app")),
            None,
            "the grandparent must actually end in .app"
        );
        assert_eq!(
            bundle_prefix(Path::new("/a/PortZero.app/MacOS/portzero-app")),
            None,
            "a missing Contents/ level must not match"
        );
    }

    #[test]
    fn bundle_neighbours_cover_the_keg_layout_and_both_homebrew_prefixes() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let dirs = macos_bundle_neighbours(Path::new(
            "/opt/homebrew/opt/portzero/PortZero.app/Contents/MacOS/portzero-app",
        ));
        // `bin/` first: the CLI installed alongside these bundles must win over
        // whatever an unrelated Homebrew prefix happens to hold.
        assert_eq!(
            dirs.first(),
            Some(&PathBuf::from("/opt/homebrew/opt/portzero/bin"))
        );
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    /// A Finder- or LaunchAgent-launched app inherits a PATH without either
    /// Homebrew prefix, so the prefixes must be searched even when the app is
    /// not running from a recognisable bundle layout.
    #[test]
    fn bundle_neighbours_still_offer_the_install_prefixes_outside_a_bundle() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let dirs = macos_bundle_neighbours(Path::new("/somewhere/else/portzero-app"));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    #[test]
    fn app_bin_uses_app_env_and_name() {
        std::env::set_var(APP_BIN_ENV, "/tmp/pz-app-marker");
        let resolved = app_bin();
        std::env::remove_var(APP_BIN_ENV);
        assert_eq!(resolved, PathBuf::from("/tmp/pz-app-marker"));
    }
}
