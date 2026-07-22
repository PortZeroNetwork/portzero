use super::*;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

fn proc_svc(name: &str, pid: u32, cwd: Option<&str>) -> DiscoveredNetworkService {
    DiscoveredNetworkService {
        name: name.to_string(),
        domain_template: format!("{name}.portzero.local"),
        substitutions: Default::default(),
        real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30000),
        service_port: 5432,
        backend_protocol: None,
        pid,
        source: ServiceSource::Process {
            cwd: cwd.map(PathBuf::from),
        },
        health_path: None,
    }
}

fn container_svc(name: &str, id: &str) -> DiscoveredNetworkService {
    DiscoveredNetworkService {
        name: name.to_string(),
        domain_template: format!("{name}.portzero.local"),
        substitutions: Default::default(),
        real_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30000),
        service_port: 5432,
        backend_protocol: None,
        pid: 0,
        source: ServiceSource::Container {
            id: id.to_string(),
            name: name.to_string(),
        },
        health_path: None,
    }
}

fn cloud_proc_svc(domain: &str, pid: u32, cwd: Option<&str>) -> DiscoveredService {
    DiscoveredService {
        domain: domain.to_string(),
        domain_template: domain.to_string(),
        substitutions: Default::default(),
        port: 8080,
        extra_ports: Vec::new(),
        health_path: None,
        pid,
        source: ServiceSource::Process {
            cwd: cwd.map(PathBuf::from),
        },
    }
}

#[test]
fn test_cloud_same_url_different_cwd_is_duplicate() {
    // Two worktrees advertising the same cloud URL from distinct contexts.
    // (The same-cwd dedup and container-vs-process cases share the same core
    // as detect_duplicate_names and are covered by its tests.)
    let svcs = vec![
        cloud_proc_svc(
            "api.alice.tunnel.portzero.cloud",
            100,
            Some("/work/feature-a"),
        ),
        cloud_proc_svc(
            "api.alice.tunnel.portzero.cloud",
            200,
            Some("/work/feature-b"),
        ),
    ];
    let issues = detect_duplicate_cloud_urls(&svcs);
    assert_eq!(issues.len(), 1);
    match &issues[0] {
        Issue::DuplicateCloudUrl { url, claimants } => {
            assert_eq!(url, "api.alice.tunnel.portzero.cloud");
            assert_eq!(claimants.len(), 2);
        }
        other => panic!("unexpected issue: {:?}", other),
    }
}

#[test]
fn test_cloud_detection_ignores_local_overlay_names() {
    // `.portzero.local` names are handled by detect_duplicate_names; the
    // cloud detector must skip them so they are not reported twice.
    let svcs = vec![
        cloud_proc_svc("db.portzero.local", 100, Some("/work/a")),
        cloud_proc_svc("db.portzero.local", 200, Some("/work/b")),
    ];
    assert!(detect_duplicate_cloud_urls(&svcs).is_empty());
}

#[test]
fn test_cloud_duplicate_url_summary_and_fix_hint() {
    let issue = Issue::DuplicateCloudUrl {
        url: "api.alice.tunnel.portzero.cloud".to_string(),
        claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
    };
    assert!(issue.summary().contains("api.alice.tunnel.portzero.cloud"));
    assert!(issue.summary().contains("2 contexts"));
    // Fix hint suggests a per-checkout template and points at `portzero status`.
    assert!(issue
        .fix_hint()
        .contains("api-{branch}.alice.tunnel.portzero.cloud"));
    assert!(issue.fix_hint().contains("portzero status"));
    assert_eq!(issue.pid(), None);
    assert!(!issue.needs_login());
}

#[test]
fn test_cloud_duplicate_url_is_a_user_facing_problem() {
    // Unlike LegacyListener, a duplicate cloud URL must surface as a
    // problem (and therefore fire a notification via publish_issues).
    let issues = IssuesState {
        issues: vec![Issue::DuplicateCloudUrl {
            url: "api.alice.tunnel.portzero.cloud".to_string(),
            claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
        }],
    };
    let problems = collect_problems(&issues, None);
    assert_eq!(problems.len(), 1);
    assert!(problems[0]
        .title
        .contains("api.alice.tunnel.portzero.cloud"));
}

#[test]
fn test_no_duplicates_when_unique_names() {
    let svcs = vec![
        proc_svc("db", 100, Some("/work/a")),
        proc_svc("api", 101, Some("/work/b")),
    ];
    assert!(detect_duplicate_names(&svcs).is_empty());
}

#[test]
fn test_same_name_same_cwd_is_not_duplicate() {
    // Same worktree scanned twice (e.g. parent + child process) — one cwd.
    let svcs = vec![
        proc_svc("db", 100, Some("/work/a")),
        proc_svc("db", 200, Some("/work/a")),
    ];
    assert!(
        detect_duplicate_names(&svcs).is_empty(),
        "same cwd must not be flagged as a worktree conflict"
    );
}

#[test]
fn test_same_name_different_cwd_is_duplicate() {
    let svcs = vec![
        proc_svc("db", 100, Some("/work/feature-a")),
        proc_svc("db", 200, Some("/work/feature-b")),
    ];
    let issues = detect_duplicate_names(&svcs);
    assert_eq!(issues.len(), 1);
    match &issues[0] {
        Issue::DuplicateName { name, claimants } => {
            assert_eq!(name, "db");
            assert_eq!(claimants.len(), 2);
        }
        other => panic!("unexpected issue: {:?}", other),
    }
}

#[test]
fn test_container_vs_process_same_name_is_duplicate() {
    let svcs = vec![
        proc_svc("db", 100, Some("/work/a")),
        container_svc("db", "abc123"),
    ];
    assert_eq!(detect_duplicate_names(&svcs).len(), 1);
}

#[test]
fn test_two_containers_same_name_distinct_ids_is_duplicate() {
    let svcs = vec![container_svc("db", "aaa"), container_svc("db", "bbb")];
    assert_eq!(detect_duplicate_names(&svcs).len(), 1);
}

#[test]
fn test_issue_summary_and_fix_hint() {
    let issue = Issue::DuplicateName {
        name: "db".to_string(),
        claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
    };
    assert!(issue.summary().contains("db"));
    assert!(issue.summary().contains("2 contexts"));
    assert!(issue.fix_hint().contains("{branch}"));
    assert!(issue.fix_hint().contains("db-"));
}

#[test]
fn test_issues_state_roundtrip() {
    let state = IssuesState {
        issues: vec![Issue::DuplicateName {
            name: "db".to_string(),
            claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
        }],
    };
    let json = state.to_json();
    let parsed = IssuesState::from_json(&json);
    assert_eq!(state, parsed);
}

#[test]
fn test_legacy_and_docker_issue_roundtrip() {
    let state = IssuesState {
        issues: vec![
            Issue::LegacyListener {
                port: 5432,
                pid: 4321,
                context: "(~/work/api)".to_string(),
            },
            Issue::DockerPortConflict {
                port: 8080,
                container: "web-1".to_string(),
            },
        ],
    };
    let parsed = IssuesState::from_json(&state.to_json());
    assert_eq!(state, parsed);

    // summary / fix_hint mention the salient details.
    let legacy = &state.issues[0];
    assert!(legacy.summary().contains("5432"));
    assert!(legacy.summary().contains("4321"));
    assert!(legacy.fix_hint().contains("PZ_TUNNEL"));

    let docker = &state.issues[1];
    assert!(docker.summary().contains("web-1"));
    assert!(docker.summary().contains("8080"));
    assert!(docker.fix_hint().contains("8080"));
}

#[test]
fn test_invalid_cloud_tunnel_scope_issue_roundtrip() {
    let state = IssuesState {
        issues: vec![Issue::InvalidCloudTunnelScope {
            domain: "myservice.portzero.cloud".to_string(),
            reason: "'myservice.portzero.cloud' is not a valid cloud tunnel domain \
                    it must end with '.tunnel.portzero.cloud' and be scoped to your \
                    username, e.g. 'myservice--<username>.tunnel.portzero.cloud'. \
                    Run `portzero whoami` to find your username."
                .to_string(),
            context: "pid 1234".to_string(),
            pid: Some(1234),
        }],
    };
    let parsed = IssuesState::from_json(&state.to_json());
    assert_eq!(state, parsed);

    let issue = &state.issues[0];
    assert!(issue.summary().contains("myservice.portzero.cloud"));
    assert!(issue.summary().contains("pid 1234"));
    assert!(issue.fix_hint().contains("portzero whoami"));
    assert!(issue.fix_hint().contains("<username>"));
    assert_eq!(issue.pid(), Some(1234));
}

#[test]
fn test_issues_state_empty_roundtrip() {
    let state = IssuesState::default();
    assert!(state.is_empty());
    let parsed = IssuesState::from_json(&state.to_json());
    assert!(parsed.is_empty());
}

#[test]
fn test_issues_state_from_garbage_is_empty() {
    assert!(IssuesState::from_json("not json").is_empty());
    assert!(IssuesState::from_json("").is_empty());
}

#[test]
fn test_detection_is_stable_ordering() {
    // Two distinct names in conflict; output order must be deterministic
    // (sorted by name) so persisted JSON is stable across scans.
    let svcs = vec![
        proc_svc("zebra", 1, Some("/a")),
        proc_svc("zebra", 2, Some("/b")),
        proc_svc("alpha", 3, Some("/c")),
        proc_svc("alpha", 4, Some("/d")),
    ];
    let issues = detect_duplicate_names(&svcs);
    assert_eq!(issues.len(), 2);
    match (&issues[0], &issues[1]) {
        (Issue::DuplicateName { name: n0, .. }, Issue::DuplicateName { name: n1, .. }) => {
            assert_eq!(n0, "alpha");
            assert_eq!(n1, "zebra");
        }
        other => panic!("unexpected issues: {:?}", other),
    }
}

#[test]
fn test_build_notify_command_nonempty() {
    let cmd = build_notify_command("Title", "Body with \"quotes\" and 'apostrophes'");
    assert!(!cmd.program.is_empty());
    assert!(!cmd.args.is_empty());
    // The body content must appear somewhere in the constructed args.
    let joined = cmd.args.join(" ");
    assert!(joined.contains("Body"));
}

#[cfg(target_os = "linux")]
#[test]
fn test_build_notify_command_linux() {
    let cmd = build_notify_command("My Title", "My Body");
    assert_eq!(cmd.program, "notify-send");
    assert!(cmd.args.contains(&"My Title".to_string()));
    assert!(cmd.args.contains(&"My Body".to_string()));
}

#[cfg(target_os = "macos")]
#[test]
fn test_applescript_escape() {
    assert_eq!(applescript_escape(r#"a"b\c"#), r#"a\"b\\c"#);
}

#[cfg(target_os = "windows")]
#[test]
fn test_powershell_escape() {
    assert_eq!(powershell_escape("it's"), "it''s");
}

#[test]
fn test_write_then_read_issues() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("issues.json");
    let state = IssuesState {
        issues: vec![Issue::DuplicateName {
            name: "db".to_string(),
            claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
        }],
    };
    write_issues(&path, &state);
    assert_eq!(read_issues(&path), state);
}

#[test]
fn test_read_issues_missing_file() {
    let state = read_issues(std::path::Path::new("/nonexistent/issues.json"));
    assert!(state.is_empty());
}

#[test]
fn test_collect_problems_combines_and_ranks_by_severity() {
    use crate::diagnostics::{Diagnostic, DiagnosticsReport, Fix, FixKind, Severity};

    // A LegacyListener issue plus a DuplicateName issue — only the latter
    // is a user-facing problem (LegacyListener is log-only, see
    // `publish_issues`).
    let issues = IssuesState {
        issues: vec![
            Issue::LegacyListener {
                port: 5432,
                pid: 4321,
                context: "(~/work/api)".to_string(),
            },
            Issue::DuplicateName {
                name: "db".to_string(),
                claimants: vec!["pid 1 (~/a)".to_string(), "pid 2 (~/b)".to_string()],
            },
        ],
    };
    let report = DiagnosticsReport {
        generated_at: "2026-07-11T00:00:00Z".to_string(),
        checks_run: 3,
        issues: vec![
            Diagnostic {
                id: "binary_missing".into(),
                severity: Severity::Critical,
                category: "binary".into(),
                title: "Binary missing".to_string(),
                detail: "detail".to_string(),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Reinstall".to_string(),
                    command: Some("portzero install".to_string()),
                }),
            },
            Diagnostic {
                id: "dns_probe_ok".into(),
                severity: Severity::Info,
                category: "dns".into(),
                title: "portzero.local resolution works".to_string(),
                detail: "ok".to_string(),
                fix: None,
            },
        ],
    };

    let problems = collect_problems(&issues, Some(&report));

    // Info-level diagnostics and the LegacyListener issue are filtered
    // out; the duplicate-name issue and the critical diagnostic survive.
    assert_eq!(problems.len(), 2);
    // Sorted severity-first: Critical before the (assumed) Error-ranked issue.
    assert_eq!(problems[0].severity, Severity::Critical);
    assert_eq!(problems[0].title, "Binary missing");
    assert_eq!(problems[0].fix_command.as_deref(), Some("portzero install"));

    assert!(
        !problems.iter().any(|p| p.title.contains("5432")),
        "LegacyListener must not be surfaced as a user-facing problem"
    );
    assert!(problems.iter().any(|p| p.title.contains("db")));

    assert!(
        !problems
            .iter()
            .any(|p| p.title.contains("resolution works")),
        "informational diagnostics must be filtered out"
    );
}

#[test]
fn cloud_tunnel_not_allowed_surfaces_domain_and_reason() {
    let issue = Issue::CloudTunnelNotAllowed {
        domain: "web--bob.tunnel.portzero.cloud".to_string(),
        reason: "namespace 'bob' is not one you can use. You can use: alice, acme.".to_string(),
        pid: Some(4321),
    };
    // Summary names the domain; the fix hint is the full reason verbatim.
    assert!(issue.summary().contains("web--bob.tunnel.portzero.cloud"));
    assert!(issue.fix_hint().contains("namespace 'bob'"));
    assert!(issue.fix_hint().contains("alice, acme"));
    assert_eq!(issue.pid(), Some(4321));
    assert!(!issue.needs_login());
    // Round-trips through the persisted issues.json shape.
    let state = IssuesState {
        issues: vec![issue],
    };
    assert_eq!(state, IssuesState::from_json(&state.to_json()));
}
