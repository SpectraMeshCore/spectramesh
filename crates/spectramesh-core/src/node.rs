use core::fmt;

/// A node's address on the mesh.
///
/// Four bytes, like Meshtastic, to keep headers small on slow hues such as
/// LoRa. It's printed the same way, as `!` followed by eight hex digits.
//
// TODO: derive from the node's public key once packets are signed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub [u8; 4]);

impl NodeId {
    pub const LEN: usize = 4;

    /// Addresses every node in range.
    pub const BROADCAST: NodeId = NodeId([0xff; 4]);

    pub const fn from_u32(id: u32) -> Self {
        Self(id.to_be_bytes())
    }

    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    pub const fn is_broadcast(self) -> bool {
        self.to_u32() == u32::MAX
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "!{:08x}", self.to_u32())
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}
