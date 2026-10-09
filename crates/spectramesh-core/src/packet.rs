//! Wire format.
//!
//! Every frame starts with the same 9 bytes. Multi-byte fields are big-endian.
//!
//! | Bytes | Field                                                                |
//! |-------|----------------------------------------------------------------------|
//! | 0     | Protocol version (high 4 bits) and frame kind (low 4 bits)          |
//! | 1..5  | Source                                                               |
//! | 5..9  | Next hop: the node that should handle this transmission, or broadcast |
//!
//! The next hop is in every header because hues like LoRa have no link-layer
//! addressing, so every receiver needs to know whether a frame is for it.
//!
//! **Control frames** carry routing messages between neighbors and never go
//! further than one hop. The source is the sending neighbor. The rest of the
//! frame is a list of [`Tlv`]s (type, length, value), as in Babel, so several
//! messages share one frame. Receivers skip TLV types they don't know, so
//! newer nodes can add types without breaking older ones.
//!
//! **Data frames** carry application data across the mesh. The source is the
//! node that created the packet, and the common header continues with:
//!
//! | Bytes  | Field       |
//! |--------|-------------|
//! | 9..13  | Destination |
//! | 13     | TTL: how many more hops the packet may take |
//!
//! followed by the payload.

use alloc::vec::Vec;
use core::time::Duration;

use crate::error::{Error, Result};
use crate::node::NodeId;

pub const VERSION: u8 = 1;
pub const CONTROL_HEADER_LEN: usize = 9;
pub const DATA_HEADER_LEN: usize = 14;

const KIND_CONTROL: u8 = 1;
const KIND_DATA: u8 = 2;

#[derive(Clone, Debug)]
pub enum Frame<'a> {
    Control {
        src: NodeId,
        next_hop: NodeId,
        tlvs: Tlvs<'a>,
    },
    Data {
        header: DataHeader,
        payload: &'a [u8],
    },
}

impl<'a> Frame<'a> {
    pub fn decode(frame: &'a [u8]) -> Result<Self> {
        if frame.len() < CONTROL_HEADER_LEN {
            return Err(Error::Truncated);
        }
        let version = frame[0] >> 4;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let src = node_at(frame, 1);
        let next_hop = node_at(frame, 5);
        match frame[0] & 0x0f {
            KIND_CONTROL => Ok(Frame::Control {
                src,
                next_hop,
                tlvs: Tlvs(&frame[CONTROL_HEADER_LEN..]),
            }),
            KIND_DATA => {
                if frame.len() < DATA_HEADER_LEN {
                    return Err(Error::Truncated);
                }
                let header = DataHeader {
                    src,
                    next_hop,
                    dst: node_at(frame, 9),
                    ttl: frame[13],
                };
                Ok(Frame::Data {
                    header,
                    payload: &frame[DATA_HEADER_LEN..],
                })
            }
            other => Err(Error::UnknownFrameKind(other)),
        }
    }
}

/// The neighbor that sent `frame`, if it's a control frame.
///
/// Every neighbor sends control frames regularly, so platforms on hues with
/// link-layer addresses can use this to learn which address belongs to which
/// node, and then unicast frames to [`Transmit::next_hop`].
///
/// [`Transmit::next_hop`]: crate::router::Transmit::next_hop
pub fn control_sender(frame: &[u8]) -> Option<NodeId> {
    match Frame::decode(frame) {
        Ok(Frame::Control { src, .. }) => Some(src),
        _ => None,
    }
}

/// Starts a control frame. Append TLVs with [`Tlv::encode`].
pub fn encode_control_header(src: NodeId, next_hop: NodeId, buf: &mut Vec<u8>) {
    buf.push(VERSION << 4 | KIND_CONTROL);
    buf.extend_from_slice(&src.0);
    buf.extend_from_slice(&next_hop.0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataHeader {
    pub src: NodeId,
    pub next_hop: NodeId,
    pub dst: NodeId,
    pub ttl: u8,
}

impl DataHeader {
    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(VERSION << 4 | KIND_DATA);
        buf.extend_from_slice(&self.src.0);
        buf.extend_from_slice(&self.next_hop.0);
        buf.extend_from_slice(&self.dst.0);
        buf.push(self.ttl);
    }
}

/// A routing message. Intervals travel as centiseconds, as in Babel, so they
/// top out at about 11 minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tlv {
    /// "I'm here." Sent on each hue every `interval`, numbered by `seqno`.
    Hello { seqno: u16, interval: Duration },
    /// "I hear you": how well this node hears `neighbor`'s hellos. `rxcost`
    /// is 256 for a perfect link and `0xFFFF` for an unusable one. The next
    /// IHU follows within `interval`.
    Ihu {
        neighbor: NodeId,
        rxcost: u16,
        interval: Duration,
    },
    /// "I can reach `dest` at this cost." A metric of [`routing::INFINITY`]
    /// withdraws the route. The next update follows within `interval`.
    ///
    /// [`routing::INFINITY`]: crate::routing::INFINITY
    Update {
        dest: NodeId,
        seqno: u16,
        metric: u32,
        interval: Duration,
    },
    /// "Please send your route to `dest` now", or the whole table if `dest`
    /// is broadcast.
    RouteRequest { dest: NodeId },
    /// "Please get `dest` to issue a route with at least this seqno." Passed
    /// towards `dest` for up to `hop_count` hops.
    SeqnoRequest {
        dest: NodeId,
        seqno: u16,
        hop_count: u8,
    },
}

impl Tlv {
    const HELLO: u8 = 1;
    const IHU: u8 = 2;
    const UPDATE: u8 = 3;
    const ROUTE_REQUEST: u8 = 4;
    const SEQNO_REQUEST: u8 = 5;

    /// Bytes this TLV takes in a frame, including its type and length.
    pub fn encoded_len(&self) -> usize {
        2 + self.body_len()
    }

    fn body_len(&self) -> usize {
        match self {
            Tlv::Hello { .. } => 4,
            Tlv::Ihu { .. } => 8,
            Tlv::Update { .. } => 12,
            Tlv::RouteRequest { .. } => 4,
            Tlv::SeqnoRequest { .. } => 7,
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Tlv::Hello { .. } => Self::HELLO,
            Tlv::Ihu { .. } => Self::IHU,
            Tlv::Update { .. } => Self::UPDATE,
            Tlv::RouteRequest { .. } => Self::ROUTE_REQUEST,
            Tlv::SeqnoRequest { .. } => Self::SEQNO_REQUEST,
        }
    }

    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.kind());
        buf.push(self.body_len() as u8);
        match *self {
            Tlv::Hello { seqno, interval } => {
                buf.extend_from_slice(&seqno.to_be_bytes());
                put_interval(buf, interval);
            }
            Tlv::Ihu {
                neighbor,
                rxcost,
                interval,
            } => {
                buf.extend_from_slice(&neighbor.0);
                buf.extend_from_slice(&rxcost.to_be_bytes());
                put_interval(buf, interval);
            }
            Tlv::Update {
                dest,
                seqno,
                metric,
                interval,
            } => {
                buf.extend_from_slice(&dest.0);
                buf.extend_from_slice(&seqno.to_be_bytes());
                buf.extend_from_slice(&metric.to_be_bytes());
                put_interval(buf, interval);
            }
            Tlv::RouteRequest { dest } => buf.extend_from_slice(&dest.0),
            Tlv::SeqnoRequest {
                dest,
                seqno,
                hop_count,
            } => {
                buf.extend_from_slice(&dest.0);
                buf.extend_from_slice(&seqno.to_be_bytes());
                buf.push(hop_count);
            }
        }
    }

    /// Decodes one TLV body. Returns `None` for types this version doesn't know.
    /// Bodies longer than expected are accepted, leaving room for future fields.
    fn decode(kind: u8, body: &[u8]) -> Result<Option<Tlv>> {
        let need = |len: usize| {
            if body.len() < len {
                Err(Error::Malformed)
            } else {
                Ok(())
            }
        };
        let tlv = match kind {
            Self::HELLO => {
                need(4)?;
                Tlv::Hello {
                    seqno: u16_at(body, 0),
                    interval: interval_at(body, 2),
                }
            }
            Self::IHU => {
                need(8)?;
                Tlv::Ihu {
                    neighbor: node_at(body, 0),
                    rxcost: u16_at(body, 4),
                    interval: interval_at(body, 6),
                }
            }
            Self::UPDATE => {
                need(12)?;
                Tlv::Update {
                    dest: node_at(body, 0),
                    seqno: u16_at(body, 4),
                    metric: u32::from_be_bytes([body[6], body[7], body[8], body[9]]),
                    interval: interval_at(body, 10),
                }
            }
            Self::ROUTE_REQUEST => {
                need(4)?;
                Tlv::RouteRequest {
                    dest: node_at(body, 0),
                }
            }
            Self::SEQNO_REQUEST => {
                need(7)?;
                Tlv::SeqnoRequest {
                    dest: node_at(body, 0),
                    seqno: u16_at(body, 4),
                    hop_count: body[6],
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(tlv))
    }
}

/// The TLVs in a control frame. Stops at the first malformed one.
#[derive(Clone, Debug)]
pub struct Tlvs<'a>(&'a [u8]);

impl Iterator for Tlvs<'_> {
    type Item = Result<Tlv>;

    fn next(&mut self) -> Option<Result<Tlv>> {
        loop {
            let (kind, body, rest) = match self.0 {
                [] => return None,
                [kind, len, rest @ ..] if rest.len() >= usize::from(*len) => {
                    let (body, rest) = rest.split_at(usize::from(*len));
                    (*kind, body, rest)
                }
                _ => {
                    self.0 = &[];
                    return Some(Err(Error::Truncated));
                }
            };
            self.0 = rest;
            match Tlv::decode(kind, body) {
                Ok(Some(tlv)) => return Some(Ok(tlv)),
                Ok(None) => continue,
                Err(err) => {
                    self.0 = &[];
                    return Some(Err(err));
                }
            }
        }
    }
}

fn node_at(buf: &[u8], at: usize) -> NodeId {
    NodeId([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

fn interval_at(buf: &[u8], at: usize) -> Duration {
    Duration::from_millis(u64::from(u16_at(buf, at)) * 10)
}

fn put_interval(buf: &mut Vec<u8>, interval: Duration) {
    let centis = (interval.as_millis() / 10).min(u16::MAX.into()) as u16;
    buf.extend_from_slice(&centis.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const A: NodeId = NodeId::from_u32(0x0102_0304);
    const B: NodeId = NodeId::from_u32(0x0a0b_0c0d);

    fn control_frame(tlvs: &[Tlv]) -> Vec<u8> {
        let mut frame = Vec::new();
        encode_control_header(A, NodeId::BROADCAST, &mut frame);
        for tlv in tlvs {
            tlv.encode(&mut frame);
        }
        frame
    }

    #[test]
    fn control_frames_round_trip() {
        let tlvs = [
            Tlv::Hello {
                seqno: 7,
                interval: Duration::from_secs(4),
            },
            Tlv::Ihu {
                neighbor: B,
                rxcost: 256,
                interval: Duration::from_secs(4),
            },
            Tlv::Update {
                dest: B,
                seqno: 0xfffe,
                metric: 123_456,
                interval: Duration::from_secs(16),
            },
            Tlv::RouteRequest {
                dest: NodeId::BROADCAST,
            },
            Tlv::SeqnoRequest {
                dest: B,
                seqno: 3,
                hop_count: 16,
            },
        ];
        let frame = control_frame(&tlvs);
        let expected_len = CONTROL_HEADER_LEN + tlvs.iter().map(Tlv::encoded_len).sum::<usize>();
        assert_eq!(frame.len(), expected_len);

        let Frame::Control {
            src,
            next_hop,
            tlvs: decoded,
        } = Frame::decode(&frame).unwrap()
        else {
            panic!("expected a control frame");
        };
        assert_eq!((src, next_hop), (A, NodeId::BROADCAST));
        assert_eq!(decoded.collect::<Result<Vec<_>>>().unwrap(), tlvs);
    }

    #[test]
    fn unknown_tlvs_are_skipped() {
        let hello = Tlv::Hello {
            seqno: 1,
            interval: Duration::from_secs(4),
        };
        let mut frame = control_frame(&[]);
        frame.extend_from_slice(&[200, 3, 0xaa, 0xbb, 0xcc]);
        hello.encode(&mut frame);

        let Frame::Control { tlvs, .. } = Frame::decode(&frame).unwrap() else {
            panic!("expected a control frame");
        };
        assert_eq!(tlvs.collect::<Result<Vec<_>>>().unwrap(), vec![hello]);
    }

    #[test]
    fn bad_tlvs_are_reported() {
        let mut truncated = control_frame(&[]);
        truncated.extend_from_slice(&[Tlv::HELLO, 4, 0, 1]);
        let mut short = control_frame(&[]);
        short.extend_from_slice(&[Tlv::HELLO, 2, 0, 1]);

        for (frame, err) in [(truncated, Error::Truncated), (short, Error::Malformed)] {
            let Frame::Control { mut tlvs, .. } = Frame::decode(&frame).unwrap() else {
                panic!("expected a control frame");
            };
            assert_eq!(tlvs.next(), Some(Err(err)));
            assert_eq!(tlvs.next(), None);
        }
    }

    #[test]
    fn data_frames_round_trip() {
        let header = DataHeader {
            src: A,
            next_hop: B,
            dst: NodeId::from_u32(9),
            ttl: 16,
        };
        let mut frame = Vec::new();
        header.encode(&mut frame);
        assert_eq!(frame.len(), DATA_HEADER_LEN);
        frame.extend_from_slice(b"hi");

        let Frame::Data {
            header: decoded,
            payload,
        } = Frame::decode(&frame).unwrap()
        else {
            panic!("expected a data frame");
        };
        assert_eq!((decoded, payload), (header, &b"hi"[..]));
    }

    #[test]
    fn rejects_bad_headers() {
        let mut frame = control_frame(&[]);
        assert!(matches!(Frame::decode(&frame[..8]), Err(Error::Truncated)));
        frame[0] = 2 << 4 | KIND_CONTROL;
        assert!(matches!(
            Frame::decode(&frame),
            Err(Error::UnsupportedVersion(2))
        ));
        frame[0] = VERSION << 4 | 9;
        assert!(matches!(
            Frame::decode(&frame),
            Err(Error::UnknownFrameKind(9))
        ));
    }
}
