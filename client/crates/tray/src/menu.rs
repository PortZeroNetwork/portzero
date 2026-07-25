//! The tray menu — its structure is built once here as a backend-neutral
//! [`MenuSpec`], so the menu is identical across macOS, Windows, and Linux by
//! construction. Each platform layer renders the same spec with its own toolkit
//! (muda on Windows/macOS, ksni on Linux) and never re-derives the structure.
//!
//! Every actionable node carries both a stable id (used by the muda backend,
//! whose clicks arrive as ids over a channel) and the [`Action`] it triggers
//! (used directly by the ksni backend, whose clicks arrive as closures).

use portzero_daemon::versions;
use portzero_domain::endpoints;

use crate::state::{Health, Snapshot};

/// Something the user asked the tray to do by clicking a menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Refresh,
    Quit,
    /// Launch the PortZero desktop app (the primary local GUI).
    OpenApp,
    /// Open a URL in the default browser — a real service URL (a tunnel link)
    /// or the Cloud onboarding page.
    OpenUrl(String),
    /// Set the "enable HTTPS for HTTP tunnels" policy to this value.
    ToggleHttps(bool),
}

/// One node in the backend-neutral menu tree.
#[derive(Debug, Clone)]
pub enum Node {
    /// A non-interactive, disabled line of text (headers, issues, hints).
    Label(String),
    /// A visual separator.
    Separator,
    /// A clickable item that performs `action`.
    Item {
        id: String,
        label: String,
        enabled: bool,
        action: Action,
    },
    /// A checkbox item that performs `action` (which sets the new value).
    Check {
        id: String,
        label: String,
        checked: bool,
        action: Action,
    },
    /// A nested submenu.
    Sub { label: String, children: Vec<Node> },
}

/// The full tray menu tree.
#[derive(Debug, Clone)]
pub struct MenuSpec {
    pub nodes: Vec<Node>,
}

// Stable ids for the fixed items. Tunnel items get generated `tunnel:<n>` ids.
const ID_START: &str = "daemon:start";
const ID_STOP: &str = "daemon:stop";
const ID_RESTART: &str = "daemon:restart";
const ID_DASHBOARD: &str = "open:dashboard";
const ID_HTTPS: &str = "config:https";
const ID_REFRESH: &str = "app:refresh";
const ID_QUIT: &str = "app:quit";
const ID_DASHBOARD_DETAILS: &str = "open:dashboard-details";
const ID_CLOUD_UPSELL: &str = "open:cloud";

/// Build the full tray menu tree from a snapshot.
pub fn build(snapshot: &Snapshot) -> MenuSpec {
    let mut nodes: Vec<Node> = Vec::new();

    // Header — a disabled one-line status summary with a health glyph.
    let glyph = match snapshot.health {
        Health::Ok => "●",
        Health::Degraded => "▲",
        Health::Down => "■",
    };
    nodes.push(Node::Label(format!("{glyph}  {}", snapshot.summary)));
    nodes.push(Node::Separator);

    // Open the desktop app (the primary local GUI, replacing the old browser
    // dashboard).
    nodes.push(action(ID_DASHBOARD, "Open PortZero", Action::OpenApp));
    nodes.push(Node::Separator);

    // Daemon lifecycle — contextual to whether it is running.
    if snapshot.running {
        nodes.push(action(ID_RESTART, "Restart Daemon", Action::Restart));
        nodes.push(action(ID_STOP, "Stop Daemon", Action::Stop));
    } else {
        nodes.push(action(ID_START, "Start Daemon", Action::Start));
    }
    nodes.push(Node::Separator);

    // Tunnels submenu.
    let mut tunnels: Vec<Node> = Vec::new();
    if snapshot.tunnels.is_empty() {
        tunnels.push(Node::Label("No tunnels discovered yet".to_string()));
    } else {
        for (i, t) in snapshot.tunnels.iter().enumerate() {
            let scheme = if t.https { "https" } else { "http" };
            let kind = if t.cloud { "cloud" } else { "local" };
            tunnels.push(Node::Item {
                id: format!("tunnel:{i}"),
                label: format!("{}   ·   {scheme} · {kind}", t.domain),
                enabled: true,
                action: Action::OpenUrl(t.url.clone()),
            });
        }
    }
    // When at least one tunnel is local-only, teach the one move that makes it
    // reachable from a phone / another machine: swap `.local` for `.cloud` in
    // `PZ_TUNNEL`. This surfaces the paid Cloud crossing at the exact moment the
    // user is looking at a working local tunnel — no separate `share` command,
    // because the swap *is* the whole action. The clickable row opens the Cloud
    // app where they sign in / pick a plan.
    if snapshot.tunnels.iter().any(|t| !t.cloud) {
        tunnels.push(Node::Separator);
        tunnels.push(Node::Label(
            "On another device? Swap .local → .cloud in PZ_TUNNEL".to_string(),
        ));
        tunnels.push(action(
            ID_CLOUD_UPSELL,
            "Set up Cloud tunnels →",
            Action::OpenUrl(endpoints::dashboard_url()),
        ));
    }
    nodes.push(Node::Sub {
        label: format!("Tunnels ({})", snapshot.tunnels.len()),
        children: tunnels,
    });

    // Global HTTPS policy toggle (affects plain-HTTP `.portzero.local` tunnels).
    // Clicking flips the current value.
    nodes.push(Node::Check {
        id: ID_HTTPS.to_string(),
        label: "Enable HTTPS for HTTP tunnels".to_string(),
        checked: snapshot.https.enable_for_port_80,
        action: Action::ToggleHttps(!snapshot.https.enable_for_port_80),
    });

    // Issues submenu — only when there is something to show. Combines
    // issues.json and diagnostics.json problems into one list (see
    // `notify::collect_problems`) so the tray, dashboard, and /status.json
    // all agree on what an "issue" is.
    if !snapshot.problems.is_empty() {
        let mut issues: Vec<Node> = Vec::new();
        for p in &snapshot.problems {
            let title = if let Some(pid) = p.pid {
                format!("{} (pid {pid})", p.title)
            } else {
                p.title.clone()
            };
            issues.push(Node::Label(format!("⚠  {}", truncate(&title, 70))));
            let fix = p.fix_command.as_deref().or(p.fix.as_deref());
            if let Some(fix) = fix {
                issues.push(Node::Label(format!("      ↳ {}", truncate(fix, 80))));
            }
        }
        issues.push(Node::Separator);
        issues.push(action(
            ID_DASHBOARD_DETAILS,
            "Open PortZero for fixes →",
            Action::OpenApp,
        ));
        nodes.push(Node::Sub {
            label: format!("Issues ({})", snapshot.problems.len()),
            children: issues,
        });
    }
    nodes.push(Node::Separator);

    // App controls.
    nodes.push(action(ID_REFRESH, "Refresh Now", Action::Refresh));
    nodes.push(action(ID_QUIT, "Quit PortZero Tray", Action::Quit));

    // The running tray's own version, so "which version am I on?" is answerable
    // without opening anything. Whether every component agrees on it is shown in
    // the desktop app's Version panel (and `portzero version`), which can see
    // all of them; the tray only speaks for itself.
    nodes.push(Node::Separator);
    nodes.push(Node::Label(format!(
        "PortZero v{}",
        versions::BUILD_VERSION
    )));

    MenuSpec { nodes }
}

/// Convenience for a simple enabled clickable item.
fn action(id: &str, label: &str, action: Action) -> Node {
    Node::Item {
        id: id.to_string(),
        label: label.to_string(),
        enabled: true,
        action,
    }
}

/// Truncate to `n` chars with an ellipsis, on a char boundary.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

// Renders the neutral spec into a `muda::Menu` and the id → action map the
// controller dispatches against. Linux uses ksni instead and never compiles
// muda (which links GTK), so this is gated off there.
#[cfg(not(target_os = "linux"))]
mod muda_backend {
    use std::collections::HashMap;

    use muda::{CheckMenuItem, IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};

    use super::{Action, MenuSpec, Node};

    /// Maps menu-item ids to the action they perform.
    pub type MenuModel = HashMap<String, Action>;

    /// Render the spec into a `muda::Menu` plus the id → action model.
    pub fn to_muda(spec: &MenuSpec) -> (Menu, MenuModel) {
        let menu = Menu::new();
        let mut model = MenuModel::new();
        for item in build_items(&spec.nodes, &mut model) {
            let _ = menu.append(item.as_ref());
        }
        (menu, model)
    }

    fn build_items(nodes: &[Node], model: &mut MenuModel) -> Vec<Box<dyn IsMenuItem>> {
        let mut items: Vec<Box<dyn IsMenuItem>> = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node {
                Node::Label(text) => {
                    items.push(Box::new(MenuItem::new(text.as_str(), false, None)));
                }
                Node::Separator => {
                    items.push(Box::new(PredefinedMenuItem::separator()));
                }
                Node::Item {
                    id,
                    label,
                    enabled,
                    action,
                } => {
                    items.push(Box::new(MenuItem::with_id(
                        MenuId(id.clone()),
                        label.as_str(),
                        *enabled,
                        None,
                    )));
                    model.insert(id.clone(), action.clone());
                }
                Node::Check {
                    id,
                    label,
                    checked,
                    action,
                } => {
                    items.push(Box::new(CheckMenuItem::with_id(
                        MenuId(id.clone()),
                        label.as_str(),
                        true,
                        *checked,
                        None,
                    )));
                    model.insert(id.clone(), action.clone());
                }
                Node::Sub { label, children } => {
                    let sub = Submenu::new(label.as_str(), true);
                    for child in build_items(children, model) {
                        let _ = sub.append(child.as_ref());
                    }
                    items.push(Box::new(sub));
                }
            }
        }
        items
    }
}

#[cfg(not(target_os = "linux"))]
pub use muda_backend::{to_muda, MenuModel};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("hello", 70), "hello");
    }

    #[test]
    fn truncate_shortens_long_strings_with_ellipsis() {
        let long = "x".repeat(100);
        let t = truncate(&long, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }

    use crate::state::Tunnel;
    use portzero_daemon::net::stack::OverlayHttpsPolicy;

    fn snapshot_with(tunnels: Vec<Tunnel>) -> Snapshot {
        Snapshot {
            running: true,
            health: Health::Ok,
            tunnels,
            problems: vec![],
            https: OverlayHttpsPolicy {
                enable_for_port_80: false,
                redirect_port_80: true,
                passthrough_port_443: true,
            },
            summary: "ok".to_string(),
        }
    }

    fn local(domain: &str) -> Tunnel {
        Tunnel {
            domain: domain.to_string(),
            url: format!("http://{domain}"),
            https: false,
            cloud: false,
        }
    }

    fn cloud(domain: &str) -> Tunnel {
        Tunnel {
            domain: domain.to_string(),
            url: format!("https://{domain}"),
            https: true,
            cloud: true,
        }
    }

    /// Pull the children of the "Tunnels (n)" submenu out of a built spec.
    fn tunnels_children(spec: &MenuSpec) -> Vec<Node> {
        for node in &spec.nodes {
            if let Node::Sub { label, children } = node {
                if label.starts_with("Tunnels") {
                    return children.clone();
                }
            }
        }
        Vec::new()
    }

    fn has_cloud_upsell(children: &[Node]) -> bool {
        children
            .iter()
            .any(|n| matches!(n, Node::Item { id, .. } if id == ID_CLOUD_UPSELL))
    }

    #[test]
    fn a_local_tunnel_surfaces_the_cloud_crossing_and_teaches_the_swap() {
        let spec = build(&snapshot_with(vec![local("web.myapp.portzero.local")]));
        let children = tunnels_children(&spec);
        assert!(has_cloud_upsell(&children), "clickable Cloud row missing");
        assert!(
            children.iter().any(|n| matches!(n, Node::Label(t)
                if t.contains(".local") && t.contains(".cloud") && t.contains("PZ_TUNNEL"))),
            "the .local → .cloud swap hint is missing"
        );
    }

    #[test]
    fn an_all_cloud_setup_does_not_upsell_cloud() {
        let spec = build(&snapshot_with(vec![cloud(
            "api.alice.tunnel.portzero.cloud",
        )]));
        assert!(!has_cloud_upsell(&tunnels_children(&spec)));
    }

    #[test]
    fn no_tunnels_means_no_cloud_upsell() {
        let spec = build(&snapshot_with(vec![]));
        assert!(!has_cloud_upsell(&tunnels_children(&spec)));
    }
}
