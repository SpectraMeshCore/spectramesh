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
//! | then 1     | Bytes per link, 16 in this version              |
//! | then each  | Neighbor ID (8), link kind (1), frequency in MHz (2), cost in microseconds (4), signal strength in dBm (1, signed; -128 if not measured) |
//!
//! Receivers read the fields they know from each link and skip the rest, so
//! later versions can add fields to the end of each link.

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

/// Bytes per link this version writes.
const LINK_LEN: usize = 16;
/// The fewest bytes per link a report may use: up to and including the cost.
const MIN_LINK_LEN: usize = 15;
/// Signal strength meaning "not measured".
const NO_RSSI: i8 = i8::MIN;

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
    /// How strongly the reporting node hears the neighbor, in dBm, if the
    /// hue measures it.
    pub rssi: Option<i8>,
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
    let mut payload = Vec::with_capacity(8 + name.len() + links.len() * LINK_LEN);
    payload.push(REPORT);
    payload.extend_from_slice(&message_id.to_be_bytes());
    payload.push(name.len() as u8);
    payload.extend_from_slice(name.as_bytes());
    payload.push(links.len() as u8);
    payload.push(LINK_LEN as u8);
    for link in links {
        payload.extend_from_slice(&link.neighbor.0);
        payload.extend_from_slice(&link.kind.encode());
        payload.extend_from_slice(&link.cost.to_be_bytes());
        // -128 means "not measured", so a real reading of -128 dBm becomes -127.
        let rssi = link.rssi.map_or(NO_RSSI, |dbm| dbm.max(NO_RSSI + 1));
        payload.push(rssi as u8);
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
    let [count, link_len, entries @ ..] = rest else {
        return Err(Error::Truncated);
    };
    let link_len = usize::from(*link_len);
    if link_len < MIN_LINK_LEN {
        return Err(Error::Malformed);
    }
    let entries = entries
        .get(..usize::from(*count) * link_len)
        .ok_or(Error::Truncated)?;
    let links = entries
        .chunks_exact(link_len)
        .filter_map(|e| {
            let kind = LinkKind::decode(e[8], u16::from_be_bytes([e[9], e[10]]))?;
            let rssi = e.get(15).map(|&b| b as i8).filter(|&dbm| dbm != NO_RSSI);
            Some(ReportedLink {
                neighbor: NodeId(e[..8].try_into().expect("8 bytes")),
                kind,
                cost: u32::from_be_bytes(e[11..15].try_into().expect("4 bytes")),
                rssi,
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
                rssi: Some(-52),
            },
            ReportedLink {
                neighbor: NodeId::from_u64(3),
                kind: LinkKind::LoRa { freq_mhz: 915 },
                cost: 160_000,
                rssi: None,
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
        assert_eq!(decode(&[0, 0, 0, 1, 0, 1, 16]), Err(Error::Truncated));
        // Links too short to hold a cost.
        assert_eq!(decode(&[0, 0, 0, 1, 0, 0, 14]), Err(Error::Malformed));
    }

    #[test]
    fn longer_links_from_newer_nodes_are_read() {
        // A later version that appends two bytes to each link.
        let mut body = vec![0, 0, 0, 9, 0, 1, 18];
        body.extend_from_slice(&NodeId::from_u64(5).0);
        body.extend_from_slice(&[2, 0, 0, 0, 0, 3, 32, (-70i8) as u8, 0xaa, 0xbb]);
        let (_, _, links) = decode(&body).unwrap();
        assert_eq!(
            links,
            [ReportedLink {
                neighbor: NodeId::from_u64(5),
                kind: LinkKind::EspNow,
                cost: 800,
                rssi: Some(-70),
            }]
        );
    }
}
