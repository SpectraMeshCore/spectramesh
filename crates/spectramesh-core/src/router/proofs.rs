//! Signed seqnos: only a node itself can raise the seqno of its own route.
//!
//! A newer seqno beats any metric, so in plain Babel a member could forge one
//! for another node and pull all of that node's traffic through itself. Here
//! every node signs each seqno it issues with its Ed25519 key, and the
//! signature travels with its updates as a [`Tlv::SeqnoProof`]. A node ignores
//! any update whose seqno is newer than the newest one it holds a valid proof
//! for, and asks the neighbor for the destination's route instead, which
//! comes with its proof.
//!
//! Proofs go out in triggered updates and in the first full update to a
//! newly verified neighbor; nodes forward the best proof they hold for each
//! destination. A replayed old proof can't claim a seqno newer than the
//! destination has signed, and if the destination restarted, it raises its
//! seqno past the replayed one as soon as neighbors ask.

use core::time::Duration;

use super::Router;
use crate::hue::HueId;
use crate::identity::PublicIdentity;
use crate::node::NodeId;
use crate::packet::Tlv;
use crate::routing::seqno_newer;
use crate::time::Instant;

/// How long to wait before asking the same destination's proof again.
const PROOF_REQUEST_HOLD: Duration = Duration::from_secs(1);

/// The newest proven seqno for a destination.
#[derive(Clone, Copy, Debug)]
pub(super) struct Proof {
    seqno: u16,
    keys: PublicIdentity,
    signature: [u8; 64],
}

/// What a node signs to issue `seqno` for its own route.
pub(super) fn seqno_message(node: NodeId, seqno: u16) -> [u8; 30] {
    let mut message = [0u8; 30];
    message[..20].copy_from_slice(b"SpectraMesh seqno v1");
    message[20..28].copy_from_slice(&node.0);
    message[28..].copy_from_slice(&seqno.to_be_bytes());
    message
}

impl Router {
    /// Signs this node's current seqno, after it changes.
    pub(super) fn sign_seqno(&mut self) {
        self.seqno_signature = self.identity.sign(&seqno_message(self.id, self.seqno));
    }

    /// Whether an update for `dest` with `seqno` is covered by a proof.
    pub(super) fn is_proven(&self, dest: NodeId, seqno: u16) -> bool {
        self.proofs
            .get(&dest)
            .is_some_and(|proof| !seqno_newer(seqno, proof.seqno))
    }

    /// The best proof this node can pass on for `dest`'s seqno.
    pub(super) fn proof_for(&self, dest: NodeId) -> Option<Tlv> {
        if dest == self.id {
            return Some(Tlv::SeqnoProof {
                keys: *self.identity.public(),
                seqno: self.seqno,
                signature: self.seqno_signature,
            });
        }
        self.proofs.get(&dest).map(|proof| Tlv::SeqnoProof {
            keys: proof.keys,
            seqno: proof.seqno,
            signature: proof.signature,
        })
    }

    /// Stores a proof if it's newer than the one held and its signature checks out.
    pub(super) fn handle_seqno_proof(
        &mut self,
        keys: PublicIdentity,
        seqno: u16,
        signature: [u8; 64],
        now: Instant,
    ) {
        let node = keys.node_id();
        if node == self.id || self.is_proven(node, seqno) {
            return;
        }
        if !keys.verify(&seqno_message(node, seqno), &signature) {
            return;
        }
        self.proofs.insert(
            node,
            Proof {
                seqno,
                keys,
                signature,
            },
        );
        // The keys are verified now, so they can open sessions too.
        self.remember_identity(keys, now);
    }

    /// Asks `neighbor` for `dest`'s route, which comes with its proof.
    pub(super) fn request_proof(
        &mut self,
        neighbor: NodeId,
        hue: HueId,
        dest: NodeId,
        now: Instant,
    ) {
        if self
            .proof_requests
            .get(&dest)
            .is_some_and(|&at| now < at + PROOF_REQUEST_HOLD)
        {
            return;
        }
        self.proof_requests.insert(dest, now);
        self.send_control(hue, neighbor, &[Tlv::RouteRequest { dest }]);
    }

    /// Forgets proofs for destinations no longer in the route table.
    pub(super) fn expire_proofs(&mut self, now: Instant) {
        let routes = &self.routes;
        self.proofs
            .retain(|dest, _| routes.for_dest(*dest).next().is_some());
        self.proof_requests
            .retain(|_, &mut at| now < at + PROOF_REQUEST_HOLD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seqno_messages_name_node_and_seqno() {
        let a = seqno_message(NodeId::from_u64(1), 5);
        assert_ne!(a, seqno_message(NodeId::from_u64(2), 5));
        assert_ne!(a, seqno_message(NodeId::from_u64(1), 6));
    }
}
