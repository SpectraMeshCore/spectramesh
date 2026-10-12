//! Neighbor reports: how every node learns the whole mesh's map.
//!
//! Routing only tells a node its own neighbors and, for each destination, the
//! neighbor to send through, not what the rest of the mesh looks like. So
//! each node periodically floods a small report: its name, and its links with
//! their kind and cost. Every node keeps the latest report from each node,
//! which together make a map of the mesh.
//!
//! Reports are broadcasts of kind 2, protected by the mesh key like every
//! frame, and flooded like channel messages:
//!
//! | Bytes      | Field                                           |
//! |------------|-------------------------------------------------|
//! | 0          | Broadcast kind: 2                               |
//! | 1..5       | Message ID, random                              |
//! | 5          | Name length, at most 32                         |
//! | 6..        | Name, UTF-8                                     |
//! | then 1     | Number of links                                 |
//! | then 15 each | Neighbor ID (8), link kind (1), frequency in MHz (2), cost in microseconds (4) |

use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::hue::HueKind;
use crate::node::NodeId;
use crate::time::Instant;

/// Broadcast kind for reports; see [`channel`](crate::channel) for the others.
pub(crate) const REPORT: u8 = 2;

/// The longest node name, in bytes.
pub const MAX_NAME_LEN: usize = 32;

const LINK_LEN: usize = 15;

/// What kind of hue a reported link is on, for drawing the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Ethernet,
    Wifi { freq_mhz: u16 },
    EspNow,
    Fsk { freq_mhz: u16 },
    LoRa { freq_mhz: u16 },
    Ip,
}

impl From<HueKind> for LinkKind {
    fn from(kind: HueKind) -> Self {
        let mhz = |khz: u32| (khz / 1000).min(u16::MAX.into()) as u16;
        match kind {
            HueKind::Ethernet => LinkKind::Ethernet,
            HueKind::Wifi { freq_mhz } => LinkKind::Wifi { freq_mhz },
            HueKind::EspNow => LinkKind::EspNow,
            HueKind::Fsk { freq_khz } => LinkKind::Fsk {
                freq_mhz: mhz(freq_khz),
            },
            HueKind::LoRa { freq_khz } => LinkKind::LoRa {
                freq_mhz: mhz(freq_khz),
            },
            HueKind::Ip => LinkKind::Ip,
        }
    }
}

impl LinkKind {
    fn encode(self) -> [u8; 3] {
        let (kind, freq) = match self {
            LinkKind::Ethernet => (0, 0),
            LinkKind::Wifi { freq_mhz } => (1, freq_mhz),
            LinkKind::EspNow => (2, 0),
            LinkKind::Fsk { freq_mhz } => (3, freq_mhz),
            LinkKind::LoRa { freq_mhz } => (4, freq_mhz),
            LinkKind::Ip => (5, 0),
        };
        let [hi, lo] = freq.to_be_bytes();
        [kind, hi, lo]
    }

    fn decode(kind: u8, freq_mhz: u16) -> Option<Self> {
        Some(match kind {
            0 => LinkKind::Ethernet,
            1 => LinkKind::Wifi { freq_mhz },
            2 => LinkKind::EspNow,
            3 => LinkKind::Fsk { freq_mhz },
            4 => LinkKind::LoRa { freq_mhz },
            5 => LinkKind::Ip,
            _ => return None,
        })
    }
}

/// One link a node reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportedLink {
    pub neighbor: NodeId,
    pub kind: LinkKind,
    /// Expected transmission time over the link, in microseconds.
    pub cost: u32,
}

/// The latest report from one node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeReport {
    pub node: NodeId,
    pub name: String,
    pub links: Vec<ReportedLink>,
    /// When this node received the report (or made it, for its own).
    pub received: Instant,
}

/// Cuts `name` to at most [`MAX_NAME_LEN`] bytes, at a character boundary.
pub(crate) fn clip_name(name: &str) -> &str {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Encodes a report's broadcast payload. Links past what fits are left out.
pub(crate) fn encode(message_id: u32, name: &str, links: &[ReportedLink]) -> Vec<u8> {
    let name = clip_name(name);
    let links = &links[..links.len().min(u8::MAX.into())];
    let mut payload = Vec::with_capacity(7 + name.len() + links.len() * LINK_LEN);
    payload.push(REPORT);
    payload.extend_from_slice(&message_id.to_be_bytes());
    payload.push(name.len() as u8);
    payload.extend_from_slice(name.as_bytes());
    payload.push(links.len() as u8);
    for link in links {
        payload.extend_from_slice(&link.neighbor.0);
        payload.extend_from_slice(&link.kind.encode());
        payload.extend_from_slice(&link.cost.to_be_bytes());
    }
    payload
}

/// Decodes a report from its payload after the kind byte. Returns the
/// message ID, name and links. Links of kinds this version doesn't know
/// are skipped.
pub(crate) fn decode(body: &[u8]) -> Result<(u32, String, Vec<ReportedLink>)> {
    let message_id = u32::from_be_bytes(body.get(..4).ok_or(Error::Truncated)?.try_into().unwrap());
    let name_len = usize::from(*body.get(4).ok_or(Error::Truncated)?);
    if name_len > MAX_NAME_LEN {
        return Err(Error::Malformed);
    }
    let name = body.get(5..5 + name_len).ok_or(Error::Truncated)?;
    let name = String::from_utf8_lossy(name).into_owned();
    let rest = &body[5 + name_len..];
    let (&count, entries) = rest.split_first().ok_or(Error::Truncated)?;
    let entries = entries
        .get(..usize::from(count) * LINK_LEN)
        .ok_or(Error::Truncated)?;
    let links = entries
        .chunks_exact(LINK_LEN)
        .filter_map(|e| {
            let kind = LinkKind::decode(e[8], u16::from_be_bytes([e[9], e[10]]))?;
            Some(ReportedLink {
                neighbor: NodeId(e[..8].try_into().expect("8 bytes")),
                kind,
                cost: u32::from_be_bytes(e[11..15].try_into().expect("4 bytes")),
            })
        })
        .collect();
    Ok((message_id, name, links))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn reports_round_trip() {
        let links = vec![
            ReportedLink {
                neighbor: NodeId::from_u64(2),
                kind: LinkKind::Wifi { freq_mhz: 5805 },
                cost: 40,
            },
            ReportedLink {
                neighbor: NodeId::from_u64(3),
                kind: LinkKind::LoRa { freq_mhz: 915 },
                cost: 160_000,
            },
        ];
        let payload = encode(77, "roof node", &links);
        assert_eq!(payload[0], REPORT);
        assert_eq!(
            decode(&payload[1..]).unwrap(),
            (77, String::from("roof node"), links)
        );
    }

    #[test]
    fn names_are_clipped_at_character_boundaries() {
        let long = "ü".repeat(20); // 40 bytes
        assert_eq!(clip_name(&long).len(), 32);
        let payload = encode(1, &long, &[]);
        assert_eq!(decode(&payload[1..]).unwrap().1, "ü".repeat(16));
    }

    #[test]
    fn bad_reports_are_rejected() {
        assert_eq!(decode(&[0, 0, 0, 1]), Err(Error::Truncated));
        assert_eq!(decode(&[0, 0, 0, 1, 33]), Err(Error::Malformed));
        // Claims a link but carries none.
        assert_eq!(decode(&[0, 0, 0, 1, 0, 1]), Err(Error::Truncated));
    }
}
