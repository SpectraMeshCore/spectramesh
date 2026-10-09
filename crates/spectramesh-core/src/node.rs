use core::fmt;

/// A node's address on the mesh: the first 8 bytes of the hash of its public
/// keys (see [`identity`](crate::identity)). Printed as `!` followed by 16 hex
/// digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub [u8; 8]);

impl NodeId {
    pub const LEN: usize = 8;

    /// Addresses every node in range.
    pub const BROADCAST: NodeId = NodeId([0xff; 8]);

    pub const fn from_u64(id: u64) -> Self {
        Self(id.to_be_bytes())
    }

    pub const fn to_u64(self) -> u64 {
        u64::from_be_bytes(self.0)
    }

    pub const fn is_broadcast(self) -> bool {
        self.to_u64() == u64::MAX
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "!{:016x}", self.to_u64())
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}
