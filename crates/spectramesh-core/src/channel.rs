//! Channels: encrypted group messages that reach the whole mesh.
//!
//! A channel is a 32-byte key its members share, like a Meshtastic channel.
//! Channel messages are flooded: every node rebroadcasts each new message
//! once, so it reaches the whole mesh, and nodes holding the key read it.
//! Nodes without the key still relay what they can't read.
//!
//! Each message is encrypted with XChaCha20-Poly1305 under a key derived from
//! the channel key. The 24-byte nonce is random, so nothing has to be counted
//! or stored across restarts. The origin node and message ID are
//! authenticated along with the data, so relays can't change them.
//!
//! **Limit:** messages aren't signed per sender, so any member of a channel
//! can claim another member's node as a message's origin. Members of a
//! channel trust each other with it; everyone else can neither read nor
//! forge its messages.
//!
//! A broadcast data frame's payload starts with its kind:
//!
//! | Kind | Message                                                          |
//! |------|------------------------------------------------------------------|
//! | 0    | For neighbors only, protected by the mesh key alone; the payload follows |
//! | 1    | Channel message, laid out as below                               |
//!
//! | Bytes  | Field                                             |
//! |--------|---------------------------------------------------|
//! | 1      | Channel hint: the first byte of the channel ID    |
//! | 2..6   | Message ID, random                                |
//! | 6..30  | Nonce                                             |
//! | 30..   | Ciphertext, then a 16-byte tag                    |

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use chacha20poly1305::aead::AeadInPlace;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use crate::auth::{KeyTextError, key_from_text, key_to_text};
use crate::crypto;
use crate::error::{Error, Result};
use crate::node::NodeId;

const KEY_PREFIX: &str = "smc1-";
const KEY_CHECKSUM: &str = "SpectraMesh channel key checksum v1";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// Broadcast kinds: the first byte of a broadcast data frame's payload.
pub(crate) const NEIGHBORS: u8 = 0;
pub(crate) const CHANNEL: u8 = 1;

/// Bytes a channel message adds to the data it carries.
pub const CHANNEL_OVERHEAD: usize = 1 + 1 + 4 + NONCE_LEN + TAG_LEN;

/// Names a channel without revealing its key: the start of a hash of it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelId(pub [u8; 8]);

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChannelId({self})")
    }
}

/// A channel's shared key.
///
/// As text, it's `smc1-` followed by 55 base32 characters, with a checksum,
/// like a mesh key.
pub struct ChannelKey([u8; 32]);

impl ChannelKey {
    /// Use 32 bytes from a cryptographic random number generator.
    pub fn from_bytes(key: [u8; 32]) -> Self {
        ChannelKey(key)
    }

    pub fn id(&self) -> ChannelId {
        let hash = crypto::hash("SpectraMesh channel id v1", &[&self.0]);
        ChannelId(hash[..8].try_into().expect("8 bytes"))
    }

    /// The key as text. Anyone with it can read and send on the channel.
    pub fn to_text(&self) -> String {
        key_to_text(KEY_PREFIX, KEY_CHECKSUM, &self.0)
    }

    /// Parses text from [`to_text`](Self::to_text).
    pub fn from_text(text: &str) -> core::result::Result<Self, KeyTextError> {
        key_from_text(KEY_PREFIX, KEY_CHECKSUM, text).map(ChannelKey)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        let mut key = crypto::derive_key(&self.0, "SpectraMesh channel cipher v1");
        let cipher = XChaCha20Poly1305::new((&key).into());
        key.zeroize();
        cipher
    }

    /// Encrypts `data` as a channel message from `origin`.
    pub(crate) fn seal(
        &self,
        origin: NodeId,
        message_id: u32,
        nonce: [u8; NONCE_LEN],
        data: &[u8],
    ) -> Vec<u8> {
        let hint = self.id().0[0];
        let mut message = Vec::with_capacity(CHANNEL_OVERHEAD + data.len());
        message.extend_from_slice(&[CHANNEL, hint]);
        message.extend_from_slice(&message_id.to_be_bytes());
        message.extend_from_slice(&nonce);
        let start = message.len();
        message.extend_from_slice(data);
        let tag = self
            .cipher()
            .encrypt_in_place_detached(
                XNonce::from_slice(&nonce),
                &associated_data(origin, hint, message_id),
                &mut message[start..],
            )
            .expect("messages are far below the cipher's limit");
        message.extend_from_slice(&tag);
        message
    }

    /// Decrypts a channel message from `origin`. `None` if it isn't for this
    /// channel, or was altered.
    pub(crate) fn open(&self, origin: NodeId, message: &ChannelMessage<'_>) -> Option<Vec<u8>> {
        if message.hint != self.id().0[0] {
            return None;
        }
        let (ciphertext, tag) = message.sealed.split_at(message.sealed.len() - TAG_LEN);
        let mut data = ciphertext.to_vec();
        self.cipher()
            .decrypt_in_place_detached(
                XNonce::from_slice(message.nonce),
                &associated_data(origin, message.hint, message.message_id),
                &mut data,
                tag.into(),
            )
            .ok()?;
        Some(data)
    }
}

fn associated_data(origin: NodeId, hint: u8, message_id: u32) -> [u8; 13] {
    let mut data = [0u8; 13];
    data[..8].copy_from_slice(&origin.0);
    data[8] = hint;
    data[9..].copy_from_slice(&message_id.to_be_bytes());
    data
}

impl Drop for ChannelKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the key.
        write!(f, "ChannelKey({})", self.id())
    }
}

/// A channel message, decoded from a broadcast payload after its kind byte.
#[derive(Debug)]
pub(crate) struct ChannelMessage<'a> {
    pub(crate) hint: u8,
    pub(crate) message_id: u32,
    nonce: &'a [u8],
    sealed: &'a [u8],
}

impl<'a> ChannelMessage<'a> {
    pub(crate) fn decode(body: &'a [u8]) -> Result<Self> {
        if body.len() < CHANNEL_OVERHEAD - 1 {
            return Err(Error::Truncated);
        }
        Ok(ChannelMessage {
            hint: body[0],
            message_id: u32::from_be_bytes(body[1..5].try_into().expect("4 bytes")),
            nonce: &body[5..5 + NONCE_LEN],
            sealed: &body[5 + NONCE_LEN..],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: NodeId = NodeId::from_u64(7);

    fn decode(message: &[u8]) -> ChannelMessage<'_> {
        assert_eq!(message[0], CHANNEL);
        ChannelMessage::decode(&message[1..]).unwrap()
    }

    #[test]
    fn members_read_what_members_send() {
        let key = ChannelKey::from_bytes([1; 32]);
        let message = key.seal(ORIGIN, 42, [9; 24], b"hello channel");
        assert_eq!(message.len(), CHANNEL_OVERHEAD + 13);
        assert!(!message.windows(5).any(|w| w == b"hello"));
        let decoded = decode(&message);
        assert_eq!(decoded.message_id, 42);
        assert_eq!(key.open(ORIGIN, &decoded), Some(b"hello channel".to_vec()));
    }

    #[test]
    fn others_cannot_read_or_redirect_messages() {
        let key = ChannelKey::from_bytes([1; 32]);
        let message = key.seal(ORIGIN, 42, [9; 24], b"hello channel");
        let decoded = decode(&message);
        // Another channel's key.
        assert_eq!(ChannelKey::from_bytes([2; 32]).open(ORIGIN, &decoded), None);
        // A relay claiming the message came from someone else.
        assert_eq!(key.open(NodeId::from_u64(8), &decoded), None);

        let mut altered = message.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert_eq!(key.open(ORIGIN, &decode(&altered)), None);
    }

    #[test]
    fn channel_keys_round_trip_as_text() {
        let key = ChannelKey::from_bytes([3; 32]);
        let text = key.to_text();
        assert!(text.starts_with("smc1-"));
        assert_eq!(ChannelKey::from_text(&text).unwrap().id(), key.id());
        // A mesh key isn't a channel key.
        let mesh = crate::auth::MeshKey::from_bytes([3; 32]).to_text();
        assert!(matches!(
            ChannelKey::from_text(&mesh),
            Err(KeyTextError::Format { prefix: "smc1-" })
        ));
    }
}
