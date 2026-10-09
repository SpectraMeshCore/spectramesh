//! The router's side of broadcasts: messages for neighbors, and channel
//! messages flooded across the mesh. See [`channel`](crate::channel).

use alloc::vec::Vec;
use core::time::Duration;

use super::{Delivery, Router};
use crate::channel::{self, CHANNEL_OVERHEAD, ChannelId, ChannelKey, ChannelMessage};
use crate::error::{Error, Result};
use crate::fragment::MAX_DATAGRAM;
use crate::node::NodeId;
use crate::packet::{DATA_HEADER_LEN, DataHeader};
use crate::time::Instant;

/// How long to remember a channel message, so copies arriving by other
/// paths aren't delivered or relayed again.
const SEEN_FOR: Duration = Duration::from_secs(60);
/// How many recent channel messages to remember.
const SEEN_LEN: usize = 1024;

impl Router {
    /// Joins a channel: this node will read its messages, and can send on it.
    /// (It relays every channel's messages either way.)
    pub fn join_channel(&mut self, key: ChannelKey) {
        let id = key.id();
        self.channels.retain(|joined| joined.id() != id);
        self.channels.push(key);
    }

    pub fn leave_channel(&mut self, id: ChannelId) {
        self.channels.retain(|joined| joined.id() != id);
    }

    /// The channels this node has joined.
    pub fn channels(&self) -> impl Iterator<Item = ChannelId> + '_ {
        self.channels.iter().map(ChannelKey::id)
    }

    /// Sends `payload` to everyone on channel `id`, across the whole mesh.
    pub fn send_to_channel(&mut self, id: ChannelId, payload: &[u8], now: Instant) -> Result<()> {
        if DATA_HEADER_LEN + CHANNEL_OVERHEAD + payload.len() > MAX_DATAGRAM {
            return Err(Error::PayloadTooLarge);
        }
        let key = self
            .channels
            .iter()
            .find(|joined| joined.id() == id)
            .ok_or(Error::UnknownChannel)?;
        let message_id = self.random.next_u32();
        let mut nonce = [0u8; 24];
        for chunk in nonce.chunks_exact_mut(8) {
            chunk.copy_from_slice(&self.random.next_u64().to_be_bytes());
        }
        let message = key.seal(self.id, message_id, nonce, payload);
        // Copies flooded back to this node are ignored.
        self.remember_broadcast(self.id, message_id, now);
        let header = DataHeader {
            origin: self.id,
            dst: NodeId::BROADCAST,
            ttl: self.config.default_ttl,
        };
        self.flood(&header, &message);
        Ok(())
    }

    /// Sends a broadcast payload on every hue.
    pub(super) fn flood(&mut self, header: &DataHeader, payload: &[u8]) {
        let hues: Vec<_> = self.hues.iter().map(|h| h.info.id).collect();
        for hue in hues {
            self.queue_data(hue, NodeId::BROADCAST, header, payload);
        }
    }

    /// Handles a broadcast data frame: delivers it if it's for this node,
    /// and passes channel messages on.
    pub(super) fn handle_broadcast(
        &mut self,
        header: DataHeader,
        payload: &[u8],
        now: Instant,
    ) -> Result<()> {
        let (&kind, body) = payload.split_first().ok_or(Error::Truncated)?;
        match kind {
            channel::NEIGHBORS => self.deliveries.push_back(Delivery {
                src: header.origin,
                payload: body.to_vec(),
                sender_keys: None,
                channel: None,
            }),
            channel::CHANNEL => {
                let message = ChannelMessage::decode(body)?;
                if !self.remember_broadcast(header.origin, message.message_id, now) {
                    return Ok(());
                }
                if header.ttl > 1 {
                    let relayed = DataHeader {
                        ttl: header.ttl - 1,
                        ..header
                    };
                    self.flood(&relayed, payload);
                }
                let opened = self.channels.iter().find_map(|key| {
                    key.open(header.origin, &message)
                        .map(|data| (key.id(), data))
                });
                if let Some((id, data)) = opened {
                    self.deliveries.push_back(Delivery {
                        src: header.origin,
                        payload: data,
                        sender_keys: None,
                        channel: Some(id),
                    });
                }
            }
            other => return Err(Error::UnknownMessage(other)),
        }
        Ok(())
    }

    /// Records a channel message as seen. Returns false if it already was.
    fn remember_broadcast(&mut self, origin: NodeId, message_id: u32, now: Instant) -> bool {
        if self.seen_broadcasts.contains_key(&(origin, message_id)) {
            return false;
        }
        if self.seen_broadcasts.len() >= SEEN_LEN {
            self.expire_broadcasts(now);
        }
        if self.seen_broadcasts.len() >= SEEN_LEN {
            let oldest = self
                .seen_broadcasts
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest {
                self.seen_broadcasts.remove(&oldest);
            }
        }
        self.seen_broadcasts.insert((origin, message_id), now);
        true
    }

    pub(super) fn expire_broadcasts(&mut self, now: Instant) {
        self.seen_broadcasts
            .retain(|_, &mut at| now < at + SEEN_FOR);
    }
}
