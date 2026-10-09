use core::fmt;

use crate::hue::HueId;
use crate::node::NodeId;

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The frame ended before a complete header or TLV was read.
    Truncated,
    /// A TLV's length is too short for its type.
    Malformed,
    /// The frame was written by an incompatible protocol version.
    UnsupportedVersion(u8),
    /// The frame kind isn't one this version understands.
    UnknownFrameKind(u8),
    /// The frame arrived on, or was addressed to, a hue the router doesn't have.
    UnknownHue(HueId),
    /// No route to the destination is known yet.
    NoRoute(NodeId),
    /// The packet is larger than the hue it must travel on can carry.
    PayloadTooLarge,
    /// The frame's tag doesn't verify with any accepted mesh key: it came
    /// from outside the mesh, or was damaged or altered.
    BadTag,
    /// The frame was already received once.
    Replay,
    /// The frame is authentic, but its sender's boot index hasn't passed a
    /// challenge yet, so it could be a recording. Routine when a neighbor
    /// appears or restarts.
    Unverified,
    /// An end-to-end message type this version doesn't know.
    UnknownMessage(u8),
    /// A handshake message didn't verify, or its keys don't match the node
    /// ID it claims to come from.
    BadHandshake,
    /// An end-to-end message didn't decrypt: altered, or for another session.
    BadCiphertext,
    /// An end-to-end message for a session this node doesn't have, perhaps
    /// because it expired or the node restarted.
    NoSession,
    /// A fragment that doesn't fit its datagram, or would exceed the limits.
    BadFragment,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated => f.write_str("frame is truncated"),
            Error::Malformed => f.write_str("TLV is too short for its type"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Error::UnknownFrameKind(k) => write!(f, "unknown frame kind {k}"),
            Error::UnknownHue(h) => write!(f, "unknown hue {}", h.0),
            Error::NoRoute(n) => write!(f, "no route to {n}"),
            Error::PayloadTooLarge => f.write_str("payload is larger than the hue's MTU"),
            Error::BadTag => f.write_str("frame tag doesn't verify with any mesh key"),
            Error::Replay => f.write_str("frame was already received"),
            Error::Unverified => f.write_str("sender not yet verified; challenge sent"),
            Error::UnknownMessage(k) => write!(f, "unknown end-to-end message type {k}"),
            Error::BadHandshake => f.write_str("handshake failed or doesn't match its node ID"),
            Error::BadCiphertext => f.write_str("end-to-end message didn't decrypt"),
            Error::NoSession => f.write_str("no session for end-to-end message"),
            Error::BadFragment => f.write_str("fragment doesn't fit its datagram"),
        }
    }
}

impl core::error::Error for Error {}
