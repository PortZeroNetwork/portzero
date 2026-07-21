//! `portzero wait <tunnel-domain>`: block until a tunnel is up (and, with
//! `--healthy` or a declared `PZ_HEALTH_PATH`, until its health path returns
//! 2xx). The readiness gate for CD smoke tests and Playwright `webServer`
//! blocks.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use portzero_daemon::discovery_loop::DaemonConfig;

use crate::export::{lookup_tunnel, TunnelUrl};

/// Default readiness timeout when `--timeout` is not given.
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// How often to re-check the tunnel / poll the health path.
const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// The outcome of a single readiness probe.
enum Probe {
    /// The tunnel is up (and healthy, if that was requested).
    Ready,
    /// The tunnel exists but is not ready yet (not discovered, health non-2xx,
    /// or awaiting cloud review). Carries a short reason for the timeout message.
    NotYet(String),
    /// The tunnel is paused at the edge (a cloud-side, edge-only pause). This is
    /// distinct from "dead": the backend may be fine, but the edge is holding
    /// traffic. Reported distinctly so callers don't confuse it with a crash.
    Paused,
}

/// `portzero wait <domain> [--healthy] [--timeout <secs>]`.
pub async fn wait(domain: &str, healthy: bool, timeout: Option<u64>) -> Result<()> {
    let config = DaemonConfig::load();
    wait_with_config(&config, domain, healthy, timeout).await
}

/// Core of `wait()`, parameterized over the `DaemonConfig` so tests can point
/// it at a throwaway state directory instead of the real `~/.portzero`.
async fn wait_with_config(
    config: &DaemonConfig,
    domain: &str,
    healthy: bool,
    timeout: Option<u64>,
) -> Result<()> {
    let target = domain.trim().to_string();
    let timeout = Duration::from_secs(timeout.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let deadline = Instant::now() + timeout;

    let client = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(false)
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

    // Set on every non-ready probe; read only in the timeout branch. Every path
    // that reaches the timeout check first passes through the `NotYet` arm
    // (`Ready` returns, `Paused` bails), so it is always assigned by then.
    let mut last_reason: String;

    loop {
        match probe(config, &client, &target, healthy).await {
            Probe::Ready => {
                println!("{target} is ready");
                return Ok(());
            }
            Probe::Paused => {
                // Fail fast and distinctly: waiting will not help a paused tunnel.
                bail!(
                    "Tunnel '{target}' is paused at the edge (edge-only pause, cloud-side \
                     feature). It is not dead — resume it in the dashboard \
                     (https://app.portzero.cloud) to continue. `portzero wait` will not \
                     block for a paused tunnel."
                );
            }
            Probe::NotYet(reason) => {
                last_reason = reason;
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "Timed out after {}s waiting for '{target}' ({last_reason}). \
                 Is the process/container running with PZ_TUNNEL set{}? Check `portzero status`.",
                timeout.as_secs(),
                if healthy {
                    " and serving its health path"
                } else {
                    ""
                }
            );
        }

        // Sleep, but never overshoot the deadline.
        let remaining = deadline.saturating_duration_since(Instant::now());
        tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
    }
}

/// One readiness probe: is the tunnel discovered, not paused, and (if required)
/// serving 2xx on its health path?
async fn probe(
    config: &DaemonConfig,
    client: &reqwest::Client,
    domain: &str,
    healthy: bool,
) -> Probe {
    // A paused status short-circuits everything (distinct from dead).
    if edge_status(config, domain).as_deref() == Some("paused") {
        return Probe::Paused;
    }

    let Some(tunnel) = lookup_tunnel(config, domain) else {
        return Probe::NotYet("tunnel not discovered yet".to_string());
    };

    // Cloud tunnels awaiting owner review are up locally but not publicly
    // routable; keep waiting and say so.
    if let Some(status) = edge_status(config, domain) {
        if status == "pending_review" {
            return Probe::NotYet("awaiting cloud review (pending_review)".to_string());
        }
        if status == "denied" {
            return Probe::NotYet("cloud review denied (denied)".to_string());
        }
    }

    // Health is polled when explicitly requested OR when the endpoint declared
    // a PZ_HEALTH_PATH. When --healthy is set without a declared path, default
    // to "/".
    let health_path = tunnel.health_path.clone();
    let must_poll = healthy || health_path.is_some();
    if !must_poll {
        return Probe::Ready;
    }

    let path = health_path.unwrap_or_else(|| "/".to_string());
    match poll_health(client, &tunnel, &path).await {
        Ok(true) => Probe::Ready,
        Ok(false) => Probe::NotYet(format!("health path {path} not yet 2xx")),
        Err(reason) => Probe::NotYet(reason),
    }
}

/// GET the tunnel's health URL; `Ok(true)` on a 2xx response.
async fn poll_health(
    client: &reqwest::Client,
    tunnel: &TunnelUrl,
    path: &str,
) -> Result<bool, String> {
    let url = format!("{}{}", tunnel.url.trim_end_matches('/'), path);
    match client.get(&url).send().await {
        Ok(resp) => Ok(resp.status().is_success()),
        Err(e) => Err(format!("health request to {url} failed: {e}")),
    }
}

/// Read this tunnel's cloud edge status (`published` / `pending_review` /
/// `denied` / `paused`) from `cloud_route_status.json`, if present.
///
/// `.portzero.local` overlay tunnels never carry an edge status, so this returns
/// `None` for them.
fn edge_status(config: &DaemonConfig, domain: &str) -> Option<String> {
    let path = config.cloud_route_status_path();
    let content = std::fs::read_to_string(path).ok()?;
    let map: std::collections::HashMap<String, String> = serde_json::from_str(&content).ok()?;
    map.into_iter()
        .find(|(d, _)| d.eq_ignore_ascii_case(domain))
        .map(|(_, status)| status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portzero_daemon::discovery::ServiceSource;
    use portzero_daemon::route_table::OverlayRoute;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Build a `DaemonConfig` pointed at a fresh, uniquely-named temp
    /// directory so tests never collide or touch a real `~/.portzero`.
    fn temp_config(tag: &str) -> DaemonConfig {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "portzero-wait-test-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp state dir");
        DaemonConfig {
            state_dir: dir,
            ..DaemonConfig::default()
        }
    }

    fn cleanup(config: &DaemonConfig) {
        let _ = std::fs::remove_dir_all(&config.state_dir);
    }

    fn write_cloud_route_status(config: &DaemonConfig, domain: &str, status: &str) {
        let mut map = std::collections::HashMap::new();
        map.insert(domain.to_string(), status.to_string());
        std::fs::write(
            config.cloud_route_status_path(),
            serde_json::to_string(&map).unwrap(),
        )
        .unwrap();
    }

    fn overlay_route(domain: &str, service_port: u16, health_path: Option<&str>) -> OverlayRoute {
        OverlayRoute {
            domain: domain.to_string(),
            domain_template: domain.to_string(),
            substitutions: Default::default(),
            service_port,
            real_addr: "127.0.0.1:0".to_string(),
            health_path: health_path.map(|s| s.to_string()),
            pid: 1234,
            source: ServiceSource::Process { cwd: None },
        }
    }

    fn write_overlay(config: &DaemonConfig, routes: Vec<OverlayRoute>) {
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes,
        };
        state.save(&config.overlay_path()).unwrap();
    }

    #[test]
    fn edge_status_none_when_no_file() {
        let config = temp_config("edge-none");
        assert_eq!(edge_status(&config, "api.example.portzero.cloud"), None);
        cleanup(&config);
    }

    #[test]
    fn edge_status_matches_case_insensitively() {
        let config = temp_config("edge-ci");
        write_cloud_route_status(&config, "Api.Example.Portzero.Cloud", "paused");
        assert_eq!(
            edge_status(&config, "api.example.portzero.cloud"),
            Some("paused".to_string())
        );
        cleanup(&config);
    }

    #[tokio::test]
    async fn probe_paused_short_circuits_before_lookup() {
        let config = temp_config("probe-paused");
        write_cloud_route_status(&config, "api.example.portzero.cloud", "paused");
        let client = reqwest::Client::new();
        let outcome = probe(&config, &client, "api.example.portzero.cloud", false).await;
        assert!(matches!(outcome, Probe::Paused));
        cleanup(&config);
    }

    #[tokio::test]
    async fn probe_not_yet_when_tunnel_undiscovered() {
        let config = temp_config("probe-undiscovered");
        let client = reqwest::Client::new();
        let outcome = probe(&config, &client, "nope.portzero.local", false).await;
        match outcome {
            Probe::NotYet(reason) => assert!(reason.contains("not discovered")),
            _ => panic!("expected NotYet"),
        }
        cleanup(&config);
    }

    #[tokio::test]
    async fn probe_ready_when_discovered_and_no_health_required() {
        let config = temp_config("probe-ready");
        write_overlay(
            &config,
            vec![overlay_route("web.portzero.local", 8080, None)],
        );
        let client = reqwest::Client::new();
        let outcome = probe(&config, &client, "web.portzero.local", false).await;
        assert!(matches!(outcome, Probe::Ready));
        cleanup(&config);
    }

    #[tokio::test]
    async fn probe_not_yet_when_health_declared_and_unreachable() {
        let config = temp_config("probe-health-unreachable");
        write_overlay(
            &config,
            vec![overlay_route("db.portzero.local", 1, Some("/health"))],
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        // "db.portzero.local" won't resolve in a sandbox with no overlay running,
        // so the health probe fails with a DNS/connect error — exercising the
        // NotYet(reason) path without needing a real overlay network.
        let outcome = probe(&config, &client, "db.portzero.local", false).await;
        match outcome {
            Probe::NotYet(reason) => assert!(!reason.is_empty()),
            _ => panic!("expected NotYet due to unreachable health path"),
        }
        cleanup(&config);
    }

    #[tokio::test]
    async fn poll_health_true_on_2xx() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let client = reqwest::Client::new();
        let tunnel = TunnelUrl {
            domain: "test.portzero.local".to_string(),
            url: format!("http://{addr}"),
            health_path: None,
        };
        let result = poll_health(&client, &tunnel, "/health").await;
        handle.join().unwrap();
        assert_eq!(result, Ok(true));
    }

    #[tokio::test]
    async fn poll_health_err_on_connection_failure() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let tunnel = TunnelUrl {
            domain: "dead.portzero.local".to_string(),
            // Port 1 is reserved (tcpmux) and virtually never has a listener,
            // so the connection is refused immediately.
            url: "http://127.0.0.1:1".to_string(),
            health_path: None,
        };
        let result = poll_health(&client, &tunnel, "/health").await;
        assert!(result.is_err());
    }

    // wait_with_config(): only scenarios that resolve without a real sleep. A
    // `--timeout 0` request means the deadline has already passed by the time
    // the loop checks it, so the timeout branch fires on the first iteration
    // without ever hitting `tokio::time::sleep`.

    #[tokio::test]
    async fn wait_bails_immediately_when_paused() {
        let config = temp_config("wait-paused");
        write_cloud_route_status(&config, "api.example.portzero.cloud", "paused");
        let err = wait_with_config(&config, "api.example.portzero.cloud", false, Some(0))
            .await
            .expect_err("expected paused tunnel to bail");
        let msg = err.to_string();
        assert!(msg.contains("paused at the edge"), "message was: {msg}");
        cleanup(&config);
    }

    #[tokio::test]
    async fn wait_times_out_with_reason_when_never_discovered() {
        let config = temp_config("wait-timeout");
        let err = wait_with_config(&config, "never.portzero.local", false, Some(0))
            .await
            .expect_err("expected timeout");
        let msg = err.to_string();
        assert!(msg.contains("Timed out after 0s"), "message was: {msg}");
        assert!(msg.contains("never.portzero.local"), "message was: {msg}");
        assert!(msg.contains("not discovered yet"), "message was: {msg}");
        assert!(!msg.contains("serving its health path"));
        cleanup(&config);
    }

    #[tokio::test]
    async fn wait_times_out_mentions_health_path_when_healthy_flag_set() {
        let config = temp_config("wait-timeout-healthy");
        let err = wait_with_config(&config, "never.portzero.local", true, Some(0))
            .await
            .expect_err("expected timeout");
        let msg = err.to_string();
        assert!(
            msg.contains("serving its health path"),
            "message was: {msg}"
        );
        cleanup(&config);
    }

    #[tokio::test]
    async fn wait_returns_ok_when_tunnel_already_ready() {
        let config = temp_config("wait-ready");
        write_overlay(
            &config,
            vec![overlay_route("web.portzero.local", 8080, None)],
        );
        wait_with_config(&config, "web.portzero.local", false, Some(0))
            .await
            .expect("expected immediate readiness");
        cleanup(&config);
    }
}
