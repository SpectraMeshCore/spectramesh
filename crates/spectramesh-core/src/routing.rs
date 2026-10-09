//! Babel's route selection: the route table, the source table and the
//! feasibility condition.
//!
//! Every node advertises a route to itself, numbered with a sequence number
//! (seqno) that only it can raise. Neighbors pass on the routes they select,
//! each adding its own link cost, so a route's metric is its total cost.
//!
//! **Loops are prevented by the feasibility condition.** For each destination,
//! a node remembers the best (seqno, metric) it has advertised: its
//! *feasibility distance*, kept in the [`SourceTable`]. It only selects a
//! route that is strictly better than that, meaning a newer seqno, or the same
//! seqno and a lower metric. A route that leads back through this node would
//! cost at least what this node advertised, so it can never be selected, and
//! loops can't form even while the network is changing.
//!
//! When every route to a destination fails the condition, the node is
//! **starved**. It asks the destination for a newer seqno (a seqno request),
//! and routes carrying the new seqno are feasible again.

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;

use crate::hue::HueId;
use crate::node::NodeId;
use crate::time::Instant;
use core::time::Duration;

/// Metric of an unreachable destination.
pub const INFINITY: u32 = u32::MAX;

/// Whether seqno `a` is newer than `b`, allowing for wraparound.
pub fn seqno_newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

#[derive(Clone, Copy, Debug)]
struct Source {
    seqno: u16,
    metric: u32,
    refreshed: Instant,
}

/// Feasibility distances: the best (seqno, metric) this node has advertised
/// for each destination.
#[derive(Clone, Debug, Default)]
pub struct SourceTable {
    sources: BTreeMap<NodeId, Source>,
}

impl SourceTable {
    /// Whether a route advertised with `seqno` and `metric` passes the
    /// feasibility condition. Withdrawals always do.
    pub fn is_feasible(&self, dest: NodeId, seqno: u16, metric: u32) -> bool {
        metric == INFINITY
            || self.sources.get(&dest).is_none_or(|source| {
                seqno_newer(seqno, source.seqno)
                    || (seqno == source.seqno && metric < source.metric)
            })
    }

    /// Records that this node advertised `dest` with `seqno` and `metric`.
    pub fn record_advertised(&mut self, dest: NodeId, seqno: u16, metric: u32, now: Instant) {
        if metric == INFINITY {
            return;
        }
        let source = self.sources.entry(dest).or_insert(Source {
            seqno,
            metric,
            refreshed: now,
        });
        if seqno_newer(seqno, source.seqno) || (seqno == source.seqno && metric < source.metric) {
            source.seqno = seqno;
            source.metric = metric;
        }
        source.refreshed = now;
    }

    /// The seqno of `dest`'s feasibility distance, if it has one.
    pub fn seqno(&self, dest: NodeId) -> Option<u16> {
        self.sources.get(&dest).map(|source| source.seqno)
    }

    /// Forgets destinations not advertised within `max_age`.
    pub fn expire(&mut self, now: Instant, max_age: Duration) {
        self.sources
            .retain(|_, source| now.saturating_duration_since(source.refreshed) < max_age);
    }
}

/// A route to one destination as advertised by one neighbor on one hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteEntry {
    pub seqno: u16,
    /// The neighbor's own metric to the destination, before this node's link cost.
    pub advertised_metric: u32,
    pub expires: Instant,
}

/// Every route this node has heard, keyed by (destination, neighbor, hue).
#[derive(Clone, Debug, Default)]
pub struct RouteTable {
    entries: BTreeMap<(NodeId, NodeId, HueId), RouteEntry>,
}

impl RouteTable {
    /// Stores an update from `neighbor` on `hue`. An update that isn't
    /// `feasible` still refreshes an existing entry (which then won't be
    /// selected), but doesn't create a new one. Returns true if the seqno or
    /// metric changed.
    pub fn apply_update(
        &mut self,
        dest: NodeId,
        neighbor: NodeId,
        hue: HueId,
        update: RouteEntry,
        feasible: bool,
    ) -> bool {
        match self.entries.entry((dest, neighbor, hue)) {
            Entry::Occupied(mut slot) => {
                let entry = slot.get_mut();
                let changed = entry.seqno != update.seqno
                    || entry.advertised_metric != update.advertised_metric;
                *entry = update;
                changed
            }
            Entry::Vacant(slot) => {
                if feasible && update.advertised_metric != INFINITY {
                    slot.insert(update);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Drops entries whose neighbor stopped refreshing them. Returns true if any were dropped.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, entry| now < entry.expires);
        self.entries.len() != before
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, NodeId, HueId, &RouteEntry)> {
        self.entries
            .iter()
            .map(|(&(dest, neighbor, hue), entry)| (dest, neighbor, hue, entry))
    }

    /// Entries for `dest`, as (neighbor, hue, entry).
    pub fn for_dest(&self, dest: NodeId) -> impl Iterator<Item = (NodeId, HueId, &RouteEntry)> {
        let first = (dest, NodeId::from_u64(0), HueId(0));
        let last = (dest, NodeId::from_u64(u64::MAX), HueId(u8::MAX));
        self.entries
            .range(first..=last)
            .map(|(&(_, neighbor, hue), entry)| (neighbor, hue, entry))
    }
}

/// A selected route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub dest: NodeId,
    /// The neighbor to hand packets for `dest` to.
    pub next_hop: NodeId,
    /// The hue to reach `next_hop` on.
    pub hue: HueId,
    pub seqno: u16,
    /// Total cost to `dest`: the neighbor's advertised metric plus the link to it.
    pub metric: u32,
}

/// Picks the cheapest feasible route to each destination.
///
/// `link_cost` gives the current cost of the link to a neighbor on a hue, or
/// `None` if that link is unusable. A neighbor heard on several hues has an
/// entry per hue, so this is also where each route's hue is chosen.
//
// TODO: hysteresis, so two routes of nearly equal cost don't flap.
pub fn select(
    routes: &RouteTable,
    sources: &SourceTable,
    link_cost: impl Fn(NodeId, HueId) -> Option<u32>,
) -> BTreeMap<NodeId, Route> {
    let mut selected: BTreeMap<NodeId, Route> = BTreeMap::new();
    for (dest, neighbor, hue, entry) in routes.iter() {
        if entry.advertised_metric == INFINITY
            || !sources.is_feasible(dest, entry.seqno, entry.advertised_metric)
        {
            continue;
        }
        let Some(cost) = link_cost(neighbor, hue) else {
            continue;
        };
        let metric = entry.advertised_metric.saturating_add(cost);
        if metric == INFINITY {
            continue;
        }
        let route = Route {
            dest,
            next_hop: neighbor,
            hue,
            seqno: entry.seqno,
            metric,
        };
        match selected.entry(dest) {
            Entry::Vacant(slot) => {
                slot.insert(route);
            }
            Entry::Occupied(mut slot) => {
                if metric < slot.get().metric {
                    slot.insert(route);
                }
            }
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEST: NodeId = NodeId::from_u64(9);
    const B: NodeId = NodeId::from_u64(2);
    const C: NodeId = NodeId::from_u64(3);
    const WIFI: HueId = HueId(0);
    const SUB_GHZ: HueId = HueId(1);
    const NOW: Instant = Instant::from_millis(0);
    const LATER: Instant = Instant::from_millis(60_000);

    fn entry(seqno: u16, metric: u32) -> RouteEntry {
        RouteEntry {
            seqno,
            advertised_metric: metric,
            expires: LATER,
        }
    }

    #[test]
    fn seqnos_wrap() {
        assert!(seqno_newer(1, 0));
        assert!(seqno_newer(0, u16::MAX));
        assert!(!seqno_newer(0, 1));
        assert!(!seqno_newer(5, 5));
    }

    #[test]
    fn feasibility_needs_a_newer_seqno_or_a_lower_metric() {
        let mut sources = SourceTable::default();
        assert!(sources.is_feasible(DEST, 1, 500));

        sources.record_advertised(DEST, 1, 500, NOW);
        assert!(sources.is_feasible(DEST, 1, 499));
        assert!(!sources.is_feasible(DEST, 1, 500));
        assert!(!sources.is_feasible(DEST, 0, 1));
        assert!(sources.is_feasible(DEST, 2, 10_000));
        assert!(sources.is_feasible(DEST, 0, INFINITY));

        // Advertising a worse metric doesn't loosen the condition.
        sources.record_advertised(DEST, 1, 800, NOW);
        assert!(!sources.is_feasible(DEST, 1, 600));
    }

    #[test]
    fn unfeasible_updates_do_not_create_entries() {
        let mut routes = RouteTable::default();
        assert!(!routes.apply_update(DEST, B, WIFI, entry(1, 100), false));
        assert!(routes.apply_update(DEST, B, WIFI, entry(1, 100), true));
        // ...but do update existing ones.
        assert!(routes.apply_update(DEST, B, WIFI, entry(1, 900), false));
        assert_eq!(routes.for_dest(DEST).count(), 1);

        assert!(routes.expire(LATER));
        assert_eq!(routes.for_dest(DEST).count(), 0);
    }

    #[test]
    fn selects_the_cheapest_hue_and_neighbor() {
        let mut routes = RouteTable::default();
        routes.apply_update(DEST, B, WIFI, entry(1, 1000), true);
        routes.apply_update(DEST, B, SUB_GHZ, entry(1, 1000), true);
        routes.apply_update(DEST, C, WIFI, entry(1, 1200), true);

        let cost = |_: NodeId, hue: HueId| Some(if hue == WIFI { 40 } else { 3200 });
        let selected = select(&routes, &SourceTable::default(), cost);
        assert_eq!(
            selected[&DEST],
            Route {
                dest: DEST,
                next_hop: B,
                hue: WIFI,
                seqno: 1,
                metric: 1040,
            }
        );

        // Without B's Wi-Fi link, C's route is cheaper than B's sub-GHz one.
        let cost = |n: NodeId, hue: HueId| match (n, hue) {
            (B, WIFI) => None,
            (_, WIFI) => Some(40),
            _ => Some(3200),
        };
        assert_eq!(
            select(&routes, &SourceTable::default(), cost)[&DEST].next_hop,
            C
        );
    }

    #[test]
    fn skips_unfeasible_routes() {
        let mut routes = RouteTable::default();
        routes.apply_update(DEST, B, WIFI, entry(1, 1000), true);
        let mut sources = SourceTable::default();
        sources.record_advertised(DEST, 1, 900, NOW);

        assert!(select(&routes, &sources, |_, _| Some(40)).is_empty());
    }
}
