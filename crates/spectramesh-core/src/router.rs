//! The router: Babel behind a sans-IO API.
//!
//! The router never touches a radio or reads a clock. The platform layer runs
//! this loop:
//!
//! 1. Pass every received frame to [`Router::handle_frame`].
//! 2. Call [`Router::poll`] at or after [`Router::next_wakeup`].
//! 3. Send each frame from [`Router::poll_transmit`] on the hue it names.
//! 4. Hand each packet from [`Router::poll_delivery`] to the application.
//!
//! Every frame the router sends is sealed with a [link tag](crate::auth), and
//! every frame it receives must carry a valid tag from a neighbor whose boot
//! index has passed a challenge.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::time::Duration;

use crate::auth::{KeyRing, ReplayWindow, TRAILER_LEN};
use crate::crypto::Random;
use crate::error::{Error, Result};
use crate::hue::{HueId, HueInfo};
use crate::neighbor::{self, NeighborTable};
use crate::node::NodeId;
use crate::packet::{
    Body, DATA_OVERHEAD, DataHeader, Frame, FrameKind, HEADER_LEN, Header, Tlv, Trailer,
};
use crate::routing::{self, INFINITY, Route, RouteEntry, RouteTable, SourceTable, seqno_newer};
use crate::time::Instant;

/// Settings that apply to every hue. Per-hue timers live in [`HueInfo`].
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Hop limit for data packets this node creates, and for its seqno requests.
    pub default_ttl: u8,
    /// How long to wait before repeating a seqno request for the same destination.
    //
    // TODO: scale with the hue the request goes out on.
    pub request_hold: Duration,
    /// How long to remember a feasibility distance after this node stops
    /// advertising the destination.
    pub source_gc: Duration,
    /// How long to remember a neighbor's boot index and counters after its
    /// last frame. A neighbor heard again after this must pass a new challenge.
    pub peer_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_ttl: 16,
            request_hold: Duration::from_secs(4),
            source_gc: Duration::from_secs(180),
            peer_timeout: Duration::from_secs(300),
        }
    }
}

/// A frame for the platform to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transmit {
    pub hue: HueId,
    /// The neighbor the frame is for, or [`NodeId::BROADCAST`]. On hues with
    /// link-layer addresses (Ethernet, Wi-Fi, ESP-NOW), the platform can send
    /// to that neighbor's address instead of broadcasting, which matters on a
    /// switched network. [`Router::handle_frame`] says which node sent each
    /// frame it accepts, so the platform can learn which address is whose.
    pub next_hop: NodeId,
    pub frame: Vec<u8>,
}

/// Application data that reached this node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub src: NodeId,
    pub payload: Vec<u8>,
}

/// What this node knows about a neighbor's frames on one hue, once its boot
/// index has passed a challenge.
#[derive(Clone, Copy, Debug)]
struct Peer {
    index: u32,
    window: ReplayWindow,
    last_heard: Instant,
}

/// A challenge sent to a neighbor with a boot index this node doesn't know yet.
#[derive(Clone, Copy, Debug)]
struct Challenge {
    index: u32,
    nonce: u64,
    sent: Instant,
}

/// The shortest wait before challenging the same neighbor and boot index again.
const CHALLENGE_RETRY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
struct HueState {
    info: HueInfo,
    hello_seqno: u16,
    next_hello: Instant,
    next_update: Instant,
    /// Whether this hue's first hello, which asks neighbors for their routes, went out.
    announced: bool,
}

pub struct Router {
    id: NodeId,
    config: Config,
    keys: KeyRing,
    random: Random,
    /// Random for each run of the router, so frames from earlier runs can't be replayed.
    boot_index: u32,
    /// Counts every frame sent, on any hue.
    counter: u32,
    peers: BTreeMap<(NodeId, HueId), Peer>,
    challenges: BTreeMap<(NodeId, HueId), Challenge>,
    /// When this node last answered a challenge from an unverified neighbor.
    /// Those frames could be replays, so answers are rate-limited.
    challenge_answers: BTreeMap<(NodeId, HueId), Instant>,
    /// The seqno of this node's route to itself.
    seqno: u16,
    hues: Vec<HueState>,
    neighbors: NeighborTable,
    routes: RouteTable,
    sources: SourceTable,
    selected: BTreeMap<NodeId, Route>,
    /// Whether routes need selecting again before the next update.
    stale: bool,
    /// Destinations to send an update for on every hue at the next poll.
    triggered: BTreeSet<NodeId>,
    /// Seqno requests to broadcast on every hue at the next poll.
    requests: Vec<Tlv>,
    /// The last seqno requested for each destination, and when.
    recent_requests: BTreeMap<NodeId, (u16, Instant)>,
    outbox: VecDeque<Transmit>,
    deliveries: VecDeque<Delivery>,
}

impl Router {
    /// Creates a router for node `id`, sending with the current key in `keys`.
    ///
    /// `seed` must be 32 fresh bytes from a cryptographic random number
    /// generator every time the router starts. Nonces and the boot index come
    /// from it, and reusing a seed would let recorded frames be replayed.
    pub fn new(id: NodeId, keys: KeyRing, config: Config, seed: [u8; 32]) -> Self {
        let mut random = Random::new(seed);
        let boot_index = random.next_u32();
        Router {
            id,
            config,
            keys,
            random,
            boot_index,
            counter: 0,
            peers: BTreeMap::new(),
            challenges: BTreeMap::new(),
            challenge_answers: BTreeMap::new(),
            seqno: 0,
            hues: Vec::new(),
            neighbors: NeighborTable::default(),
            routes: RouteTable::default(),
            sources: SourceTable::default(),
            selected: BTreeMap::new(),
            stale: false,
            triggered: BTreeSet::new(),
            requests: Vec::new(),
            recent_requests: BTreeMap::new(),
            outbox: VecDeque::new(),
            deliveries: VecDeque::new(),
        }
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Registers a hue, replacing any existing one with the same ID. A new
    /// hue says hello and asks for its neighbors' routes at the next poll.
    pub fn add_hue(&mut self, info: HueInfo) {
        match self.hues.iter_mut().find(|h| h.info.id == info.id) {
            Some(state) => state.info = info,
            None => self.hues.push(HueState {
                info,
                hello_seqno: 0,
                next_hello: Instant::default(),
                next_update: Instant::default(),
                announced: false,
            }),
        }
    }

    pub fn hue(&self, id: HueId) -> Option<&HueInfo> {
        self.hues.iter().map(|h| &h.info).find(|h| h.id == id)
    }

    pub fn neighbors(&self) -> &NeighborTable {
        &self.neighbors
    }

    /// Selected routes, as of the last [`poll`](Self::poll).
    pub fn routes(&self) -> impl Iterator<Item = &Route> {
        self.selected.values()
    }

    pub fn route_to(&self, dest: NodeId) -> Option<&Route> {
        self.selected.get(&dest)
    }

    /// When [`poll`](Self::poll) next has work to do. If that's earlier than
    /// now, which it is after most received frames, poll straight away.
    pub fn next_wakeup(&self) -> Instant {
        if self.stale || !self.triggered.is_empty() || !self.requests.is_empty() {
            return Instant::default();
        }
        self.hues
            .iter()
            .map(|h| h.next_hello.min(h.next_update))
            .min()
            .unwrap_or(Instant::from_millis(u64::MAX))
    }

    /// Processes a frame received on `hue`.
    ///
    /// Returns the neighbor that sent the frame once its tag and freshness
    /// check out, so platforms can learn its link-layer address, or `None`
    /// for this node's own frames, echoed back. Frames meant for another next
    /// hop are checked but otherwise ignored.
    ///
    /// Frames from a neighbor this node hasn't verified yet are answered with
    /// a challenge and dropped with [`Error::Unverified`]. That's routine when
    /// a neighbor appears or restarts, not a fault.
    pub fn handle_frame(
        &mut self,
        hue: HueId,
        bytes: &[u8],
        now: Instant,
    ) -> Result<Option<NodeId>> {
        if self.hue(hue).is_none() {
            return Err(Error::UnknownHue(hue));
        }
        let frame = Frame::decode(bytes)?;
        let sender = frame.header.sender;
        if sender == self.id {
            return Ok(None);
        }
        if !self
            .keys
            .verify(frame.header.key_id, frame.signed, &frame.trailer.tag)
        {
            return Err(Error::BadTag);
        }
        self.check_fresh(&frame, hue, now)?;

        if !self.is_for_me(frame.header.next_hop) {
            return Ok(Some(sender));
        }
        match frame.body {
            Body::Control(tlvs) => {
                for tlv in tlvs {
                    self.handle_tlv(sender, hue, tlv?, now);
                }
            }
            Body::Data { header, payload } => {
                if header.origin == self.id {
                    return Ok(Some(sender));
                }
                if header.dst == self.id || header.dst.is_broadcast() {
                    self.deliveries.push_back(Delivery {
                        src: header.origin,
                        payload: payload.to_vec(),
                    });
                } else {
                    self.forward(header, payload);
                }
            }
        }
        Ok(Some(sender))
    }

    /// Checks that an authentic frame isn't a replay. A frame with a boot
    /// index this node doesn't know is only accepted if it answers a
    /// challenge; otherwise it's challenged and dropped.
    fn check_fresh(&mut self, frame: &Frame<'_>, hue: HueId, now: Instant) -> Result<()> {
        let key = (frame.header.sender, hue);
        let Trailer { index, counter, .. } = frame.trailer;
        if let Some(peer) = self.peers.get_mut(&key).filter(|peer| peer.index == index) {
            if !peer.window.accept(counter) {
                return Err(Error::Replay);
            }
            peer.last_heard = now;
            return Ok(());
        }

        let challenge = self.challenges.get(&key).filter(|c| c.index == index);
        let answered = challenge.is_some_and(|challenge| {
            control_tlvs(frame).any(|tlv| {
                tlv == Tlv::ChallengeReply {
                    nonce: challenge.nonce,
                }
            })
        });
        if !answered {
            // Let it check this node too, then check it. The frame could be a
            // replay, so don't answer more often than a real neighbor needs.
            let answered_recently = self
                .challenge_answers
                .get(&key)
                .is_some_and(|&at| now < at + CHALLENGE_RETRY);
            if !answered_recently {
                for tlv in control_tlvs(frame) {
                    if let Tlv::ChallengeRequest { nonce } = tlv {
                        self.send_control(hue, key.0, &[Tlv::ChallengeReply { nonce }]);
                        self.challenge_answers.insert(key, now);
                    }
                }
            }
            self.challenge(key, index, now);
            return Err(Error::Unverified);
        }

        self.challenges.remove(&key);
        // A new boot index means the neighbor restarted, so its hello count
        // restarted too. Measure the link afresh.
        if self.neighbors.remove(key.0, key.1) {
            self.stale = true;
        }
        self.peers.insert(
            key,
            Peer {
                index,
                window: ReplayWindow::new(counter),
                last_heard: now,
            },
        );
        // A new or restarted neighbor: say hello and send it routes straight
        // away. It may not have verified this node yet and could drop them,
        // so also ask for its routes, which it sends once it has.
        if let Some(state) = self.hues.iter_mut().find(|h| h.info.id == hue) {
            state.next_hello = now;
            state.next_update = now;
        }
        let request = Tlv::RouteRequest {
            dest: NodeId::BROADCAST,
        };
        self.send_control(hue, key.0, &[request]);
        Ok(())
    }

    fn challenge(&mut self, (neighbor, hue): (NodeId, HueId), index: u32, now: Instant) {
        let recently_sent = self
            .challenges
            .get(&(neighbor, hue))
            .is_some_and(|sent| sent.index == index && now < sent.sent + CHALLENGE_RETRY);
        if recently_sent {
            return;
        }
        let nonce = self.random.next_u64();
        self.challenges.insert(
            (neighbor, hue),
            Challenge {
                index,
                nonce,
                sent: now,
            },
        );
        self.send_control(hue, neighbor, &[Tlv::ChallengeRequest { nonce }]);
    }

    /// Queues `payload` for `dst`.
    ///
    /// Sending to [`NodeId::BROADCAST`] reaches direct neighbors on every hue
    /// the payload fits on.
    //
    // TODO: mesh-wide broadcast, like a Meshtastic channel.
    pub fn send(&mut self, dst: NodeId, payload: &[u8]) -> Result<()> {
        let len = DATA_OVERHEAD + payload.len();
        if dst.is_broadcast() {
            let hues: Vec<HueId> = self
                .hues
                .iter()
                .filter(|h| len <= usize::from(h.info.mtu))
                .map(|h| h.info.id)
                .collect();
            if hues.is_empty() {
                return Err(Error::PayloadTooLarge);
            }
            let header = DataHeader {
                origin: self.id,
                dst,
                ttl: 1,
            };
            for hue in hues {
                self.queue_data(hue, NodeId::BROADCAST, &header, payload);
            }
            return Ok(());
        }

        let route = *self.selected.get(&dst).ok_or(Error::NoRoute(dst))?;
        if len > self.mtu(route.hue) {
            return Err(Error::PayloadTooLarge);
        }
        let header = DataHeader {
            origin: self.id,
            dst,
            ttl: self.config.default_ttl,
        };
        self.queue_data(route.hue, route.next_hop, &header, payload);
        Ok(())
    }

    /// Runs timed work: expiry, route selection, hellos and updates.
    pub fn poll(&mut self, now: Instant) {
        self.stale |= self.neighbors.expire(now);
        self.stale |= self.routes.expire(now);
        self.sources.expire(now, self.config.source_gc);
        let hold = self.config.request_hold;
        self.recent_requests
            .retain(|_, &mut (_, sent)| now < sent + hold);
        let peer_timeout = self.config.peer_timeout;
        self.peers
            .retain(|_, peer| now < peer.last_heard + peer_timeout);
        self.challenges
            .retain(|_, challenge| now < challenge.sent + peer_timeout);
        self.challenge_answers
            .retain(|_, &mut at| now < at + CHALLENGE_RETRY);
        // Link costs drift as hellos arrive or go missing, so reselect every hello round.
        self.stale |= self.hues.iter().any(|h| now >= h.next_hello);

        if self.stale {
            self.reselect(now);
        }
        for i in 0..self.hues.len() {
            self.send_scheduled(i, now);
        }
        self.triggered.clear();
        self.requests.clear();
    }

    pub fn poll_transmit(&mut self) -> Option<Transmit> {
        self.outbox.pop_front()
    }

    pub fn poll_delivery(&mut self) -> Option<Delivery> {
        self.deliveries.pop_front()
    }

    fn is_for_me(&self, next_hop: NodeId) -> bool {
        next_hop == self.id || next_hop.is_broadcast()
    }

    fn handle_tlv(&mut self, from: NodeId, hue: HueId, tlv: Tlv, now: Instant) {
        match tlv {
            Tlv::Hello { seqno, interval } => {
                self.neighbors.record_hello(from, hue, seqno, interval, now);
                self.stale = true;
            }
            Tlv::Ihu {
                neighbor,
                rxcost,
                interval,
            } => {
                if neighbor == self.id {
                    self.neighbors.record_ihu(from, hue, rxcost, interval, now);
                    self.stale = true;
                }
            }
            Tlv::Update {
                dest,
                seqno,
                metric,
                interval,
            } => self.handle_update(from, hue, dest, seqno, metric, interval, now),
            Tlv::RouteRequest { dest } => {
                if dest.is_broadcast() {
                    if let Some(state) = self.hues.iter_mut().find(|h| h.info.id == hue) {
                        state.next_update = now;
                    }
                } else {
                    self.triggered.insert(dest);
                }
            }
            Tlv::SeqnoRequest {
                dest,
                seqno,
                hop_count,
            } => self.handle_seqno_request(from, dest, seqno, hop_count, now),
            Tlv::ChallengeRequest { nonce } => {
                self.send_control(hue, from, &[Tlv::ChallengeReply { nonce }]);
            }
            // Only meaningful from an unverified neighbor; see `check_fresh`.
            Tlv::ChallengeReply { .. } => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_update(
        &mut self,
        from: NodeId,
        hue: HueId,
        dest: NodeId,
        seqno: u16,
        metric: u32,
        interval: Duration,
        now: Instant,
    ) {
        if dest == self.id {
            return;
        }
        let feasible = self.sources.is_feasible(dest, seqno, metric);
        let entry = RouteEntry {
            seqno,
            advertised_metric: metric,
            expires: now + interval * 7 / 2,
        };
        if self.routes.apply_update(dest, from, hue, entry, feasible) {
            self.stale = true;
        }
        // A route this node can't use yet, to a destination it has no route
        // to: ask the neighbor for a newer seqno, which would make it usable.
        if !feasible && !self.selected.contains_key(&dest) {
            let wanted = self
                .sources
                .seqno(dest)
                .map_or(seqno, |s| s.wrapping_add(1));
            if self.should_request(dest, wanted, now) {
                let request = Tlv::SeqnoRequest {
                    dest,
                    seqno: wanted,
                    hop_count: self.config.default_ttl,
                };
                self.send_control(hue, from, &[request]);
            }
        }
    }

    fn handle_seqno_request(
        &mut self,
        from: NodeId,
        dest: NodeId,
        seqno: u16,
        hop_count: u8,
        now: Instant,
    ) {
        if dest == self.id {
            if seqno_newer(seqno, self.seqno) {
                self.seqno = seqno;
            }
            self.triggered.insert(dest);
            return;
        }
        if self
            .selected
            .get(&dest)
            .is_some_and(|route| !seqno_newer(seqno, route.seqno))
        {
            // Already have a new enough route; advertising it satisfies the request.
            self.triggered.insert(dest);
            return;
        }
        if hop_count <= 1 || !self.should_request(dest, seqno, now) {
            return;
        }
        // Pass the request on towards the destination, but not back where it came from.
        let towards = self
            .routes
            .for_dest(dest)
            .filter(|&(neighbor, _, entry)| neighbor != from && entry.advertised_metric != INFINITY)
            .min_by_key(|&(_, _, entry)| entry.advertised_metric)
            .map(|(neighbor, hue, _)| (neighbor, hue));
        if let Some((neighbor, hue)) = towards {
            let request = Tlv::SeqnoRequest {
                dest,
                seqno,
                hop_count: hop_count - 1,
            };
            self.send_control(hue, neighbor, &[request]);
        }
    }

    /// Whether to send a seqno request now, rather than rely on one sent recently.
    fn should_request(&mut self, dest: NodeId, seqno: u16, now: Instant) -> bool {
        if let Some(&(sent_seqno, _)) = self.recent_requests.get(&dest) {
            if !seqno_newer(seqno, sent_seqno) {
                return false;
            }
        }
        self.recent_requests.insert(dest, (seqno, now));
        true
    }

    /// Selects routes again and works out which changes to tell neighbors about.
    fn reselect(&mut self, now: Instant) {
        self.stale = false;
        let costs = self.link_costs(now);
        let selected = routing::select(&self.routes, &self.sources, |neighbor, hue| {
            costs.get(&(neighbor, hue)).copied()
        });

        for (dest, route) in &selected {
            let worth_sending = match self.selected.get(dest) {
                None => true,
                Some(old) => {
                    old.seqno != route.seqno || old.metric.abs_diff(route.metric) > old.metric / 4
                }
            };
            if worth_sending {
                self.triggered.insert(*dest);
            }
        }
        let lost: Vec<Route> = self
            .selected
            .values()
            .filter(|old| !selected.contains_key(&old.dest))
            .copied()
            .collect();
        for old in lost {
            // Lost with no feasible alternative: withdraw it, and ask for a newer
            // seqno in case an unfeasible route could take over.
            let dest = old.dest;
            self.triggered.insert(dest);
            let seqno = old.seqno.wrapping_add(1);
            if self.should_request(dest, seqno, now) {
                self.requests.push(Tlv::SeqnoRequest {
                    dest,
                    seqno,
                    hop_count: self.config.default_ttl,
                });
            }
        }
        self.selected = selected;
    }

    /// The cost of each usable link: its expected transmission count times the
    /// time one transmission takes on its hue, in microseconds.
    fn link_costs(&self, now: Instant) -> BTreeMap<(NodeId, HueId), u32> {
        self.neighbors
            .iter()
            .filter_map(|(node, hue, neighbor)| {
                let info = self.hue(hue)?;
                let etx = neighbor.etx(now, info.link_model)?;
                let airtime = info.airtime_us();
                let cost = u64::from(etx) * u64::from(airtime) / 256;
                Some(((node, hue), cost.clamp(1, u64::from(INFINITY - 1)) as u32))
            })
            .collect()
    }

    /// Sends whatever is due on one hue, packed into as few frames as fit.
    fn send_scheduled(&mut self, index: usize, now: Instant) {
        let info = self.hues[index].info;
        let mut tlvs = Vec::new();

        if now >= self.hues[index].next_hello {
            let state = &mut self.hues[index];
            state.hello_seqno = state.hello_seqno.wrapping_add(1);
            state.next_hello = now + info.hello_interval;
            tlvs.push(Tlv::Hello {
                seqno: state.hello_seqno,
                interval: info.hello_interval,
            });
            if !state.announced {
                state.announced = true;
                tlvs.push(Tlv::RouteRequest {
                    dest: NodeId::BROADCAST,
                });
            }
            // TODO: send IHUs every few hellos rather than every one on slow hues.
            for (neighbor, link) in self.neighbors.on_hue(info.id) {
                let rxcost = link.rxcost(now, info.link_model);
                if rxcost != neighbor::INFINITY {
                    tlvs.push(Tlv::Ihu {
                        neighbor,
                        rxcost,
                        interval: info.hello_interval,
                    });
                }
            }
        }

        let mut dests = self.triggered.clone();
        if now >= self.hues[index].next_update {
            self.hues[index].next_update = now + info.update_interval;
            dests.insert(self.id);
            dests.extend(self.selected.keys());
        }
        for dest in dests {
            tlvs.push(self.update_for(dest, info.update_interval, now));
        }
        tlvs.extend_from_slice(&self.requests);

        if !tlvs.is_empty() {
            self.send_control(info.id, NodeId::BROADCAST, &tlvs);
        }
    }

    /// An update advertising this node's route to `dest`, or withdrawing it if
    /// there is none. Records the advertisement in the source table.
    fn update_for(&mut self, dest: NodeId, interval: Duration, now: Instant) -> Tlv {
        let (seqno, metric) = if dest == self.id {
            (self.seqno, 0)
        } else if let Some(route) = self.selected.get(&dest) {
            self.sources
                .record_advertised(dest, route.seqno, route.metric, now);
            (route.seqno, route.metric)
        } else {
            (self.sources.seqno(dest).unwrap_or(0), INFINITY)
        };
        Tlv::Update {
            dest,
            seqno,
            metric,
            interval,
        }
    }

    fn send_control(&mut self, hue: HueId, next_hop: NodeId, tlvs: &[Tlv]) {
        let mtu = self.mtu(hue);
        let mut frame = Vec::new();
        for tlv in tlvs {
            if frame.len() > HEADER_LEN && frame.len() + tlv.encoded_len() + TRAILER_LEN > mtu {
                let full = core::mem::take(&mut frame);
                self.transmit(hue, next_hop, full);
            }
            if frame.is_empty() {
                self.header(FrameKind::Control, next_hop).encode(&mut frame);
            }
            tlv.encode(&mut frame);
        }
        if !frame.is_empty() {
            self.transmit(hue, next_hop, frame);
        }
    }

    // TODO: count drops (TTL, no route, too large) for diagnostics.
    fn forward(&mut self, mut header: DataHeader, payload: &[u8]) {
        if header.ttl <= 1 {
            return;
        }
        let Some(route) = self.selected.get(&header.dst).copied() else {
            return;
        };
        if DATA_OVERHEAD + payload.len() > self.mtu(route.hue) {
            return;
        }
        header.ttl -= 1;
        self.queue_data(route.hue, route.next_hop, &header, payload);
    }

    fn queue_data(&mut self, hue: HueId, next_hop: NodeId, data: &DataHeader, payload: &[u8]) {
        let mut frame = Vec::with_capacity(DATA_OVERHEAD + payload.len());
        self.header(FrameKind::Data, next_hop).encode(&mut frame);
        data.encode(&mut frame);
        frame.extend_from_slice(payload);
        self.transmit(hue, next_hop, frame);
    }

    fn header(&self, kind: FrameKind, next_hop: NodeId) -> Header {
        Header {
            kind,
            key_id: self.keys.current_id(),
            sender: self.id,
            next_hop,
        }
    }

    /// Seals a frame with its trailer and queues it.
    fn transmit(&mut self, hue: HueId, next_hop: NodeId, mut frame: Vec<u8>) {
        if self.counter == u32::MAX {
            // Out of counters: start over with a new boot index, which
            // neighbors will challenge.
            self.boot_index = self.random.next_u32();
            self.counter = 0;
        }
        self.counter += 1;
        Trailer::encode_signed_part(self.boot_index, self.counter, &mut frame);
        let tag = self.keys.tag(&frame);
        frame.extend_from_slice(&tag);
        self.outbox.push_back(Transmit {
            hue,
            next_hop,
            frame,
        });
    }

    fn mtu(&self, hue: HueId) -> usize {
        self.hue(hue).map_or(0, |h| usize::from(h.mtu))
    }
}

/// The well-formed TLVs in a frame, or none for a data frame.
fn control_tlvs<'a>(frame: &Frame<'a>) -> impl Iterator<Item = Tlv> + 'a {
    let tlvs = match &frame.body {
        Body::Control(tlvs) => Some(tlvs.clone()),
        Body::Data { .. } => None,
    };
    tlvs.into_iter().flatten().filter_map(Result::ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MeshKey;
    use crate::hue::HueKind;
    use alloc::vec;

    const WIFI: HueId = HueId(0);
    const SUB_GHZ: HueId = HueId(1);
    const ETHERNET: HueId = HueId(2);

    fn wifi() -> HueInfo {
        HueInfo::new(WIFI, HueKind::Wifi { freq_mhz: 2437 }, 1400, 20_000_000)
    }

    /// A 915 MHz FSK backbone link: slower than Wi-Fi, but not LoRa-slow.
    fn sub_ghz() -> HueInfo {
        HueInfo::new(SUB_GHZ, HueKind::Fsk { freq_khz: 915_000 }, 255, 250_000)
    }

    /// Gigabit Ethernet or fiber.
    fn ethernet() -> HueInfo {
        HueInfo::new(ETHERNET, HueKind::Ethernet, 1500, 1_000_000_000)
    }

    fn mesh_key() -> MeshKey {
        MeshKey::from_bytes([0x42; 32])
    }

    fn router(id: u64, hues: &[HueInfo]) -> Router {
        router_with(id, KeyRing::new(&mesh_key()), [id as u8; 32], hues)
    }

    fn router_with(id: u64, keys: KeyRing, seed: [u8; 32], hues: &[HueInfo]) -> Router {
        let mut router = Router::new(NodeId::from_u64(id), keys, Config::default(), seed);
        for &hue in hues {
            router.add_hue(hue);
        }
        router
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// Runs `routers` from `from` for `duration` in 100 ms steps, passing each
    /// frame to every router that shares a link with the sender on that hue.
    /// `links` lists (router index, router index, hue). Returns the end time.
    fn simulate(
        routers: &mut [Router],
        links: &[(usize, usize, HueId)],
        from: Instant,
        duration: Duration,
    ) -> Instant {
        let until = from + duration;
        let mut now = from;
        while now < until {
            for i in 0..routers.len() {
                routers[i].poll(now);
                while let Some(tx) = routers[i].poll_transmit() {
                    for &(a, b, hue) in links {
                        let peer = match (a == i, b == i) {
                            (true, _) => b,
                            (_, true) => a,
                            _ => continue,
                        };
                        if hue == tx.hue {
                            match routers[peer].handle_frame(hue, &tx.frame, now) {
                                // Neighbors meeting for the first time challenge each other.
                                Ok(_) | Err(Error::Unverified) => {}
                                Err(err) => panic!("router {peer} rejected a frame: {err}"),
                            }
                        }
                    }
                }
            }
            now = now + Duration::from_millis(100);
        }
        until
    }

    fn next_hop(router: &Router, dest: &Router) -> Option<(NodeId, HueId)> {
        router
            .route_to(dest.id())
            .map(|route| (route.next_hop, route.hue))
    }

    #[test]
    fn routes_across_hues() {
        // A and B share Wi-Fi; B and C share a sub-GHz link. A reaches C through B.
        let mut routers = [
            router(1, &[wifi()]),
            router(2, &[wifi(), sub_ghz()]),
            router(3, &[sub_ghz()]),
        ];
        let links = [(0, 1, WIFI), (1, 2, SUB_GHZ)];

        let now = simulate(&mut routers, &links, Instant::default(), secs(30));
        let [a, b, c] = &routers;
        assert_eq!(next_hop(a, c), Some((b.id(), WIFI)));
        assert_eq!(next_hop(c, a), Some((b.id(), SUB_GHZ)));

        let c_id = c.id();
        routers[0].send(c_id, b"hello across two hues").unwrap();
        simulate(&mut routers, &links, now, Duration::from_millis(200));
        assert_eq!(
            routers[2].poll_delivery(),
            Some(Delivery {
                src: routers[0].id(),
                payload: b"hello across two hues".to_vec(),
            })
        );
    }

    #[test]
    fn falls_back_to_the_slower_hue() {
        // A and B share both hues and should prefer Wi-Fi until it goes away.
        let mut routers = [
            router(1, &[wifi(), sub_ghz()]),
            router(2, &[wifi(), sub_ghz()]),
        ];

        let both = [(0, 1, WIFI), (0, 1, SUB_GHZ)];
        let now = simulate(&mut routers, &both, Instant::default(), secs(30));
        assert_eq!(next_hop(&routers[0], &routers[1]).unwrap().1, WIFI);

        let sub_ghz_only = [(0, 1, SUB_GHZ)];
        simulate(&mut routers, &sub_ghz_only, now, secs(30));
        assert_eq!(next_hop(&routers[0], &routers[1]).unwrap().1, SUB_GHZ);
    }

    #[test]
    fn prefers_wired_links_and_falls_back_to_radio() {
        // A, B and C are cabled in a line; A and C can also hear each other on Wi-Fi.
        let mut routers = [
            router(1, &[ethernet(), wifi()]),
            router(2, &[ethernet()]),
            router(3, &[ethernet(), wifi()]),
        ];
        let cabled = [(0, 1, ETHERNET), (1, 2, ETHERNET), (0, 2, WIFI)];
        let now = simulate(&mut routers, &cabled, Instant::default(), secs(30));
        let (b, c) = (routers[1].id(), routers[2].id());
        // Two cabled hops (cost 1 each) beat one Wi-Fi hop (cost 40).
        let route = *routers[0].route_to(c).unwrap();
        assert_eq!((route.next_hop, route.hue, route.metric), (b, ETHERNET, 2));

        // Unplug B-C. The cut is noticed after two missed hellos, and A falls
        // back to Wi-Fi.
        let unplugged = [(0, 1, ETHERNET), (0, 2, WIFI)];
        simulate(&mut routers, &unplugged, now, secs(15));
        assert_eq!(next_hop(&routers[0], &routers[2]), Some((c, WIFI)));
        assert_no_loops(&routers, c);
    }

    #[test]
    fn transmits_name_their_next_hop() {
        let mut routers = [router(1, &[ethernet()]), router(2, &[ethernet()])];
        let links = [(0, 1, ETHERNET)];
        simulate(&mut routers, &links, Instant::default(), secs(10));
        let b = routers[1].id();

        routers[0].send(b, b"unicast").unwrap();
        let tx = routers[0].poll_transmit().unwrap();
        assert_eq!(tx.next_hop, b);

        let now = Instant::from_millis(60_000);
        routers[0].poll(now);
        let hello = routers[0].poll_transmit().unwrap();
        assert_eq!(hello.next_hop, NodeId::BROADCAST);
        // The receiver names the sender, so platforms can learn its address.
        assert_eq!(
            routers[1].handle_frame(ETHERNET, &hello.frame, now),
            Ok(Some(routers[0].id()))
        );
    }

    #[test]
    fn reroutes_around_a_failed_link_without_loops() {
        // A square of Wi-Fi links: A-B, B-C, C-D, D-A.
        let mut routers = [
            router(1, &[wifi()]),
            router(2, &[wifi()]),
            router(3, &[wifi()]),
            router(4, &[wifi()]),
        ];
        let square = [(0, 1, WIFI), (1, 2, WIFI), (2, 3, WIFI), (3, 0, WIFI)];
        let now = simulate(&mut routers, &square, Instant::default(), secs(30));
        let ids: Vec<NodeId> = routers.iter().map(Router::id).collect();
        let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
        let via = |r: &Router| r.route_to(c).map(|route| route.next_hop);
        // Two equal paths from A to C; either is fine.
        assert!(matches!(via(&routers[0]), Some(n) if n == b || n == d));

        // B-C fails. B's only way to C is now back through A, which B can't
        // use until C issues a newer seqno, so this also tests seqno requests.
        let broken = [(0, 1, WIFI), (2, 3, WIFI), (3, 0, WIFI)];
        simulate(&mut routers, &broken, now, secs(60));
        assert_eq!(via(&routers[0]), Some(d));
        assert_eq!(via(&routers[1]), Some(a));
        assert_eq!(via(&routers[3]), Some(c));
        // B's new route carries a seqno C raised in answer to a request.
        assert!(routers[1].route_to(c).unwrap().seqno > 0);
        assert_no_loops(&routers, c);
    }

    /// Follows next hops towards `dest` from every router, failing if any path revisits a node.
    fn assert_no_loops(routers: &[Router], dest: NodeId) {
        let by_id = |id: NodeId| routers.iter().find(|r| r.id() == id).unwrap();
        for start in routers {
            let mut at = start;
            let mut visited = vec![at.id()];
            while at.id() != dest {
                let Some(route) = at.route_to(dest) else {
                    break;
                };
                assert!(!visited.contains(&route.next_hop), "loop: {visited:?}");
                visited.push(route.next_hop);
                at = by_id(route.next_hop);
            }
        }
    }

    #[test]
    fn broadcast_reaches_neighbors_on_every_hue() {
        let mut routers = [
            router(1, &[wifi(), sub_ghz()]),
            router(2, &[wifi()]),
            router(3, &[sub_ghz()]),
        ];
        let links = [(0, 1, WIFI), (0, 2, SUB_GHZ)];
        let now = simulate(&mut routers, &links, Instant::default(), secs(5));

        routers[0].send(NodeId::BROADCAST, b"anyone?").unwrap();
        simulate(&mut routers, &links, now, Duration::from_millis(100));
        for peer in &mut routers[1..] {
            assert_eq!(peer.poll_delivery().unwrap().payload, b"anyone?");
        }
    }

    #[test]
    fn large_tables_split_across_frames() {
        let mut router = router(1, &[sub_ghz()]);
        let many = vec![
            Tlv::Update {
                dest: NodeId::from_u64(2),
                seqno: 0,
                metric: 0,
                interval: secs(40),
            };
            50
        ];
        router.send_control(SUB_GHZ, NodeId::BROADCAST, &many);

        let mut count = 0;
        while let Some(tx) = router.poll_transmit() {
            assert!(tx.frame.len() <= 255);
            count += control_tlvs(&Frame::decode(&tx.frame).unwrap()).count();
        }
        assert_eq!(count, 50);
    }

    #[test]
    fn rejects_unknown_hues_and_oversized_payloads() {
        let mut a = router(1, &[sub_ghz()]);
        assert_eq!(
            a.handle_frame(WIFI, &[], Instant::default()),
            Err(Error::UnknownHue(WIFI))
        );
        assert_eq!(
            a.send(NodeId::BROADCAST, &[0; 300]),
            Err(Error::PayloadTooLarge)
        );
        assert_eq!(
            a.send(NodeId::from_u64(9), b"hi"),
            Err(Error::NoRoute(NodeId::from_u64(9)))
        );
    }

    /// The next frame `router` sends after polling at `now`.
    fn next_frame(router: &mut Router, now: Instant) -> Vec<u8> {
        router.poll(now);
        let frame = router.poll_transmit().expect("router sent nothing").frame;
        while router.poll_transmit().is_some() {}
        frame
    }

    #[test]
    fn frames_without_the_mesh_key_are_rejected() {
        let mut routers = [router(1, &[wifi()]), router(2, &[wifi()])];
        let now = simulate(&mut routers, &[(0, 1, WIFI)], Instant::default(), secs(10));

        // An outsider with a different key.
        let other_key = KeyRing::new(&MeshKey::from_bytes([0x99; 32]));
        let mut outsider = router_with(3, other_key, [3; 32], &[wifi()]);
        let forged = next_frame(&mut outsider, now);
        assert_eq!(
            routers[0].handle_frame(WIFI, &forged, now),
            Err(Error::BadTag)
        );

        // A member's frame, altered in flight.
        let mut tampered = next_frame(&mut routers[1], now + secs(10));
        tampered[HEADER_LEN] ^= 1;
        assert_eq!(
            routers[0].handle_frame(WIFI, &tampered, now + secs(10)),
            Err(Error::BadTag)
        );
        assert!(
            routers[0]
                .neighbors()
                .iter()
                .all(|(n, _, _)| n == routers[1].id())
        );
    }

    #[test]
    fn replayed_frames_are_rejected() {
        let mut routers = [router(1, &[wifi()]), router(2, &[wifi()])];
        let now = simulate(&mut routers, &[(0, 1, WIFI)], Instant::default(), secs(10));

        let frame = next_frame(&mut routers[1], now + secs(10));
        let b = routers[1].id();
        assert_eq!(routers[0].handle_frame(WIFI, &frame, now), Ok(Some(b)));
        assert_eq!(
            routers[0].handle_frame(WIFI, &frame, now),
            Err(Error::Replay)
        );
    }

    #[test]
    fn restarted_neighbors_must_pass_a_challenge() {
        // A line: A - B - C.
        let mut routers = [
            router(1, &[wifi()]),
            router(2, &[wifi()]),
            router(3, &[wifi()]),
        ];
        let links = [(0, 1, WIFI), (1, 2, WIFI)];
        let now = simulate(&mut routers, &links, Instant::default(), secs(30));
        let (a, b, c) = (routers[0].id(), routers[1].id(), routers[2].id());
        let recorded = next_frame(&mut routers[1], now + secs(10));

        // B restarts: same keys, fresh boot, empty tables.
        routers[1] = router_with(2, KeyRing::new(&mesh_key()), [0xbb; 32], &[wifi()]);
        let now = simulate(&mut routers, &links, now, secs(30));
        assert_eq!(next_hop(&routers[0], &routers[2]), Some((b, WIFI)));
        assert_eq!(next_hop(&routers[2], &routers[0]), Some((b, WIFI)));
        assert!(routers[1].route_to(a).is_some() && routers[1].route_to(c).is_some());

        // A frame recorded before the restart carries the old boot index, and
        // can't answer the challenge it provokes.
        assert_eq!(
            routers[0].handle_frame(WIFI, &recorded, now),
            Err(Error::Unverified)
        );
    }

    #[test]
    fn replayed_challenges_are_not_answered_every_time() {
        let (mut a, mut b) = (router(1, &[wifi()]), router(2, &[wifi()]));
        let now = Instant::default();

        // A's hello reaches B, which doesn't know A yet and challenges it.
        let hello = next_frame(&mut a, now);
        assert_eq!(b.handle_frame(WIFI, &hello, now), Err(Error::Unverified));
        let challenge = b.poll_transmit().unwrap().frame;

        // A answers and challenges B back...
        assert_eq!(
            a.handle_frame(WIFI, &challenge, now),
            Err(Error::Unverified)
        );
        let mut sent = 0;
        while a.poll_transmit().is_some() {
            sent += 1;
        }
        assert_eq!(sent, 2);

        // ...but a recording of B's challenge, replayed straight away, gets nothing.
        assert_eq!(
            a.handle_frame(WIFI, &challenge, now),
            Err(Error::Unverified)
        );
        assert!(a.poll_transmit().is_none());
    }

    #[test]
    fn keys_can_change_without_downtime() {
        let (old, new) = (mesh_key(), MeshKey::from_bytes([0x77; 32]));
        // Mid-change: A already sends with the new key, B still with the old;
        // each accepts both.
        let mut a_keys = KeyRing::new(&new);
        a_keys.accept(&old);
        let mut b_keys = KeyRing::new(&old);
        b_keys.accept(&new);
        let mut routers = [
            router_with(1, a_keys, [1; 32], &[wifi()]),
            router_with(2, b_keys, [2; 32], &[wifi()]),
        ];

        simulate(&mut routers, &[(0, 1, WIFI)], Instant::default(), secs(10));
        assert!(routers[0].route_to(routers[1].id()).is_some());
        assert!(routers[1].route_to(routers[0].id()).is_some());
    }
}
