//! port-zero CLI: manage the local tunnel daemon and cloud integration.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

/// A single process-global lock serializing every test that mutates the
/// `HOME` environment variable. Because `HOME` is shared across the whole
/// binary, each module's HOME-dependent tests must take the *same* lock —
/// separate per-module mutexes would not serialize against each other and
/// would race (one test's `save` landing in a directory another test has
/// already repointed `HOME` away from).
#[cfg(test)]
pub(crate) fn home_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Test-only redirect for [`client_home`], set by [`with_temp_home`].
#[cfg(test)]
static HOME_OVERRIDE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// The home directory every `~/.portzero` path in this crate is resolved
/// against.
///
/// In test builds an override set by [`with_temp_home`] wins. That indirection
/// exists because `dirs::home_dir()` **cannot be redirected on Windows**: it
/// calls `SHGetKnownFolderPath(FOLDERID_Profile)` and ignores both `HOME` and
/// `USERPROFILE`. Tests that pointed `HOME` at a temp dir therefore read and
/// wrote the runner's real profile on Windows — so they clobbered each other
/// (a store written by one test made another's "no store yet" assertion fail)
/// and scribbled into the developer's actual `~/.portzero`.
pub(crate) fn client_home() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    {
        let override_dir = HOME_OVERRIDE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if override_dir.is_some() {
            return override_dir;
        }
    }
    dirs::home_dir()
}

/// Run `f` with [`client_home`] pointed at a fresh temp dir, holding
/// [`home_env_lock`] for the duration and restoring the previous state after.
///
/// `HOME` is set too, for the code that reads it directly rather than going
/// through `client_home` (`~/.claude` transcript discovery, for one).
#[cfg(test)]
pub(crate) fn with_temp_home<T>(label: &str, f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _guard = home_env_lock();
    let dir = std::env::temp_dir().join(format!(
        "pz-{}-test-{}-{}",
        label,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let prev_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &dir);
    *HOME_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.clone());

    let out = f(&dir);

    *HOME_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    match prev_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

mod agents;
mod api_client;
mod auth;
mod autostart;
mod browser;
mod cost_store;
mod daemon;
mod demo;
mod demo_server;
mod doctor;
mod export;
mod frontdoor;
mod github_repo_login;
mod inspect;
mod mcp;
mod mcp_cost;
mod mcp_feedback;
mod purge;
mod review;
mod selfupdate;
mod setup;
mod skill;
mod trust;
mod update;
mod version;
mod wait;

#[derive(Parser)]
#[command(
    name = "portzero",
    version,
    about = "Eliminate port conflicts: stable *.portzero.local and cloud tunnel names for your dev services"
)]
struct Cli {
    /// Optional: with no subcommand, `portzero` prints a short state-aware
    /// summary (daemon, auth, routes) and the most useful next step.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the discovery daemon and tunnel connection.
    Start {
        /// Run in the foreground instead of daemonizing (used internally).
        #[arg(long, hide = true)]
        foreground: bool,

        /// Do not automatically open the dashboard in a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Stop the discovery daemon.
    Stop,
    /// Restart the discovery daemon.
    Restart,
    /// Show daemon and tunnel status.
    Status,

    /// See a working Local tunnel in one command: starts the daemon if
    /// needed, runs a tiny built-in web server on port 0 with PZ_TUNNEL set,
    /// and opens http://hello.portzero.local when it is reachable.
    Demo {
        /// Do not open the demo page in a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// The tiny web server spawned by `portzero demo` (used internally).
    #[command(name = "demo-server", hide = true)]
    DemoServer,

    /// Diagnose the overlay, DNS, and TLS path in one command.
    ///
    /// Runs a series of named checks (daemon running, overlay active, scoped
    /// resolver, embedded DNS, end-to-end resolution, local CA trust,
    /// per-tunnel reachability, cloud state), printing pass/warn/fail with a
    /// concrete fix on failure. Exits non-zero if any check fails. Works even
    /// when the daemon is not running.
    Doctor,

    /// Print the resolved URL for a tunnel domain (script-safe: only the URL
    /// is written to stdout).
    Url {
        /// The tunnel domain, e.g. `web.myapp.portzero.local` or
        /// `api.alice.tunnel.portzero.cloud`.
        domain: String,
    },
    /// Print `export NAME="URL"` lines for every discovered tunnel.
    Env {
        /// Append `NAME=URL` lines to `$GITHUB_ENV` instead of printing export
        /// lines (for use inside a GitHub Actions job).
        #[arg(long)]
        github: bool,
    },
    /// Block until a tunnel is up (readiness gate for CI and test runs).
    Wait {
        /// The tunnel domain, e.g. `web.myapp.portzero.local`.
        domain: String,
        /// Also poll the tunnel's health path until it returns 2xx. Health is
        /// polled automatically when the endpoint declares PZ_HEALTH_PATH.
        #[arg(long)]
        healthy: bool,
        /// Maximum seconds to wait before failing (default: 60).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Show the daemon's observed runtime truth as human-friendly text
    /// (discovered services, tunnels, observed edges, exercised routes).
    Inspect,
    /// Run the Model Context Protocol server (JSON-RPC over stdio) exposing the
    /// same runtime truth to AI coding agents.
    Mcp,

    /// Run privileged first-run setup after package installation.
    #[command(alias = "post-install")]
    Setup,

    /// Show the version of every PortZero component and whether they agree.
    ///
    /// `portzero --version` speaks only for this binary. The daemon, tray, and
    /// desktop app are separate processes that keep running the build they
    /// started with, so after an upgrade they can lag behind — this command
    /// asks each of them and says whether the install is coherent.
    Version,

    /// Update portzero to the latest release, replacing this binary in place.
    ///
    /// Downloads the newest published build for this OS/arch and swaps it in,
    /// then refreshes system integration. This is the command the update notice
    /// points to.
    Update {
        /// Only report whether a newer version is available; change nothing.
        #[arg(long)]
        check: bool,
        /// Reinstall the latest build even if it matches the installed version
        /// (repairs a broken install).
        #[arg(long)]
        force: bool,
    },

    /// Log in to portzero.cloud (opens browser by default).
    Login {
        /// Use interactive terminal prompts instead of browser login.
        /// Useful on headless servers without a browser.
        #[arg(long)]
        interactive: bool,

        /// Email address (only used with --interactive, skips prompt).
        #[arg(long)]
        email: Option<String>,

        /// Display name (only used with --interactive, skips prompt).
        #[arg(long)]
        name: Option<String>,

        /// Authenticate non-interactively by proving push access to a GitHub
        /// repository (for CI jobs and agent sandboxes). Requires --team.
        #[arg(long, conflicts_with_all = ["interactive", "email", "name"])]
        github_repo: bool,

        /// Team slug whose trust rules the repository is checked against
        /// (required with --github-repo).
        #[arg(long, requires = "github_repo")]
        team: Option<String>,

        /// GitHub repository as owner/repo (only used with --github-repo;
        /// default: parsed from `git remote get-url origin`).
        #[arg(long, requires = "github_repo")]
        repo: Option<String>,
    },
    /// Log out and remove stored credentials.
    Logout,
    /// Erase all local personal data under ~/.portzero/ (credentials, agent
    /// cost metadata, observations, the daemon log, and daemon state files).
    ///
    /// Unlike `logout` (which removes only auth.json), this clears everything
    /// the client stores locally. It never touches Port Zero's servers. Stop
    /// the daemon first (`portzero stop`) — purge refuses while it is running.
    /// See docs/users/privacy.md for the full inventory.
    Purge,
    /// Show the currently authenticated user.
    Whoami,

    /// Upload a review record (branch commits + diff) to portzero.cloud so
    /// feedback threads pinned on your tunneled app link back to the code.
    /// Commit messages containing "Fixes PZ-<n>" advance the matching
    /// feedback thread to fix-proposed automatically.
    Review {
        /// Base ref to diff against (default: origin's default branch, else "main").
        #[arg(long)]
        base: Option<String>,

        /// Tunnel domain hosting the live app for this review
        /// (default: auto-detected from discovered cloud tunnels).
        #[arg(long)]
        domain: Option<String>,

        /// Project name (default: auto-detected from the git repository).
        #[arg(long)]
        project: Option<String>,

        /// Open the review record in the dashboard after upload.
        #[arg(long)]
        open: bool,
    },

    /// Team management has moved to the dashboard.
    Team,

    /// Manage starting the daemon automatically at boot.
    #[command(subcommand)]
    Autostart(AutostartCommand),

    /// Manage the local CA certificate and OS trust store.
    #[command(subcommand)]
    Trust(TrustCommand),

    /// Install AI coding-agent skills into your project.
    #[command(subcommand)]
    Skill(SkillCommand),

    /// Configure AI coding agents to use Port Zero (MCP registration + agent
    /// instructions), so you never have to hand copy-paste JSON again.
    #[command(subcommand)]
    Agents(AgentsCommand),

    /// Manage the discovery daemon (grouped aliases for the top-level
    /// `start` / `stop` / `restart` / `status` commands).
    #[command(subcommand)]
    Daemon(DaemonCommand),
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the discovery daemon and tunnel connection.
    Start {
        /// Run in the foreground instead of daemonizing (used internally).
        #[arg(long, hide = true)]
        foreground: bool,
        /// Do not automatically open the dashboard in a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Stop the discovery daemon.
    Stop,
    /// Restart the discovery daemon.
    Restart,
    /// Show daemon and tunnel status.
    Status,
}

#[derive(Subcommand)]
enum SkillCommand {
    /// Install the PaaS-agnostic "extract production config" skill into this
    /// project (default: .claude/skills/). Use --print to emit it to stdout for
    /// another agent tool, or --dir to choose the location.
    Install {
        /// Directory to install into (defaults to `.claude/skills`).
        #[arg(long)]
        dir: Option<std::path::PathBuf>,
        /// Overwrite an existing SKILL.md.
        #[arg(long)]
        force: bool,
        /// Print the skill to stdout instead of writing a file.
        #[arg(long)]
        print: bool,
    },
}

#[derive(Subcommand)]
enum AgentsCommand {
    /// Detect installed AI coding agents (Claude Code, Codex, pi.dev,
    /// opencode, Grok Build) and register the Port Zero MCP server + a
    /// user-level instructions block for each one found. When run inside a
    /// git repository, also provisions that repo for cloud AI dev
    /// environments (.claude/settings.json install hook, AGENTS.md tunnel
    /// instructions, .mcp.json MCP entry). Safe to re-run: existing config
    /// entries and content outside the portzero-managed blocks are preserved.
    Setup {
        /// Print what would change without writing any files or invoking any
        /// agent's own `mcp add` command.
        #[arg(long)]
        dry_run: bool,
        /// Only provision the current repository; skip machine-level agent
        /// config. Requires running inside a git repository.
        #[arg(long, conflicts_with = "machine_only")]
        repo_only: bool,
        /// Only configure machine-level agent config; skip repository
        /// provisioning even when inside a git repository.
        #[arg(long)]
        machine_only: bool,
    },
    /// Session-end cost hook (internal). Reads the Claude Code Stop-hook JSON
    /// on stdin, computes this session's agent-labor cost from usage metadata
    /// only, and records it when cost tracking is consented for the repo
    /// (otherwise prints a one-line teaser and persists nothing). Wired into
    /// `.claude/settings.json` by `agents setup`; not meant to be run by hand.
    #[command(hide = true)]
    CostHook,
}

#[derive(Subcommand)]
enum TrustCommand {
    /// Generate the local CA certificate (no root required).
    /// Run this before `trust install` so the cert exists when root reads it.
    Generate,
    /// Install the local CA into the OS trust store (requires root on Linux/macOS).
    Install,
    /// Remove the local CA from the OS trust store (requires root on Linux/macOS).
    Uninstall,
}

#[derive(Subcommand)]
enum AutostartCommand {
    /// Install the daemon as a system service that starts at boot.
    Enable,
    /// Remove the autostart system service.
    Disable,
    /// Show whether autostart is installed.
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    portzero_daemon::install_default_crypto_provider();

    let cli = Cli::parse();

    // Spawn update check in the background — it never blocks the command.
    let update_handle = tokio::spawn(update::check_for_update());

    // Bare `portzero` is the front door: a short state-aware summary with the
    // most useful next step, not a usage error.
    let Some(command) = cli.command else {
        frontdoor::run();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), update_handle).await;
        return Ok(());
    };

    match command {
        Command::Start {
            foreground,
            no_browser,
        } => {
            if foreground {
                daemon::start_foreground().await?;
            } else {
                daemon::start(!no_browser).await?;
            }
        }
        Command::Stop => daemon::stop()?,
        Command::Restart => daemon::restart().await?,
        Command::Status => daemon::status().await?,
        Command::Demo { no_browser } => demo::run(no_browser).await?,
        Command::DemoServer => demo_server::serve()?,
        Command::Doctor => doctor::run().await?,
        Command::Url { domain } => export::url(&domain)?,
        Command::Env { github } => export::env(github)?,
        Command::Wait {
            domain,
            healthy,
            timeout,
        } => wait::wait(&domain, healthy, timeout).await?,
        Command::Inspect => inspect::inspect()?,
        Command::Mcp => mcp::serve()?,
        Command::Setup => setup::run().await?,
        Command::Version => version::run()?,
        Command::Update { check, force } => selfupdate::run(check, force).await?,

        Command::Login {
            interactive,
            email,
            name,
            github_repo,
            team,
            repo,
        } => {
            if github_repo {
                // Handles its own daemon restart (only when one is running).
                github_repo_login::run(team, repo).await?;
            } else {
                auth::login(interactive, email, name).await?;
                daemon::restart().await?;
            }
        }
        Command::Logout => auth::logout()?,
        Command::Purge => purge::run()?,
        Command::Whoami => auth::whoami().await?,

        Command::Review {
            base,
            domain,
            project,
            open,
        } => review::run(base, domain, project, open).await?,

        Command::Team => {
            println!("Team management has moved to https://app.portzero.cloud/teams");
        }
        Command::Autostart(cmd) => match cmd {
            AutostartCommand::Enable => autostart::enable()?,
            AutostartCommand::Disable => autostart::disable()?,
            AutostartCommand::Status => autostart::status()?,
        },
        Command::Trust(cmd) => match cmd {
            TrustCommand::Generate => trust::generate()?,
            TrustCommand::Install => trust::install()?,
            TrustCommand::Uninstall => trust::uninstall()?,
        },
        Command::Skill(cmd) => match cmd {
            SkillCommand::Install { dir, force, print } => skill::install(dir, force, print)?,
        },
        Command::Agents(cmd) => match cmd {
            AgentsCommand::Setup {
                dry_run,
                repo_only,
                machine_only,
            } => agents::setup(dry_run, repo_only, machine_only)?,
            AgentsCommand::CostHook => agents::run_cost_hook()?,
        },
        Command::Daemon(cmd) => match cmd {
            DaemonCommand::Start {
                foreground,
                no_browser,
            } => {
                if foreground {
                    daemon::start_foreground().await?;
                } else {
                    daemon::start(!no_browser).await?;
                }
            }
            DaemonCommand::Stop => daemon::stop()?,
            DaemonCommand::Restart => daemon::restart().await?,
            DaemonCommand::Status => daemon::status().await?,
        },
    }

    // Wait briefly for the update check to print its notice (if any).
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), update_handle).await;

    Ok(())
}
