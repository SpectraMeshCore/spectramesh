//! Node identities: each node's key pairs, and the node ID derived from them.
//!
//! A node keeps one 32-byte secret, generated randomly on first boot and
//! stored by the platform. Its Ed25519 key pair (for signatures) and X25519
//! key pair (for key agreement) are both derived from that secret.
//!
//! The **identity hash** is BLAKE2s-256 over both public keys, and the
//! [`NodeId`] is its first 8 bytes. IDs are self-certifying: anyone can check
//! that a set of public keys belongs to an ID by hashing them, and claiming an
//! ID means finding keys that hash to it.

use core::fmt;

use zeroize::{Zeroize, Zeroizing};

use crate::crypto;
use crate::node::NodeId;

/// A node's public keys.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicIdentity {
    pub ed25519: [u8; 32],
    pub x25519: [u8; 32],
}

impl PublicIdentity {
    /// The full identity hash. End-to-end sessions are opened to this, not to
    /// the shorter node ID.
    pub fn hash(&self) -> [u8; 32] {
        crypto::hash("SpectraMesh identity v1", &[&self.ed25519, &self.x25519])
    }

    pub fn node_id(&self) -> NodeId {
        let mut id = [0; NodeId::LEN];
        id.copy_from_slice(&self.hash()[..NodeId::LEN]);
        NodeId(id)
    }

    /// Whether `signature` is this node's Ed25519 signature over `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&self.ed25519) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(signature);
        key.verify_strict(message, &signature).is_ok()
    }
}

impl fmt::Debug for PublicIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicIdentity({})", self.node_id())
    }
}

/// A node's secret, and the public identity derived from it.
pub struct Identity {
    secret: [u8; 32],
    public: PublicIdentity,
}

impl Identity {
    /// Derives an identity from a secret. Use 32 bytes from a cryptographic
    /// random number generator the first time, then store them.
    pub fn from_secret(secret: [u8; 32]) -> Self {
        let mut ed_seed = crypto::derive_key(&secret, "SpectraMesh ed25519 v1");
        let mut x_seed = x25519_secret(&secret);
        let ed25519 = ed25519_dalek::SigningKey::from_bytes(&ed_seed)
            .verifying_key()
            .to_bytes();
        let x25519 =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(x_seed)).to_bytes();
        ed_seed.zeroize();
        x_seed.zeroize();
        Identity {
            secret,
            public: PublicIdentity { ed25519, x25519 },
        }
    }

    /// The secret, for the platform to store.
    pub fn secret(&self) -> &[u8; 32] {
        &self.secret
    }

    pub fn public(&self) -> &PublicIdentity {
        &self.public
    }

    pub fn node_id(&self) -> NodeId {
        self.public.node_id()
    }

    /// Signs `message` with this node's Ed25519 key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        let seed = Zeroizing::new(crypto::derive_key(&self.secret, "SpectraMesh ed25519 v1"));
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .sign(message)
            .to_bytes()
    }

    /// The X25519 private key, for end-to-end handshakes.
    pub(crate) fn x25519_secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(x25519_secret(&self.secret))
    }
}

fn x25519_secret(secret: &[u8; 32]) -> [u8; 32] {
    crypto::derive_key(secret, "SpectraMesh x25519 v1")
}

impl Drop for Identity {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the secret.
        write!(f, "Identity({})", self.node_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn ids_follow_from_the_secret() {
        let a = Identity::from_secret([1; 32]);
        assert_eq!(a.node_id(), Identity::from_secret([1; 32]).node_id());
        assert_ne!(a.node_id(), Identity::from_secret([2; 32]).node_id());
        assert_eq!(a.node_id(), a.public().node_id());
        assert_ne!(a.public().ed25519, a.public().x25519);
    }

    #[test]
    fn changing_either_key_changes_the_id() {
        let public = *Identity::from_secret([1; 32]).public();
        let mut other = public;
        other.x25519[0] ^= 1;
        assert_ne!(public.node_id(), other.node_id());
        other = public;
        other.ed25519[31] ^= 1;
        assert_ne!(public.node_id(), other.node_id());
    }

    #[test]
    fn signatures_verify_only_for_their_signer_and_message() {
        let (a, b) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
        );
        let signature = a.sign(b"seqno 5");
        assert!(a.public().verify(b"seqno 5", &signature));
        assert!(!a.public().verify(b"seqno 6", &signature));
        assert!(!b.public().verify(b"seqno 5", &signature));
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let identity = Identity::from_secret([0xab; 32]);
        let printed = format!("{identity:?}");
        assert!(printed.starts_with("Identity(!"), "{printed}");
        assert!(!printed.contains("abab"));
    }
}
