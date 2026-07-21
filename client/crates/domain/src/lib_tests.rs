use super::*;

#[test]
fn test_sanitize_basic() {
    assert_eq!(sanitize_for_dns("feat/login"), "feat-login");
    assert_eq!(sanitize_for_dns("my_branch.v2"), "my-branch-v2");
    assert_eq!(sanitize_for_dns("Hello World!"), "hello-world");
}

#[test]
fn test_sanitize_collapses_hyphens() {
    assert_eq!(sanitize_for_dns("a//b__c..d"), "a-b-c-d");
}

#[test]
fn test_sanitize_trims_hyphens() {
    assert_eq!(sanitize_for_dns("--foo--"), "foo");
    assert_eq!(sanitize_for_dns("/branch/"), "branch");
}

#[test]
fn test_sanitize_truncates_to_63() {
    let long = "a".repeat(100);
    let result = sanitize_for_dns(&long);
    assert_eq!(result.len(), 63);
}

#[test]
fn test_sanitize_truncation_no_trailing_hyphen() {
    let input = format!("{}/{}", "a".repeat(62), "bbb");
    let result = sanitize_for_dns(&input);
    assert!(!result.ends_with('-'));
    assert!(result.len() <= 63);
}

#[test]
fn test_sanitize_empty() {
    assert_eq!(sanitize_for_dns(""), "");
    assert_eq!(sanitize_for_dns("---"), "");
}

#[test]
fn test_resolve_all_variables() {
    let ctx = DomainContext {
        service: "api".to_string(),
        project: "myapp".to_string(),
        branch: "main".to_string(),
        worktree: Some("wt-feature".to_string()),
        user: "alice".to_string(),
        machine: "dev-box".to_string(),
        uid: Some("abc12345".to_string()),
        username: Some("alice-cloud".to_string()),
        pr: None,
        run_id: None,
    };

    let result = ctx.resolve(
        "{service}-{project}-{branch}-{user}-{uid}.{cloud-username}.tunnel.portzero.cloud",
    );
    assert_eq!(
        result,
        "api-myapp-main-alice-abc12345.alice-cloud.tunnel.portzero.cloud"
    );
}

#[test]
fn test_resolve_uid_falls_back_to_user() {
    let ctx = DomainContext {
        service: "web".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "bob".to_string(),
        machine: "laptop".to_string(),
        uid: None,
        username: None,
        pr: None,
        run_id: None,
    };

    let result = ctx.resolve("{service}-{uid}.{local-username}.tunnel.portzero.cloud");
    assert_eq!(result, "web-bob.bob.tunnel.portzero.cloud");
}

#[test]
fn test_resolve_worktree_falls_back_to_branch() {
    let ctx = DomainContext {
        service: "web".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "bob".to_string(),
        machine: "laptop".to_string(),
        uid: None,
        username: None,
        pr: None,
        run_id: None,
    };

    let result = ctx.resolve("{service}-{worktree}.{local-username}.tunnel.portzero.cloud");
    assert_eq!(result, "web-main.bob.tunnel.portzero.cloud");
}

#[test]
fn test_resolve_sanitizes_values() {
    let ctx = DomainContext {
        service: "my_api".to_string(),
        project: "My App".to_string(),
        branch: "feat/long-branch".to_string(),
        worktree: None,
        user: "Alice.B".to_string(),
        machine: "Dev Box!".to_string(),
        uid: None,
        username: None,
        pr: None,
        run_id: None,
    };

    let result = ctx.resolve("{service}-{project}-{branch}-{user}.{local-username}.example.com");
    assert_eq!(
        result,
        "my-api-my-app-feat-long-branch-alice-b.alice-b.example.com"
    );
}

#[test]
fn test_resolve_local_username_ignores_login_state() {
    let ctx = DomainContext {
        service: "api".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "osuser".to_string(),
        machine: "laptop".to_string(),
        uid: Some("abc12345".to_string()),
        username: Some("alice".to_string()),
        pr: None,
        run_id: None,
    };

    // {local-username} always resolves to the OS user, regardless of
    // whether a cloud account is logged in.
    let result = ctx.resolve("{service}.{local-username}.portzero.local");
    assert_eq!(result, "api.osuser.portzero.local");
}

#[test]
fn test_resolve_cloud_username_template() {
    let ctx = DomainContext {
        service: "api".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "osuser".to_string(),
        machine: "laptop".to_string(),
        uid: Some("abc12345".to_string()),
        username: Some("alice".to_string()),
        pr: None,
        run_id: None,
    };

    let result = ctx.resolve("{service}.{cloud-username}.tunnel.portzero.cloud");
    assert_eq!(result, "api.alice.tunnel.portzero.cloud");
}

#[test]
fn test_resolve_cloud_username_empty_when_not_logged_in() {
    let ctx = DomainContext {
        service: "api".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "osuser".to_string(),
        machine: "laptop".to_string(),
        uid: None,
        username: None,
        pr: None,
        run_id: None,
    };

    // No silent fallback to {uid}/{user} — an unresolved {cloud-username}
    // must surface as a diagnostic, not a mystery domain.
    let result = ctx.resolve("{service}.{cloud-username}.tunnel.portzero.cloud");
    assert_eq!(result, "api..tunnel.portzero.cloud");
}

// validate_username_placeholders tests

#[test]
fn test_validate_username_placeholders_local_username_in_cloud_template_errors() {
    let err =
        validate_username_placeholders("web.{local-username}.tunnel.portzero.cloud", false, true)
            .unwrap_err();
    assert!(err.contains("{local-username}"), "got: {err}");
    assert!(err.contains("cloud tunnel"), "got: {err}");
}

#[test]
fn test_validate_username_placeholders_local_username_in_local_template_ok() {
    assert!(
        validate_username_placeholders("web.{local-username}.portzero.local", true, false).is_ok()
    );
}

#[test]
fn test_validate_username_placeholders_cloud_username_in_local_template_requires_login() {
    let err = validate_username_placeholders("web.{cloud-username}.portzero.local", true, false)
        .unwrap_err();
    assert!(err.contains("logged in"), "got: {err}");
    assert!(
        err.contains("free"),
        "diagnostic must clarify local tunnels stay free, got: {err}"
    );
}

#[test]
fn test_validate_username_placeholders_cloud_username_in_local_template_ok_when_logged_in() {
    assert!(
        validate_username_placeholders("web.{cloud-username}.portzero.local", true, true).is_ok()
    );
}

#[test]
fn test_validate_username_placeholders_cloud_username_in_cloud_template_ok() {
    // Cloud templates already require login structurally (an unresolved
    // {cloud-username} fails validate_tunnel_domain), so this helper
    // doesn't need to special-case it.
    assert!(validate_username_placeholders(
        "web.{cloud-username}.tunnel.portzero.cloud",
        false,
        false
    )
    .is_ok());
}

fn ci_ctx(pr: Option<&str>, run_id: Option<&str>) -> DomainContext {
    DomainContext {
        service: "web".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "alice".to_string(),
        machine: "runner".to_string(),
        uid: None,
        username: Some("alice".to_string()),
        pr: pr.map(str::to_string),
        run_id: run_id.map(str::to_string),
    }
}

#[test]
fn test_resolve_pr_and_run_id_tokens() {
    let ctx = ci_ctx(Some("42"), Some("123456"));
    assert_eq!(
        ctx.resolve("web-pr-{pr}.alice.tunnel.portzero.cloud"),
        "web-pr-42.alice.tunnel.portzero.cloud"
    );
    assert_eq!(
        ctx.resolve("web-{run-id}.alice.tunnel.portzero.cloud"),
        "web-123456.alice.tunnel.portzero.cloud"
    );
}

#[test]
fn test_resolve_user_token_still_works() {
    // {user} resolves to the (local) username — unchanged existing behavior.
    let ctx = ci_ctx(None, None);
    assert_eq!(
        ctx.resolve("web-{user}.portzero.local"),
        "web-alice.portzero.local"
    );
}

#[test]
fn test_unresolved_pr_left_literal_then_flagged() {
    // Outside a PR, {pr} stays literal so validate_resolved_name can flag it
    // rather than producing "web-.example.com".
    let ctx = ci_ctx(None, None);
    let resolved = ctx.resolve("web-{pr}.alice.tunnel.portzero.cloud");
    assert_eq!(resolved, "web-{pr}.alice.tunnel.portzero.cloud");
    assert_eq!(unresolved_tokens(&resolved), vec!["{pr}".to_string()]);
    let err = validate_resolved_name(&resolved).unwrap_err();
    assert!(err.contains("{pr}"), "got: {err}");
}

#[test]
fn test_unresolved_run_id_flagged() {
    let ctx = ci_ctx(Some("7"), None);
    let resolved = ctx.resolve("web-{run-id}.alice.tunnel.portzero.cloud");
    assert!(validate_resolved_name(&resolved).is_err());
}

#[test]
fn test_validate_resolved_name_ok_for_clean_names() {
    assert!(validate_resolved_name("web-42.alice.tunnel.portzero.cloud").is_ok());
    assert!(validate_resolved_name("web-main.portzero.local").is_ok());
    // A single, deliberate `--` hierarchy separator is allowed.
    assert!(validate_resolved_name("feat--myapp.alice.tunnel.portzero.cloud").is_ok());
}

#[test]
fn test_validate_no_internal_double_hyphen() {
    assert!(validate_no_internal_double_hyphen("feat--myapp").is_ok());
    assert!(validate_no_internal_double_hyphen("my-api--web").is_ok());
    assert!(validate_no_internal_double_hyphen("plain").is_ok());
    // Ambiguous forms are rejected.
    assert!(validate_no_internal_double_hyphen("feat--").is_err());
    assert!(validate_no_internal_double_hyphen("--myapp").is_err());
    assert!(validate_no_internal_double_hyphen("a----b").is_err());
    assert!(validate_no_internal_double_hyphen("a---b").is_err());
}

#[test]
fn test_validate_resolved_name_rejects_internal_double_hyphen_label() {
    let err = validate_resolved_name("bad--.alice.tunnel.portzero.cloud").unwrap_err();
    assert!(err.contains("--"), "got: {err}");
}

#[test]
fn test_detect_project_name() {
    let name = detect_project_name(Path::new("/home/user/src/myapp"));
    assert_eq!(name, "myapp");
}

#[test]
fn test_optional_context_without_project_dir_stays_unknown() {
    let ctx = DomainContext::from_optional_environment("svc", None, None, None);
    assert_eq!(ctx.project, "unknown");
    assert_eq!(ctx.branch, "unknown");
    assert_eq!(ctx.worktree, None);
}

// validate_tunnel_domain tests

#[test]
fn test_validate_tunnel_domain_valid() {
    // Single-label `<name>--<cloud-username>` (or team slug), covered by the one
    // `*.tunnel.portzero.cloud` wildcard cert.
    assert!(validate_tunnel_domain("myapp--alice.tunnel.portzero.cloud").is_ok());
    assert!(validate_tunnel_domain("api-myapp-main--alice.tunnel.portzero.cloud").is_ok());
    assert!(validate_tunnel_domain("a--b.tunnel.portzero.cloud").is_ok());
    assert!(validate_tunnel_domain("my-api--alice.tunnel.portzero.cloud").is_ok());
    // Team slug as the namespace, hierarchical name via `--`.
    assert!(validate_tunnel_domain("svc--team-name.tunnel.portzero.cloud").is_ok());
    assert!(validate_tunnel_domain("ci-42--web--acme.tunnel.portzero.cloud").is_ok());
}

#[test]
fn test_validate_tunnel_domain_reserved_bare() {
    let err = validate_tunnel_domain("tunnel.portzero.cloud").unwrap_err();
    assert!(
        err.contains("reserved"),
        "Expected reserved error, got: {err}"
    );
}

#[test]
fn test_validate_tunnel_domain_missing_username_scope() {
    // A single label with no `--` carries no namespace scope.
    let err = validate_tunnel_domain("myapp.tunnel.portzero.cloud").unwrap_err();
    assert!(
        err.contains("<name>--<cloud-username>.tunnel.portzero.cloud"),
        "got: {err}"
    );
    assert!(err.contains("portzero whoami"), "got: {err}");
}

#[test]
fn test_validate_tunnel_domain_missing_tunnel_prefix() {
    // e.g. a user drops the `tunnel.` segment entirely: myservice.portzero.cloud
    let err = validate_tunnel_domain("myservice.portzero.cloud").unwrap_err();
    assert!(
        err.contains("tunnel.portzero.cloud"),
        "should mention the expected base domain, got: {err}"
    );
    assert!(
        err.contains("<cloud-username>"),
        "should mention the cloud username scope requirement, got: {err}"
    );
    assert!(err.contains("portzero whoami"), "got: {err}");
}

#[test]
fn test_validate_tunnel_domain_multi_label_rejected() {
    // Dotted multi-label names have no wildcard-cert coverage on the shared apex
    // and must be rejected, with the flattened single-label form suggested.
    let err = validate_tunnel_domain("api.myapp.alice.tunnel.portzero.cloud").unwrap_err();
    assert!(
        err.contains("api--myapp--alice.tunnel.portzero.cloud"),
        "should suggest the flattened single-label host, got: {err}"
    );
    // The retired dotted namespaced form the client used to mint.
    assert!(validate_tunnel_domain("csharp-docker.nate.tunnel.portzero.cloud").is_err());
    assert!(validate_tunnel_domain("api..alice.tunnel.portzero.cloud").is_err());
}

#[test]
fn test_validate_tunnel_domain_hyphen_edges() {
    assert!(validate_tunnel_domain("-bad--alice.tunnel.portzero.cloud").is_err());
    assert!(validate_tunnel_domain("bad---alice.tunnel.portzero.cloud").is_err());
}

#[test]
fn test_validate_tunnel_domain_invalid_chars() {
    assert!(validate_tunnel_domain("my_app--alice.tunnel.portzero.cloud").is_err());
    assert!(validate_tunnel_domain("my app--alice.tunnel.portzero.cloud").is_err());
}

#[test]
fn test_validate_tunnel_domain_label_too_long() {
    let long = format!("{}.alice.tunnel.portzero.cloud", "a".repeat(64));
    assert!(validate_tunnel_domain(&long).is_err());
}

// split_tunnel_port tests

#[test]
fn test_split_tunnel_port_no_port() {
    assert_eq!(
        split_tunnel_port("db.portzero.local"),
        ("db.portzero.local", None)
    );
}

#[test]
fn test_split_tunnel_port_valid() {
    assert_eq!(
        split_tunnel_port("db.portzero.local:5432"),
        ("db.portzero.local", Some(5432))
    );
    // Lowest and highest valid ports.
    assert_eq!(
        split_tunnel_port("x.portzero.local:1"),
        ("x.portzero.local", Some(1))
    );
    assert_eq!(
        split_tunnel_port("x.portzero.local:65535"),
        ("x.portzero.local", Some(65535))
    );
}

#[test]
fn test_split_tunnel_port_templated_name() {
    // The template is preserved in the domain part; only the port is split.
    assert_eq!(
        split_tunnel_port("web-{branch}.portzero.local:8080"),
        ("web-{branch}.portzero.local", Some(8080))
    );
}

#[test]
fn test_split_tunnel_port_cloud_domain() {
    assert_eq!(
        split_tunnel_port("api.alice.tunnel.portzero.cloud:8080"),
        ("api.alice.tunnel.portzero.cloud", Some(8080))
    );
}

#[test]
fn test_split_tunnel_port_out_of_range() {
    // 70000 > u16::MAX -> not a valid port; whole value is the domain.
    assert_eq!(
        split_tunnel_port("db.portzero.local:70000"),
        ("db.portzero.local:70000", None)
    );
}

#[test]
fn test_split_tunnel_port_zero_rejected() {
    // Port 0 is not addressable for a canonical VIP listen port.
    assert_eq!(
        split_tunnel_port("db.portzero.local:0"),
        ("db.portzero.local:0", None)
    );
}

#[test]
fn test_split_tunnel_port_non_numeric() {
    assert_eq!(
        split_tunnel_port("db.portzero.local:abc"),
        ("db.portzero.local:abc", None)
    );
}

#[test]
fn test_split_tunnel_port_empty_after_colon() {
    assert_eq!(
        split_tunnel_port("db.portzero.local:"),
        ("db.portzero.local:", None)
    );
}

#[test]
fn test_split_tunnel_port_empty_value() {
    assert_eq!(split_tunnel_port(""), ("", None));
}

#[test]
fn test_split_tunnel_port_uses_last_colon() {
    // Only the final segment is considered the port.
    assert_eq!(
        split_tunnel_port("a:b.portzero.local:5432"),
        ("a:b.portzero.local", Some(5432))
    );
}

// validate_dns_label edge cases

#[test]
fn test_validate_dns_label_exactly_63_chars_accepted() {
    let label = "a".repeat(63);
    assert!(validate_dns_label(&label).is_ok());
}

#[test]
fn test_validate_dns_label_64_chars_rejected() {
    let label = "a".repeat(64);
    let err = validate_dns_label(&label).unwrap_err();
    assert!(err.contains("too long"), "got: {err}");
}

#[test]
fn test_validate_dns_label_empty_rejected() {
    let err = validate_dns_label("").unwrap_err();
    assert!(err.contains("empty"), "got: {err}");
}

// unresolved / unknown placeholder detection

#[test]
fn test_unknown_placeholder_left_literal_and_flagged() {
    // {bogus} is not a recognized placeholder, so resolve() leaves it
    // untouched and unresolved_tokens/validate_resolved_name must flag it
    // rather than silently registering a garbled name.
    let ctx = DomainContext {
        service: "web".to_string(),
        project: "proj".to_string(),
        branch: "main".to_string(),
        worktree: None,
        user: "alice".to_string(),
        machine: "laptop".to_string(),
        uid: None,
        username: None,
        pr: None,
        run_id: None,
    };
    let resolved = ctx.resolve("web-{bogus}.alice.tunnel.portzero.cloud");
    assert_eq!(resolved, "web-{bogus}.alice.tunnel.portzero.cloud");
    assert_eq!(unresolved_tokens(&resolved), vec!["{bogus}".to_string()]);
    let err = validate_resolved_name(&resolved).unwrap_err();
    assert!(err.contains("{bogus}"), "got: {err}");
}

#[test]
fn test_unresolved_tokens_multiple() {
    let resolved = "web-{a}-{b}.example.com";
    assert_eq!(
        unresolved_tokens(resolved),
        vec!["{a}".to_string(), "{b}".to_string()]
    );
}

#[test]
fn test_unresolved_tokens_unterminated_brace_ignored() {
    // A stray unmatched '{' with no closing '}' should not be reported as
    // a token (nothing to substitute against) and must not panic/loop.
    assert_eq!(unresolved_tokens("web-{oops.example.com").len(), 0);
}

#[test]
fn test_unresolved_tokens_none() {
    assert!(unresolved_tokens("web-42.alice.tunnel.portzero.cloud").is_empty());
}

// default_template / PZ_TUNNEL_BASE_DOMAIN override

#[test]
fn test_default_template_uses_default_base_domain() {
    // Ensure no leaked override from another test running in-process.
    // SAFETY: test-only env mutation, serialized via `env_lock`.
    let _guard = env_lock();
    std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
    assert_eq!(
        default_template(),
        "{service}-{project}-{branch}--{cloud-username}.tunnel.portzero.cloud"
    );
}

#[test]
fn test_default_template_honors_base_domain_override() {
    let _guard = env_lock();
    std::env::set_var("PZ_TUNNEL_BASE_DOMAIN", "tunnel.example.dev");
    let result = default_template();
    std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
    assert_eq!(
        result,
        "{service}-{project}-{branch}--{cloud-username}.tunnel.example.dev"
    );
}

#[test]
fn test_validate_tunnel_domain_honors_base_domain_override() {
    let _guard = env_lock();
    std::env::set_var("PZ_TUNNEL_BASE_DOMAIN", "tunnel.example.dev");
    let result = validate_tunnel_domain("myapp--alice.tunnel.example.dev");
    let rejected = validate_tunnel_domain("myapp--alice.tunnel.portzero.cloud");
    std::env::remove_var("PZ_TUNNEL_BASE_DOMAIN");
    assert!(result.is_ok(), "got: {result:?}");
    assert!(
        rejected.is_err(),
        "default suffix should not validate once overridden"
    );
}

/// Serializes tests that mutate process-wide env vars (`std::env::set_var`
/// is process-global, so parallel tests touching `PZ_TUNNEL_BASE_DOMAIN`
/// or `USER`/`USERNAME`/`GITHUB_REF`/etc. would otherwise race).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

// detect_user / detect_pr / detect_run_id

#[test]
fn test_detect_user_reads_user_env_var() {
    let _guard = env_lock();
    let prev = std::env::var("USER").ok();
    std::env::set_var("USER", "testuser123");
    let result = detect_user();
    match prev {
        Some(v) => std::env::set_var("USER", v),
        None => std::env::remove_var("USER"),
    }
    assert_eq!(result, "testuser123");
}

#[test]
fn test_detect_user_falls_back_to_username_var() {
    let _guard = env_lock();
    let prev_user = std::env::var("USER").ok();
    let prev_username = std::env::var("USERNAME").ok();
    std::env::remove_var("USER");
    std::env::set_var("USERNAME", "winuser");
    let result = detect_user();
    match prev_user {
        Some(v) => std::env::set_var("USER", v),
        None => std::env::remove_var("USER"),
    }
    match prev_username {
        Some(v) => std::env::set_var("USERNAME", v),
        None => std::env::remove_var("USERNAME"),
    }
    assert_eq!(result, "winuser");
}

#[test]
fn test_detect_pr_from_github_ref() {
    let _guard = env_lock();
    let prev = std::env::var("GITHUB_REF").ok();
    std::env::set_var("GITHUB_REF", "refs/pull/42/merge");
    let result = detect_pr();
    match prev {
        Some(v) => std::env::set_var("GITHUB_REF", v),
        None => std::env::remove_var("GITHUB_REF"),
    }
    assert_eq!(result, Some("42".to_string()));
}

#[test]
fn test_detect_pr_from_explicit_override() {
    let _guard = env_lock();
    let prev_ref = std::env::var("GITHUB_REF").ok();
    let prev_pr = std::env::var("PZ_PR_NUMBER").ok();
    std::env::remove_var("GITHUB_REF");
    std::env::set_var("PZ_PR_NUMBER", "99");
    let result = detect_pr();
    match prev_ref {
        Some(v) => std::env::set_var("GITHUB_REF", v),
        None => std::env::remove_var("GITHUB_REF"),
    }
    match prev_pr {
        Some(v) => std::env::set_var("PZ_PR_NUMBER", v),
        None => std::env::remove_var("PZ_PR_NUMBER"),
    }
    assert_eq!(result, Some("99".to_string()));
}

#[test]
fn test_detect_pr_none_outside_ci() {
    let _guard = env_lock();
    let prev_ref = std::env::var("GITHUB_REF").ok();
    let prev_pr = std::env::var("PZ_PR_NUMBER").ok();
    std::env::remove_var("GITHUB_REF");
    std::env::remove_var("PZ_PR_NUMBER");
    let result = detect_pr();
    match prev_ref {
        Some(v) => std::env::set_var("GITHUB_REF", v),
        None => std::env::remove_var("GITHUB_REF"),
    }
    match prev_pr {
        Some(v) => std::env::set_var("PZ_PR_NUMBER", v),
        None => std::env::remove_var("PZ_PR_NUMBER"),
    }
    assert_eq!(result, None);
}

#[test]
fn test_detect_pr_non_numeric_ref_ignored() {
    let _guard = env_lock();
    let prev = std::env::var("GITHUB_REF").ok();
    std::env::set_var("GITHUB_REF", "refs/heads/main");
    let result = detect_pr();
    match prev {
        Some(v) => std::env::set_var("GITHUB_REF", v),
        None => std::env::remove_var("GITHUB_REF"),
    }
    assert_eq!(result, None);
}

#[test]
fn test_detect_run_id_from_env() {
    let _guard = env_lock();
    let prev = std::env::var("GITHUB_RUN_ID").ok();
    std::env::set_var("GITHUB_RUN_ID", "123456789");
    let result = detect_run_id();
    match prev {
        Some(v) => std::env::set_var("GITHUB_RUN_ID", v),
        None => std::env::remove_var("GITHUB_RUN_ID"),
    }
    assert_eq!(result, Some("123456789".to_string()));
}

#[test]
fn test_detect_run_id_none_when_absent() {
    let _guard = env_lock();
    let prev = std::env::var("GITHUB_RUN_ID").ok();
    std::env::remove_var("GITHUB_RUN_ID");
    let result = detect_run_id();
    if let Some(v) = prev {
        std::env::set_var("GITHUB_RUN_ID", v);
    }
    assert_eq!(result, None);
}

// find_git_root / detect_branch / detect_worktree_name (filesystem-based)

/// Build a throwaway directory under the OS temp dir; caller is
/// responsible for cleanup via `std::fs::remove_dir_all`.
fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pz-domain-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn test_find_git_root_finds_dot_git_dir() {
    let root = temp_dir("find-root");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let nested = root.join("a/b/c");
    std::fs::create_dir_all(&nested).unwrap();

    let found = find_git_root(&nested);
    assert!(found.is_some());
    assert_eq!(found.unwrap().0, root.as_path());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_find_git_root_none_when_absent() {
    let root = temp_dir("find-root-none");
    let nested = root.join("a/b");
    std::fs::create_dir_all(&nested).unwrap();

    assert!(find_git_root(&nested).is_none());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_branch_reads_head_ref() {
    let root = temp_dir("branch-ref");
    let git_dir = root.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/foo\n").unwrap();

    let branch = detect_branch(&root).unwrap();
    assert_eq!(branch, "feature/foo");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_branch_detached_head_uses_short_sha() {
    let root = temp_dir("branch-detached");
    let git_dir = root.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    std::fs::write(
        git_dir.join("HEAD"),
        "abcdef1234567890abcdef1234567890abcdef12\n",
    )
    .unwrap();

    let branch = detect_branch(&root).unwrap();
    assert_eq!(branch, "abcdef123456");
    assert_eq!(branch.len(), 12);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_branch_no_git_repo_errors() {
    let root = temp_dir("branch-no-repo");
    let err = detect_branch(&root).unwrap_err();
    assert!(err.to_string().contains("No .git found"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_worktree_name_from_gitdir_file() {
    let root = temp_dir("worktree-name");
    // Simulate a worktree: <root>/.git is a *file* pointing at
    // <main>/.git/worktrees/<name>.
    let main_git = root.join("main-repo/.git/worktrees/my-feature-wt");
    std::fs::create_dir_all(&main_git).unwrap();
    std::fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", main_git.display()),
    )
    .unwrap();

    let name = detect_worktree_name(&root);
    assert_eq!(name, Some("my-feature-wt".to_string()));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_worktree_name_none_for_normal_repo() {
    let root = temp_dir("worktree-none");
    // A normal (non-worktree) repo has .git as a directory, not a file.
    std::fs::create_dir_all(root.join(".git")).unwrap();

    assert_eq!(detect_worktree_name(&root), None);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn test_detect_project_name_worktree_uses_main_repo_name() {
    let root = temp_dir("project-name-wt");
    let main_git = root.join("myproject/.git/worktrees/some-wt");
    std::fs::create_dir_all(&main_git).unwrap();
    let wt_dir = root.join("wt-checkout");
    std::fs::create_dir_all(&wt_dir).unwrap();
    std::fs::write(
        wt_dir.join(".git"),
        format!("gitdir: {}\n", main_git.display()),
    )
    .unwrap();

    let name = detect_project_name(&wt_dir);
    assert_eq!(name, "myproject");

    std::fs::remove_dir_all(&root).ok();
}
