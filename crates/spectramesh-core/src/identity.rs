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

use zeroize::Zeroize;

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
        let mut x_seed = crypto::derive_key(&secret, "SpectraMesh x25519 v1");
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
    fn debug_output_hides_the_secret() {
        let identity = Identity::from_secret([0xab; 32]);
        let printed = format!("{identity:?}");
        assert!(printed.starts_with("Identity(!"), "{printed}");
        assert!(!printed.contains("abab"));
    }
}
