use super::*;

#[test]
fn test_daemon_config_default() {
    let config = DaemonConfig::default();
    assert!(config
        .routes_path()
        .to_string_lossy()
        .contains("routes.json"));
    assert!(config.pid_path().to_string_lossy().contains("daemon.pid"));
    assert!(config.log_path().to_string_lossy().contains("daemon.log"));
    assert_eq!(config.scan_interval_secs, 2);
}

#[test]
fn test_reconnect_backoff_initial_due() {
    let backoff = ReconnectBackoff::new();
    assert!(backoff.is_due());
}

#[test]
fn test_reconnect_backoff_not_due_after_failure() {
    let mut backoff = ReconnectBackoff::new();
    backoff.on_failure();
    // Immediately after failure the timer hasn't elapsed yet
    assert!(!backoff.is_due());
}

#[test]
fn test_reconnect_backoff_doubles_on_failure() {
    let mut backoff = ReconnectBackoff::new();
    assert_eq!(backoff.current_delay, Duration::from_secs(1));
    backoff.on_failure();
    assert_eq!(backoff.current_delay, Duration::from_secs(2));
    backoff.on_failure();
    assert_eq!(backoff.current_delay, Duration::from_secs(4));
}

#[test]
fn test_reconnect_backoff_capped_at_max() {
    let mut backoff = ReconnectBackoff {
        next_attempt_at: None,
        current_delay: Duration::from_secs(RECONNECT_MAX_DELAY_SECS / 2 + 1),
    };
    backoff.on_failure();
    assert_eq!(
        backoff.current_delay,
        Duration::from_secs(RECONNECT_MAX_DELAY_SECS)
    );
    backoff.on_failure();
    assert_eq!(
        backoff.current_delay,
        Duration::from_secs(RECONNECT_MAX_DELAY_SECS)
    );
}

#[test]
fn test_reconnect_backoff_resets_on_success() {
    let mut backoff = ReconnectBackoff::new();
    backoff.on_failure();
    backoff.on_failure();
    backoff.on_success();
    assert!(backoff.is_due());
    assert_eq!(
        backoff.current_delay,
        Duration::from_secs(RECONNECT_BASE_DELAY_SECS)
    );
}

#[test]
fn test_grace_period_tracker() {
    let mut tracker = GracePeriodTracker {
        missing_since: HashMap::new(),
        grace_duration: Duration::from_millis(0), // instant expiry for testing
    };

    // First call: starts tracking, but with 0ms grace it expires immediately
    assert!(tracker.process_missing("test.portzero.cloud"));

    // Mark alive clears it
    tracker.process_alive("test.portzero.cloud");
    assert!(tracker.missing_since.is_empty());
}

#[test]
fn test_grace_period_tracker_not_expired() {
    let mut tracker = GracePeriodTracker {
        missing_since: HashMap::new(),
        grace_duration: Duration::from_secs(60), // very long grace
    };

    // With a 60s grace, it should not expire on first check
    assert!(!tracker.process_missing("test.portzero.cloud"));
}

#[test]
fn test_grace_period_prune() {
    let mut tracker = GracePeriodTracker::new();
    tracker
        .missing_since
        .insert("old.portzero.cloud".to_string(), Instant::now());
    tracker
        .missing_since
        .insert("current.portzero.cloud".to_string(), Instant::now());

    let current = "current.portzero.cloud".to_string();
    let active = vec![&current];
    tracker.prune(&active);

    assert!(!tracker.missing_since.contains_key("old.portzero.cloud"));
    assert!(tracker.missing_since.contains_key("current.portzero.cloud"));
}

#[test]
fn test_is_process_alive_zero_pid() {
    assert!(!is_process_alive(0));
}

#[test]
fn test_is_process_alive_current() {
    let current_pid = std::process::id();
    assert!(is_process_alive(current_pid));
}

#[test]
fn test_is_process_alive_nonexistent() {
    // PID 99999999 is very unlikely to exist
    assert!(!is_process_alive(99_999_999));
}

#[test]
fn test_update_domain_router() {
    use crate::discovery::ServiceSource;
    use crate::route_table::Route;

    let router = DomainRouter::new();
    let changes = RouteChanges {
        added: vec![Route {
            domain: "api.test.portzero.cloud".to_string(),
            domain_template: "api.test.portzero.cloud".to_string(),
            substitutions: Default::default(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            extra_ports: vec![],
            health_path: None,
            source: ServiceSource::Process { cwd: None },
            pid: 1,
            discovered_at: chrono::Utc::now(),
        }],
        removed: vec![],
        changed: vec![],
    };

    update_domain_router(&router, &changes);
    assert_eq!(router.resolve("api.test.portzero.cloud"), Some(8080));
}

#[test]
fn test_build_overlay_table_registers_services() {
    use crate::discovery::{DiscoveredNetworkService, ServiceSource};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let services = vec![
        DiscoveredNetworkService {
            name: "my-db".to_string(),
            domain_template: "my-db.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32768),
            service_port: 5432,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process { cwd: None },
            health_path: None,
        },
        DiscoveredNetworkService {
            name: "my-api".to_string(),
            domain_template: "my-api.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
            service_port: 8080,
            backend_protocol: None,
            pid: 101,
            source: ServiceSource::Process { cwd: None },
            health_path: None,
        },
    ];

    let table = build_overlay_table(&services, 0);
    assert_eq!(table.len(), 2);

    let db = table.get("my-db").expect("my-db registered");
    assert_eq!(db.service_port, 5432);
    assert_eq!(db.real_addr.port(), 32768);
    assert_eq!(db.pid, 100);

    let api = table.get("my-api").expect("my-api registered");
    assert_eq!(api.service_port, 8080);
    // Distinct names receive distinct virtual IPs.
    assert_ne!(db.vip, api.vip);
}

#[test]
fn test_build_overlay_table_keeps_multi_label_names_distinct() {
    use crate::discovery::{DiscoveredNetworkService, ServiceSource};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let services = vec![
        DiscoveredNetworkService {
            name: "staging.portzero.net".to_string(),
            domain_template: "staging.portzero.net.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 32768),
            service_port: 443,
            backend_protocol: None,
            pid: 100,
            source: ServiceSource::Process { cwd: None },
            health_path: None,
        },
        DiscoveredNetworkService {
            name: "staging".to_string(),
            domain_template: "staging.portzero.local".to_string(),
            substitutions: Default::default(),
            real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000),
            service_port: 443,
            backend_protocol: None,
            pid: 101,
            source: ServiceSource::Process { cwd: None },
            health_path: None,
        },
    ];

    let table = build_overlay_table(&services, 0);
    let nested = table
        .get("staging.portzero.net")
        .expect("nested service registered");
    let short = table.get("staging").expect("short service registered");
    assert_ne!(nested.vip, short.vip);
}

#[test]
fn test_write_overlay_state_preserves_multi_label_domain() {
    use crate::discovery::{DiscoveredNetworkService, ServiceSource};
    use crate::route_table::OverlayState;

    let (config, _dir) = temp_config();
    let services = vec![DiscoveredNetworkService {
        name: "staging.portzero.net".to_string(),
        domain_template: "staging.portzero.net.portzero.local".to_string(),
        substitutions: Default::default(),
        real_addr: "127.0.0.1:32768".parse().unwrap(),
        service_port: 443,
        backend_protocol: None,
        pid: 100,
        source: ServiceSource::Process { cwd: None },
        health_path: None,
    }];

    write_overlay_state(&config, &services, true);

    let state = OverlayState::load(&config.overlay_path()).expect("overlay state");
    assert!(state.overlay_active);
    assert_eq!(state.routes.len(), 1);
    assert_eq!(
        state.routes[0].domain,
        "staging.portzero.net.portzero.local"
    );
}

#[test]
fn test_build_overlay_table_empty() {
    let table = build_overlay_table(&[], 0);
    assert!(table.is_empty());
}

#[test]
fn test_read_daemon_pid_no_file() {
    let config = DaemonConfig {
        state_dir: PathBuf::from("/nonexistent/path"),
        scan_interval_secs: 2,
        overlay_https: OverlayHttpsPolicy::default(),
        dns_first_hit_policy: DnsFirstHitPolicy::default(),
        auto_open_http_tunnels: true,
    };
    assert!(read_daemon_pid(&config).is_none());
}

fn temp_config() -> (DaemonConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = DaemonConfig {
        state_dir: dir.path().to_path_buf(),
        scan_interval_secs: 2,
        overlay_https: OverlayHttpsPolicy::default(),
        dns_first_hit_policy: DnsFirstHitPolicy::default(),
        auto_open_http_tunnels: true,
    };
    (config, dir)
}

#[test]
fn test_cloud_state_connected() {
    let (config, _dir) = temp_config();
    write_cloud_state(&config, true, None, None, None, None);
    assert_eq!(read_cloud_connected(&config), Some(true));
    assert_eq!(read_cloud_error(&config), None);
}

#[test]
fn test_cloud_state_disconnected_no_error() {
    let (config, _dir) = temp_config();
    write_cloud_state(&config, false, None, None, None, None);
    assert_eq!(read_cloud_connected(&config), Some(false));
    assert_eq!(read_cloud_error(&config), None);
}

#[test]
fn test_cloud_state_disconnected_with_error() {
    let (config, _dir) = temp_config();
    write_cloud_state(
        &config,
        false,
        Some("Connection refused (os error 111)".to_string()),
        None,
        None,
        None,
    );
    assert_eq!(read_cloud_connected(&config), Some(false));
    assert_eq!(
        read_cloud_error(&config),
        Some("Connection refused (os error 111)".to_string())
    );
}

#[test]
fn test_cloud_state_error_with_special_chars() {
    let (config, _dir) = temp_config();
    write_cloud_state(
        &config,
        false,
        Some(r#"error with "quotes" and \backslash"#.to_string()),
        None,
        None,
        None,
    );
    assert_eq!(
        read_cloud_error(&config),
        Some(r#"error with "quotes" and \backslash"#.to_string())
    );
}

#[test]
fn test_cloud_state_missing_file() {
    let config = DaemonConfig {
        state_dir: PathBuf::from("/nonexistent/path"),
        scan_interval_secs: 2,
        overlay_https: OverlayHttpsPolicy::default(),
        dns_first_hit_policy: DnsFirstHitPolicy::default(),
        auto_open_http_tunnels: true,
    };
    assert_eq!(read_cloud_connected(&config), None);
    assert_eq!(read_cloud_error(&config), None);
}

#[test]
fn test_cloud_state_with_plan_and_message() {
    let (config, _dir) = temp_config();
    write_cloud_state(
        &config,
        true,
        None,
        Some("free".to_string()),
        Some(false),
        Some("Your plan does not include cloud tunnels.".to_string()),
    );
    assert_eq!(read_cloud_connected(&config), Some(true));
    assert_eq!(read_cloud_plan(&config), Some("free".to_string()));
    assert_eq!(read_cloud_can_use_tunnels(&config), Some(false));
    assert_eq!(
        read_cloud_message(&config),
        Some("Your plan does not include cloud tunnels.".to_string())
    );
    assert_eq!(read_cloud_error(&config), None);
}

#[test]
fn test_file_config_overrides_https_policy() {
    let file: FileConfig = toml::from_str(
        r#"
            [overlay.https]
            enable_for_port_80 = false
            redirect_port_80 = false
            passthrough_port_443 = true
            "#,
    )
    .unwrap();
    let mut config = DaemonConfig::default();
    file.apply_to(&mut config);

    assert!(!config.overlay_https.enable_for_port_80);
    assert!(!config.overlay_https.redirect_port_80);
    assert!(config.overlay_https.passthrough_port_443);
}

#[test]
fn test_file_config_overrides_dns_first_hit_policy() {
    let file: FileConfig = toml::from_str(
        r#"
            [overlay]
            dns_first_hit_policy = "proactive-vip"
            "#,
    )
    .unwrap();
    let mut config = DaemonConfig::default();
    file.apply_to(&mut config);

    assert_eq!(config.dns_first_hit_policy, DnsFirstHitPolicy::ProactiveVip);
}

#[test]
fn test_write_https_policy_roundtrips_via_file() {
    let (cfg, _dir) = temp_config();
    // config.toml lives in parent of the (temp) state dir
    let policy = OverlayHttpsPolicy {
        enable_for_port_80: false,
        redirect_port_80: true,
        passthrough_port_443: false,
    };
    cfg.write_https_policy(policy)
        .expect("write should succeed for temp dir");

    // Load by pointing a fresh default? Simulate: parse the sibling config.toml directly
    let cfg_path = cfg.config_path();
    let raw = std::fs::read_to_string(&cfg_path).expect("config file written");
    let parsed: FileConfig = toml::from_str(&raw).unwrap();
    let mut loaded = DaemonConfig::default();
    parsed.apply_to(&mut loaded);

    assert!(!loaded.overlay_https.enable_for_port_80);
    assert!(loaded.overlay_https.redirect_port_80);
    assert!(!loaded.overlay_https.passthrough_port_443);
}

#[test]
fn reject_unauthorized_cloud_routes_flags_only_disallowed_apex() {
    use crate::discovery::{DiscoveredService, ServiceSource};

    fn svc(domain: &str, pid: u32) -> DiscoveredService {
        DiscoveredService {
            domain: domain.into(),
            domain_template: domain.into(),
            substitutions: Default::default(),
            port: 8080,
            extra_ports: vec![],
            health_path: None,
            pid,
            source: ServiceSource::Process { cwd: None },
        }
    }

    let mut discovered = vec![
        svc("web--alice.tunnel.portzero.cloud", 1), // username → kept
        svc("api--acme.tunnel.portzero.cloud", 2),  // team slug → kept
        svc("db.portzero.local", 3),                // local overlay → untouched
        svc("web--bob.tunnel.portzero.cloud", 4),   // not owned → dropped + flagged
    ];
    let allowed = vec!["alice".to_string(), "acme".to_string()];

    let issues = reject_unauthorized_cloud_routes(&mut discovered, &allowed);

    let kept: Vec<&str> = discovered.iter().map(|s| s.domain.as_str()).collect();
    assert_eq!(
        kept,
        vec![
            "web--alice.tunnel.portzero.cloud",
            "api--acme.tunnel.portzero.cloud",
            "db.portzero.local",
        ]
    );
    assert_eq!(issues.len(), 1);
    match &issues[0] {
        notify::Issue::CloudTunnelNotAllowed {
            domain,
            reason,
            pid,
        } => {
            assert_eq!(domain, "web--bob.tunnel.portzero.cloud");
            assert!(
                reason.contains("'bob'"),
                "reason names the bad namespace: {reason}"
            );
            assert!(
                reason.contains("alice"),
                "reason lists allowed namespaces: {reason}"
            );
            assert_eq!(*pid, Some(4));
        }
        other => panic!("unexpected issue: {other:?}"),
    }
}

#[test]
fn cloud_rejection_issues_only_for_present_routes() {
    use crate::discovery::ServiceSource;
    use crate::route_table::Route;

    let mut table = RouteTable::new();
    table.routes.insert(
        "web--alice.tunnel.portzero.cloud".into(),
        Route {
            domain: "web--alice.tunnel.portzero.cloud".into(),
            domain_template: "web--alice.tunnel.portzero.cloud".into(),
            substitutions: Default::default(),
            host: "127.0.0.1".into(),
            port: 8080,
            extra_ports: vec![],
            health_path: None,
            source: ServiceSource::Process { cwd: None },
            pid: 7,
            discovered_at: chrono::Utc::now(),
        },
    );

    let mut rejected = std::collections::HashMap::new();
    rejected.insert(
        "web--alice.tunnel.portzero.cloud".to_string(),
        "name violates team naming policy".to_string(),
    );
    // A rejection for a tunnel that is no longer present must not be surfaced.
    rejected.insert(
        "gone--alice.tunnel.portzero.cloud".to_string(),
        "stale".to_string(),
    );

    let issues = cloud_rejection_issues(&rejected, &table);
    assert_eq!(issues.len(), 1);
    match &issues[0] {
        notify::Issue::CloudTunnelNotAllowed {
            domain,
            reason,
            pid,
        } => {
            assert_eq!(domain, "web--alice.tunnel.portzero.cloud");
            assert_eq!(reason, "name violates team naming policy");
            assert_eq!(*pid, Some(7));
        }
        other => panic!("unexpected issue: {other:?}"),
    }
}
