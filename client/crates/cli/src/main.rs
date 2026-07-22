//! port-zero CLI: manage the local tunnel daemon and cloud integration.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

mod agents;
mod api_client;
mod auth;
mod autostart;
mod browser;
mod daemon;
mod demo;
mod demo_server;
mod doctor;
mod export;
mod frontdoor;
mod github_repo_login;
mod inspect;
mod mcp;
mod review;
mod setup;
mod skill;
mod trust;
mod update;
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
    /// user-level instructions block for each one found. Safe to re-run:
    /// existing config entries and instructions content outside the
    /// portzero-managed block are preserved.
    Setup {
        /// Print what would change without writing any files or invoking any
        /// agent's own `mcp add` command.
        #[arg(long)]
        dry_run: bool,
    },
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
            AgentsCommand::Setup { dry_run } => agents::setup(dry_run)?,
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
