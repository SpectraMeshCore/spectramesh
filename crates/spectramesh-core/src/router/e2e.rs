//! The router's end-to-end side: finding other nodes' keys, setting up
//! sessions, and encrypting unicast data. See [`session`](crate::session).

use alloc::vec::Vec;
use core::time::Duration;

use super::{Delivery, Router};
use crate::error::{Error, Result};
use crate::hue::HueId;
use crate::identity::PublicIdentity;
use crate::node::NodeId;
use crate::packet::{DataHeader, Tlv};
use crate::session::{self, Content, Initiation, Message};
use crate::time::Instant;

/// How many other nodes' keys to remember.
const IDENTITY_CACHE_LEN: usize = 256;
/// How many payloads to hold per destination while a session is set up.
const WAITING_LEN: usize = 16;
/// How long to wait for a handshake response or an identity before asking again.
const RETRY_AFTER: Duration = Duration::from_secs(5);
/// How many times to ask before giving up and dropping waiting data.
const ATTEMPTS: u8 = 3;
/// Sessions kept per peer, including ones being replaced.
const SESSIONS_PER_PEER: usize = 4;

/// An identity request this node is waiting on an answer to.
#[derive(Clone, Debug)]
pub(super) struct IdentityRequest {
    created: Instant,
    /// Neighbors that asked this node, to pass the answer back to.
    requesters: Vec<(NodeId, HueId)>,
    /// Whether this node itself wants the keys.
    local: bool,
    sent: Option<Instant>,
    attempts: u8,
}

impl Router {
    /// Encrypts `payload` for `dst` and sends it, or holds it while a
    /// session is set up.
    pub(super) fn send_unicast(&mut self, dst: NodeId, payload: &[u8], now: Instant) -> Result<()> {
        if let Some(session) = self.current_session(dst, now) {
            let message = session.seal(Content::Data(payload), now)?;
            let rekey = session.needs_rekey(now);
            self.send_end_to_end(dst, &message);
            if rekey {
                self.connect(dst, now);
            }
            return Ok(());
        }
        let waiting = self.waiting.entry(dst).or_default();
        if waiting.len() == WAITING_LEN {
            waiting.pop_front();
        }
        waiting.push_back(payload.to_vec());
        self.connect(dst, now);
        Ok(())
    }

    /// The usable session data for `peer` should be sent on, if any.
    fn current_session(&mut self, peer: NodeId, now: Instant) -> Option<&mut session::Session> {
        let index = *self.current_sessions.get(&peer)?;
        self.sessions
            .get_mut(&index)
            .filter(|s| s.confirmed && !s.is_expired(now))
    }

    /// Starts a handshake with `peer`, first finding its keys if necessary.
    fn connect(&mut self, peer: NodeId, now: Instant) {
        if self.initiations.contains_key(&peer) {
            return;
        }
        match self.identities.get_mut(&peer) {
            Some((public, used)) => {
                *used = now;
                let public = *public;
                self.initiate(public, 1, now);
            }
            None => self.request_identity(peer, None, self.config.default_ttl, now),
        }
    }

    fn initiate(&mut self, peer: PublicIdentity, attempts: u8, now: Instant) {
        let index = self.new_index();
        let seed = self.random.next_seed();
        match Initiation::start(&self.identity, peer, index, seed, now) {
            Ok((mut initiation, message)) => {
                initiation.attempts = attempts;
                let node = peer.node_id();
                self.send_end_to_end(node, &message);
                self.initiations.insert(node, initiation);
            }
            // Only fails on keys that aren't valid curve points.
            Err(_) => {
                self.identities.remove(&peer.node_id());
            }
        }
    }

    /// A random session index not already in use.
    fn new_index(&mut self) -> u32 {
        loop {
            let index = self.random.next_u32();
            let taken = index == 0
                || self.sessions.contains_key(&index)
                || self.initiations.values().any(|i| i.index == index);
            if !taken {
                return index;
            }
        }
    }

    /// Sends an end-to-end message to `dst` along its route. Dropped if
    /// there's no route; timers retry handshakes, and data is held until
    /// one completes.
    fn send_end_to_end(&mut self, dst: NodeId, message: &[u8]) {
        let Some(route) = self.selected.get(&dst).copied() else {
            return;
        };
        let header = DataHeader {
            origin: self.id,
            dst,
            ttl: self.config.default_ttl,
        };
        self.queue_data(route.hue, route.next_hop, &header, message);
    }

    /// Asks for `node`'s keys along its route. `requester` is the neighbor
    /// that asked this node, or `None` if this node wants them.
    fn request_identity(
        &mut self,
        node: NodeId,
        requester: Option<(NodeId, HueId)>,
        hop_count: u8,
        now: Instant,
    ) {
        let pending = self
            .identity_requests
            .entry(node)
            .or_insert_with(|| IdentityRequest {
                created: now,
                requesters: Vec::new(),
                local: false,
                sent: None,
                attempts: 0,
            });
        match requester {
            Some(neighbor) if !pending.requesters.contains(&neighbor) => {
                pending.requesters.push(neighbor)
            }
            Some(_) => {}
            None => pending.local = true,
        }
        if pending.sent.is_some_and(|sent| now < sent + RETRY_AFTER) {
            return;
        }
        let Some(route) = self.selected.get(&node).copied() else {
            return;
        };
        pending.sent = Some(now);
        pending.attempts += 1;
        let request = Tlv::IdentityRequest { node, hop_count };
        self.send_control(route.hue, route.next_hop, &[request]);
    }

    pub(super) fn handle_identity_request(
        &mut self,
        from: NodeId,
        hue: HueId,
        node: NodeId,
        hop_count: u8,
        now: Instant,
    ) {
        let known = if node == self.id {
            Some(*self.identity.public())
        } else {
            self.identities.get_mut(&node).map(|(public, used)| {
                *used = now;
                *public
            })
        };
        match known {
            Some(public) => self.send_control(hue, from, &[Tlv::Identity(public)]),
            None if hop_count > 1 => {
                self.request_identity(node, Some((from, hue)), hop_count - 1, now)
            }
            None => {}
        }
    }

    /// Keys only count if someone asked for them: they're cached, passed to
    /// the neighbors that asked, and used to start a handshake if this node
    /// asked. The keys are checked against the node ID by hashing them.
    pub(super) fn handle_identity(&mut self, public: PublicIdentity, now: Instant) {
        let node = public.node_id();
        let Some(pending) = self.identity_requests.remove(&node) else {
            return;
        };
        self.remember_identity(public, now);
        for (neighbor, hue) in pending.requesters {
            self.send_control(hue, neighbor, &[Tlv::Identity(public)]);
        }
        if pending.local {
            self.connect(node, now);
        }
    }

    pub(super) fn remember_identity(&mut self, public: PublicIdentity, now: Instant) {
        let node = public.node_id();
        if !self.identities.contains_key(&node) && self.identities.len() >= IDENTITY_CACHE_LEN {
            let oldest = self
                .identities
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(node, _)| *node);
            if let Some(oldest) = oldest {
                self.identities.remove(&oldest);
            }
        }
        self.identities.insert(node, (public, now));
    }

    /// Handles an end-to-end message addressed to this node.
    pub(super) fn handle_end_to_end(
        &mut self,
        origin: NodeId,
        payload: &[u8],
        now: Instant,
    ) -> Result<()> {
        match Message::decode(payload)? {
            Message::Init { sender, noise } => {
                let index = self.new_index();
                let seed = self.random.next_seed();
                let (session, response) =
                    session::respond(&self.identity, origin, sender, noise, index, seed, now)?;
                self.remember_identity(session.peer, now);
                self.limit_sessions(origin);
                self.sessions.insert(index, session);
                self.send_end_to_end(origin, &response);
            }
            Message::Response {
                sender,
                receiver,
                noise,
            } => {
                if self
                    .initiations
                    .get(&origin)
                    .is_none_or(|i| i.index != receiver)
                {
                    return Err(Error::NoSession);
                }
                let initiation = self.initiations.remove(&origin).expect("checked above");
                let session = initiation.complete(sender, noise, now)?;
                let index = session.index;
                self.limit_sessions(origin);
                self.sessions.insert(index, session);
                self.current_sessions.insert(origin, index);
                self.flush_waiting(origin, true, now);
            }
            Message::Transport {
                receiver,
                counter,
                ciphertext,
            } => {
                let session = self
                    .sessions
                    .get_mut(&receiver)
                    .filter(|s| s.peer.node_id() == origin && !s.is_expired(now))
                    .ok_or(Error::NoSession)?;
                let newly_confirmed = !session.confirmed;
                let data = session.open(counter, ciphertext, now)?;
                let peer = session.peer;
                if newly_confirmed {
                    // The initiator has used it, so this node can send on it too.
                    self.current_sessions.insert(origin, receiver);
                    self.flush_waiting(origin, false, now);
                }
                if let Some(payload) = data {
                    self.deliveries.push_back(Delivery {
                        src: origin,
                        payload,
                        sender_keys: Some(peer),
                    });
                }
            }
        }
        Ok(())
    }

    /// Sends data held for `peer` on its new session. An initiator with
    /// nothing to send sends a keepalive instead, so the responder can use
    /// the session too.
    fn flush_waiting(&mut self, peer: NodeId, keepalive_if_empty: bool, now: Instant) {
        let waiting = self.waiting.remove(&peer).unwrap_or_default();
        let Some(session) = self.current_session(peer, now) else {
            return;
        };
        let mut messages = Vec::new();
        if waiting.is_empty() && keepalive_if_empty {
            messages.extend(session.seal(Content::Keepalive, now));
        }
        for payload in &waiting {
            messages.extend(session.seal(Content::Data(payload), now));
        }
        for message in messages {
            self.send_end_to_end(peer, &message);
        }
    }

    /// Keeps the number of sessions with `peer` under the limit, dropping
    /// the oldest first.
    fn limit_sessions(&mut self, peer: NodeId) {
        let mut theirs: Vec<(Instant, u32)> = self
            .sessions
            .values()
            .filter(|s| s.peer.node_id() == peer)
            .map(|s| (s.created, s.index))
            .collect();
        theirs.sort_unstable();
        while theirs.len() >= SESSIONS_PER_PEER {
            let (_, index) = theirs.remove(0);
            self.sessions.remove(&index);
        }
    }

    /// Timed end-to-end work: expiry, retries, keepalives and rekeying.
    pub(super) fn poll_end_to_end(&mut self, now: Instant) {
        self.sessions.retain(|_, s| !s.is_expired(now));
        let sessions = &self.sessions;
        self.current_sessions
            .retain(|_, index| sessions.contains_key(index));

        // Handshakes that got no response: try again, then give up.
        let stale: Vec<NodeId> = self
            .initiations
            .iter()
            .filter(|(_, i)| now >= i.sent + RETRY_AFTER)
            .map(|(node, _)| *node)
            .collect();
        for node in stale {
            let initiation = self.initiations.remove(&node).expect("listed above");
            if initiation.attempts < ATTEMPTS {
                self.initiate(initiation.peer, initiation.attempts + 1, now);
            } else {
                // Unreachable for now. Its old session is no use either.
                self.waiting.remove(&node);
                self.current_sessions.remove(&node);
            }
        }

        // Identity requests that got no answer.
        let stale: Vec<NodeId> = self
            .identity_requests
            .iter()
            .filter(|(_, r)| r.sent.is_none_or(|sent| now >= sent + RETRY_AFTER))
            .map(|(node, _)| *node)
            .collect();
        for node in stale {
            let pending = &self.identity_requests[&node];
            let given_up = pending.attempts >= ATTEMPTS
                || now >= pending.created + RETRY_AFTER * ATTEMPTS.into();
            if given_up {
                let pending = self.identity_requests.remove(&node).expect("listed above");
                if pending.local {
                    self.waiting.remove(&node);
                }
            } else {
                self.request_identity(node, None, self.config.default_ttl, now);
                // Re-asking for the neighbors' sake doesn't mean this node wants the keys.
                if let Some(pending) = self.identity_requests.get_mut(&node) {
                    pending.local = self.waiting.contains_key(&node);
                }
            }
        }

        // Keepalives, and handshakes with peers that have gone quiet.
        // (Rekeying happens when there's data to send, so idle sessions
        // simply expire.)
        let mut keepalives = Vec::new();
        let mut reconnect = Vec::new();
        for (&peer, index) in &self.current_sessions {
            let Some(session) = self.sessions.get(index) else {
                continue;
            };
            if session.keepalive_due(now) {
                keepalives.push(peer);
            }
            if session.peer_silent(now) {
                reconnect.push(peer);
            }
        }
        for peer in keepalives {
            let message = self
                .current_session(peer, now)
                .and_then(|session| session.seal(Content::Keepalive, now).ok());
            if let Some(message) = message {
                self.send_end_to_end(peer, &message);
            }
        }
        // Data still waiting, perhaps after a failed handshake, needs one too.
        reconnect.extend(self.waiting.keys().copied());
        for peer in reconnect {
            if !self.identity_requests.contains_key(&peer) {
                self.connect(peer, now);
            }
        }
    }
}
