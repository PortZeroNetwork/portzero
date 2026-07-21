//! Domain template engine for PZ_TUNNEL support.
//!
//! Resolves templates like `{service}-{project}-{branch}.{cloud-username}.tunnel.portzero.cloud`
//! into stable DNS-safe domain names for tunnel routes.
//!
//! There are two distinct username placeholders, and they are never
//! interchangeable:
//!
//! - `{local-username}` — the OS login username. Always available, and never
//!   changes based on whether you are logged in to Port Zero Cloud. Using it
//!   in a `.local` tunnel template lets you scope a tunnel name per-machine
//!   without that name shifting the moment you run `portzero login`.
//! - `{cloud-username}` — the Port Zero Cloud account username (or a team slug).
//!   Only available when logged in. Cloud tunnel routes are a single label on
//!   the shared apex: `<name>--<cloud-username>.tunnel.portzero.cloud`.
//!
//! `{local-username}` must never be used in a cloud tunnel template — cloud
//! tunnels are already scoped by `{cloud-username}`, and allowing the OS
//! username in as well would break the orthogonality between `.local` and
//! `.cloud` tunnel names. `{cloud-username}` may be used in a `.local`
//! template, but requires being logged in to resolve.
//!
//! A cloud tunnel host is ONE DNS label: the name and the `{cloud-username}`
//! namespace are joined with `--` (e.g. `api--team--alice` for a team, or
//! `api--alice` for a personal tunnel), never a real dot. A wildcard cert covers
//! exactly one label, so keeping the whole host to a single label lets the one
//! `*.tunnel.portzero.cloud` cert serve every tunnel; a dotted two-label name has
//! no cert and fails TLS. Each `--`-separated segment must be a valid DNS
//! sub-label (ASCII alphanumeric + hyphens, no leading/trailing hyphen, ≤ 63
//! chars for the whole label).

use std::path::{Path, PathBuf};

/// Canonical service endpoints (API/edge/dashboard/web) and their
/// `PZ_TUNNEL_*` overrides — the single source of truth for the URLs the
/// client targets.
pub mod endpoints;

/// Locate and launch the PortZero desktop app (`portzero-app`) — the shared
/// launcher the tray and CLI both use so they resolve and open the app the same
/// way.
pub mod app;

/// Default base domain for portzero.cloud tunnels. The portzero-cloud edge only
/// accepts tunnel routes ending in this suffix.
pub const DEFAULT_BASE_DOMAIN: &str = "tunnel.portzero.cloud";

/// Default domain template used when none is specified.
///
/// Produces a cloud-username-scoped hostname that is a SINGLE DNS label on the
/// shared apex: `<name>--<cloud-username>.tunnel.portzero.cloud`. The name and
/// the namespace are joined with `--`, never a dot, so one wildcard cert
/// (`*.tunnel.portzero.cloud`) covers every tunnel regardless of how many users
/// or teams exist — a dotted two-label host has no wildcard-cert coverage and
/// fails TLS. The base domain can be overridden via `PZ_TUNNEL_BASE_DOMAIN` for
/// local development.
pub const DEFAULT_TEMPLATE: &str =
    "{service}-{project}-{branch}--{cloud-username}.tunnel.portzero.cloud";

/// Build the default domain template using the configured base domain.
///
/// Reads `PZ_TUNNEL_BASE_DOMAIN` from the environment, falling back to
/// `tunnel.portzero.cloud`.
pub fn default_template() -> String {
    let base =
        std::env::var("PZ_TUNNEL_BASE_DOMAIN").unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    format!("{{service}}-{{project}}-{{branch}}--{{cloud-username}}.{base}")
}

/// Context for resolving PZ_TUNNEL templates.
#[derive(Debug, Clone)]
pub struct DomainContext {
    pub service: String,
    pub project: String,
    pub branch: String,
    pub worktree: Option<String>,
    pub user: String,
    pub machine: String,
    /// Short account identifier (first 8 chars of account UUID).
    /// Falls back to the OS username when not logged in.
    pub uid: Option<String>,
    /// Cloud account username (e.g. "alice") or team slug. Used for
    /// `{cloud-username}` in namespace-aware tunnel domains like
    /// `{service}--{cloud-username}.tunnel.portzero.cloud`.
    ///
    /// `None` when not logged in. Unlike the old unified `{username}`
    /// placeholder, this deliberately has no fallback — resolving
    /// `{cloud-username}` without an account would silently change a tunnel's
    /// identity depending on login state, which is exactly what the
    /// `{local-username}`/`{cloud-username}` split exists to prevent.
    pub username: Option<String>,
    /// Pull-request number for `{pr}`, from the CI environment (GitHub Actions
    /// `GITHUB_REF=refs/pull/<n>/merge`, or an explicit `PZ_PR_NUMBER`). `None`
    /// outside a pull request — resolving `{pr}` then fails discovery for that
    /// tunnel with a clear diagnostic rather than producing a garbled name.
    pub pr: Option<String>,
    /// CI run id for `{run-id}`, from GitHub Actions `GITHUB_RUN_ID`. `None`
    /// outside Actions.
    pub run_id: Option<String>,
}

impl DomainContext {
    /// Build context from the current environment.
    ///
    /// - `service_name`: compose service name or directory name
    /// - `project_dir`: path to the project root (git repo root)
    /// - `account_id`: authenticated account UUID (used for `{uid}`)
    /// - `username`: cloud account username (used for `{cloud-username}`)
    ///
    /// Branch and worktree detection failures are silently treated as
    /// "unknown" so the daemon keeps running even outside a git repo.
    pub fn from_environment(
        service_name: &str,
        project_dir: &Path,
        account_id: Option<&str>,
        username: Option<&str>,
    ) -> Self {
        let project = detect_project_name(project_dir);
        let branch = detect_branch(project_dir).unwrap_or_else(|_| "unknown".to_string());
        let worktree = detect_worktree_name(project_dir);
        let user = detect_user();
        let machine = detect_machine();
        let uid = account_id.map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect::<String>()
                .to_ascii_lowercase()
        });

        Self {
            service: service_name.to_string(),
            project,
            branch,
            worktree,
            user,
            machine,
            uid,
            username: username.map(|u| u.to_string()),
            pr: detect_pr(),
            run_id: detect_run_id(),
        }
    }

    /// Build context from an optional project directory.
    ///
    /// When no project directory is available, git-derived values stay
    /// `"unknown"`/`None` instead of consulting the caller's current directory.
    pub fn from_optional_environment(
        service_name: &str,
        project_dir: Option<&Path>,
        account_id: Option<&str>,
        username: Option<&str>,
    ) -> Self {
        match project_dir {
            Some(project_dir) => {
                Self::from_environment(service_name, project_dir, account_id, username)
            }
            None => {
                let user = detect_user();
                let machine = detect_machine();
                let uid = account_id.map(|id| {
                    id.chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(8)
                        .collect::<String>()
                        .to_ascii_lowercase()
                });

                Self {
                    service: service_name.to_string(),
                    project: "unknown".to_string(),
                    branch: "unknown".to_string(),
                    worktree: None,
                    user,
                    machine,
                    uid,
                    username: username.map(|u| u.to_string()),
                    pr: detect_pr(),
                    run_id: detect_run_id(),
                }
            }
        }
    }

    /// Resolve a domain template, replacing all `{placeholder}` variables.
    ///
    /// All values are DNS-sanitized before substitution.
    ///
    /// Supported variables: `{service}`, `{project}`, `{branch}`, `{worktree}`,
    /// `{user}`, `{machine}`, `{uid}`, `{local-username}`, `{cloud-username}`.
    ///
    /// `{local-username}` always resolves to the OS username and never
    /// changes based on login state. `{cloud-username}` resolves to the
    /// logged-in cloud account username, or an empty string when not logged
    /// in — callers should validate placeholder usage with
    /// [`validate_username_placeholders`] before resolving so that misuse
    /// (or a missing login) surfaces as a diagnostic instead of a silently
    /// broken domain.
    pub fn resolve(&self, template: &str) -> String {
        let worktree_value = self.worktree.as_deref().unwrap_or(&self.branch);
        let uid_value = self.uid.as_deref().unwrap_or(&self.user);
        let cloud_username_value = self.username.as_deref().unwrap_or("");

        let mut resolved = template
            .replace("{service}", &sanitize_for_dns(&self.service))
            .replace("{project}", &sanitize_for_dns(&self.project))
            .replace("{branch}", &sanitize_for_dns(&self.branch))
            .replace("{worktree}", &sanitize_for_dns(worktree_value))
            .replace("{user}", &sanitize_for_dns(&self.user))
            .replace("{machine}", &sanitize_for_dns(&self.machine))
            .replace("{uid}", &sanitize_for_dns(uid_value))
            .replace("{local-username}", &sanitize_for_dns(&self.user))
            .replace("{cloud-username}", &sanitize_for_dns(cloud_username_value));

        // `{pr}` and `{run-id}` come from the CI environment and are only
        // available inside GitHub Actions (a pull request / a run). When absent
        // we deliberately leave the literal `{pr}` / `{run-id}` in place so that
        // [`unresolved_tokens`] can flag it and discovery fails cleanly with a
        // clear diagnostic — never a garbled name like `web-.example.com`.
        if let Some(pr) = self.pr.as_deref() {
            resolved = resolved.replace("{pr}", &sanitize_for_dns(pr));
        }
        if let Some(run_id) = self.run_id.as_deref() {
            resolved = resolved.replace("{run-id}", &sanitize_for_dns(run_id));
        }

        resolved
    }
}

/// Return every `{...}` token still present in an already-resolved name.
///
/// After [`DomainContext::resolve`] has run, a remaining `{token}` means the
/// template referenced something the environment could not supply (e.g. `{pr}`
/// outside a pull request) or an unknown placeholder. Discovery uses this to
/// fail the tunnel with a clear diagnostic rather than registering a name that
/// still contains braces.
pub fn unresolved_tokens(resolved: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = resolved;
    while let Some(start) = rest.find('{') {
        let after = &rest[start..];
        if let Some(end) = after.find('}') {
            out.push(after[..=end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// Validate that a single DNS label does not contain an ambiguous internal
/// `--`.
///
/// `--` is the reserved hierarchy separator inside a single-label tunnel name
/// (e.g. `{branch}--myapp`). For that to stay unambiguous, each `--`-delimited
/// segment must itself be a clean sub-label: non-empty and not starting or
/// ending with a hyphen. This rejects `a--` / `--a` / `a----b` (empty segment)
/// and `a---b` (a `---` run), while accepting `feat--myapp` and
/// `my-api--web`.
pub fn validate_no_internal_double_hyphen(label: &str) -> Result<(), String> {
    if !label.contains("--") {
        return Ok(());
    }
    for segment in label.split("--") {
        if segment.is_empty() || segment.starts_with('-') || segment.ends_with('-') {
            return Err(format!(
                "label '{label}' has an ambiguous internal '--': '--' separates hierarchy \
                 segments, so each segment must be a non-empty label without a leading or \
                 trailing hyphen (got an empty or hyphen-edged segment)"
            ));
        }
    }
    Ok(())
}

/// Validate the resolved (post-template) domain for token / `--` problems.
///
/// Returns `Err` with a single clear diagnostic when the name still contains an
/// unresolved `{token}` or any dot-separated label carries an ambiguous internal
/// `--`. Suitable for both `.local` overlay names and cloud tunnel domains.
pub fn validate_resolved_name(resolved: &str) -> Result<(), String> {
    let unresolved = unresolved_tokens(resolved);
    if !unresolved.is_empty() {
        return Err(format!(
            "'{resolved}' still contains unresolved template token(s) {}: this value is only \
             available in the right context (e.g. {{pr}} needs a pull request and {{run-id}} \
             needs a GitHub Actions run). Discovery was skipped rather than register a garbled name.",
            unresolved.join(", ")
        ));
    }
    for label in resolved.split('.') {
        validate_no_internal_double_hyphen(label)?;
    }
    Ok(())
}

/// Validate that `{local-username}`/`{cloud-username}` are used correctly for
/// the kind of tunnel `template` targets.
///
/// - `{local-username}` is reserved for `.local` tunnels. Using it in a cloud
///   tunnel template would tie the cloud tunnel's name to this machine's OS
///   username, breaking the orthogonality between `.local` and `.cloud`
///   tunnel names — this is always an error, regardless of login state.
/// - `{cloud-username}` may be used in a `.local` template, but needs an
///   authenticated account to resolve. Note that logging in is only required
///   to resolve the *value*: the `.local` tunnel itself remains free either
///   way — all local tunnels are free, always.
///
/// `is_local` should reflect the template's suffix (`.portzero.local` vs.
/// `*.tunnel.portzero.cloud`), which callers can determine before resolving
/// since the suffix is literal text, never templated.
pub fn validate_username_placeholders(
    template: &str,
    is_local: bool,
    logged_in: bool,
) -> Result<(), String> {
    if !is_local && template.contains("{local-username}") {
        return Err(format!(
            "'{template}' uses {{local-username}} in a cloud tunnel domain — \
             {{local-username}} is reserved for .local tunnels, so that a .local tunnel's \
             name never changes when you run `portzero login`. Use {{cloud-username}} instead."
        ));
    }

    if is_local && template.contains("{cloud-username}") && !logged_in {
        return Err(format!(
            "'{template}' uses {{cloud-username}}, which requires being logged in to resolve. \
             Run `portzero login` to fix this. Note: this is a .local tunnel, and local tunnels \
             are always free — logging in is only needed here to resolve {{cloud-username}}, not \
             because this tunnel will be billed."
        ));
    }

    Ok(())
}

/// Split an optional trailing `:<port>` off a raw `PZ_TUNNEL` value.
///
/// The canonical-port feature lets a developer declare the port the overlay
/// should expose a service on by appending `:<port>` to the value, e.g.
/// `db.portzero.local:5432` or `web-{branch}.portzero.local:8080`.
///
/// This helper is PURE: it only splits the value; it does NOT resolve
/// templates or validate the domain. The returned domain part is what the
/// caller should template-resolve and suffix-classify.
///
/// Rules:
/// - The port is the segment after the LAST `:`. It must parse as an integer
///   in `1..=65535`.
/// - If there is no `:`, or the trailing segment is not a valid port (empty,
///   non-numeric, zero, or out of range), the WHOLE value is treated as the
///   domain and `None` is returned. This is a graceful fallback — discovery
///   must never break on a malformed port.
/// - Templates are preserved in the domain part (the helper does not touch
///   `{...}`); `web-{branch}.portzero.local:8080` → (`web-{branch}.portzero.local`,
///   `Some(8080)`).
pub fn split_tunnel_port(value: &str) -> (&str, Option<u16>) {
    match value.rsplit_once(':') {
        Some((domain, port_str)) => match port_str.parse::<u16>() {
            Ok(port) if port >= 1 => (domain, Some(port)),
            _ => (value, None),
        },
        None => (value, None),
    }
}

/// Validate that `domain` is a legal cloud tunnel subdomain.
///
/// Cloud tunnels on the shared apex are a SINGLE DNS label of the form
/// `<name>--<cloud-username>.tunnel.portzero.cloud` (the namespace may be a
/// username or a team slug). A wildcard cert covers exactly one label, so the
/// single `*.tunnel.portzero.cloud` cert covers every such host — this is why
/// the boundary between name and namespace is `--`, never a dot.
///
/// Rules:
/// - Must end with `.tunnel.portzero.cloud` (or the configured base domain).
/// - Exactly ONE label may appear before the base domain: a dotted multi-label
///   name (`api.alice.tunnel…`) has no wildcard-cert coverage and is rejected
///   with the flattened form to use instead.
/// - That label must be a valid DNS label and carry the cloud-username scope as
///   its trailing `--` segment (`myservice--alice`); its `--` segments must each
///   be clean sub-labels.
/// - Bare base domains like `tunnel.portzero.cloud`, and unscoped single labels
///   like `alice.tunnel.portzero.cloud` (no `--`), are rejected.
///
/// The expected base domain (e.g. `tunnel.portzero.cloud`) is derived from
/// `PZ_TUNNEL_BASE_DOMAIN`.
pub fn validate_tunnel_domain(domain: &str) -> Result<(), String> {
    let base =
        std::env::var("PZ_TUNNEL_BASE_DOMAIN").unwrap_or_else(|_| DEFAULT_BASE_DOMAIN.to_string());
    let suffix = format!(".{base}");

    if domain == base {
        return Err(format!(
            "'{base}' is a reserved hostname — cloud tunnels must be subdomains of it"
        ));
    }

    let label = domain.strip_suffix(&suffix).ok_or_else(|| {
        format!(
            "'{domain}' is not a valid cloud tunnel domain: it must end with '.{base}' and be \
             scoped to your cloud username, e.g. 'myservice--<cloud-username>.{base}'. \
             Run `portzero whoami` to find your cloud username."
        )
    })?;

    if label.is_empty() {
        return Err(format!(
            "'{base}' is a reserved hostname — cloud tunnels must be subdomains of it"
        ));
    }

    // A wildcard cert covers exactly one label; a dotted name cannot be served.
    if label.contains('.') {
        let flattened = label.replace('.', "--");
        return Err(format!(
            "'{domain}' has a dotted multi-label name ('{label}'), which no wildcard \
             certificate covers and cannot serve HTTPS. Cloud tunnels are a single label with \
             '--' for hierarchy — use '{flattened}.{base}' instead."
        ));
    }

    validate_dns_label(label)?;
    validate_no_internal_double_hyphen(label)?;

    // The label must carry the cloud-username (or team-slug) scope as its
    // trailing `--` segment. No `--` means the scope is missing.
    if label.rsplit_once("--").is_none() {
        return Err(format!(
            "'{domain}' is missing the cloud username scope — cloud tunnel names must be \
             '<name>--<cloud-username>.{base}' (e.g. '{label}--<cloud-username>.{base}'). \
             Run `portzero whoami` to find your cloud username, or `portzero login` if you are \
             not logged in yet."
        ));
    }

    Ok(())
}

/// Validate a single DNS label (the part between dots).
pub fn validate_dns_label(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("DNS label cannot be empty".to_string());
    }
    if label.len() > 63 {
        return Err(format!(
            "DNS label '{label}' is too long ({} chars, max 63)",
            label.len()
        ));
    }
    if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "DNS label '{label}' contains invalid characters (only letters, digits, and hyphens allowed)"
        ));
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(format!(
            "DNS label '{label}' must not start or end with a hyphen"
        ));
    }
    Ok(())
}

/// Sanitize a string for use as a DNS label.
///
/// - Replaces non-alphanumeric characters with hyphens
/// - Lowercases everything
/// - Collapses consecutive hyphens
/// - Trims leading/trailing hyphens
/// - Truncates to 63 characters (DNS label limit)
pub fn sanitize_for_dns(s: &str) -> String {
    let lowered = s.to_lowercase();

    let replaced: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut collapsed = String::with_capacity(replaced.len());
    let mut prev_hyphen = false;
    for c in replaced.chars() {
        if c == '-' {
            if !prev_hyphen {
                collapsed.push(c);
            }
            prev_hyphen = true;
        } else {
            collapsed.push(c);
            prev_hyphen = false;
        }
    }

    let trimmed = collapsed.trim_matches('-');

    let mut result: String = trimmed.chars().take(63).collect();
    while result.ends_with('-') {
        result.pop();
    }

    result
}

// Variable detection helpers

/// Walk up from `start` to find the nearest directory containing `.git`.
pub fn find_git_root(start: &Path) -> Option<(&Path, std::path::PathBuf)> {
    let mut dir = start;
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return Some((dir, candidate));
        }
        dir = dir.parent()?;
    }
}

/// Given a list of host filesystem paths (e.g. from Docker bind mounts or compose
/// working directories), return the first one that is (or is inside) a git repository.
/// This is useful for resolving templates like `{branch}` or `{worktree}` when
/// discovering Docker containers from the host.
pub fn find_git_project_dir(candidates: &[&Path]) -> Option<PathBuf> {
    for c in candidates {
        if let Some((root, _)) = find_git_root(c) {
            return Some(root.to_path_buf());
        }
    }
    None
}

/// Detect the project name from the project directory.
fn detect_project_name(project_dir: &Path) -> String {
    let git_root = find_git_root(project_dir);

    if let Some((root, git_path)) = &git_root {
        if git_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(git_path) {
                if let Some(gitdir) = content.strip_prefix("gitdir:") {
                    let gitdir = gitdir.trim();
                    let gitdir_path = Path::new(gitdir);
                    if let Some(main_git_dir) = gitdir_path
                        .parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                    {
                        if let Some(name) = main_git_dir.file_name() {
                            return name.to_string_lossy().to_string();
                        }
                    }
                }
            }
        }

        if let Some(name) = root.file_name() {
            return name.to_string_lossy().to_string();
        }
    }

    project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Detect the current git branch by reading `.git/HEAD` directly.
fn detect_branch(project_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let (_root, git_path) = find_git_root(project_dir).ok_or_else(|| {
        format!(
            "No .git found in '{}' or any parent directory. \
             Is this inside a git repository?",
            project_dir.display()
        )
    })?;

    let head_path = if git_path.is_file() {
        let content = std::fs::read_to_string(&git_path)?;
        let gitdir = content
            .strip_prefix("gitdir:")
            .ok_or("Invalid .git file: missing 'gitdir:' prefix")?
            .trim()
            .to_string();
        Path::new(&gitdir).join("HEAD")
    } else {
        git_path.join("HEAD")
    };

    let head_content = std::fs::read_to_string(&head_path).map_err(|e| {
        format!(
            "Could not read git HEAD at {}: {}. \
             Is '{}' inside a git repository?",
            head_path.display(),
            e,
            project_dir.display()
        )
    })?;

    if let Some(ref_path) = head_content.strip_prefix("ref: ") {
        let ref_path = ref_path.trim();
        let branch = ref_path.strip_prefix("refs/heads/").unwrap_or(ref_path);
        Ok(branch.to_string())
    } else {
        Ok(head_content.trim().chars().take(12).collect())
    }
}

/// Detect the git worktree name, if we are in a worktree.
fn detect_worktree_name(project_dir: &Path) -> Option<String> {
    let (_root, git_path) = find_git_root(project_dir)?;
    if !git_path.is_file() {
        return None;
    }

    let content = std::fs::read_to_string(&git_path).ok()?;
    let gitdir = content.strip_prefix("gitdir:")?.trim().to_string();
    let gitdir_path = Path::new(&gitdir);

    gitdir_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
}

/// Detect the current OS username.
fn detect_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Detect the pull-request number for `{pr}` from the CI environment.
///
/// GitHub Actions sets `GITHUB_REF=refs/pull/<n>/merge` (or `/head`) on
/// `pull_request` events. As an escape hatch for other CI systems, an explicit
/// `PZ_PR_NUMBER` is also honoured. Returns `None` outside a pull request.
fn detect_pr() -> Option<String> {
    if let Ok(git_ref) = std::env::var("GITHUB_REF") {
        if let Some(rest) = git_ref.strip_prefix("refs/pull/") {
            if let Some(num) = rest.split('/').next() {
                if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                    return Some(num.to_string());
                }
            }
        }
    }
    if let Ok(explicit) = std::env::var("PZ_PR_NUMBER") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Detect the GitHub Actions run id for `{run-id}` from `GITHUB_RUN_ID`.
fn detect_run_id() -> Option<String> {
    std::env::var("GITHUB_RUN_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Detect the machine hostname.
fn detect_machine() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
