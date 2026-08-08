use super::*;

#[test]
fn test_parse_http_port_selection() {
    assert_eq!(
        parse_http_port_selection(""),
        HttpPortSelection::ChooseLowest
    );
    assert_eq!(
        parse_http_port_selection("CHOOSE_LOWEST"),
        HttpPortSelection::ChooseLowest
    );
    assert_eq!(
        parse_http_port_selection("CHOOSE_HIGHEST"),
        HttpPortSelection::ChooseHighest
    );
    assert_eq!(
        parse_http_port_selection("8080"),
        HttpPortSelection::Explicit(8080)
    );
    assert_eq!(
        parse_http_port_selection("3000"),
        HttpPortSelection::Explicit(3000)
    );
    // Invalid falls back to ChooseLowest
    assert_eq!(
        parse_http_port_selection("notaport"),
        HttpPortSelection::ChooseLowest
    );
}

#[cfg(target_os = "macos")]
#[test]
fn test_parse_macos_ps_env_candidate() {
    let line = "64039 target/debug/portzero-rust-local-process CARGO=/Users/loumtech/.cargo/bin/cargo PWD=/tmp/project PZ_TUNNEL=rust-demo.portzero.local:80 RUST_RECURSION_COUNT=1";
    assert_eq!(
        parse_macos_ps_env_candidate(line, ENV_VAR_NAME),
        Some((64039, "rust-demo.portzero.local:80".to_string()))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn test_parse_macos_ps_env_candidate_ignores_missing_var() {
    let line = "70462 /Users/loumtech/.cargo/bin/portzero start --foreground";
    assert_eq!(parse_macos_ps_env_candidate(line, ENV_VAR_NAME), None);
}

#[cfg(target_os = "macos")]
#[test]
fn test_parse_procargs2_env() {
    // Synthesize a KERN_PROCARGS2 buffer: argc, exec_path, zero padding,
    // argc argv strings, then the environment (and a trailing apple[]).
    let mut buf = Vec::new();
    let argc: i32 = 2;
    buf.extend_from_slice(&argc.to_ne_bytes());
    buf.extend_from_slice(b"/usr/bin/demo\0");
    buf.extend_from_slice(b"\0\0"); // padding between exec_path and argv[0]
    buf.extend_from_slice(b"demo\0"); // argv[0]
    buf.extend_from_slice(b"--serve\0"); // argv[1]
    buf.extend_from_slice(b"PWD=/tmp/project\0");
    buf.extend_from_slice(b"PZ_TUNNEL=rust-demo.portzero.local:80\0");
    buf.extend_from_slice(b"executable_path=/usr/bin/demo\0"); // apple[]

    let env = parse_procargs2_env(&buf).expect("env parsed");
    assert!(env.contains(&"PWD=/tmp/project".to_string()));
    assert!(env.contains(&"PZ_TUNNEL=rust-demo.portzero.local:80".to_string()));
    // argv strings must not leak into the environment.
    assert!(!env.iter().any(|e| e == "demo" || e == "--serve"));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_parse_lsof_line_ipv4_loopback() {
    let p = parse_lsof_line(
        "Python    32520 loumtech    3u  IPv4 0xc2df...      0t0  TCP 127.0.0.1:50706 (LISTEN)",
    )
    .expect("should parse a port");
    assert_eq!(p.port, 50706);
    assert_eq!(p.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_parse_lsof_line_public_forms() {
    let star =
        parse_lsof_line("node 100 u 5u IPv4 0x0 0t0 TCP *:8080 (LISTEN)").expect("star parses");
    assert_eq!(star.port, 8080);
    assert_eq!(star.addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    let any = parse_lsof_line("node 100 u 5u IPv4 0x0 0t0 TCP 0.0.0.0:8080 (LISTEN)")
        .expect("0.0.0.0 parses");
    assert_eq!(any.port, 8080);
    assert_eq!(any.addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_parse_lsof_line_ipv6() {
    let loopback = parse_lsof_line(
        "Parallels  7174 loumtech   13u  IPv6 0x1e18...      0t0  TCP [::1]:57889 (LISTEN)",
    )
    .expect("[::1] parses");
    assert_eq!(loopback.port, 57889);
    // The address family is the whole point: dialing 127.0.0.1 for this
    // listener reaches nothing.
    assert_eq!(loopback.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(loopback.dial_addr(), IpAddr::V6(Ipv6Addr::LOCALHOST));

    let public =
        parse_lsof_line("node 100 u 5u IPv6 0x0 0t0 TCP [::]:8080 (LISTEN)").expect("[::] parses");
    assert_eq!(public.port, 8080);
    assert_eq!(public.addr, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    assert_eq!(public.dial_addr(), IpAddr::V6(Ipv6Addr::LOCALHOST));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_parse_lsof_line_no_address() {
    // A line with no address token (just a state) must not panic or produce a port.
    assert_eq!(parse_lsof_line("(LISTEN)"), None);
    assert_eq!(parse_lsof_line(""), None);
    // Header-ish / non-address tokens only.
    assert_eq!(parse_lsof_line("COMMAND PID USER FD TYPE"), None);
    // Port 0 is filtered.
    assert_eq!(
        parse_lsof_line("node 1 u 5u IPv4 0x0 0t0 TCP *:0 (LISTEN)"),
        None
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn test_parse_lsof_stdout_skips_header() {
    let stdout = "COMMAND     PID     USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME\n\
            Python    32520 loumtech    3u  IPv4 0xc2df...      0t0  TCP 127.0.0.1:50706 (LISTEN)\n\
            Parallels  7174 loumtech   13u  IPv6 0x1e18...      0t0  TCP [::1]:57889 (LISTEN)\n\
            ollama      580 loumtech    3u  IPv4 0xd11a...      0t0  TCP 127.0.0.1:11434 (LISTEN)\n";
    let ports = parse_lsof_stdout(stdout);
    assert_eq!(
        ports,
        vec![
            ListeningPort::v4_loopback(50706),
            // The `IPv6` TYPE column plus `[::1]` — a listener that only an
            // IPv6 dial can reach.
            ListeningPort::v6_loopback(57889),
            ListeningPort::v4_loopback(11434),
        ]
    );
}

#[cfg(target_os = "windows")]
#[test]
fn test_parse_windows_tcp_connection_lines() {
    assert_eq!(
        parse_windows_tcp_connection_line("0.0.0.0|8080"),
        Some(ListeningPort::v4_any(8080))
    );
    assert_eq!(
        parse_windows_tcp_connection_line("::|3000"),
        Some(ListeningPort::v4_any(3000))
    );
    assert_eq!(
        parse_windows_tcp_connection_line("127.0.0.1|5173"),
        Some(ListeningPort::v4_loopback(5173))
    );
    assert_eq!(parse_windows_tcp_connection_line("127.0.0.1|0"), None);
    assert_eq!(parse_windows_tcp_connection_line("bad"), None);
}

#[cfg(target_os = "windows")]
#[test]
fn test_parse_windows_netstat_lines() {
    assert_eq!(
        parse_windows_netstat_line(
            "  TCP    127.0.0.1:5173         0.0.0.0:0              LISTENING       1234",
            1234,
        ),
        Some(ListeningPort::v4_loopback(5173))
    );
    assert_eq!(
        parse_windows_netstat_line(
            "  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       1234",
            1234,
        ),
        Some(ListeningPort::v4_any(8080))
    );
    assert_eq!(
        parse_windows_netstat_line(
            "  TCP    [::]:3000              [::]:0                 LISTENING       1234",
            1234,
        ),
        Some(ListeningPort::v4_any(3000))
    );
    assert_eq!(
        parse_windows_netstat_line(
            "  TCP    127.0.0.1:5173         0.0.0.0:0              ESTABLISHED     1234",
            1234,
        ),
        None
    );
    assert_eq!(
        parse_windows_netstat_line(
            "  TCP    127.0.0.1:5173         0.0.0.0:0              LISTENING       9999",
            1234,
        ),
        None
    );
}

#[cfg(target_os = "windows")]
#[test]
fn test_parse_windows_environment_block() {
    let mut words: Vec<u16> = "Path=C:\\Windows\0PZ_TUNNEL=api.portzero.local\0\0"
        .encode_utf16()
        .collect();
    assert_eq!(
        parse_windows_environment_block(&words),
        vec![
            "Path=C:\\Windows".to_string(),
            "PZ_TUNNEL=api.portzero.local".to_string()
        ]
    );

    words.clear();
    words.extend("\0".encode_utf16());
    assert!(parse_windows_environment_block(&words).is_empty());
}

#[cfg(target_os = "windows")]
#[test]
fn test_scan_process_env_windows_spawned_child() {
    let expected = "spawned-child.alice.portzero.cloud";
    let mut child = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ])
        .env("PZ_TUNNEL", expected)
        .spawn()
        .expect("spawn child process");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut result = None;
    while std::time::Instant::now() < deadline {
        result = scan_process_env(child.id(), ENV_VAR_NAME);
        if result.as_deref() == Some(expected) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(result.as_deref(), Some(expected));
}

#[cfg(target_os = "windows")]
#[test]
fn test_discover_ports_windows_current_process_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral listener");
    let port = listener.local_addr().expect("listener address").port();

    let ports = discover_process_ports(std::process::id());

    assert!(
        ports.iter().any(|p| p.port == port),
        "expected to discover listener port {port}, got {ports:?}"
    );
}

#[test]
fn test_normalize_health_path() {
    assert_eq!(
        normalize_health_path("/health"),
        Some("/health".to_string())
    );
    // A bare path gets a leading slash.
    assert_eq!(normalize_health_path("health"), Some("/health".to_string()));
    assert_eq!(
        normalize_health_path("  /healthz  "),
        Some("/healthz".to_string())
    );
    assert_eq!(normalize_health_path("readyz"), Some("/readyz".to_string()));
    // Absent / empty stays None so it changes nothing (fully optional).
    assert_eq!(normalize_health_path(""), None);
    assert_eq!(normalize_health_path("   "), None);
}

#[test]
fn test_parse_extra_ports() {
    let mappings = parse_extra_ports("9222:9222;9300:9300");
    assert_eq!(mappings.len(), 2);
    assert_eq!(
        mappings[0],
        PortMapping {
            local_port: 9222,
            tunnel_port: 9222
        }
    );
    assert_eq!(
        mappings[1],
        PortMapping {
            local_port: 9300,
            tunnel_port: 9300
        }
    );
}

#[test]
fn test_parse_extra_ports_single() {
    let mappings = parse_extra_ports("5432:5432");
    assert_eq!(mappings.len(), 1);
    assert_eq!(
        mappings[0],
        PortMapping {
            local_port: 5432,
            tunnel_port: 5432
        }
    );
}

#[test]
fn test_parse_extra_ports_empty() {
    assert!(parse_extra_ports("").is_empty());
    assert!(parse_extra_ports("  ").is_empty());
}

#[test]
fn test_select_http_port_explicit_owned() {
    let ports = vec![
        ListeningPort::v4_any(3000),
        ListeningPort::v4_loopback(8080),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::Explicit(3000)),
        SelectedPort::Found(3000)
    );
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::Explicit(8080)),
        SelectedPort::Found(8080)
    );
}

#[test]
fn test_select_http_port_explicit_not_owned() {
    let ports = vec![
        ListeningPort::v4_any(3000),
        ListeningPort::v4_loopback(8080),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::Explicit(9000)),
        SelectedPort::ExplicitNotOwned(9000)
    );
}

#[test]
fn test_select_http_port_explicit_no_listening_ports() {
    assert_eq!(
        select_http_port(&[], &HttpPortSelection::Explicit(8080)),
        SelectedPort::ExplicitNotOwned(8080)
    );
}

#[test]
fn test_select_http_port_prefers_public() {
    let ports = vec![
        ListeningPort::v4_loopback(9229), // debugger
        ListeningPort::v4_any(3000),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::ChooseLowest),
        SelectedPort::Found(3000)
    );
}

#[test]
fn test_select_http_port_falls_back_to_loopback() {
    let ports = vec![
        ListeningPort::v4_loopback(8080),
        ListeningPort::v4_loopback(3000),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::ChooseLowest),
        SelectedPort::Found(3000)
    );
}

#[test]
fn test_select_http_port_choose_highest_public() {
    let ports = vec![
        ListeningPort::v4_any(3000),
        ListeningPort::v4_any(8080),
        ListeningPort::v4_loopback(9229),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::ChooseHighest),
        SelectedPort::Found(8080)
    );
}

#[test]
fn test_select_http_port_no_listening_ports() {
    assert_eq!(
        select_http_port(&[], &HttpPortSelection::ChooseLowest),
        SelectedPort::NoneListening
    );
    assert_eq!(
        select_http_port(&[], &HttpPortSelection::ChooseHighest),
        SelectedPort::NoneListening
    );
}

#[test]
fn test_parse_docker_ports_typical() {
    let json = r#"{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"58321"}]}"#;
    assert_eq!(
        parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
        58321
    );
}

#[test]
fn test_parse_docker_ports_choose_highest() {
    let json = r#"{"3000/tcp":[{"HostIp":"0.0.0.0","HostPort":"3000"}],"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"8080"}]}"#;
    assert_eq!(
        parse_docker_ports(json, &HttpPortSelection::ChooseHighest),
        8080
    );
}

#[test]
fn test_parse_docker_ports_empty() {
    assert_eq!(
        parse_docker_ports("{}", &HttpPortSelection::ChooseLowest),
        0
    );
    assert_eq!(
        parse_docker_ports("null", &HttpPortSelection::ChooseLowest),
        0
    );
}

#[test]
fn test_parse_docker_ports_no_bindings() {
    let json = r#"{"8080/tcp":null}"#;
    assert_eq!(
        parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
        0
    );
}

#[test]
fn test_service_source_display_process() {
    let src = ServiceSource::Process {
        cwd: Some(PathBuf::from("/tmp/myapp")),
    };
    let display = format!("{}", src);
    assert!(display.contains("/tmp/myapp"));
}

#[test]
fn test_service_source_display_container() {
    let src = ServiceSource::Container {
        id: "abc123".to_string(),
        name: "myapp-postgres-1".to_string(),
    };
    assert_eq!(format!("{}", src), "container myapp-postgres-1");
}

#[cfg(target_os = "linux")]
#[test]
fn test_parse_proc_net_tcp_listen_line_public() {
    // 0A = LISTEN, local addr 00000000:1F90 = 0.0.0.0:8080, inode = 12345
    let line = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(12345u64);
    let result = parse_proc_net_tcp_line(line, &inodes);
    assert!(result.is_some());
    let p = result.unwrap();
    assert_eq!(p.port, 8080);
    assert_eq!(p.addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
}

#[cfg(target_os = "linux")]
#[test]
fn test_parse_proc_net_tcp_listen_line_loopback() {
    // local addr 0100007F:1F90 = 127.0.0.1:8080
    let line = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 99 1 0000000000000000 100 0 0 10 0";
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(99u64);
    let result = parse_proc_net_tcp_line(line, &inodes);
    assert!(result.is_some());
    let p = result.unwrap();
    assert_eq!(p.port, 8080);
    assert_eq!(p.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[cfg(target_os = "linux")]
#[test]
fn test_parse_proc_net_tcp_wrong_inode_skipped() {
    let line = "   0: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
    let inodes = std::collections::HashSet::new(); // empty — doesn't own this socket
    assert!(parse_proc_net_tcp_line(line, &inodes).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn test_parse_proc_net_tcp_established_skipped() {
    // state 01 = ESTABLISHED, not LISTEN
    let line = "   1: 0100007F:1F90 0100007F:C000 01 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0";
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(12345u64);
    assert!(parse_proc_net_tcp_line(line, &inodes).is_none());
}

#[test]
fn test_looks_like_repo_path() {
    assert!(looks_like_repo_path("/repo/app.jar"));
    assert!(looks_like_repo_path("./server/main.py"));
    assert!(looks_like_repo_path("dist/app.js"));
    assert!(looks_like_repo_path("app.jar"));
    assert!(!looks_like_repo_path("serve"));
}

#[test]
fn test_template_substitutions_without_project_dir_do_not_use_daemon_cwd() {
    let substitutions = template_substitutions(None, None, None, None);
    assert_eq!(
        substitutions.get("branch").map(String::as_str),
        Some("unknown")
    );
    assert_eq!(
        substitutions.get("project").map(String::as_str),
        Some("unknown")
    );
    assert_eq!(
        substitutions.get("worktree").map(String::as_str),
        Some("unknown")
    );
}

#[test]
fn test_extract_local_label_preserves_multi_label_prefix() {
    assert_eq!(
        extract_local_label("staging.portzero.net.portzero.local"),
        "staging.portzero.net"
    );
    assert_eq!(
        extract_local_label("Staging.PortZero.Net.PortZero.Local"),
        "staging.portzero.net"
    );
    assert_eq!(
        extract_local_label("bad_label.example.portzero.local"),
        "bad-label.example"
    );
}

#[test]
fn test_dedupe_network_services_collapses_same_process_context() {
    let svc = DiscoveredNetworkService {
        name: "web".to_string(),
        domain_template: "web.portzero.local".to_string(),
        substitutions: BTreeMap::new(),
        real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
        service_port: 5173,
        backend_protocol: None,
        pid: 100,
        source: ServiceSource::Process {
            cwd: Some(PathBuf::from("/work/app")),
        },
        health_path: None,
    };

    let dup = DiscoveredNetworkService { ..svc.clone() };

    let deduped = dedupe_network_services(vec![svc, dup]);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].service_port, 5173);
}

#[test]
fn test_dedupe_network_services_collapses_same_endpoint_in_same_worktree() {
    let a = DiscoveredNetworkService {
        name: "web".to_string(),
        domain_template: "web.portzero.local".to_string(),
        substitutions: BTreeMap::new(),
        real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
        service_port: 5173,
        backend_protocol: None,
        pid: 100,
        source: ServiceSource::Process {
            cwd: Some(PathBuf::from("/work/a")),
        },
        health_path: None,
    };

    let b = DiscoveredNetworkService {
        pid: 200,
        ..a.clone()
    };

    let deduped = dedupe_network_services(vec![a, b]);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].pid, 100);
}

#[test]
fn test_dedupe_network_services_keeps_distinct_ports_in_same_worktree() {
    let a = DiscoveredNetworkService {
        name: "web".to_string(),
        domain_template: "web.portzero.local".to_string(),
        substitutions: BTreeMap::new(),
        real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
        service_port: 5173,
        backend_protocol: None,
        pid: 100,
        source: ServiceSource::Process {
            cwd: Some(PathBuf::from("/work/a")),
        },
        health_path: None,
    };

    let b = DiscoveredNetworkService {
        real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5174)),
        service_port: 5174,
        pid: 200,
        ..a.clone()
    };

    let deduped = dedupe_network_services(vec![a, b]);
    assert_eq!(deduped.len(), 2);
}

#[test]
fn test_dedupe_network_services_keeps_distinct_pid_when_cwd_unknown() {
    let a = DiscoveredNetworkService {
        name: "web".to_string(),
        domain_template: "web.portzero.local".to_string(),
        substitutions: BTreeMap::new(),
        real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 5173)),
        service_port: 5173,
        backend_protocol: None,
        pid: 100,
        source: ServiceSource::Process { cwd: None },
        health_path: None,
    };

    let b = DiscoveredNetworkService {
        pid: 200,
        ..a.clone()
    };

    let deduped = dedupe_network_services(vec![a, b]);
    assert_eq!(deduped.len(), 2);
}
