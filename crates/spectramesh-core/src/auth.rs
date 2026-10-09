//! Link authentication: mesh keys, frame tags and replay protection.
//!
//! Every node in a mesh shares a 32-byte **mesh key**. Every frame ends with
//! a trailer: the sender's boot index, a counter, and an 8-byte tag keyed with
//! a key derived from the mesh key. Receivers drop frames whose tag doesn't
//! verify, so nodes without the key can't inject or alter anything.
//!
//! **Replays** are caught with the boot index and counter, as in Babel's RFC
//! 8967. A node picks a random boot index whenever it starts and counts every
//! frame it sends. Receivers remember each neighbor's index and recent
//! counters, and drop anything they've seen before. When a neighbor shows up
//! with an index they don't know, they challenge it to echo a random nonce,
//! which a recording can't do; the router handles that exchange.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use zeroize::Zeroize;

use crate::crypto;

/// Bytes in a frame's trailer: boot index (4), counter (4) and tag (8).
pub const TRAILER_LEN: usize = 4 + 4 + TAG_LEN;

/// Bytes in a frame tag.
pub const TAG_LEN: usize = 8;

/// A shared mesh key.
///
/// As text, it's `smk1-` followed by 55 base32 characters: the key and a
/// 2-byte checksum, so typos are caught instead of producing a different key.
pub struct MeshKey([u8; 32]);

const MESH_KEY_PREFIX: &str = "smk1-";
const MESH_KEY_CHECKSUM: &str = "SpectraMesh mesh key checksum v1";

impl MeshKey {
    /// Use 32 bytes from a cryptographic random number generator.
    pub fn from_bytes(key: [u8; 32]) -> Self {
        MeshKey(key)
    }

    /// The key's 1-byte ID, sent in every frame so receivers know which key
    /// to check it with while keys are being changed. It's derived from the
    /// key, so two keys can share an ID; receivers then try both.
    pub fn id(&self) -> u8 {
        crypto::derive_key(&self.0, "SpectraMesh key id v1")[0]
    }

    /// An IPv6 unique local `/64` prefix for the mesh: `fd`, 40 bits
    /// derived from the key, and subnet 0. Each node's address is this
    /// prefix followed by its 8-byte node ID.
    ///
    /// Changing the mesh key changes the prefix, so a mesh that wants stable
    /// addresses across key changes should fix its prefix in configuration.
    pub fn ipv6_prefix(&self) -> [u8; 8] {
        let hash = crypto::derive_key(&self.0, "SpectraMesh IPv6 prefix v1");
        [0xfd, hash[0], hash[1], hash[2], hash[3], hash[4], 0, 0]
    }

    fn link_key(&self) -> [u8; 32] {
        crypto::derive_key(&self.0, "SpectraMesh link v1")
    }

    /// The key as text, for configuration files. Treat it like a password.
    pub fn to_text(&self) -> String {
        key_to_text(MESH_KEY_PREFIX, MESH_KEY_CHECKSUM, &self.0)
    }

    /// Parses text from [`to_text`](Self::to_text). Case and surrounding
    /// whitespace don't matter.
    pub fn from_text(text: &str) -> Result<Self, KeyTextError> {
        key_from_text(MESH_KEY_PREFIX, MESH_KEY_CHECKSUM, text).map(MeshKey)
    }
}

impl Drop for MeshKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for MeshKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the key.
        write!(f, "MeshKey(id {})", self.id())
    }
}

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A 32-byte key as text: `prefix`, then 55 base32 characters encoding the
/// key and a 2-byte checksum keyed with `checksum_label`.
pub(crate) fn key_to_text(prefix: &str, checksum_label: &str, key: &[u8; 32]) -> String {
    let mut bytes = Vec::with_capacity(34);
    bytes.extend_from_slice(key);
    bytes.extend_from_slice(&checksum(checksum_label, key));
    let mut text = String::from(prefix);
    // 34 bytes is 272 bits: 54 full 5-bit groups plus 2 bits.
    let (mut acc, mut bits) = (0u32, 0);
    for &byte in &bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            text.push(char::from(BASE32[((acc >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        text.push(char::from(BASE32[((acc << (5 - bits)) & 31) as usize]));
    }
    bytes.zeroize();
    acc.zeroize();
    text
}

/// Parses text from [`key_to_text`]. Case and surrounding whitespace don't matter.
pub(crate) fn key_from_text(
    prefix: &'static str,
    checksum_label: &str,
    text: &str,
) -> Result<[u8; 32], KeyTextError> {
    let format = KeyTextError::Format { prefix };
    let text = text.trim();
    let body = text
        .get(..prefix.len())
        .filter(|found| found.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
        .ok_or(format)?;
    if body.len() != 55 {
        return Err(format);
    }
    let mut bytes = [0u8; 34];
    let (mut acc, mut bits, mut out) = (0u32, 0, 0);
    for c in body.bytes() {
        let value = BASE32
            .iter()
            .position(|&b| b == c.to_ascii_lowercase())
            .ok_or(format)?;
        acc = (acc << 5) | value as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes[out] = (acc >> bits) as u8;
            out += 1;
        }
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes[..32]);
    let valid = bytes[32..] == checksum(checksum_label, &key);
    bytes.zeroize();
    acc.zeroize();
    if valid {
        Ok(key)
    } else {
        key.zeroize();
        Err(KeyTextError::Checksum)
    }
}

fn checksum(label: &str, key: &[u8; 32]) -> [u8; 2] {
    let hash = crypto::hash(label, &[key]);
    [hash[0], hash[1]]
}

/// Why a mesh or channel key's text didn't parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyTextError {
    /// Not `prefix` followed by 55 base32 characters.
    Format { prefix: &'static str },
    /// The checksum doesn't match, probably because of a typo.
    Checksum,
}

impl fmt::Display for KeyTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyTextError::Format { prefix } => {
                write!(f, "expected \"{prefix}\" followed by 55 letters and digits")
            }
            KeyTextError::Checksum => {
                f.write_str("the key's checksum doesn't match; check for typos")
            }
        }
    }
}

impl core::error::Error for KeyTextError {}

/// The mesh keys a node accepts, and the one it sends with.
///
/// To change keys without downtime: give every node the new key as an extra
/// key, then make it the current key everywhere, then remove the old one.
pub struct KeyRing {
    /// (key ID, link key). The first is the current key.
    keys: Vec<(u8, [u8; 32])>,
}

impl KeyRing {
    pub fn new(current: &MeshKey) -> Self {
        KeyRing {
            keys: alloc::vec![(current.id(), current.link_key())],
        }
    }

    /// Also accepts frames tagged with `key`.
    pub fn accept(&mut self, key: &MeshKey) {
        self.keys.push((key.id(), key.link_key()));
    }

    /// The ID frames are sent with.
    pub fn current_id(&self) -> u8 {
        self.keys[0].0
    }

    /// The IDs of every accepted key, the current one first.
    pub fn ids(&self) -> impl Iterator<Item = u8> + '_ {
        self.keys.iter().map(|(id, _)| *id)
    }

    /// Tags `signed`, the frame so far, with the current key.
    pub fn tag(&self, signed: &[u8]) -> [u8; TAG_LEN] {
        crypto::tag(&self.keys[0].1, &[signed])
    }

    /// Whether `tag` is valid for `signed` under any accepted key with `key_id`.
    pub fn verify(&self, key_id: u8, signed: &[u8], tag: &[u8; TAG_LEN]) -> bool {
        self.keys
            .iter()
            .filter(|(id, _)| *id == key_id)
            .any(|(_, key)| crypto::verify_tag(key, &[signed], tag))
    }
}

impl fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Key IDs only, never the keys.
        let ids: Vec<u8> = self.keys.iter().map(|(id, _)| *id).collect();
        write!(f, "KeyRing(ids {ids:?})")
    }
}

impl Drop for KeyRing {
    fn drop(&mut self) {
        for (_, key) in &mut self.keys {
            key.zeroize();
        }
    }
}

/// Counters seen recently from one sender, for spotting replays.
///
/// Remembers the highest counter and the 63 before it, so frames that arrive
/// slightly out of order are still accepted, once each.
#[derive(Clone, Copy, Debug)]
pub struct ReplayWindow {
    highest: u64,
    /// Bit `n` is set if counter `highest - n` has been seen.
    seen: u64,
}

impl ReplayWindow {
    pub fn new(first: u64) -> Self {
        ReplayWindow {
            highest: first,
            seen: 1,
        }
    }

    /// Whether `counter` would be accepted, without recording it.
    pub fn is_fresh(&self, counter: u64) -> bool {
        counter > self.highest || {
            let age = self.highest - counter;
            age < 64 && self.seen & (1 << age) == 0
        }
    }

    /// Records `counter`. Returns false if it was seen before or is too old to tell.
    pub fn accept(&mut self, counter: u64) -> bool {
        if counter > self.highest {
            let shift = counter - self.highest;
            self.seen = if shift >= 64 { 0 } else { self.seen << shift };
            self.seen |= 1;
            self.highest = counter;
            return true;
        }
        let age = self.highest - counter;
        if age >= 64 || self.seen & (1 << age) != 0 {
            return false;
        }
        self.seen |= 1 << age;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_keys_round_trip_as_text() {
        let key = MeshKey::from_bytes([0x5a; 32]);
        let text = key.to_text();
        assert_eq!(text.len(), 5 + 55);
        assert!(text.starts_with("smk1-"));
        let parsed = MeshKey::from_text(&text.to_uppercase()).unwrap();
        assert_eq!(parsed.0, key.0);
        assert_eq!(parsed.id(), key.id());
    }

    #[test]
    fn ipv6_prefixes_are_unique_local() {
        let prefix = MeshKey::from_bytes([3; 32]).ipv6_prefix();
        assert_eq!(prefix[0], 0xfd);
        assert_eq!(prefix[6..], [0, 0]);
        assert_ne!(prefix, MeshKey::from_bytes([4; 32]).ipv6_prefix());
    }

    #[test]
    fn typos_are_caught() {
        let text = MeshKey::from_bytes([7; 32]).to_text();
        let mut typo = text.clone().into_bytes();
        typo[20] = if typo[20] == b'a' { b'b' } else { b'a' };
        let typo = String::from_utf8(typo).unwrap();
        assert_eq!(
            MeshKey::from_text(&typo).unwrap_err(),
            KeyTextError::Checksum
        );
        assert_eq!(
            MeshKey::from_text(&text[..40]).unwrap_err(),
            KeyTextError::Format { prefix: "smk1-" }
        );
        assert_eq!(
            MeshKey::from_text("smk1-!!!").unwrap_err(),
            KeyTextError::Format { prefix: "smk1-" }
        );
    }

    #[test]
    fn key_rings_verify_only_their_keys() {
        let (old, new) = (MeshKey::from_bytes([1; 32]), MeshKey::from_bytes([2; 32]));
        let old_ring = KeyRing::new(&old);
        let mut rotating = KeyRing::new(&new);
        rotating.accept(&old);

        let tag = old_ring.tag(b"frame");
        assert!(rotating.verify(old.id(), b"frame", &tag));
        assert!(!rotating.verify(new.id(), b"frame", &tag));
        assert!(!old_ring.verify(old.id(), b"other", &tag));

        let tag = rotating.tag(b"frame");
        assert_eq!(rotating.current_id(), new.id());
        assert!(!old_ring.verify(new.id(), b"frame", &tag));
    }

    #[test]
    fn replay_window_accepts_each_counter_once() {
        let mut window = ReplayWindow::new(100);
        assert!(!window.accept(100));
        assert!(window.accept(102));
        assert!(window.accept(101), "slightly out of order is fine");
        assert!(!window.accept(101));
        assert!(window.accept(500));
        assert!(!window.accept(436), "too old to tell");
        assert!(window.accept(437));
        assert!(!window.accept(437));
    }
}
