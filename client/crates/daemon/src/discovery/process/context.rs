//! PZ_TUNNEL value resolution, host project-dir discovery, and HTTP-port
//! selection for process-sourced services.

use super::platform::scan_process_env;
#[allow(unused_imports)]
use crate::discovery::*;

/// Emit a warning when a `PZ_TUNNEL` value had a trailing `:something` that
/// LOOKED like a canonical port but was rejected by [`split_tunnel_port`]
/// (out of range or zero — i.e. an all-digit segment that is not `1..=65535`).
///
/// We deliberately only warn on the all-numeric case: a non-numeric trailing
/// segment after `:` is almost certainly part of the value itself (or a typo we
/// can't distinguish), and warning on it would be noisy. When the port parsed
/// fine (`canonical` is `Some`) we say nothing.
pub(in crate::discovery) fn warn_if_port_like_rejected(
    raw: &str,
    canonical: Option<u16>,
    pid: u32,
) {
    if canonical.is_some() {
        return;
    }
    if let Some((_, tail)) = raw.rsplit_once(':') {
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            tracing::warn!(
                pid,
                value = raw,
                rejected_port = tail,
                "PZ_TUNNEL has a trailing ':<port>' that is not a valid port \
                 (must be 1..=65535); ignoring the port and using the whole value as the domain"
            );
        }
    }
}

/// Resolve template variables in a PZ_TUNNEL value.
pub(in crate::discovery) fn resolve_tunnel_template(
    raw: &str,
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
) -> String {
    if !raw.contains('{') {
        return raw.to_string();
    }
    let ctx = DomainContext::from_optional_environment("", project_dir, account_id, username);
    ctx.resolve(raw)
}

pub(in crate::discovery) fn template_substitutions(
    project_dir: Option<&Path>,
    account_id: Option<&str>,
    username: Option<&str>,
    source_name: Option<&str>,
) -> BTreeMap<String, String> {
    let ctx = DomainContext::from_optional_environment("", project_dir, account_id, username);
    let folder_name = source_name
        .map(|s| s.trim_start_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            project_dir
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let mut values = BTreeMap::new();
    values.insert("branch".to_string(), ctx.branch);
    values.insert(
        "worktree".to_string(),
        ctx.worktree.unwrap_or_else(|| "unknown".to_string()),
    );
    values.insert("folder-name".to_string(), folder_name);
    values.insert("project".to_string(), ctx.project);
    values.insert("user".to_string(), ctx.user.clone());
    values.insert("machine".to_string(), ctx.machine);
    values.insert(
        "uid".to_string(),
        ctx.uid.unwrap_or_else(|| "unknown".to_string()),
    );
    values.insert("local-username".to_string(), ctx.user);
    values.insert(
        "cloud-username".to_string(),
        ctx.username.unwrap_or_else(|| "not-logged-in".to_string()),
    );
    // CI-only tokens: surface the resolved value for the dashboard/inspect when
    // present. Absent outside a pull request / GitHub Actions run.
    if let Some(pr) = ctx.pr {
        values.insert("pr".to_string(), pr);
    }
    if let Some(run_id) = ctx.run_id {
        values.insert("run-id".to_string(), run_id);
    }
    values
}

pub(in crate::discovery) fn process_template_context(
    sys: &System,
    pid: u32,
) -> ProcessTemplateContext {
    let mut context = ProcessTemplateContext::default();
    let mut repo_candidates = Vec::new();
    let mut current = Some(sysinfo::Pid::from_u32(pid));
    let mut depth = 0usize;

    while let Some(current_pid) = current {
        if depth >= 8 {
            break;
        }
        let Some(process) = sys.process(current_pid) else {
            break;
        };

        let lineage_pid = current_pid.as_u32();

        if let Some(cwd) = process
            .cwd()
            .map(|p| p.to_path_buf())
            .filter(|p| p.is_dir())
        {
            if context.cwd.is_none() {
                context.cwd = Some(cwd.clone());
            }
            repo_candidates.push(cwd);
        }

        if let Some(env_pwd) = scan_process_env(lineage_pid, "PWD")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
        {
            if context.cwd.is_none() {
                context.cwd = Some(env_pwd.clone());
            }
            repo_candidates.push(env_pwd);
        }

        collect_process_repo_candidates(process, context.cwd.as_deref(), &mut repo_candidates);

        current = process.parent();
        depth += 1;
    }

    context.project_dir = first_git_project_dir(&repo_candidates);
    context
}

fn first_git_project_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    for candidate in candidates {
        if let Some((root, _)) = portzero_domain::find_git_root(candidate) {
            return Some(root.to_path_buf());
        }
    }
    None
}

fn collect_process_repo_candidates(
    process: &sysinfo::Process,
    base_dir: Option<&Path>,
    out: &mut Vec<PathBuf>,
) {
    if let Some(exe) = process.exe() {
        push_repo_candidate(out, exe);
    }

    let cmd = process.cmd();
    let launcher = process.name().to_string_lossy().to_ascii_lowercase();

    let mut i = 0usize;
    while i < cmd.len() {
        let arg = cmd[i].to_string_lossy();
        match arg.as_ref() {
            "-jar" | "--manifest-path" | "--project-dir" | "--project" | "--file" | "--config" => {
                if let Some(next) = cmd.get(i + 1) {
                    push_repo_candidate_arg(out, next, base_dir);
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    if launcher.contains("java") || launcher.contains("kotlin") {
        for pair in cmd.windows(2) {
            if pair[0].to_string_lossy() == "-jar" {
                push_repo_candidate_arg(out, &pair[1], base_dir);
            }
        }
    } else if launcher.contains("python")
        || launcher.contains("node")
        || launcher.contains("bun")
        || launcher.contains("deno")
        || launcher.contains("ruby")
        || launcher.contains("php")
        || launcher.contains("perl")
        || launcher.contains("dotnet")
    {
        if let Some(script_arg) = cmd
            .iter()
            .skip(1)
            .find(|arg| !arg.to_string_lossy().starts_with('-'))
        {
            push_repo_candidate_arg(out, script_arg, base_dir);
        }
    }

    for arg in cmd.iter().skip(1) {
        let text = arg.to_string_lossy();
        if text.starts_with('-') || text.is_empty() {
            continue;
        }
        if looks_like_repo_path(&text) {
            push_repo_candidate_arg(out, arg, base_dir);
        }
    }
}

fn push_repo_candidate_arg(out: &mut Vec<PathBuf>, arg: &std::ffi::OsStr, base_dir: Option<&Path>) {
    let path = PathBuf::from(arg);
    if path.is_absolute() {
        push_repo_candidate(out, &path);
    } else if let Some(base_dir) = base_dir {
        push_repo_candidate(out, &base_dir.join(path));
    }
}

fn push_repo_candidate(out: &mut Vec<PathBuf>, path: &Path) {
    let candidate = if path.is_dir() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        parent.to_path_buf()
    } else {
        return;
    };

    if candidate.as_os_str().is_empty() {
        return;
    }

    out.push(candidate);
}

pub(in crate::discovery) fn looks_like_repo_path(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with("./") || value.starts_with("../") {
        return true;
    }

    let path = Path::new(value);
    if path.components().count() > 1 {
        return true;
    }

    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(
            "jar"
                | "class"
                | "js"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "py"
                | "rb"
                | "php"
                | "pl"
                | "sh"
                | "dll"
                | "exe"
                | "go"
        )
    )
}

/// Parse PZ_TUNNEL_HTTP_PORT into a selection strategy.
pub(in crate::discovery) fn parse_http_port_selection(val: &str) -> HttpPortSelection {
    match val.trim() {
        "" | "CHOOSE_LOWEST" => HttpPortSelection::ChooseLowest,
        "CHOOSE_HIGHEST" => HttpPortSelection::ChooseHighest,
        other => match other.parse::<u16>() {
            Ok(port) => HttpPortSelection::Explicit(port),
            Err(_) => HttpPortSelection::ChooseLowest,
        },
    }
}

/// Parse PZ_TUNNEL_PORTS into a list of port mappings.
///
/// Format: `local:tunnel[;local:tunnel...]`  e.g. `9222:9222;9300:9300`
pub(in crate::discovery) fn parse_extra_ports(val: &str) -> Vec<PortMapping> {
    val.split(';')
        .filter_map(|entry| {
            let mut parts = entry.trim().splitn(2, ':');
            let local: u16 = parts.next()?.trim().parse().ok()?;
            let tunnel: u16 = parts.next()?.trim().parse().ok()?;
            Some(PortMapping {
                local_port: local,
                tunnel_port: tunnel,
            })
        })
        .collect()
}

/// Choose the HTTP port from the list of listening ports.
///
/// Prefers ports bound to 0.0.0.0 over loopback-only. Falls back to loopback
/// when no public-facing port is found.
///
/// For `Explicit`, the requested port must be in `ports` (owned by the
/// process); otherwise returns `ExplicitNotOwned`.
pub(in crate::discovery) fn select_http_port(
    ports: &[ListeningPort],
    selection: &HttpPortSelection,
) -> SelectedPort {
    match selection {
        HttpPortSelection::Explicit(p) => {
            if ports.iter().any(|lp| lp.port == *p) {
                SelectedPort::Found(*p)
            } else {
                SelectedPort::ExplicitNotOwned(*p)
            }
        }
        HttpPortSelection::ChooseLowest => {
            let port = ports
                .iter()
                .filter(|p| p.bind == BindAddr::Public)
                .map(|p| p.port)
                .min()
                .or_else(|| ports.iter().map(|p| p.port).min());
            match port {
                Some(p) => SelectedPort::Found(p),
                None => SelectedPort::NoneListening,
            }
        }
        HttpPortSelection::ChooseHighest => {
            let port = ports
                .iter()
                .filter(|p| p.bind == BindAddr::Public)
                .map(|p| p.port)
                .max()
                .or_else(|| ports.iter().map(|p| p.port).max());
            match port {
                Some(p) => SelectedPort::Found(p),
                None => SelectedPort::NoneListening,
            }
        }
    }
}
