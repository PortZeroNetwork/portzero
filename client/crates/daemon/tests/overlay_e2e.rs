//! Privileged real-TUN and trust-store e2e tests for the virtual overlay
//! network.
//!
//! * [`real_tun_overlay`] is gated behind `PORTZERO_REQUIRE_REAL_TUN_E2E=1` on
//!   every platform, then behind the platform privilege check (root on Unix,
//!   elevated Administrator on Windows). Plain `cargo test` skips cleanly; run
//!   it via `just e2e` when you want the real TUN/Wintun path. This verifies
//!   both DNS and a real OS TCP socket talking to a VIP through the tunnel.
//!
//! * [`real_tun_overlay_trust_lifecycle`] additionally requires
//!   `PORTZERO_REQUIRE_TRUST_LIFECYCLE=1`. It brings the real overlay up with
//!   trust-store installation enabled, verifies the CA landed in the OS trust
//!   store, tears the overlay down, runs the real trust uninstall, and asserts
//!   the CA is gone.
//!
//! The unprivileged in-process scenarios live in `overlay_unprivileged.rs` and
//! `overlay_tls.rs`. Every wait here is bounded by a timeout — there is no
//! unbounded blocking.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use portzero_daemon::net::service_table::ServiceTable;

#[cfg(target_os = "linux")]
use common::LinuxHostRoute;
#[cfg(target_os = "macos")]
use common::MacHostRoute;
#[cfg(target_os = "windows")]
use common::WindowsHostRoute;
use common::{assert_tcp_echo, dns_query_a, spawn_echo_backend, TEST_TIMEOUT};

/// Privileged real-TUN e2e. Skips unless explicitly opted in, so ordinary
/// `cargo test` stays side-effect safe even if it is run as root/Admin.
///
/// This is the path behind `just e2e`: it creates the real OS TUN/Wintun
/// interface, registers a service, resolves its overlay name via the embedded
/// DNS server, then opens a normal OS TCP socket to the VIP and verifies that
/// bytes pass through the tunnel to the backend and back.
#[cfg(any(unix, target_os = "windows"))]
#[test]
fn real_tun_overlay() {
    if std::env::var("PORTZERO_REQUIRE_REAL_TUN_E2E").as_deref() != Ok("1") {
        println!(
            "real_tun_overlay: skipped: set PORTZERO_REQUIRE_REAL_TUN_E2E=1 to run privileged real-TUN e2e"
        );
        return;
    }

    #[cfg(unix)]
    {
        // SAFETY: geteuid is always safe to call.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            println!("real_tun_overlay: skipped: requires root (euid={euid})");
            return;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let elevated = std::process::Command::new("fltmc")
            .arg("filters")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            elevated,
            "real_tun_overlay requires an elevated Administrator process on Windows"
        );
    }

    #[cfg(target_os = "windows")]
    if std::env::var("PORTZERO_REAL_TUN_E2E_CHILD").as_deref() != Ok("1") {
        run_windows_real_tun_overlay_child();
        return;
    }

    run_real_tun_overlay_worker();
}

#[cfg(target_os = "windows")]
fn run_windows_real_tun_overlay_child() {
    use std::process::Stdio;
    use std::time::Instant;

    let progress_path = std::env::temp_dir().join(format!(
        "portzero-real-tun-e2e-{}.progress",
        std::process::id()
    ));
    let _ = std::fs::write(&progress_path, RealTunStep::NotStarted.as_str());

    let exe = std::env::current_exe().expect("failed to locate current test executable");
    println!(
        "real_tun_overlay: starting child process with {TEST_TIMEOUT:?} watchdog: {}",
        exe.display()
    );

    let mut child = std::process::Command::new(exe)
        .args(["--exact", "real_tun_overlay", "--nocapture"])
        .env("PORTZERO_REQUIRE_REAL_TUN_E2E", "1")
        .env("PORTZERO_REAL_TUN_E2E_CHILD", "1")
        .env("PORTZERO_REAL_TUN_E2E_PROGRESS_FILE", &progress_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn real_tun_overlay child process");

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = std::fs::remove_file(&progress_path);
                return;
            }
            Ok(Some(status)) => {
                let last_step = read_real_tun_progress_file(&progress_path);
                let _ = std::fs::remove_file(&progress_path);
                panic!("real_tun_overlay child exited with {status}; last step: {last_step}");
            }
            Ok(None) if started.elapsed() >= TEST_TIMEOUT => {
                let last_step = read_real_tun_progress_file(&progress_path);
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&progress_path);
                panic!(
                    "real_tun_overlay child timed out after {TEST_TIMEOUT:?}; last step: {last_step}"
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                let last_step = read_real_tun_progress_file(&progress_path);
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&progress_path);
                panic!("failed to poll real_tun_overlay child: {e}; last step: {last_step}");
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn read_real_tun_progress_file(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(any(unix, target_os = "windows"))]
fn run_real_tun_overlay_worker() {
    println!("real_tun_overlay: starting worker with {TEST_TIMEOUT:?} watchdog");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build real_tun_overlay runtime");
            rt.block_on(real_tun_overlay_inner())
        })
        .map_err(|panic| {
            if let Some(s) = panic.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else {
                "real_tun_overlay worker panicked with non-string payload".to_string()
            }
        })
        .and_then(|inner| inner);

        let _ = tx.send(result);
    });

    match rx.recv_timeout(TEST_TIMEOUT) {
        Ok(Ok(())) => {
            println!("real_tun_overlay: passed");
        }
        Ok(Err(e)) => {
            panic!("{e}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "real_tun_overlay timed out after {TEST_TIMEOUT:?}; last step: {}",
                REAL_TUN_PROGRESS
                    .load(std::sync::atomic::Ordering::Relaxed)
                    .as_str()
            );
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("real_tun_overlay worker exited without reporting a result");
        }
    }
}

#[cfg(any(unix, target_os = "windows"))]
static REAL_TUN_PROGRESS: RealTunProgress = RealTunProgress(std::sync::atomic::AtomicU8::new(
    RealTunStep::NotStarted as u8,
));

#[cfg(any(unix, target_os = "windows"))]
struct RealTunProgress(std::sync::atomic::AtomicU8);

#[cfg(any(unix, target_os = "windows"))]
impl RealTunProgress {
    fn store(&self, step: RealTunStep) {
        self.0
            .store(step as u8, std::sync::atomic::Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        if let Ok(path) = std::env::var("PORTZERO_REAL_TUN_E2E_PROGRESS_FILE") {
            let _ = std::fs::write(path, step.as_str());
        }
    }

    fn load(&self, ordering: std::sync::atomic::Ordering) -> RealTunStep {
        RealTunStep::from_u8(self.0.load(ordering))
    }
}

#[cfg(any(unix, target_os = "windows"))]
#[derive(Clone, Copy)]
enum RealTunStep {
    NotStarted = 0,
    StartingOverlay = 1,
    SpawningBackend = 2,
    UpdatingServices = 3,
    QueryingDns = 4,
    InstallingHostRoute = 5,
    CheckingTcpEcho = 6,
    ShuttingDown = 7,
    LoadingLocalCa = 8,
    InstallingTrust = 9,
    CreatingTunDevice = 10,
    BuildingTlsConfig = 11,
    SpawningVirtualStack = 12,
    StartingDnsServer = 13,
    InstallingResolverConfig = 14,
    OverlayStarted = 15,
}

#[cfg(any(unix, target_os = "windows"))]
impl RealTunStep {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::StartingOverlay,
            2 => Self::SpawningBackend,
            3 => Self::UpdatingServices,
            4 => Self::QueryingDns,
            5 => Self::InstallingHostRoute,
            6 => Self::CheckingTcpEcho,
            7 => Self::ShuttingDown,
            8 => Self::LoadingLocalCa,
            9 => Self::InstallingTrust,
            10 => Self::CreatingTunDevice,
            11 => Self::BuildingTlsConfig,
            12 => Self::SpawningVirtualStack,
            13 => Self::StartingDnsServer,
            14 => Self::InstallingResolverConfig,
            15 => Self::OverlayStarted,
            _ => Self::NotStarted,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not started",
            Self::StartingOverlay => "starting overlay",
            Self::SpawningBackend => "spawning echo backend",
            Self::UpdatingServices => "updating overlay services",
            Self::QueryingDns => "querying embedded DNS",
            Self::InstallingHostRoute => "installing host route",
            Self::CheckingTcpEcho => "checking TCP echo through tunnel",
            Self::ShuttingDown => "shutting down overlay",
            Self::LoadingLocalCa => "loading local CA",
            Self::InstallingTrust => "installing local CA trust",
            Self::CreatingTunDevice => "creating TUN device",
            Self::BuildingTlsConfig => "building TLS config",
            Self::SpawningVirtualStack => "spawning virtual stack",
            Self::StartingDnsServer => "starting DNS server",
            Self::InstallingResolverConfig => "installing resolver config",
            Self::OverlayStarted => "overlay started",
        }
    }
}

#[cfg(any(unix, target_os = "windows"))]
impl From<portzero_daemon::net::overlay::OverlayStartStep> for RealTunStep {
    fn from(step: portzero_daemon::net::overlay::OverlayStartStep) -> Self {
        match step {
            portzero_daemon::net::overlay::OverlayStartStep::LoadingLocalCa => Self::LoadingLocalCa,
            portzero_daemon::net::overlay::OverlayStartStep::InstallingTrust => {
                Self::InstallingTrust
            }
            portzero_daemon::net::overlay::OverlayStartStep::CreatingTunDevice => {
                Self::CreatingTunDevice
            }
            portzero_daemon::net::overlay::OverlayStartStep::BuildingTlsConfig => {
                Self::BuildingTlsConfig
            }
            portzero_daemon::net::overlay::OverlayStartStep::SpawningVirtualStack => {
                Self::SpawningVirtualStack
            }
            portzero_daemon::net::overlay::OverlayStartStep::StartingDnsServer => {
                Self::StartingDnsServer
            }
            portzero_daemon::net::overlay::OverlayStartStep::InstallingResolverConfig => {
                Self::InstallingResolverConfig
            }
            portzero_daemon::net::overlay::OverlayStartStep::Complete => Self::OverlayStarted,
        }
    }
}

#[cfg(any(unix, target_os = "windows"))]
async fn real_tun_overlay_inner() -> Result<(), String> {
    use portzero_daemon::net::overlay::{OverlayConfig, OverlayNetwork};
    use portzero_daemon::net::tun_device::TunConfig;

    println!("real_tun_overlay: bringing up real overlay");
    REAL_TUN_PROGRESS.store(RealTunStep::StartingOverlay);
    let result = tokio::time::timeout(TEST_TIMEOUT, async {
        // Use the embedded DNS on a high local port to avoid clashing with the
        // system resolver during the test.
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        let mut tun = TunConfig::default();

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        let tun = TunConfig::default();

        #[cfg(target_os = "linux")]
        {
            // Do not collide with the real daemon's default TUN interface
            // (`deven0`). `just e2e` is commonly run while the daemon is
            // installed or active.
            tun.name = Some("portzero-e2e".to_string());
            // Keep the test gateway inside 10.254.0.0/16 without reusing the
            // daemon's production gateway address.
            tun.address = Ipv4Addr::new(10, 254, 250, 1);
        }

        #[cfg(target_os = "windows")]
        {
            // Do not collide with the real daemon's default Wintun adapter
            // (`deven0`). Wintun permits only one active session per adapter,
            // and `just e2e` is commonly run while the daemon is installed.
            tun.name = Some("portzero-e2e".to_string());
            // Windows rejects assigning the same static IPv4 address to two
            // adapters. Keep this test off the production gateway address while
            // staying inside 10.254.0.0/16 so VIP routes still target the real
            // overlay subnet.
            tun.address = Ipv4Addr::new(10, 254, 250, 1);
        }

        let config = OverlayConfig {
            dns_listen: "127.0.0.1:53000".parse().unwrap(),
            tun,
            install_trust: false,
            ..Default::default()
        };

        let overlay = OverlayNetwork::start_with_progress(
            config,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            |step| REAL_TUN_PROGRESS.store(step.into()),
        )
        .await
        .expect("overlay start failed under root");

        // Register a service so the stack installs a listener and DNS answers.
        REAL_TUN_PROGRESS.store(RealTunStep::SpawningBackend);
        let backend_addr = spawn_echo_backend().await;
        let mut table = ServiceTable::new();
        table.register("rooted".to_string(), backend_addr, 5432, 0);
        REAL_TUN_PROGRESS.store(RealTunStep::UpdatingServices);
        overlay
            .update_services(table)
            .await
            .expect("update_services failed");

        // Give the stack a moment to apply listeners, then resolve via the real
        // embedded DNS.
        tokio::time::sleep(Duration::from_millis(200)).await;
        REAL_TUN_PROGRESS.store(RealTunStep::QueryingDns);
        let ip = dns_query_a("127.0.0.1:53000".parse().unwrap(), "rooted.portzero.local").await;
        let ip = ip.expect("real overlay DNS did not resolve service");
        REAL_TUN_PROGRESS.store(RealTunStep::InstallingHostRoute);
        #[cfg(target_os = "linux")]
        let _route = LinuxHostRoute::install(ip, "portzero-e2e");
        #[cfg(target_os = "macos")]
        let _route = MacHostRoute::install(ip, overlay.link_name());
        #[cfg(target_os = "windows")]
        let _route = WindowsHostRoute::install(ip, "portzero-e2e");

        // Prove this is a real end-to-end tunnel check: a normal OS TCP socket
        // connects to the service VIP, the packet enters the TUN/Wintun
        // interface, the stack proxies it to the localhost backend, and the echo
        // comes back through the same path.
        let tunnel_addr = SocketAddr::from((ip, 5432));
        REAL_TUN_PROGRESS.store(RealTunStep::CheckingTcpEcho);
        assert_tcp_echo(tunnel_addr, b"hello real tun overlay").await;

        REAL_TUN_PROGRESS.store(RealTunStep::ShuttingDown);
        overlay.shutdown().await;
    })
    .await;

    result
        .map_err(|_| "real_tun_overlay async phase timed out".to_string())
        .map(|_| ())
}

/// Privileged trust-store lifecycle e2e: brings the real overlay up with
/// `install_trust: true` (the production default), asserts the PortZero local CA
/// actually landed in this host's OS trust store, tears the overlay down, runs
/// the real `trust uninstall`, and asserts the CA is GONE.
///
/// This closes the coverage gap the launch audit flagged: trust-store *install*
/// is disabled in [`real_tun_overlay`] (`install_trust: false`) to keep ordinary
/// `just e2e` side-effect free, so nothing else in the suite proves the CA is
/// installed on a real machine — and *nothing at all* proved uninstall removes
/// it. Leftover trusted-CA residue after uninstall is the loud-complaint bug
/// class, so it gets its own asserted test.
///
/// Double-gated on purpose: it mutates the host's real trust store, so it needs
/// BOTH `PORTZERO_REQUIRE_REAL_TUN_E2E=1` (the existing real-TUN opt-in) AND
/// `PORTZERO_REQUIRE_TRUST_LIFECYCLE=1`. Plain `just e2e` sets only the former,
/// so this stays skipped there and runs only where a caller (the VM harness, or
/// `just e2e-trust`) explicitly opts into trust-store mutation. Also requires
/// root (Unix) / elevated Administrator (Windows), like [`real_tun_overlay`].
#[cfg(any(unix, target_os = "windows"))]
#[test]
fn real_tun_overlay_trust_lifecycle() {
    if std::env::var("PORTZERO_REQUIRE_REAL_TUN_E2E").as_deref() != Ok("1")
        || std::env::var("PORTZERO_REQUIRE_TRUST_LIFECYCLE").as_deref() != Ok("1")
    {
        println!(
            "real_tun_overlay_trust_lifecycle: skipped: set PORTZERO_REQUIRE_REAL_TUN_E2E=1 AND \
             PORTZERO_REQUIRE_TRUST_LIFECYCLE=1 to run the privileged trust-store lifecycle e2e"
        );
        return;
    }

    #[cfg(unix)]
    {
        // SAFETY: geteuid is always safe to call.
        let euid = unsafe { libc::geteuid() };
        if euid != 0 {
            println!("real_tun_overlay_trust_lifecycle: skipped: requires root (euid={euid})");
            return;
        }
    }
    #[cfg(target_os = "windows")]
    {
        let elevated = std::process::Command::new("fltmc")
            .arg("filters")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(
            elevated,
            "real_tun_overlay_trust_lifecycle requires an elevated Administrator process on Windows"
        );
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build trust-lifecycle runtime");
            rt.block_on(real_tun_overlay_trust_lifecycle_inner())
        })
        .map_err(|panic| {
            if let Some(s) = panic.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = panic.downcast_ref::<String>() {
                s.clone()
            } else {
                "trust-lifecycle worker panicked with non-string payload".to_string()
            }
        })
        .and_then(|inner| inner);
        let _ = tx.send(result);
    });

    // Generous budget: real `update-ca-certificates` / `update-ca-trust` /
    // `security` / certutil invocations are slower than the in-memory paths.
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => println!("real_tun_overlay_trust_lifecycle: passed"),
        Ok(Err(e)) => panic!("{e}"),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("real_tun_overlay_trust_lifecycle timed out")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("real_tun_overlay_trust_lifecycle worker exited without reporting a result")
        }
    }
}

#[cfg(any(unix, target_os = "windows"))]
async fn real_tun_overlay_trust_lifecycle_inner() -> Result<(), String> {
    use portzero_daemon::net::overlay::{OverlayConfig, OverlayNetwork};
    use portzero_daemon::net::tun_device::TunConfig;
    use portzero_daemon::tls::ca::LocalCa;
    use portzero_daemon::tls::trust;

    // The on-disk CA PEM the trust store install/verify/uninstall all operate on.
    // `load_or_create` is what production startup uses, so the path is identical
    // to a real install.
    LocalCa::load_or_create().map_err(|e| format!("load_or_create CA failed: {e:#}"))?;
    let ca_path =
        LocalCa::ca_cert_path().map_err(|e| format!("resolve ca_cert_path failed: {e:#}"))?;

    // Keep the test overlay off the production interface/gateway, exactly like
    // `real_tun_overlay` does, so it can run while the daemon is installed.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let mut tun = TunConfig::default();
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let tun = TunConfig::default();
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        tun.name = Some("portzero-e2e".to_string());
        tun.address = Ipv4Addr::new(10, 254, 250, 1);
    }

    let config = OverlayConfig {
        dns_listen: "127.0.0.1:53000".parse().unwrap(),
        tun,
        // The whole point: exercise the real trust-store install on startup.
        install_trust: true,
        ..Default::default()
    };

    let overlay = tokio::time::timeout(
        TEST_TIMEOUT,
        OverlayNetwork::start(config, std::sync::Arc::new(tokio::sync::Notify::new())),
    )
    .await
    .map_err(|_| "overlay start (install_trust) timed out".to_string())?
    .map_err(|e| format!("overlay start failed under root: {e:#}"))?;

    // 1. The CA must actually be in the OS trust store now.
    let report = trust::verify_installation(&ca_path)
        .map_err(|e| format!("verify_installation after install failed: {e:#}"))?;
    if !report.is_clean() {
        overlay.shutdown().await;
        return Err(format!(
            "CA was NOT fully installed after overlay startup; missing stores: {:?}",
            report.missing
        ));
    }
    println!("real_tun_overlay_trust_lifecycle: CA installed and verified in every trust store");

    // Tear the overlay (and its TUN) down before removing trust, mirroring a
    // real uninstall: stop the daemon, then remove the CA.
    overlay.shutdown().await;

    // 2. Run the real uninstall and assert the CA is gone from every store.
    trust::uninstall().map_err(|e| format!("trust uninstall failed: {e:#}"))?;
    let report = trust::verify_installation(&ca_path)
        .map_err(|e| format!("verify_installation after uninstall failed: {e:#}"))?;
    if report.is_clean() {
        return Err(
            "CA is STILL present in the trust store after `trust uninstall` — leftover \
             trusted-CA residue is exactly the bug this test guards against"
                .to_string(),
        );
    }
    println!(
        "real_tun_overlay_trust_lifecycle: CA removed from trust store after uninstall \
         (now-missing stores: {:?})",
        report.missing
    );

    Ok(())
}

/// Non-Unix builds still compile and report this privileged Unix TUN scenario
/// as skipped when they do not support the privileged test path.
#[cfg(not(any(unix, target_os = "windows")))]
#[tokio::test]
async fn real_tun_overlay() {
    println!("real_tun_overlay: skipped: unsupported platform");
}
