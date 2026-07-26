//! Builds the macOS `.app` bundles for `portzero-app` and `portzero-tray`.
//!
//! Why this exists: the release workflow builds both GUI binaries with plain
//! `cargo build --bin ...`, which on macOS produces bare Mach-O executables.
//! macOS has nowhere to read an icon or an activation policy from for such a
//! file, so both showed up with the generic "exec" Unix-executable icon, and the
//! tray — which should live only in the menu bar — got a Dock tile like any
//! ordinary app. Neither is fixable from inside the program: the fix is a real
//! bundle with an `Info.plist`.
//!
//! `portzero-app` is a Tauri app and `cargo tauri build` would bundle it, but
//! the tray is not a Tauri app at all, and the release matrix builds both with
//! one `cargo build` invocation across four targets. Bundling here keeps a
//! single code path for both binaries and leaves the build matrix alone.
//!
//! Usage:
//!   cargo run -p portzero-xtask --bin bundle-macos -- \
//!       --bin-dir target/aarch64-apple-darwin/release \
//!       --out-dir dist
//!
//! Options:
//!   --bin-dir <dir>   where the built binaries are (required)
//!   --out-dir <dir>   where the `.app` bundles are written (required)
//!   --version <ver>   bundle version; defaults to this crate's version
//!   --icon <png>      square source icon; defaults to the app's icon.png
//!
//! Exit status: 0 when every bundle was written, 1 otherwise.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// One `.app` to produce.
struct BundleSpec {
    /// Bundle directory name, e.g. `PortZero.app`.
    dir_name: &'static str,
    /// Binary copied to `Contents/MacOS/`, and the value of `CFBundleExecutable`.
    binary: &'static str,
    /// Reverse-DNS bundle identifier. Unrelated to the launchd job labels
    /// (`cloud.portzero.*`), which name services rather than bundles.
    bundle_id: &'static str,
    /// Shown in Finder, the Dock, and the About box.
    display_name: &'static str,
    /// A menu-bar agent: `LSUIElement`, so it has no Dock tile and no menu bar.
    /// This is the entire reason the tray needs a bundle.
    agent: bool,
}

const BUNDLES: &[BundleSpec] = &[
    BundleSpec {
        dir_name: "PortZero.app",
        binary: "portzero-app",
        bundle_id: "net.portzero.app",
        display_name: "PortZero",
        agent: false,
    },
    BundleSpec {
        dir_name: "PortZero Tray.app",
        binary: "portzero-tray",
        bundle_id: "net.portzero.tray",
        display_name: "PortZero Tray",
        agent: true,
    },
];

/// Oldest macOS the bundles claim to support. Matches what the Rust toolchain
/// targets for `*-apple-darwin` builds.
const MIN_MACOS_VERSION: &str = "10.15";

/// `.icns` base name, referenced by `CFBundleIconFile`.
const ICON_NAME: &str = "PortZero";

fn main() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "bundle-macos builds macOS .app bundles and must run on macOS — it \
             needs `sips` and `iconutil`, which ship only with macOS.\n\n\
             In CI, run this step on a `macos-*` runner."
        );
    }

    let opts = Options::parse(std::env::args().skip(1))?;

    for spec in BUNDLES {
        let binary = opts.bin_dir.join(spec.binary);
        if !binary.is_file() {
            bail!(
                "No `{}` in {}.\n\n\
                 Build it first, e.g.:\n    \
                 cargo build --release --bin {}",
                spec.binary,
                opts.bin_dir.display(),
                spec.binary,
            );
        }
    }

    let icns = build_icns(&opts.icon, &opts.out_dir)
        .context("Failed to build the .icns icon for the macOS bundles")?;

    for spec in BUNDLES {
        let bundle = opts.out_dir.join(spec.dir_name);
        write_bundle(spec, &opts, &icns, &bundle)
            .with_context(|| format!("Failed to build {}", bundle.display()))?;
        println!("built {}", bundle.display());
    }

    // The staging icon lives beside the bundles; it is an input, not an artifact.
    let _ = std::fs::remove_file(&icns);

    Ok(())
}

/// Parsed command line.
struct Options {
    bin_dir: PathBuf,
    out_dir: PathBuf,
    version: String,
    icon: PathBuf,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut bin_dir = None;
        let mut out_dir = None;
        let mut version = None;
        let mut icon = None;

        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            let mut take = |name: &str| {
                args.next()
                    .with_context(|| format!("{name} needs a value, e.g. `{name} <path>`"))
            };
            match arg.as_str() {
                "--bin-dir" => bin_dir = Some(PathBuf::from(take("--bin-dir")?)),
                "--out-dir" => out_dir = Some(PathBuf::from(take("--out-dir")?)),
                "--version" => version = Some(take("--version")?),
                "--icon" => icon = Some(PathBuf::from(take("--icon")?)),
                other => bail!(
                    "Unknown argument `{other}`.\n\n\
                     Usage: bundle-macos --bin-dir <dir> --out-dir <dir> \
                     [--version <ver>] [--icon <png>]"
                ),
            }
        }

        let bin_dir = bin_dir.context(
            "--bin-dir is required: the directory holding the built \
             `portzero-app` and `portzero-tray` binaries.",
        )?;
        let out_dir =
            out_dir.context("--out-dir is required: where the .app bundles should be written.")?;
        std::fs::create_dir_all(&out_dir)
            .with_context(|| format!("Failed to create {}", out_dir.display()))?;

        let icon = match icon {
            Some(icon) => icon,
            None => default_icon()?,
        };
        if !icon.is_file() {
            bail!("Icon source {} does not exist.", icon.display());
        }

        Ok(Self {
            bin_dir,
            out_dir,
            version: bundle_version(version.as_deref().unwrap_or(env!("CARGO_PKG_VERSION"))),
            icon,
        })
    }
}

/// The app crate's icon, resolved relative to this source file so the tool works
/// from any working directory.
fn default_icon() -> Result<PathBuf> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .context("Could not locate the repository root from CARGO_MANIFEST_DIR")?;
    Ok(repo_root.join("client/crates/app/icons/icon.png"))
}

/// Reduce a release version to the dotted-numeric core Apple accepts.
///
/// `CFBundleVersion` and `CFBundleShortVersionString` must be one to three
/// period-separated integers. Unstable builds are versioned `1.2.3-rc.75`, and
/// shipping that verbatim makes `codesign`/`notarytool` reject the bundle — so
/// the prerelease suffix is dropped here rather than at four call sites.
fn bundle_version(version: &str) -> String {
    let core: String = version
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let core = core.trim_end_matches('.');
    if core.is_empty() {
        "0.0.0".to_string()
    } else {
        core.to_string()
    }
}

/// Render `source` into a multi-resolution `.icns` next to the bundles.
///
/// `sips` and `iconutil` are the system tools for this; there is no crate that
/// produces an `.icns` Finder reliably renders at every size.
fn build_icns(source: &Path, out_dir: &Path) -> Result<PathBuf> {
    // (pixel size, iconset file name) — the sizes Finder, the Dock, and the
    // Force Quit dialog actually ask for. 1024px is omitted deliberately: the
    // source art is 512px, so it would only ever be an upscale.
    const SIZES: &[(u32, &str)] = &[
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
    ];

    let iconset = out_dir.join(format!("{ICON_NAME}.iconset"));
    if iconset.exists() {
        std::fs::remove_dir_all(&iconset)
            .with_context(|| format!("Failed to clear {}", iconset.display()))?;
    }
    std::fs::create_dir_all(&iconset)
        .with_context(|| format!("Failed to create {}", iconset.display()))?;

    for (size, name) in SIZES {
        run(
            "sips",
            &[
                "-z".as_ref(),
                size.to_string().as_ref(),
                size.to_string().as_ref(),
                source.as_os_str(),
                "--out".as_ref(),
                iconset.join(name).as_os_str(),
            ],
        )?;
    }

    let icns = out_dir.join(format!("{ICON_NAME}.icns"));
    run(
        "iconutil",
        &[
            "-c".as_ref(),
            "icns".as_ref(),
            iconset.as_os_str(),
            "-o".as_ref(),
            icns.as_os_str(),
        ],
    )?;

    std::fs::remove_dir_all(&iconset)
        .with_context(|| format!("Failed to clean up {}", iconset.display()))?;
    Ok(icns)
}

/// Write one complete `.app` bundle.
fn write_bundle(spec: &BundleSpec, opts: &Options, icns: &Path, bundle: &Path) -> Result<()> {
    if bundle.exists() {
        std::fs::remove_dir_all(bundle)
            .with_context(|| format!("Failed to replace {}", bundle.display()))?;
    }

    let contents = bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&macos_dir)
        .with_context(|| format!("Failed to create {}", macos_dir.display()))?;
    std::fs::create_dir_all(&resources)
        .with_context(|| format!("Failed to create {}", resources.display()))?;

    let dest = macos_dir.join(spec.binary);
    std::fs::copy(opts.bin_dir.join(spec.binary), &dest)
        .with_context(|| format!("Failed to copy {} into the bundle", spec.binary))?;
    make_executable(&dest)?;

    std::fs::copy(icns, resources.join(format!("{ICON_NAME}.icns")))
        .context("Failed to copy the icon into the bundle")?;

    std::fs::write(contents.join("Info.plist"), info_plist(spec, &opts.version))
        .context("Failed to write Info.plist")?;

    // Classic-era metadata that some Finder/LaunchServices paths still consult.
    std::fs::write(contents.join("PkgInfo"), "APPL????").context("Failed to write PkgInfo")?;

    Ok(())
}

/// Ensure the copied binary keeps its executable bit.
fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .with_context(|| format!("Failed to stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("Failed to chmod {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Render a bundle's `Info.plist`.
fn info_plist(spec: &BundleSpec, version: &str) -> String {
    // `LSUIElement` is what keeps the tray out of the Dock and out of the
    // app switcher; without a bundle there is nowhere to declare it, which is
    // why the tray used to take a Dock tile it has no window to go with.
    let agent_keys = if spec.agent {
        "    <key>LSUIElement</key>\n    <true/>\n"
    } else {
        ""
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>CFBundleName</key>
    <string>{name}</string>
    <key>CFBundleDisplayName</key>
    <string>{name}</string>
    <key>CFBundleIdentifier</key>
    <string>{id}</string>
    <key>CFBundleExecutable</key>
    <string>{exe}</string>
    <key>CFBundleIconFile</key>
    <string>{icon}</string>
    <key>CFBundleShortVersionString</key>
    <string>{version}</string>
    <key>CFBundleVersion</key>
    <string>{version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>{min_os}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
{agent_keys}</dict>
</plist>
"#,
        name = xml_escape(spec.display_name),
        id = xml_escape(spec.bundle_id),
        exe = xml_escape(spec.binary),
        icon = ICON_NAME,
        version = xml_escape(version),
        min_os = MIN_MACOS_VERSION,
        agent_keys = agent_keys,
    )
}

/// Escape text for an XML element body.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Run a command, failing with its own output when it fails.
fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run `{program}`. Is it installed and on PATH?"))?;
    if !out.status.success() {
        bail!(
            "`{program}` failed ({}).\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstable_versions_are_reduced_to_apples_dotted_numeric_form() {
        // codesign/notarytool reject a CFBundleVersion with a prerelease suffix.
        assert_eq!(bundle_version("1.1.6-rc.75"), "1.1.6");
        assert_eq!(bundle_version("1.1.6"), "1.1.6");
        assert_eq!(bundle_version("2.0"), "2.0");
    }

    #[test]
    fn an_unparseable_version_still_yields_a_valid_one() {
        assert_eq!(bundle_version("nightly"), "0.0.0");
        assert_eq!(bundle_version(""), "0.0.0");
        assert_eq!(bundle_version("1.2.3."), "1.2.3");
    }

    #[test]
    fn only_the_tray_is_a_background_agent() {
        let app = BUNDLES.iter().find(|b| b.binary == "portzero-app").unwrap();
        let tray = BUNDLES
            .iter()
            .find(|b| b.binary == "portzero-tray")
            .unwrap();
        assert!(!app.agent, "the desktop app must keep its Dock tile");
        assert!(tray.agent, "the tray must not take a Dock tile");
    }

    #[test]
    fn the_tray_plist_declares_lsuielement_and_the_app_does_not() {
        let tray = BUNDLES
            .iter()
            .find(|b| b.binary == "portzero-tray")
            .unwrap();
        let app = BUNDLES.iter().find(|b| b.binary == "portzero-app").unwrap();
        assert!(info_plist(tray, "1.2.3").contains("<key>LSUIElement</key>"));
        assert!(!info_plist(app, "1.2.3").contains("<key>LSUIElement</key>"));
    }

    #[test]
    fn every_plist_names_its_executable_icon_and_version() {
        for spec in BUNDLES {
            let plist = info_plist(spec, "1.2.3");
            assert!(
                plist.contains(&format!("<string>{}</string>", spec.binary)),
                "{} plist does not name its executable",
                spec.dir_name
            );
            assert!(
                plist.contains(&format!("<string>{ICON_NAME}</string>")),
                "{} plist does not name its icon",
                spec.dir_name
            );
            assert!(plist.contains("<string>1.2.3</string>"));
            assert!(plist.trim_end().ends_with("</plist>"));
        }
    }

    #[test]
    fn bundle_ids_are_unique() {
        let mut ids: Vec<_> = BUNDLES.iter().map(|b| b.bundle_id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "two bundles share an identifier");
    }
}
