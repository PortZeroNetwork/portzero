//! Which claimant of a contested tunnel name actually serves traffic.
//!
//! A tunnel name maps to exactly one backend. When several live processes or
//! containers set `PZ_TUNNEL` to the same value — leaked backends from repeated
//! test runs, two worktrees, a restart whose predecessor has not exited — one of
//! them serves every request and the rest are shadowed. That is invisible unless
//! every surface agrees on *who wins* and says so, which is what this module is
//! for: the overlay service table, `portzero status`, `portzero inspect`, and the
//! MCP view all rank claimants through here, so they cannot disagree.
//!
//! The rule mirrors [`crate::route_table::RouteTable::update`] for cloud routes:
//! a claimant whose port is already known beats one whose port is not, and among
//! equals the lowest pid wins. Both halves are stable across scans, so the winner
//! does not flip while nothing has changed — a shifting winner is precisely what
//! makes a shadowed backend look like an intermittent application bug.

use std::collections::BTreeMap;

/// Something that can claim a tunnel name.
pub trait Claimant {
    /// The contested key: an overlay name, or a full tunnel domain.
    fn claim_key(&self) -> &str;
    /// The port this claimant is reached on (0 when not yet discovered).
    fn claim_port(&self) -> u16;
    /// The owning process id.
    fn claim_pid(&self) -> u32;
}

/// Sort key implementing the winner rule; lower sorts better.
pub fn claim_rank<C: Claimant + ?Sized>(claimant: &C) -> (bool, u32) {
    // `false < true`, so a known port sorts ahead of an undiscovered one.
    (claimant.claim_port() == 0, claimant.claim_pid())
}

/// Group claimants by the name they claim, best-first within each group.
///
/// The first entry of every group is the claimant that serves traffic; any
/// further entries are shadowed. Uncontested names are included as
/// single-element groups so callers can iterate one map rather than joining two.
pub fn group_by_claim<'a, C>(
    claimants: impl IntoIterator<Item = &'a C>,
) -> BTreeMap<String, Vec<&'a C>>
where
    C: Claimant + 'a,
{
    let mut out: BTreeMap<String, Vec<&'a C>> = BTreeMap::new();
    for claimant in claimants {
        out.entry(claimant.claim_key().to_string())
            .or_default()
            .push(claimant);
    }
    for group in out.values_mut() {
        group.sort_by_key(|claimant| claim_rank(*claimant));
    }
    out
}

/// Whether a claimant at `index` within its (best-first) group serves traffic.
pub fn is_serving(index: usize) -> bool {
    index == 0
}

/// The word every surface uses for a claimant's standing, so the vocabulary is
/// identical in the CLI, the desktop app, and the MCP payload.
pub fn standing(index: usize) -> &'static str {
    if is_serving(index) {
        "serving"
    } else {
        "shadowed"
    }
}

impl Claimant for crate::discovery::DiscoveredNetworkService {
    fn claim_key(&self) -> &str {
        &self.name
    }
    fn claim_port(&self) -> u16 {
        self.service_port
    }
    fn claim_pid(&self) -> u32 {
        self.pid
    }
}

impl Claimant for crate::route_table::OverlayRoute {
    fn claim_key(&self) -> &str {
        &self.domain
    }
    fn claim_port(&self) -> u16 {
        self.service_port
    }
    fn claim_pid(&self) -> u32 {
        self.pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::ServiceSource;
    use crate::route_table::OverlayRoute;

    fn route(domain: &str, port: u16, pid: u32) -> OverlayRoute {
        OverlayRoute {
            domain: domain.to_string(),
            domain_template: domain.to_string(),
            substitutions: Default::default(),
            service_port: port,
            backend_protocol: None,
            real_addr: format!("127.0.0.1:{}", 40000 + pid),
            health_path: None,
            pid,
            source: ServiceSource::Process { cwd: None },
        }
    }

    #[test]
    fn lowest_pid_serves_among_claimants_with_known_ports() {
        let routes = [
            route("api.portzero.local", 8080, 900),
            route("api.portzero.local", 8081, 300),
            route("api.portzero.local", 8082, 600),
        ];
        let groups = group_by_claim(routes.iter());
        let claimants = &groups["api.portzero.local"];
        assert_eq!(claimants[0].pid, 300);
        assert_eq!(claimants[1].pid, 600);
        assert_eq!(claimants[2].pid, 900);
    }

    #[test]
    fn known_port_beats_undiscovered_port_regardless_of_pid() {
        // A claimant whose port is still unknown cannot serve traffic, so it
        // must never outrank one that can just because its pid is lower.
        let routes = [
            route("api.portzero.local", 0, 100),
            route("api.portzero.local", 8080, 999),
        ];
        let groups = group_by_claim(routes.iter());
        assert_eq!(groups["api.portzero.local"][0].pid, 999);
    }

    #[test]
    fn ranking_is_stable_across_input_order() {
        let forward = [
            route("api.portzero.local", 8080, 700),
            route("api.portzero.local", 8081, 200),
        ];
        let reversed: Vec<OverlayRoute> = forward.iter().cloned().rev().collect();
        let a = group_by_claim(forward.iter());
        let b = group_by_claim(reversed.iter());
        assert_eq!(
            a["api.portzero.local"][0].pid,
            b["api.portzero.local"][0].pid
        );
    }

    #[test]
    fn uncontested_names_are_single_element_groups() {
        let routes = [
            route("api.portzero.local", 8080, 1),
            route("web.portzero.local", 3000, 2),
        ];
        let groups = group_by_claim(routes.iter());
        assert_eq!(groups.len(), 2);
        assert!(groups.values().all(|g| g.len() == 1));
    }

    #[test]
    fn standing_names_the_winner_and_the_rest() {
        assert_eq!(standing(0), "serving");
        assert_eq!(standing(1), "shadowed");
        assert_eq!(standing(7), "shadowed");
    }
}
