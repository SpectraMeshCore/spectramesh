//! End-to-end sessions between two nodes.
//!
//! Unicast data is encrypted between the node that sends it and the node it's
//! for, so relays and other mesh members can neither read nor alter it. Two
//! nodes set up a session with one round trip of the Noise IK handshake
//! (`Noise_IK_25519_ChaChaPoly_BLAKE2s`), the pattern WireGuard uses:
//!
//! 1. The initiator already has the responder's public keys, from identity
//!    discovery. Its **handshake init** carries its own X25519 key and,
//!    encrypted, its Ed25519 key, so the responder can check that the keys
//!    belong to the node ID the frame claims to come from.
//! 2. The responder answers with a **handshake response**. Both sides now
//!    share keys nobody else has, with forward secrecy.
//! 3. Data travels as **transport** messages: a counter and ChaCha20-Poly1305
//!    ciphertext. Counters let messages arrive out of order, and a replay
//!    window rejects repeats.
//!
//! As in WireGuard, a responder doesn't send on a session until the initiator
//! has used it, which proves the handshake init wasn't a replay. Sessions are
//! replaced after two minutes and dropped after three. Two more timers, also
//! from WireGuard, notice when the other side has lost the session, for
//! example by restarting: a node that received data but has sent nothing for
//! 10 seconds sends a keepalive, and a node that sent data but has heard
//! nothing for 15 seconds starts a new handshake.
//!
//! Each message is the payload of a data frame:
//!
//! | Message            | Layout                                                          |
//! |--------------------|-----------------------------------------------------------------|
//! | Handshake init     | type 1, sender index (4), Noise message (128)                   |
//! | Handshake response | type 2, sender index (4), receiver index (4), Noise message (48) |
//! | Transport          | type 3, receiver index (4), counter (8), ciphertext             |
//!
//! Indexes are random numbers each side picks so the other can name the
//! session, as in WireGuard. The first plaintext byte of every transport
//! message says what it carries: a keepalive or data.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use snow::params::{CipherChoice, DHChoice, HashChoice, NoiseParams};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::types::{Cipher, Dh, Hash};
use snow::{Builder, HandshakeState, StatelessTransportState};
use zeroize::{Zeroize, Zeroizing};

use crate::auth::ReplayWindow;
use crate::crypto::Random;
use crate::error::{Error, Result};
use crate::identity::{Identity, PublicIdentity};
use crate::node::NodeId;
use crate::time::Instant;

const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
/// Binds every handshake to this protocol and version.
const PROLOGUE: &[u8] = b"SpectraMesh v2";
const TAG_LEN: usize = 16;

/// Start a new handshake once a session is this old.
pub const REKEY_AFTER: Duration = Duration::from_secs(120);
/// Stop using a session once it's this old.
pub const REJECT_AFTER: Duration = Duration::from_secs(180);
/// Start a new handshake after this many messages, long before nonces run out.
const REKEY_AFTER_MESSAGES: u64 = 1 << 60;
/// Send a keepalive this long after receiving data, if nothing else was sent.
pub const KEEPALIVE_AFTER: Duration = Duration::from_secs(10);
/// Start a new handshake if data sent this long ago got no reply at all.
pub const REKEY_IF_SILENT: Duration = Duration::from_secs(15);

const INIT: u8 = 1;
const RESPONSE: u8 = 2;
const TRANSPORT: u8 = 3;

const CONTENT_KEEPALIVE: u8 = 0;
const CONTENT_DATA: u8 = 1;

/// Bytes a transport message adds to the data it carries.
pub const TRANSPORT_OVERHEAD: usize = 1 + 4 + 8 + TAG_LEN + 1;

/// An end-to-end message, decoded from a data frame's payload.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message<'a> {
    Init {
        sender: u32,
        noise: &'a [u8],
    },
    Response {
        sender: u32,
        receiver: u32,
        noise: &'a [u8],
    },
    Transport {
        receiver: u32,
        counter: u64,
        ciphertext: &'a [u8],
    },
}

impl<'a> Message<'a> {
    pub(crate) fn decode(bytes: &'a [u8]) -> Result<Self> {
        let (&kind, rest) = bytes.split_first().ok_or(Error::Truncated)?;
        let field = |at: usize, len: usize| rest.get(at..at + len).ok_or(Error::Truncated);
        let u32_at = |at| Ok::<_, Error>(u32::from_be_bytes(field(at, 4)?.try_into().unwrap()));
        match kind {
            INIT => Ok(Message::Init {
                sender: u32_at(0)?,
                noise: &rest[4..],
            }),
            RESPONSE => Ok(Message::Response {
                sender: u32_at(0)?,
                receiver: u32_at(4)?,
                noise: &rest[8..],
            }),
            TRANSPORT => Ok(Message::Transport {
                receiver: u32_at(0)?,
                counter: u64::from_be_bytes(field(4, 8)?.try_into().unwrap()),
                ciphertext: &rest[12..],
            }),
            other => Err(Error::UnknownMessage(other)),
        }
    }
}

/// What a transport message carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content<'a> {
    /// Nothing; used to confirm a new session.
    Keepalive,
    Data(&'a [u8]),
}

/// Gives snow the default primitives, and randomness from a seed drawn from
/// the router's generator, keeping the core free of OS dependencies.
struct Resolver {
    seed: [u8; 32],
}

impl CryptoResolver for Resolver {
    fn resolve_rng(&self) -> Option<Box<dyn snow::types::Random>> {
        Some(Box::new(Random::new(self.seed)))
    }

    fn resolve_dh(&self, choice: &DHChoice) -> Option<Box<dyn Dh>> {
        DefaultResolver.resolve_dh(choice)
    }

    fn resolve_hash(&self, choice: &HashChoice) -> Option<Box<dyn Hash>> {
        DefaultResolver.resolve_hash(choice)
    }

    fn resolve_cipher(&self, choice: &CipherChoice) -> Option<Box<dyn Cipher>> {
        DefaultResolver.resolve_cipher(choice)
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.seed.zeroize();
    }
}

fn builder<'a>(seed: [u8; 32]) -> Builder<'a> {
    let params: NoiseParams = NOISE_PARAMS.parse().expect("valid Noise parameters");
    Builder::with_resolver(params, Box::new(Resolver { seed }))
}

fn bad_handshake(_: snow::Error) -> Error {
    Error::BadHandshake
}

/// A handshake this node started, waiting for its response.
pub(crate) struct Initiation {
    pub(crate) peer: PublicIdentity,
    pub(crate) index: u32,
    pub(crate) sent: Instant,
    pub(crate) attempts: u8,
    state: HandshakeState,
}

impl Initiation {
    /// Starts a handshake with `peer`. Returns it with the handshake init to send.
    pub(crate) fn start(
        identity: &Identity,
        peer: PublicIdentity,
        index: u32,
        seed: [u8; 32],
        now: Instant,
    ) -> Result<(Self, Vec<u8>)> {
        let secret = identity.x25519_secret();
        let mut state = builder(seed)
            .prologue(PROLOGUE)
            .and_then(|b| b.local_private_key(&secret[..]))
            .and_then(|b| b.remote_public_key(&peer.x25519))
            .and_then(Builder::build_initiator)
            .map_err(bad_handshake)?;
        let mut message = vec![0u8; 5 + 128];
        message[0] = INIT;
        message[1..5].copy_from_slice(&index.to_be_bytes());
        let len = state
            .write_message(&identity.public().ed25519, &mut message[5..])
            .map_err(bad_handshake)?;
        message.truncate(5 + len);
        let initiation = Initiation {
            peer,
            index,
            sent: now,
            attempts: 1,
            state,
        };
        Ok((initiation, message))
    }

    /// Completes the handshake with the responder's answer.
    pub(crate) fn complete(
        mut self,
        responder_index: u32,
        noise: &[u8],
        now: Instant,
    ) -> Result<Session> {
        let mut payload = [0u8; 64];
        self.state
            .read_message(noise, &mut payload)
            .map_err(bad_handshake)?;
        let transport = self
            .state
            .into_stateless_transport_mode()
            .map_err(bad_handshake)?;
        Ok(Session::new(
            transport,
            self.index,
            responder_index,
            self.peer,
            now,
            true,
        ))
    }
}

/// Answers a handshake init that arrived from `origin`. Returns the new
/// session, unconfirmed, and the handshake response to send.
pub(crate) fn respond(
    identity: &Identity,
    origin: NodeId,
    initiator_index: u32,
    noise: &[u8],
    index: u32,
    seed: [u8; 32],
    now: Instant,
) -> Result<(Session, Vec<u8>)> {
    let secret = identity.x25519_secret();
    let mut state = builder(seed)
        .prologue(PROLOGUE)
        .and_then(|b| b.local_private_key(&secret[..]))
        .and_then(Builder::build_responder)
        .map_err(bad_handshake)?;
    let mut payload = Zeroizing::new([0u8; 64]);
    let len = state
        .read_message(noise, &mut payload[..])
        .map_err(bad_handshake)?;
    let ed25519: [u8; 32] = payload[..len].try_into().map_err(|_| Error::BadHandshake)?;
    let x25519: [u8; 32] = state
        .get_remote_static()
        .and_then(|key| key.try_into().ok())
        .ok_or(Error::BadHandshake)?;
    let peer = PublicIdentity { ed25519, x25519 };
    // The keys must be the ones the claimed node ID was derived from.
    if peer.node_id() != origin {
        return Err(Error::BadHandshake);
    }

    let mut message = vec![0u8; 9 + 48];
    message[0] = RESPONSE;
    message[1..5].copy_from_slice(&index.to_be_bytes());
    message[5..9].copy_from_slice(&initiator_index.to_be_bytes());
    let len = state
        .write_message(&[], &mut message[9..])
        .map_err(bad_handshake)?;
    message.truncate(9 + len);
    let transport = state
        .into_stateless_transport_mode()
        .map_err(bad_handshake)?;
    let session = Session::new(transport, index, initiator_index, peer, now, false);
    Ok((session, message))
}

/// An established session with one peer.
pub(crate) struct Session {
    transport: StatelessTransportState,
    /// This node's index for the session; the peer puts it in its messages.
    pub(crate) index: u32,
    remote_index: u32,
    pub(crate) peer: PublicIdentity,
    pub(crate) created: Instant,
    /// Whether this node may send on it. A responder must first receive a
    /// message from the initiator, proving the handshake wasn't replayed.
    pub(crate) confirmed: bool,
    next_counter: u64,
    window: Option<ReplayWindow>,
    /// When data was first sent since anything was last received.
    unanswered_since: Option<Instant>,
    /// When data was first received since anything was last sent.
    unacknowledged_since: Option<Instant>,
}

impl Session {
    fn new(
        transport: StatelessTransportState,
        index: u32,
        remote_index: u32,
        peer: PublicIdentity,
        now: Instant,
        confirmed: bool,
    ) -> Self {
        Session {
            transport,
            index,
            remote_index,
            peer,
            created: now,
            confirmed,
            next_counter: 0,
            window: None,
            unanswered_since: None,
            unacknowledged_since: None,
        }
    }

    /// Whether the peer has gone quiet after this node sent it data, a sign
    /// it lost the session.
    pub(crate) fn peer_silent(&self, now: Instant) -> bool {
        self.unanswered_since
            .is_some_and(|since| now >= since + REKEY_IF_SILENT)
    }

    /// Whether to send a keepalive, so the peer knows its data arrived.
    pub(crate) fn keepalive_due(&self, now: Instant) -> bool {
        self.unacknowledged_since
            .is_some_and(|since| now >= since + KEEPALIVE_AFTER)
    }

    /// Whether it's time to set up a replacement.
    pub(crate) fn needs_rekey(&self, now: Instant) -> bool {
        now >= self.created + REKEY_AFTER || self.next_counter >= REKEY_AFTER_MESSAGES
    }

    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now >= self.created + REJECT_AFTER
    }

    /// Encrypts `content` as a transport message.
    pub(crate) fn seal(&mut self, content: Content<'_>, now: Instant) -> Result<Vec<u8>> {
        self.unacknowledged_since = None;
        if matches!(content, Content::Data(_)) {
            self.unanswered_since.get_or_insert(now);
        }
        let mut plaintext: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::new());
        match content {
            Content::Keepalive => plaintext.push(CONTENT_KEEPALIVE),
            Content::Data(data) => {
                plaintext.push(CONTENT_DATA);
                plaintext.extend_from_slice(data);
            }
        }
        let counter = self.next_counter;
        self.next_counter += 1;

        let mut message = vec![0u8; 13 + plaintext.len() + TAG_LEN];
        message[0] = TRANSPORT;
        message[1..5].copy_from_slice(&self.remote_index.to_be_bytes());
        message[5..13].copy_from_slice(&counter.to_be_bytes());
        let len = self
            .transport
            .write_message(counter, &plaintext, &mut message[13..])
            .map_err(|_| Error::BadCiphertext)?;
        message.truncate(13 + len);
        Ok(message)
    }

    /// Decrypts a transport message. Returns its data, or `None` for a keepalive.
    pub(crate) fn open(
        &mut self,
        counter: u64,
        ciphertext: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>> {
        if self
            .window
            .as_ref()
            .is_some_and(|window| !window.is_fresh(counter))
        {
            return Err(Error::Replay);
        }
        let mut plaintext = vec![0u8; ciphertext.len()];
        let len = self
            .transport
            .read_message(counter, ciphertext, &mut plaintext)
            .map_err(|_| Error::BadCiphertext)?;
        plaintext.truncate(len);
        // Only authentic messages move the window.
        match &mut self.window {
            Some(window) => {
                window.accept(counter);
            }
            None => self.window = Some(ReplayWindow::new(counter)),
        }
        self.confirmed = true;
        self.unanswered_since = None;

        match plaintext.first() {
            Some(&CONTENT_DATA) => {
                self.unacknowledged_since.get_or_insert(now);
                plaintext.remove(0);
                Ok(Some(plaintext))
            }
            Some(&CONTENT_KEEPALIVE) => Ok(None),
            _ => Err(Error::BadCiphertext),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Instant = Instant::from_millis(0);

    /// Runs a handshake between two fresh identities.
    fn handshake() -> (Identity, Identity, Session, Session) {
        let (a, b) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
        );
        let (initiation, init) = Initiation::start(&a, *b.public(), 11, [3; 32], NOW).unwrap();
        assert_eq!(init.len(), 5 + 128);
        let Message::Init { sender, noise } = Message::decode(&init).unwrap() else {
            panic!("expected a handshake init");
        };
        let (responder, response) =
            respond(&b, a.node_id(), sender, noise, 22, [4; 32], NOW).unwrap();
        assert_eq!(response.len(), 9 + 48);
        assert_eq!(responder.peer, *a.public());
        assert!(!responder.confirmed);

        let Message::Response {
            sender,
            receiver,
            noise,
        } = Message::decode(&response).unwrap()
        else {
            panic!("expected a handshake response");
        };
        assert_eq!(receiver, 11);
        let initiator = initiation.complete(sender, noise, NOW).unwrap();
        assert!(initiator.confirmed);
        (a, b, initiator, responder)
    }

    fn transport(message: &[u8]) -> (u32, u64, &[u8]) {
        match Message::decode(message).unwrap() {
            Message::Transport {
                receiver,
                counter,
                ciphertext,
            } => (receiver, counter, ciphertext),
            other => panic!("expected transport, got {other:?}"),
        }
    }

    #[test]
    fn sessions_carry_data_both_ways() {
        let (_, _, mut initiator, mut responder) = handshake();

        let message = initiator.seal(Content::Data(b"hello"), NOW).unwrap();
        assert_eq!(message.len(), TRANSPORT_OVERHEAD + 5);
        let (receiver, counter, ciphertext) = transport(&message);
        assert_eq!(receiver, responder.index);
        assert!(!ciphertext.windows(5).any(|w| w == b"hello"));
        assert_eq!(
            responder.open(counter, ciphertext, NOW).unwrap(),
            Some(b"hello".to_vec())
        );
        assert!(responder.confirmed);

        let reply = responder.seal(Content::Keepalive, NOW).unwrap();
        let (receiver, counter, ciphertext) = transport(&reply);
        assert_eq!(receiver, initiator.index);
        assert_eq!(initiator.open(counter, ciphertext, NOW).unwrap(), None);
    }

    #[test]
    fn replays_and_tampering_are_rejected() {
        let (_, _, mut initiator, mut responder) = handshake();
        let first = initiator.seal(Content::Data(b"one"), NOW).unwrap();
        let second = initiator.seal(Content::Data(b"two"), NOW).unwrap();

        // Out of order is fine, once each.
        let (_, counter, ciphertext) = transport(&second);
        assert!(responder.open(counter, ciphertext, NOW).is_ok());
        let (_, counter, ciphertext) = transport(&first);
        assert!(responder.open(counter, ciphertext, NOW).is_ok());
        assert_eq!(responder.open(counter, ciphertext, NOW), Err(Error::Replay));

        let mut tampered = initiator.seal(Content::Data(b"three"), NOW).unwrap();
        *tampered.last_mut().unwrap() ^= 1;
        let (_, counter, ciphertext) = transport(&tampered);
        assert_eq!(
            responder.open(counter, ciphertext, NOW),
            Err(Error::BadCiphertext)
        );
        // A forged message doesn't burn its counter: the real one still gets through.
        let mut real = tampered.clone();
        *real.last_mut().unwrap() ^= 1;
        let (_, counter, ciphertext) = transport(&real);
        assert_eq!(
            responder.open(counter, ciphertext, NOW),
            Ok(Some(b"three".to_vec()))
        );
    }

    #[test]
    fn handshakes_must_match_the_claimed_node_id() {
        let (a, b) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
        );
        let (_, init) = Initiation::start(&a, *b.public(), 11, [3; 32], NOW).unwrap();
        let Message::Init { sender, noise } = Message::decode(&init).unwrap() else {
            panic!("expected a handshake init");
        };
        let someone_else = Identity::from_secret([9; 32]).node_id();
        assert!(matches!(
            respond(&b, someone_else, sender, noise, 22, [4; 32], NOW),
            Err(Error::BadHandshake)
        ));
    }

    #[test]
    fn only_the_intended_responder_can_answer() {
        let (a, b, c) = (
            Identity::from_secret([1; 32]),
            Identity::from_secret([2; 32]),
            Identity::from_secret([5; 32]),
        );
        // A meant to reach B; C intercepts the handshake.
        let (_, init) = Initiation::start(&a, *b.public(), 11, [3; 32], NOW).unwrap();
        let Message::Init { sender, noise } = Message::decode(&init).unwrap() else {
            panic!("expected a handshake init");
        };
        assert!(matches!(
            respond(&c, a.node_id(), sender, noise, 22, [4; 32], NOW),
            Err(Error::BadHandshake)
        ));
    }

    #[test]
    fn silence_and_unacknowledged_data_are_noticed() {
        let (_, _, mut initiator, mut responder) = handshake();
        let later = |s| NOW + Duration::from_secs(s);

        let message = initiator.seal(Content::Data(b"ping"), NOW).unwrap();
        assert!(!initiator.peer_silent(later(14)));
        assert!(initiator.peer_silent(later(15)));

        let (_, counter, ciphertext) = transport(&message);
        responder.open(counter, ciphertext, later(1)).unwrap();
        assert!(!responder.keepalive_due(later(10)));
        assert!(responder.keepalive_due(later(11)));

        let keepalive = responder.seal(Content::Keepalive, later(11)).unwrap();
        assert!(!responder.keepalive_due(later(30)));
        let (_, counter, ciphertext) = transport(&keepalive);
        initiator.open(counter, ciphertext, later(12)).unwrap();
        assert!(!initiator.peer_silent(later(30)));
    }

    #[test]
    fn sessions_age_out() {
        let (_, _, initiator, _) = handshake();
        assert!(!initiator.needs_rekey(NOW + Duration::from_secs(119)));
        assert!(initiator.needs_rekey(NOW + REKEY_AFTER));
        assert!(!initiator.is_expired(NOW + Duration::from_secs(179)));
        assert!(initiator.is_expired(NOW + REJECT_AFTER));
    }
}
