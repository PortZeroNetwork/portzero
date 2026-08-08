use super::*;

// parse_http_port_selection

#[test]
fn parse_http_port_selection_covers_all_forms() {
    let cases: &[(&str, HttpPortSelection)] = &[
        ("", HttpPortSelection::ChooseLowest),
        ("   ", HttpPortSelection::ChooseLowest),
        ("CHOOSE_LOWEST", HttpPortSelection::ChooseLowest),
        ("CHOOSE_HIGHEST", HttpPortSelection::ChooseHighest),
        (" 8080 ", HttpPortSelection::Explicit(8080)),
        ("65535", HttpPortSelection::Explicit(65535)),
        ("1", HttpPortSelection::Explicit(1)),
        // Malformed/out-of-range input degrades to the default rather
        // than panicking.
        ("not-a-port", HttpPortSelection::ChooseLowest),
        ("70000", HttpPortSelection::ChooseLowest), // overflows u16
        ("-1", HttpPortSelection::ChooseLowest),
        ("choose_lowest", HttpPortSelection::ChooseLowest), // wrong case, not a number
    ];
    for (input, expected) in cases {
        assert_eq!(
            parse_http_port_selection(input),
            *expected,
            "input: {input:?}"
        );
    }
}

// parse_extra_ports

#[test]
fn parse_extra_ports_valid_entries() {
    let mappings = parse_extra_ports("9222:9222;9300:9301");
    assert_eq!(
        mappings,
        vec![
            PortMapping {
                local_port: 9222,
                tunnel_port: 9222
            },
            PortMapping {
                local_port: 9300,
                tunnel_port: 9301
            },
        ]
    );
}

#[test]
fn parse_extra_ports_skips_malformed_entries_but_keeps_good_ones() {
    // A missing tunnel port, a non-numeric port, and an empty entry
    // between semicolons must all be skipped without dropping the
    // otherwise-valid entries around them.
    let mappings = parse_extra_ports("9222:9222;bad-entry;;5000:notaport;6000:6001");
    assert_eq!(
        mappings,
        vec![
            PortMapping {
                local_port: 9222,
                tunnel_port: 9222
            },
            PortMapping {
                local_port: 6000,
                tunnel_port: 6001
            },
        ]
    );
}

#[test]
fn parse_extra_ports_empty_and_whitespace_only() {
    assert!(parse_extra_ports("").is_empty());
    assert!(parse_extra_ports("   ").is_empty());
    assert!(parse_extra_ports(";;;").is_empty());
}

#[test]
fn parse_extra_ports_trims_whitespace_around_entries() {
    let mappings = parse_extra_ports(" 1000 : 2000 ; 3000:4000 ");
    assert_eq!(
        mappings,
        vec![
            PortMapping {
                local_port: 1000,
                tunnel_port: 2000
            },
            PortMapping {
                local_port: 3000,
                tunnel_port: 4000
            },
        ]
    );
}

// select_http_port

#[test]
fn select_http_port_explicit_prefers_requested_over_lowest() {
    let ports = vec![ListeningPort::v4_any(3000), ListeningPort::v4_any(8080)];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::Explicit(8080)),
        SelectedPort::Found(8080)
    );
}

#[test]
fn select_http_port_choose_highest_falls_back_to_loopback_when_no_public() {
    let ports = vec![
        ListeningPort::v4_loopback(4000),
        ListeningPort::v4_loopback(9000),
    ];
    assert_eq!(
        select_http_port(&ports, &HttpPortSelection::ChooseHighest),
        SelectedPort::Found(9000)
    );
}

// parse_proc_net_tcp_line (Linux /proc/<pid>/net/tcp[6] parsing)

#[cfg(target_os = "linux")]
#[test]
fn parse_proc_net_tcp_line_ipv6_public_bind() {
    // 32 hex chars of zero == IPv6 "::" (all interfaces), port 1F90 = 8080.
    let line = "0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 100 0 0 10 0";
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(555u64);
    let p = parse_proc_net_tcp_line(line, &inodes).expect("should parse");
    assert_eq!(p.port, 8080);
    assert_eq!(p.addr, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
}

#[cfg(target_os = "linux")]
#[test]
fn parse_proc_net_tcp_line_malformed_lines_return_none() {
    let inodes = std::collections::HashSet::from([1u64]);
    // Too few whitespace-separated columns.
    assert!(parse_proc_net_tcp_line("0: 0100007F:1F90 0A", &inodes).is_none());
    // Empty line.
    assert!(parse_proc_net_tcp_line("", &inodes).is_none());
    // Non-hex garbage in the port position.
    let garbage = "0: 0100007F:ZZZZ 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0000000000000000 100 0 0 10 0";
    assert!(parse_proc_net_tcp_line(garbage, &inodes).is_none());
    // Port 0 is filtered out even for an owned, listening inode.
    let zero_port = "0: 0100007F:0000 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0000000000000000 100 0 0 10 0";
    assert!(parse_proc_net_tcp_line(zero_port, &inodes).is_none());
    // Missing `:` separator between address and port.
    let no_colon = "0: 0100007F1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0000000000000000 100 0 0 10 0";
    assert!(parse_proc_net_tcp_line(no_colon, &inodes).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn parse_proc_net_tcp_line_ipv6_loopback_bind() {
    // The Vite-on-dual-stack case: Node >= 17 stopped reordering DNS results,
    // so a server told to bind "localhost" lands on [::1] alone. `::1` in
    // /proc is the last 32-bit word set, printed in host byte order.
    let line = "0: 00000000000000000000000001000000:1435 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 100 0 0 10 0";
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(555u64);
    let p = parse_proc_net_tcp_line(line, &inodes).expect("should parse");
    assert_eq!(p.port, 5173);
    assert_eq!(p.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(p.dial_addr(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert!(!p.is_wildcard());
}

#[cfg(target_os = "linux")]
#[test]
fn parse_proc_net_tcp_line_specific_and_v4_mapped_addresses() {
    let mut inodes = std::collections::HashSet::new();
    inodes.insert(555u64);

    // C0A80105 = 192.168.1.5 in host byte order — a specific interface, which
    // the old all-zeros check mislabeled as loopback.
    let lan = "0: 0501A8C0:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 100 0 0 10 0";
    let p = parse_proc_net_tcp_line(lan, &inodes).expect("should parse");
    assert_eq!(p.addr, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)));
    assert!(!p.is_wildcard());
    // Dialed as bound: there is nothing on loopback to reach.
    assert_eq!(p.dial_addr(), IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)));

    // An IPv4-mapped address in tcp6 (::ffff:127.0.0.1) is an IPv4 listener.
    let mapped = "0: 0000000000000000FFFF00000100007F:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 100 0 0 10 0";
    let p = parse_proc_net_tcp_line(mapped, &inodes).expect("should parse");
    assert_eq!(p.addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
}

// parse_lsof_stdout / parse_lsof_line

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parse_lsof_line_reads_the_family_of_a_bare_star_from_the_type_column() {
    // lsof prints a wildcard bind as `*` for both families, so only the TYPE
    // column separates `0.0.0.0` from `::`.
    let v6 = parse_lsof_line("node 100 u 5u IPv6 0x0 0t0 TCP *:5173 (LISTEN)")
        .expect("IPv6 star parses");
    assert_eq!(v6.addr, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    // A `::` socket accepts ::1 whether or not IPV6_V6ONLY is set.
    assert_eq!(v6.dial_addr(), IpAddr::V6(Ipv6Addr::LOCALHOST));

    let v4 = parse_lsof_line("node 100 u 5u IPv4 0x0 0t0 TCP *:5173 (LISTEN)")
        .expect("IPv4 star parses");
    assert_eq!(v4.addr, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(v4.dial_addr(), IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parse_lsof_line_drops_an_ipv6_zone_index() {
    // A link-local address carries a zone (`%lo0`) that `IpAddr` cannot parse;
    // it must not sink the whole listener.
    let p = parse_lsof_line("node 100 u 5u IPv6 0x0 0t0 TCP [fe80::1%lo0]:8080 (LISTEN)")
        .expect("zoned address parses");
    assert_eq!(p.port, 8080);
    assert_eq!(p.addr, IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap()));
}

#[test]
fn dial_addr_maps_each_bind_form_to_something_reachable() {
    // 0.0.0.0 and :: are not connectable addresses; loopback of the same
    // family is. Everything else is already an address to dial.
    assert_eq!(
        ListeningPort::v4_any(80).dial_addr(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    assert_eq!(
        ListeningPort::v6_any(80).dial_addr(),
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
    assert_eq!(
        ListeningPort::v4_loopback(80).dial_addr(),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    assert_eq!(
        ListeningPort::v6_loopback(80).dial_addr(),
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
}

#[test]
fn dial_host_for_port_follows_the_family_of_the_selected_port() {
    // A process with an IPv4 debugger port and an IPv6-only HTTP port: each
    // port has to be dialed on its own family.
    let ports = vec![
        ListeningPort::v4_loopback(9229),
        ListeningPort::v6_loopback(5173),
    ];
    assert_eq!(
        dial_host_for_port(&ports, 5173),
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    );
    assert_eq!(
        dial_host_for_port(&ports, 9229),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
    // A port discovery never saw keeps the historical IPv4 loopback.
    assert_eq!(
        dial_host_for_port(&ports, 8080),
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parse_lsof_stdout_empty_and_header_only() {
    assert!(parse_lsof_stdout("").is_empty());
    assert!(
        parse_lsof_stdout("COMMAND     PID     USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\n")
            .is_empty()
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn parse_lsof_line_malformed_or_partial() {
    // No colon anywhere in the line.
    assert!(parse_lsof_line("just some garbage text").is_none());
    // A colon present but with a non-numeric tail.
    assert!(parse_lsof_line("node 1 u 5u IPv4 0x0 0t0 TCP host:abc (LISTEN)").is_none());
}

// parse_windows_tcp_connection_stdout / _line

#[test]
fn parse_windows_tcp_connection_stdout_mixed_valid_and_invalid_lines() {
    let stdout = "0.0.0.0|8080\nmalformed-line\n127.0.0.1|0\n127.0.0.1|5173\n\n";
    let ports = parse_windows_tcp_connection_stdout(stdout);
    assert_eq!(
        ports,
        vec![
            ListeningPort::v4_any(8080),
            ListeningPort::v4_loopback(5173),
        ]
    );
}

#[test]
fn parse_windows_tcp_connection_line_overflowing_port_is_none() {
    assert!(parse_windows_tcp_connection_line("0.0.0.0|99999").is_none());
    assert!(parse_windows_tcp_connection_line("no-pipe-here").is_none());
    assert!(parse_windows_tcp_connection_line("").is_none());
}

// parse_windows_netstat_stdout / _by_pid / line helpers

#[test]
fn parse_windows_netstat_stdout_by_pid_groups_multiple_pids_and_skips_noise() {
    let stdout = "\
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       111
  TCP    127.0.0.1:5173         0.0.0.0:0              LISTENING       111
  TCP    127.0.0.1:9229         0.0.0.0:0              ESTABLISHED     222
  UDP    0.0.0.0:5353           *:*                                    333
  TCP    [::]:3000              [::]:0                 LISTENING       444
  garbage line with too few columns
";
    let by_pid = parse_windows_netstat_stdout_by_pid(stdout);
    assert_eq!(by_pid.len(), 2);
    let mut pid_111 = by_pid.get(&111).cloned().unwrap_or_default();
    pid_111.sort_by_key(|p| p.port);
    assert_eq!(
        pid_111,
        vec![
            ListeningPort::v4_loopback(5173),
            ListeningPort::v4_any(8080),
        ]
    );
    assert_eq!(by_pid.get(&444), Some(&vec![ListeningPort::v6_any(3000)]));
    assert!(!by_pid.contains_key(&222)); // ESTABLISHED, not LISTENING
    assert!(!by_pid.contains_key(&333)); // UDP, not TCP
}

#[test]
fn parse_windows_netstat_stdout_by_pid_keeps_an_ipv6_only_listener() {
    // netstat prints `TCP` in the Proto column for both families and brackets
    // the IPv6 address. The rows only exist when netstat is invoked WITHOUT
    // `-p tcp` (that flag means IPv4 TCP only on Windows, and hid IPv6-only
    // services from discovery entirely) — see NETSTAT_ARGS.
    let stdout = "\
  Proto  Local Address          Foreign Address        State           PID
  TCP    [::1]:18081            [::]:0                 LISTENING       777
";
    let by_pid = parse_windows_netstat_stdout_by_pid(stdout);
    let ports = by_pid.get(&777).cloned().unwrap_or_default();
    assert_eq!(ports, vec![ListeningPort::v6_loopback(18081)]);
    assert_eq!(ports[0].dial_addr(), IpAddr::V6(Ipv6Addr::LOCALHOST));
}

#[test]
fn parse_windows_netstat_line_filters_by_requested_pid() {
    let line = "  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       111";
    assert!(parse_windows_netstat_line(line, 111).is_some());
    assert!(parse_windows_netstat_line(line, 999).is_none());
}

#[test]
fn parse_windows_local_address_port_ipv6_bracket_form() {
    let p = parse_windows_local_address_port("[::1]:57889").expect("should parse");
    assert_eq!(p.port, 57889);
    assert_eq!(p.addr, IpAddr::V6(Ipv6Addr::LOCALHOST));

    let p = parse_windows_local_address_port("[::]:8080").expect("should parse");
    assert_eq!(p.port, 8080);
    assert_eq!(p.addr, IpAddr::V6(Ipv6Addr::UNSPECIFIED));
}

#[test]
fn parse_windows_local_address_port_malformed() {
    // Bracket opened but never closed with "]:".
    assert!(parse_windows_local_address_port("[::1").is_none());
    // No colon at all.
    assert!(parse_windows_local_address_port("nocolonhere").is_none());
    // Port 0 filtered.
    assert!(parse_windows_local_address_port("127.0.0.1:0").is_none());
}

// parse_windows_environment_block (UTF-16, NUL-delimited, double-NUL end)

#[test]
fn parse_windows_environment_block_multiple_entries() {
    let words: Vec<u16> = "Path=C:\\Windows\0PZ_TUNNEL=api.portzero.local\0TEMP=C:\\Temp\0\0"
        .encode_utf16()
        .collect();
    assert_eq!(
        parse_windows_environment_block(&words),
        vec![
            "Path=C:\\Windows".to_string(),
            "PZ_TUNNEL=api.portzero.local".to_string(),
            "TEMP=C:\\Temp".to_string(),
        ]
    );
}

#[test]
fn parse_windows_environment_block_empty_and_single_terminator() {
    assert!(parse_windows_environment_block(&[]).is_empty());
    // A single leading NUL means the very first "entry" is zero-length —
    // must stop immediately rather than emit an empty string.
    assert!(parse_windows_environment_block(&[0]).is_empty());
}

// parse_procargs2_env (macOS KERN_PROCARGS2 buffer layout)

fn build_procargs2_buf(argc: i32, exec_path: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&argc.to_ne_bytes());
    buf.extend_from_slice(exec_path.as_bytes());
    buf.push(0);
    buf.push(0); // padding between exec_path and argv[0]
    for arg in argv {
        buf.extend_from_slice(arg.as_bytes());
        buf.push(0);
    }
    for e in env {
        buf.extend_from_slice(e.as_bytes());
        buf.push(0);
    }
    buf
}

#[test]
fn parse_procargs2_env_typical_buffer() {
    let buf = build_procargs2_buf(
        2,
        "/usr/bin/demo",
        &["demo", "--serve"],
        &["PWD=/tmp/project", "PZ_TUNNEL=rust-demo.portzero.local:80"],
    );
    let env = parse_procargs2_env(&buf).expect("env parsed");
    assert!(env.contains(&"PWD=/tmp/project".to_string()));
    assert!(env.contains(&"PZ_TUNNEL=rust-demo.portzero.local:80".to_string()));
    assert!(!env.iter().any(|e| e == "demo" || e == "--serve"));
}

#[test]
fn parse_procargs2_env_buffer_too_short_returns_none() {
    // Fewer bytes than a single i32 argc field.
    assert!(parse_procargs2_env(&[0u8, 1u8]).is_none());
    assert!(parse_procargs2_env(&[]).is_none());
}

#[test]
fn parse_procargs2_env_zero_argc_yields_only_environment() {
    let buf = build_procargs2_buf(
        0,
        "/bin/sh",
        &[],
        &["FOO=bar", "PZ_TUNNEL=x.portzero.local"],
    );
    let env = parse_procargs2_env(&buf).expect("env parsed");
    assert_eq!(
        env,
        vec![
            "FOO=bar".to_string(),
            "PZ_TUNNEL=x.portzero.local".to_string()
        ]
    );
}

#[test]
fn parse_procargs2_env_no_environment_present_yields_empty_vec() {
    // Buffer ends immediately after the argv strings — no environment.
    let buf = build_procargs2_buf(1, "/bin/sh", &["sh"], &[]);
    let env = parse_procargs2_env(&buf).expect("env parsed");
    assert!(env.is_empty());
}

// parse_macos_ps_env_candidate(s) (`ps -axo pid=,command= -wwwE` fallback)

#[test]
fn parse_macos_ps_env_candidate_extracts_pid_and_value() {
    let line = "64039 target/debug/portzero-rust-local-process CARGO=/Users/loumtech/.cargo/bin/cargo PWD=/tmp/project PZ_TUNNEL=rust-demo.portzero.local:80 RUST_RECURSION_COUNT=1";
    assert_eq!(
        parse_macos_ps_env_candidate(line, ENV_VAR_NAME),
        Some((64039, "rust-demo.portzero.local:80".to_string()))
    );
}

#[test]
fn parse_macos_ps_env_candidate_malformed_lines() {
    // No variable present at all.
    assert_eq!(
        parse_macos_ps_env_candidate(
            "70462 /Users/loumtech/.cargo/bin/portzero start --foreground",
            ENV_VAR_NAME
        ),
        None
    );
    // Variable present but with an empty value must be treated as absent.
    assert_eq!(
        parse_macos_ps_env_candidate("123 some-command PZ_TUNNEL=", ENV_VAR_NAME),
        None
    );
    // PID isn't numeric.
    assert_eq!(
        parse_macos_ps_env_candidate("not-a-pid command PZ_TUNNEL=x", ENV_VAR_NAME),
        None
    );
    // Only a PID, no command/env at all.
    assert_eq!(parse_macos_ps_env_candidate("123", ENV_VAR_NAME), None);
    // Empty line.
    assert_eq!(parse_macos_ps_env_candidate("", ENV_VAR_NAME), None);
}

#[test]
fn parse_macos_ps_env_candidates_multi_line_stdout() {
    let stdout = "\
1 /sbin/launchd
64039 demo CARGO=/usr/bin/cargo PZ_TUNNEL=demo.portzero.local
70462 /usr/bin/portzero start --foreground
99999 other PZ_TUNNEL=other.tunnel.portzero.cloud
";
    let candidates = parse_macos_ps_env_candidates(stdout, ENV_VAR_NAME);
    assert_eq!(
        candidates,
        vec![
            (64039, "demo.portzero.local".to_string()),
            (99999, "other.tunnel.portzero.cloud".to_string()),
        ]
    );
}

// looks_like_repo_path

#[test]
fn looks_like_repo_path_covers_prefixes_and_extensions() {
    assert!(looks_like_repo_path("/absolute/path"));
    assert!(looks_like_repo_path("./relative"));
    assert!(looks_like_repo_path("../parent-relative"));
    assert!(looks_like_repo_path("nested/dir"));
    assert!(looks_like_repo_path("app.py"));
    assert!(looks_like_repo_path("main.go"));
    assert!(!looks_like_repo_path("serve"));
    assert!(!looks_like_repo_path(""));
    assert!(!looks_like_repo_path("--flag"));
}

// resolve_tunnel_template / template_substitutions

#[test]
fn resolve_tunnel_template_returns_raw_when_no_placeholders() {
    assert_eq!(
        resolve_tunnel_template("plain-name.portzero.local", None, None, None),
        "plain-name.portzero.local"
    );
}

#[test]
fn resolve_tunnel_template_substitutes_branch_placeholder_without_project_dir() {
    // With no project_dir, git-derived values stay "unknown" — deterministic
    // regardless of the host running the test.
    assert_eq!(
        resolve_tunnel_template("svc-{branch}.portzero.local", None, None, None),
        "svc-unknown.portzero.local"
    );
}

#[test]
fn template_substitutions_uses_source_name_over_project_dir() {
    let subs = template_substitutions(
        Some(Path::new("/work/from-project-dir")),
        None,
        None,
        Some("/my-container-name"),
    );
    // Leading '/' is trimmed and the explicit source name wins over the
    // project dir's file name.
    assert_eq!(
        subs.get("folder-name").map(String::as_str),
        Some("my-container-name")
    );
}

#[test]
fn template_substitutions_falls_back_to_unknown_folder_name() {
    let subs = template_substitutions(None, None, None, None);
    assert_eq!(subs.get("folder-name").map(String::as_str), Some("unknown"));
}

// warn_if_port_like_rejected: pure side-effect (tracing) function, exercised
// here only to ensure no panics across its branches.
#[test]
fn warn_if_port_like_rejected_does_not_panic_on_any_branch() {
    warn_if_port_like_rejected("svc.portzero.local:99999", None, 1); // rejected numeric tail
    warn_if_port_like_rejected("svc.portzero.local:abc", None, 1); // non-numeric tail, no warning
    warn_if_port_like_rejected("svc.portzero.local", None, 1); // no colon at all
    warn_if_port_like_rejected("svc.portzero.local:8080", Some(8080), 1); // accepted port, no warning
}
