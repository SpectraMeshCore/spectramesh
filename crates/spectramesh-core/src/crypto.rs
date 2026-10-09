//! The few cryptographic building blocks SpectraMesh uses, all on BLAKE2s.
//!
//! Every use takes a distinct label, so a hash or key made for one purpose
//! can never be mistaken for one made for another.

use blake2::digest::consts::{U8, U32};
use blake2::digest::{Digest, KeyInit, Mac};
use blake2::{Blake2s256, Blake2sMac};

/// BLAKE2s-256 of `label` followed by `parts`.
pub(crate) fn hash(label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2s256::new();
    hasher.update(label.as_bytes());
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// A 32-byte key for one purpose, derived from `key` with `label`.
pub(crate) fn derive_key(key: &[u8; 32], label: &str) -> [u8; 32] {
    let mut mac = <Blake2sMac<U32> as KeyInit>::new(key.into());
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().into()
}

/// An 8-byte tag over `parts`, keyed with `key`.
pub(crate) fn tag(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 8] {
    let mut mac = <Blake2sMac<U8> as KeyInit>::new(key.into());
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// Checks a tag from [`tag`] in constant time.
pub(crate) fn verify_tag(key: &[u8; 32], parts: &[&[u8]], expected: &[u8; 8]) -> bool {
    let mut mac = <Blake2sMac<U8> as KeyInit>::new(key.into());
    for part in parts {
        mac.update(part);
    }
    mac.verify_slice(expected).is_ok()
}

/// Random numbers for nonces and boot indexes, from a 32-byte seed the
/// platform draws from its hardware or OS random number generator at startup.
///
/// Output `i` is BLAKE2s keyed with the seed over `i`: unpredictable without
/// the seed, and never repeated.
pub(crate) struct Random {
    seed: [u8; 32],
    counter: u64,
}

impl Random {
    pub(crate) fn new(seed: [u8; 32]) -> Self {
        Random { seed, counter: 0 }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.counter += 1;
        u64::from_be_bytes(tag(&self.seed, &[&self.counter.to_be_bytes()]))
    }

    pub(crate) fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }
}

impl Drop for Random {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.seed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_depend_on_key_and_data() {
        let (a, b) = ([1; 32], [2; 32]);
        let t = tag(&a, &[b"hello", b" world"]);
        assert!(verify_tag(&a, &[b"hello world"], &t));
        assert!(!verify_tag(&b, &[b"hello world"], &t));
        assert!(!verify_tag(&a, &[b"hello worle"], &t));
    }

    #[test]
    fn labels_separate_uses() {
        assert_ne!(derive_key(&[7; 32], "one"), derive_key(&[7; 32], "two"));
        assert_ne!(hash("one", &[b"x"]), hash("two", &[b"x"]));
    }

    #[test]
    fn random_numbers_follow_the_seed() {
        let (mut a, mut b, mut c) = (
            Random::new([1; 32]),
            Random::new([1; 32]),
            Random::new([2; 32]),
        );
        let first = a.next_u64();
        assert_eq!(first, b.next_u64());
        assert_ne!(first, a.next_u64());
        assert_ne!(first, c.next_u64());
    }
}
