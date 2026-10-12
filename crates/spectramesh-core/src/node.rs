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

/// Parses a node ID as [`Display`](fmt::Display) prints it: `!` and 16 hex
/// digits. The `!` is optional.
impl core::str::FromStr for NodeId {
    type Err = ParseIdError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.strip_prefix('!').unwrap_or(text);
        parse_hex_id(digits).map(NodeId)
    }
}

/// Text that isn't a node or channel ID: they're 16 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseIdError;

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected 16 hex digits")
    }
}

impl core::error::Error for ParseIdError {}

/// Parses exactly 16 hex digits.
pub(crate) fn parse_hex_id(digits: &str) -> Result<[u8; 8], ParseIdError> {
    if digits.len() != 16 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ParseIdError);
    }
    u64::from_str_radix(digits, 16)
        .map(u64::to_be_bytes)
        .map_err(|_| ParseIdError)
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn ids_round_trip_as_text() {
        let id = NodeId::from_u64(0x023e_13d3_6796_696d);
        assert_eq!(id.to_string().parse(), Ok(id));
        assert_eq!("023e13d36796696d".parse(), Ok(id));
        for bad in [
            "",
            "!",
            "!023e13d36796696",
            "!023e13d36796696d0",
            "+023e13d36796696",
            "!023e13d36796696g",
        ] {
            assert_eq!(bad.parse::<NodeId>(), Err(ParseIdError), "{bad:?}");
        }
    }
}
