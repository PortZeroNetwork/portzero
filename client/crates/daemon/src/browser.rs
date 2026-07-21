//! Best-effort "open this URL in the user's default browser" helper for the
//! daemon. Mirrors the CLI's opener (`cli/src/browser.rs`) so the daemon can pop
//! a browser tab when a new HTTP/HTTPS tunnel appears, without pulling in a GUI
//! toolkit. Spawns detached and never blocks the discovery loop.

/// Try to open `url` in the default browser. Returns whether a command was
/// spawned successfully. Never panics and never blocks.
pub fn open_url(url: &str) -> bool {
    #[cfg(target_os = "linux")]
    let result = {
        let candidates: &[(&str, &[&str])] = &[
            ("xdg-open", &[]),
            ("gio", &["open"]),
            ("gnome-open", &[]),
            ("kde-open5", &[]),
            ("kde-open", &[]),
            ("sensible-browser", &[]),
        ];

        let mut spawned = Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no browser opener found",
        ));
        for &(command, args) in candidates {
            let mut cmd = std::process::Command::new(command);
            cmd.args(args)
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            match cmd.spawn() {
                Ok(child) => {
                    spawned = Ok(child);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    spawned = Err(error);
                    break;
                }
            }
        }
        spawned
    };

    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let result: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    result.is_ok()
}
