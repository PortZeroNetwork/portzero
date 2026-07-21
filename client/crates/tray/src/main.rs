//! PortZero system-tray companion binary. All logic lives in the library crate
//! (`portzero_tray`); this is a thin entry point so the modules can also be
//! exercised by integration tests.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    portzero_daemon::install_default_crypto_provider();

    portzero_tray::run()
}
