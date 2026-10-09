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
        }
    }
}

impl core::error::Error for Error {}
