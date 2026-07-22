//! `portzero url` and `portzero env`: export discovered tunnel URLs so test
//! processes and CI jobs can learn where a tunnel lives.
//!
//! These are the script-friendly escape hatch for non-Playwright consumers; the
//! Playwright fixture package talks to the daemon directly and is the preferred
//! integration.

use anyhow::{bail, Context, Result};

use portzero_daemon::discovery::is_local_overlay_domain;
use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::route_table::{OverlayState, RouteTable};

/// A discovered tunnel and its resolved URL.
pub struct TunnelUrl {
    pub domain: String,
    pub url: String,
    /// Declared readiness path (`PZ_HEALTH_PATH`), if any.
    pub health_path: Option<String>,
}

/// Resolve the URL a client should use to reach a tunnel.
///
/// - Cloud tunnels (anything not ending in `.portzero.local` / `.local`) are
///   served over HTTPS by the edge; the local backend port is irrelevant to the
///   public URL, so no port is emitted.
/// - Local overlay tunnels are reached by name on the virtual IP. Port 443 is
///   HTTPS, port 80 (or an unknown 0) is HTTP with no explicit port, and any
///   other port is emitted explicitly as `http://<domain>:<port>`.
pub fn tunnel_url(domain: &str, service_port: u16) -> String {
    if is_local_overlay_domain(domain) {
        match service_port {
            443 => format!("https://{domain}"),
            80 | 0 => format!("http://{domain}"),
            p => format!("http://{domain}:{p}"),
        }
    } else {
        format!("https://{domain}")
    }
}

/// Derive a stable, shell-safe environment variable name for a tunnel domain.
///
/// `web.portzero.local` → `PZ_URL_WEB_PORTZERO_LOCAL`.
pub fn env_var_name(domain: &str) -> String {
    let sanitized: String = domain
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PZ_URL_{sanitized}")
}

/// Collect every discovered tunnel (cloud routes + local overlay routes) with
/// its resolved URL, sorted by domain for stable output.
pub fn discovered_tunnels(config: &DaemonConfig) -> Vec<TunnelUrl> {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();

    let mut out: Vec<TunnelUrl> = Vec::new();
    for route in table.routes.values() {
        out.push(TunnelUrl {
            url: tunnel_url(&route.domain, route.port),
            domain: route.domain.clone(),
            health_path: route.health_path.clone(),
        });
    }
    for ov in &overlay.routes {
        out.push(TunnelUrl {
            url: tunnel_url(&ov.domain, ov.service_port),
            domain: ov.domain.clone(),
            health_path: ov.health_path.clone(),
        });
    }

    out.sort_by(|a, b| a.domain.cmp(&b.domain));
    out.dedup_by(|a, b| a.domain.eq_ignore_ascii_case(&b.domain));
    out
}

/// Look up a single discovered tunnel by domain (case-insensitive).
pub fn lookup_tunnel(config: &DaemonConfig, domain: &str) -> Option<TunnelUrl> {
    let target = domain.trim();
    discovered_tunnels(config)
        .into_iter()
        .find(|t| t.domain.eq_ignore_ascii_case(target))
}

/// `portzero url <domain>` — print exactly the resolved URL on stdout.
///
/// Nothing else is written to stdout so the output is safe to capture in a
/// script (`BASE_URL=$(portzero url web.myapp.portzero.local)`). Errors go to
/// stderr and the process exits non-zero.
pub fn url(domain: &str) -> Result<()> {
    let config = DaemonConfig::load();
    url_with_config(&config, domain)
}

/// Core of `url()`, parameterized over the `DaemonConfig` so tests can point
/// it at a throwaway state directory instead of the real `~/.portzero`.
fn url_with_config(config: &DaemonConfig, domain: &str) -> Result<()> {
    let target = domain.trim();

    let tunnels = discovered_tunnels(config);
    if let Some(t) = tunnels
        .iter()
        .find(|t| t.domain.eq_ignore_ascii_case(target))
    {
        // Exactly the URL, nothing else.
        println!("{}", t.url);
        return Ok(());
    }

    // Not found: give an actionable, stderr-only diagnostic.
    if tunnels.is_empty() {
        bail!(
            "No tunnel named '{target}' is known to the daemon (no tunnels are currently \
             discovered). Is the daemon running (`portzero status`) and is PZ_TUNNEL set on \
             the process/container?"
        );
    }
    let known: Vec<&str> = tunnels.iter().map(|t| t.domain.as_str()).collect();
    bail!(
        "No tunnel named '{target}' is known to the daemon. Discovered tunnels: {}.",
        known.join(", ")
    );
}

/// `portzero env [--github]` — export every discovered tunnel's URL.
///
/// Without `--github`, prints `export NAME="URL"` lines to stdout for
/// `eval "$(portzero env)"`. With `--github`, appends `NAME=URL` lines to the
/// file named by `$GITHUB_ENV` so the values are exported to subsequent steps
/// of a GitHub Actions job.
pub fn env(github: bool) -> Result<()> {
    let config = DaemonConfig::load();
    env_with_config(&config, github)
}

/// Core of `env()`, parameterized over the `DaemonConfig` so tests can point
/// it at a throwaway state directory instead of the real `~/.portzero`.
fn env_with_config(config: &DaemonConfig, github: bool) -> Result<()> {
    let tunnels = discovered_tunnels(config);

    if github {
        let github_env = std::env::var("GITHUB_ENV").context(
            "GITHUB_ENV is not set — `portzero env --github` only works inside a GitHub \
             Actions job. Use `portzero env` (without --github) elsewhere.",
        )?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&github_env)
            .with_context(|| format!("Failed to open $GITHUB_ENV file: {github_env}"))?;
        for t in &tunnels {
            // GitHub's environment-file format is `NAME=value`, one per line.
            writeln!(file, "{}={}", env_var_name(&t.domain), t.url)
                .with_context(|| format!("Failed to write to $GITHUB_ENV file: {github_env}"))?;
        }
        eprintln!("Exported {} tunnel URL(s) to $GITHUB_ENV.", tunnels.len());
        return Ok(());
    }

    if tunnels.is_empty() {
        eprintln!(
            "# No tunnels discovered yet. Is the daemon running and PZ_TUNNEL set? \
             (`portzero status`)"
        );
        return Ok(());
    }
    for t in &tunnels {
        println!("export {}=\"{}\"", env_var_name(&t.domain), t.url);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tunnel_url_cloud_is_https_no_port() {
        assert_eq!(
            tunnel_url("api.alice.tunnel.portzero.cloud", 8080),
            "https://api.alice.tunnel.portzero.cloud"
        );
    }

    #[test]
    fn test_tunnel_url_local_ports() {
        assert_eq!(
            tunnel_url("web.portzero.local", 80),
            "http://web.portzero.local"
        );
        assert_eq!(
            tunnel_url("web.portzero.local", 0),
            "http://web.portzero.local"
        );
        assert_eq!(
            tunnel_url("secure.portzero.local", 443),
            "https://secure.portzero.local"
        );
        assert_eq!(
            tunnel_url("db.portzero.local", 5432),
            "http://db.portzero.local:5432"
        );
    }

    #[test]
    fn test_env_var_name() {
        assert_eq!(
            env_var_name("web.portzero.local"),
            "PZ_URL_WEB_PORTZERO_LOCAL"
        );
        assert_eq!(
            env_var_name("api-x.alice.tunnel.portzero.cloud"),
            "PZ_URL_API_X_ALICE_TUNNEL_PORTZERO_CLOUD"
        );
    }

    #[test]
    fn test_env_var_name_sanitizes_special_chars() {
        // '=' and newlines can't appear in a real domain, but the sanitizer
        // should still turn any non-alphanumeric byte into '_' rather than
        // letting it leak into the emitted KEY=VALUE line.
        assert_eq!(env_var_name("a=b"), "PZ_URL_A_B");
        assert_eq!(env_var_name("a\nb"), "PZ_URL_A_B");
    }

    use portzero_daemon::discovery::ServiceSource;
    use portzero_daemon::discovery_loop::DaemonConfig;
    use portzero_daemon::route_table::OverlayRoute;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Build a `DaemonConfig` pointed at a fresh, uniquely-named temp
    /// directory so tests never collide or touch a real `~/.portzero`.
    fn temp_config(tag: &str) -> DaemonConfig {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "portzero-export-test-{tag}-{}-{n}",
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

    fn overlay_route(domain: &str, service_port: u16) -> OverlayRoute {
        OverlayRoute {
            domain: domain.to_string(),
            domain_template: domain.to_string(),
            substitutions: Default::default(),
            service_port,
            backend_protocol: None,
            real_addr: "127.0.0.1:0".to_string(),
            health_path: None,
            pid: 1,
            source: ServiceSource::Process { cwd: None },
        }
    }

    #[test]
    fn discovered_tunnels_empty_when_no_state() {
        let config = temp_config("empty");
        assert!(discovered_tunnels(&config).is_empty());
        cleanup(&config);
    }

    #[test]
    fn discovered_tunnels_are_sorted_by_domain() {
        let config = temp_config("sort");
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes: vec![
                overlay_route("zeta.portzero.local", 8080),
                overlay_route("alpha.portzero.local", 3000),
                overlay_route("mid.portzero.local", 3001),
            ],
        };
        state.save(&config.overlay_path()).unwrap();

        let tunnels = discovered_tunnels(&config);
        let domains: Vec<&str> = tunnels.iter().map(|t| t.domain.as_str()).collect();
        assert_eq!(
            domains,
            vec![
                "alpha.portzero.local",
                "mid.portzero.local",
                "zeta.portzero.local"
            ]
        );
        cleanup(&config);
    }

    #[test]
    fn discovered_tunnels_dedup_removes_exact_duplicate_domain() {
        // A domain could in principle show up twice within the same overlay
        // snapshot (e.g. a stale write raced a fresh scan); `discovered_tunnels`
        // should collapse exact (same-case, adjacent-after-sort) duplicates
        // rather than listing the domain twice.
        let config = temp_config("dedup-exact");
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes: vec![
                overlay_route("web.portzero.local", 8080),
                overlay_route("web.portzero.local", 8080),
            ],
        };
        state.save(&config.overlay_path()).unwrap();

        let tunnels = discovered_tunnels(&config);
        let count = tunnels
            .iter()
            .filter(|t| t.domain == "web.portzero.local")
            .count();
        assert_eq!(count, 1);
        cleanup(&config);
    }

    #[test]
    fn lookup_tunnel_is_case_insensitive_and_trims() {
        let config = temp_config("lookup");
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes: vec![overlay_route("web.portzero.local", 8080)],
        };
        state.save(&config.overlay_path()).unwrap();

        let found = lookup_tunnel(&config, "  WEB.PORTZERO.LOCAL  ");
        assert!(found.is_some());
        assert_eq!(found.unwrap().domain, "web.portzero.local");

        assert!(lookup_tunnel(&config, "missing.portzero.local").is_none());
        cleanup(&config);
    }

    #[test]
    fn url_errs_when_no_tunnels_discovered() {
        let config = temp_config("url-empty");
        let err = url_with_config(&config, "web.portzero.local").unwrap_err();
        assert!(err.to_string().contains("no tunnels are currently"));
        cleanup(&config);
    }

    #[test]
    fn url_errs_and_lists_known_domains_when_not_found() {
        let config = temp_config("url-notfound");
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes: vec![overlay_route("web.portzero.local", 8080)],
        };
        state.save(&config.overlay_path()).unwrap();

        let err = url_with_config(&config, "missing.portzero.local").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing.portzero.local"));
        assert!(msg.contains("web.portzero.local"));
        cleanup(&config);
    }

    #[test]
    fn url_ok_when_tunnel_found() {
        let config = temp_config("url-found");
        let state = portzero_daemon::route_table::OverlayState {
            overlay_active: true,
            routes: vec![overlay_route("web.portzero.local", 8080)],
        };
        state.save(&config.overlay_path()).unwrap();

        assert!(url_with_config(&config, "web.portzero.local").is_ok());
        cleanup(&config);
    }

    // GITHUB_ENV is a process-global env var, so serialize every test that
    // touches it to avoid cross-test interference when the suite runs tests
    // in parallel threads.
    static ENV_VAR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_github_env<F: FnOnce(&std::path::Path)>(tag: &str, f: F) {
        let _guard = ENV_VAR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "portzero-export-test-github-env-{tag}-{}-{n}",
            std::process::id()
        ));
        std::env::set_var("GITHUB_ENV", &path);
        f(&path);
        std::env::remove_var("GITHUB_ENV");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn env_github_writes_key_value_lines_for_each_tunnel() {
        with_github_env("basic", |github_env_path| {
            let config = temp_config("env-github-basic");
            let state = portzero_daemon::route_table::OverlayState {
                overlay_active: true,
                routes: vec![
                    overlay_route("web.portzero.local", 8080),
                    overlay_route("db.portzero.local", 443),
                ],
            };
            state.save(&config.overlay_path()).unwrap();

            env_with_config(&config, true).expect("env --github should succeed");

            let content = std::fs::read_to_string(github_env_path).unwrap();
            assert!(content.contains("PZ_URL_DB_PORTZERO_LOCAL=https://db.portzero.local\n"));
            assert!(content.contains("PZ_URL_WEB_PORTZERO_LOCAL=http://web.portzero.local:8080\n"));
            cleanup(&config);
        });
    }

    #[test]
    fn env_github_appends_and_preserves_existing_content() {
        with_github_env("append", |github_env_path| {
            std::fs::write(github_env_path, "EXISTING_VAR=1\n").unwrap();

            let config = temp_config("env-github-append");
            let state = portzero_daemon::route_table::OverlayState {
                overlay_active: true,
                routes: vec![overlay_route("web.portzero.local", 8080)],
            };
            state.save(&config.overlay_path()).unwrap();

            env_with_config(&config, true).expect("env --github should succeed");

            let content = std::fs::read_to_string(github_env_path).unwrap();
            assert!(content.starts_with("EXISTING_VAR=1\n"));
            assert!(content.contains("PZ_URL_WEB_PORTZERO_LOCAL=http://web.portzero.local:8080\n"));
            cleanup(&config);
        });
    }

    #[test]
    fn env_github_writes_nothing_when_no_tunnels() {
        with_github_env("no-tunnels", |github_env_path| {
            let config = temp_config("env-github-empty");
            env_with_config(&config, true).expect("env --github should succeed even if empty");

            let content = std::fs::read_to_string(github_env_path).unwrap();
            assert!(content.is_empty());
            cleanup(&config);
        });
    }

    #[test]
    fn env_errs_without_github_flag_when_github_env_unset() {
        // env(false) never touches $GITHUB_ENV, so it must succeed even when
        // the var is completely unset (the common non-CI case). Run this
        // under the same lock as the other GITHUB_ENV tests to avoid a data
        // race on the process environment.
        let _guard = ENV_VAR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("GITHUB_ENV");
        let config = temp_config("env-no-github");
        assert!(env_with_config(&config, false).is_ok());
        cleanup(&config);
    }
}
