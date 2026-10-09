//! Neighbors and link costs, measured in both directions as in Babel.
//!
//! Each node broadcasts numbered hellos on every hue. A receiver keeps a
//! 16-bit history of which recent hellos arrived, which gives its **rxcost**:
//! the cost of the link towards it. It reports that back to the neighbor in an
//! IHU ("I heard you"), which becomes the neighbor's **txcost**: the cost of
//! the link away from it. A link's cost uses both, so a link that only works
//! one way is never used.

use alloc::collections::BTreeMap;
use core::time::Duration;

use crate::hue::HueId;
use crate::node::NodeId;
use crate::time::Instant;

/// rxcost or txcost of an unusable link.
pub const INFINITY: u16 = u16::MAX;

/// rxcost or txcost of a link that loses nothing.
pub const PERFECT: u16 = 256;

/// Number of recent hellos the history covers.
const HISTORY_LEN: u32 = 16;

#[derive(Clone, Debug)]
struct Hellos {
    seqno: u16,
    /// One bit per recent hello; bit 0 is the newest.
    history: u16,
    /// How many bits of `history` cover time since the neighbor was first
    /// heard, up to 16. Earlier bits don't count as losses.
    slots: u32,
    last: Instant,
    interval: Duration,
}

/// A neighbor as heard on one hue.
#[derive(Clone, Debug)]
pub struct Neighbor {
    hellos: Option<Hellos>,
    txcost: u16,
    txcost_expires: Instant,
}

impl Neighbor {
    fn new() -> Self {
        Neighbor {
            hellos: None,
            txcost: INFINITY,
            txcost_expires: Instant::default(),
        }
    }

    fn record_hello(&mut self, seqno: u16, interval: Duration, now: Instant) {
        let Some(hellos) = &mut self.hellos else {
            self.hellos = Some(Hellos {
                seqno,
                history: 1,
                slots: 1,
                last: now,
                interval,
            });
            return;
        };
        let gap = seqno.wrapping_sub(hellos.seqno);
        if gap == 0 {
            // The same hello, heard twice.
            return;
        } else if gap < 0x8000 {
            hellos.history = if u32::from(gap) >= HISTORY_LEN {
                0
            } else {
                hellos.history << gap
            };
            hellos.slots = (hellos.slots + u32::from(gap)).min(HISTORY_LEN);
        } else if gap.wrapping_neg() > 8 {
            // Well behind: the neighbor restarted its count, probably by rebooting.
            hellos.history = 0;
            hellos.slots = 1;
        } else {
            // A late, reordered hello.
            return;
        }
        hellos.history |= 1;
        hellos.seqno = seqno;
        hellos.last = now;
        hellos.interval = interval;
    }

    fn record_ihu(&mut self, rxcost: u16, interval: Duration, now: Instant) {
        self.txcost = rxcost;
        self.txcost_expires = now + interval * 7 / 2;
    }

    /// Hello history with overdue hellos counted as lost, and how many of its
    /// bits count. A hello is overdue once it's half an interval late.
    fn history(&self, now: Instant) -> (u16, u32) {
        let Some(hellos) = &self.hellos else {
            return (0, 0);
        };
        let interval = hellos.interval.as_millis().max(1);
        let elapsed = now.saturating_duration_since(hellos.last).as_millis();
        let missed = ((elapsed + interval / 2) / interval)
            .saturating_sub(1)
            .min(HISTORY_LEN.into()) as u32;
        let history = ((u32::from(hellos.history) << missed) & 0xffff) as u16;
        (history, (hellos.slots + missed).min(HISTORY_LEN))
    }

    /// Cost of the link towards this node, from the share of recent hellos
    /// that arrived. [`INFINITY`] once four in a row are lost.
    pub fn rxcost(&self, now: Instant) -> u16 {
        let (history, slots) = self.history(now);
        if history & 0xf == 0 {
            return INFINITY;
        }
        (u32::from(PERFECT) * slots / history.count_ones()) as u16
    }

    /// Cost of the link away from this node, as the neighbor last reported it.
    /// [`INFINITY`] if its reports have stopped.
    pub fn txcost(&self, now: Instant) -> u16 {
        if now < self.txcost_expires {
            self.txcost
        } else {
            INFINITY
        }
    }

    /// Expected transmissions per delivered packet, counting both directions,
    /// ×256 (256 is a perfect link). `None` if the link is unusable.
    pub fn etx(&self, now: Instant) -> Option<u32> {
        let (rx, tx) = (self.rxcost(now), self.txcost(now));
        if rx == INFINITY || tx == INFINITY {
            return None;
        }
        Some(u32::from(rx) * u32::from(tx) / u32::from(PERFECT))
    }

    fn is_gone(&self, now: Instant) -> bool {
        self.history(now).0 == 0 && now >= self.txcost_expires
    }
}

#[derive(Clone, Debug, Default)]
pub struct NeighborTable {
    neighbors: BTreeMap<(NodeId, HueId), Neighbor>,
}

impl NeighborTable {
    pub fn record_hello(
        &mut self,
        node: NodeId,
        hue: HueId,
        seqno: u16,
        interval: Duration,
        now: Instant,
    ) {
        self.entry(node, hue).record_hello(seqno, interval, now);
    }

    pub fn record_ihu(
        &mut self,
        node: NodeId,
        hue: HueId,
        rxcost: u16,
        interval: Duration,
        now: Instant,
    ) {
        self.entry(node, hue).record_ihu(rxcost, interval, now);
    }

    fn entry(&mut self, node: NodeId, hue: HueId) -> &mut Neighbor {
        self.neighbors
            .entry((node, hue))
            .or_insert_with(Neighbor::new)
    }

    pub fn get(&self, node: NodeId, hue: HueId) -> Option<&Neighbor> {
        self.neighbors.get(&(node, hue))
    }

    /// Drops neighbors with no recent hellos or IHUs. Returns true if any were dropped.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.neighbors.len();
        self.neighbors.retain(|_, neighbor| !neighbor.is_gone(now));
        self.neighbors.len() != before
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeId, HueId, &Neighbor)> {
        self.neighbors
            .iter()
            .map(|(&(node, hue), neighbor)| (node, hue, neighbor))
    }

    pub fn on_hue(&self, hue: HueId) -> impl Iterator<Item = (NodeId, &Neighbor)> {
        self.iter()
            .filter(move |&(_, h, _)| h == hue)
            .map(|(node, _, neighbor)| (node, neighbor))
    }

    pub fn len(&self) -> usize {
        self.neighbors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.neighbors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(1);

    fn at(secs: u64) -> Instant {
        Instant::from_millis(secs * 1000)
    }

    /// A neighbor that has sent hellos 0..16, one a second, all received.
    fn steady() -> Neighbor {
        let mut neighbor = Neighbor::new();
        for seqno in 0..16 {
            neighbor.record_hello(seqno, INTERVAL, at(seqno.into()));
        }
        neighbor
    }

    #[test]
    fn rxcost_reflects_hello_loss() {
        assert_eq!(steady().rxcost(at(15)), PERFECT);

        let mut lossy = Neighbor::new();
        // Every other hello is lost.
        for seqno in (0..32).step_by(2) {
            lossy.record_hello(seqno, INTERVAL, at(seqno.into()));
        }
        assert_eq!(lossy.rxcost(at(30)), 2 * PERFECT);
    }

    #[test]
    fn duplicates_and_late_hellos_are_ignored() {
        let mut neighbor = steady();
        neighbor.record_hello(15, INTERVAL, at(15));
        neighbor.record_hello(14, INTERVAL, at(15));
        assert_eq!(neighbor.rxcost(at(15)), PERFECT);
    }

    #[test]
    fn silence_makes_the_link_unusable() {
        let neighbor = steady();
        // Not yet overdue at 1.4 intervals; one lost at 1.5.
        assert_eq!(neighbor.rxcost(Instant::from_millis(16_400)), PERFECT);
        assert_eq!(neighbor.rxcost(Instant::from_millis(16_500)), 273);
        // Four lost in a row.
        assert_eq!(neighbor.rxcost(Instant::from_millis(19_500)), INFINITY);
    }

    #[test]
    fn new_neighbors_are_not_penalized_for_time_before_they_were_heard() {
        let mut neighbor = Neighbor::new();
        neighbor.record_hello(0, INTERVAL, at(0));
        assert_eq!(neighbor.rxcost(at(0)), PERFECT);
        neighbor.record_hello(2, INTERVAL, at(2));
        assert_eq!(neighbor.rxcost(at(2)), 3 * PERFECT / 2);
    }

    #[test]
    fn a_restarted_count_starts_a_fresh_history() {
        let mut neighbor = steady();
        neighbor.record_hello(0, INTERVAL, at(17));
        assert_eq!(neighbor.rxcost(at(17)), PERFECT);
        neighbor.record_hello(2, INTERVAL, at(19));
        assert_eq!(neighbor.rxcost(at(19)), 3 * PERFECT / 2);
    }

    #[test]
    fn etx_needs_both_directions() {
        let mut neighbor = steady();
        assert_eq!(neighbor.etx(at(15)), None);

        neighbor.record_ihu(2 * PERFECT, INTERVAL, at(15));
        assert_eq!(neighbor.etx(at(15)), Some(512));
        // The IHU expires after 3.5 intervals.
        assert_eq!(neighbor.txcost(Instant::from_millis(18_500)), INFINITY);
    }

    #[test]
    fn quiet_neighbors_expire() {
        let mut table = NeighborTable::default();
        table.record_hello(NodeId::from_u32(1), HueId(0), 0, INTERVAL, at(0));
        table.record_hello(NodeId::from_u32(2), HueId(0), 0, INTERVAL, at(10));

        assert!(table.expire(at(20)));
        assert_eq!(table.len(), 1);
        assert!(table.get(NodeId::from_u32(2), HueId(0)).is_some());
    }
}
