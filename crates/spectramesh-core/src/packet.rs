//! Wire format, version 2. See `docs/design/wire-format-v2.md` for the reasoning.
//!
//! Every frame is a header, a body and a trailer. Multi-byte fields are big-endian.
//!
//! **Header** (18 bytes):
//!
//! | Bytes  | Field                                                       |
//! |--------|-------------------------------------------------------------|
//! | 0      | Protocol version (high 4 bits) and frame kind (low 4 bits)  |
//! | 1      | Mesh key ID                                                 |
//! | 2..10  | Sender: the node that transmitted this frame                |
//! | 10..18 | Next hop: the node that should handle it, or broadcast      |
//!
//! The next hop is in every header because hues like LoRa have no link-layer
//! addressing, so every receiver needs to know whether a frame is for it.
//!
//! **Body**, for **control frames**: routing messages between neighbors, as a
//! list of [`Tlv`]s (type, length, value). Receivers skip TLV types they don't
//! know, so newer nodes can add types without breaking older ones.
//!
//! **Body**, for **data frames**: application data crossing the mesh.
//!
//! | Bytes | Field                                        |
//! |-------|----------------------------------------------|
//! | 0..8  | Origin: the node that created the packet     |
//! | 8..16 | Destination                                  |
//! | 16    | TTL: how many more hops the packet may take  |
//! | 17..  | Payload                                      |
//!
//! **Trailer** (16 bytes): the sender's boot index (4), a counter (4) and a
//! tag (8) over everything before it. See [`auth`](crate::auth).

use alloc::vec::Vec;
use core::time::Duration;

use crate::auth::{TAG_LEN, TRAILER_LEN};
use crate::error::{Error, Result};
use crate::node::NodeId;

pub const VERSION: u8 = 2;
pub const HEADER_LEN: usize = 18;
/// Bytes of a data frame's body before the payload.
pub const DATA_HEADER_LEN: usize = 17;
/// Bytes every control frame spends on its header and trailer.
pub const CONTROL_OVERHEAD: usize = HEADER_LEN + TRAILER_LEN;
/// Bytes every data frame spends on its headers and trailer.
pub const DATA_OVERHEAD: usize = HEADER_LEN + DATA_HEADER_LEN + TRAILER_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    Control = 1,
    Data = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: FrameKind,
    pub key_id: u8,
    pub sender: NodeId,
    pub next_hop: NodeId,
}

impl Header {
    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(VERSION << 4 | self.kind as u8);
        buf.push(self.key_id);
        buf.extend_from_slice(&self.sender.0);
        buf.extend_from_slice(&self.next_hop.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataHeader {
    pub origin: NodeId,
    pub dst: NodeId,
    pub ttl: u8,
}

impl DataHeader {
    pub fn encode(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.origin.0);
        buf.extend_from_slice(&self.dst.0);
        buf.push(self.ttl);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trailer {
    pub index: u32,
    pub counter: u32,
    pub tag: [u8; TAG_LEN],
}

impl Trailer {
    /// Appends the boot index and counter: the part of the trailer the tag covers.
    pub fn encode_signed_part(index: u32, counter: u32, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&index.to_be_bytes());
        buf.extend_from_slice(&counter.to_be_bytes());
    }
}

#[derive(Clone, Debug)]
pub enum Body<'a> {
    Control(Tlvs<'a>),
    Data {
        header: DataHeader,
        payload: &'a [u8],
    },
}

/// A decoded frame. Nothing in it is authenticated until its tag is checked.
#[derive(Clone, Debug)]
pub struct Frame<'a> {
    pub header: Header,
    pub body: Body<'a>,
    pub trailer: Trailer,
    /// Everything the tag covers: the whole frame except the tag itself.
    pub signed: &'a [u8],
}

impl<'a> Frame<'a> {
    pub fn decode(frame: &'a [u8]) -> Result<Self> {
        if frame.len() < CONTROL_OVERHEAD {
            return Err(Error::Truncated);
        }
        let version = frame[0] >> 4;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let kind = match frame[0] & 0x0f {
            1 => FrameKind::Control,
            2 => FrameKind::Data,
            other => return Err(Error::UnknownFrameKind(other)),
        };
        let header = Header {
            kind,
            key_id: frame[1],
            sender: node_at(frame, 2),
            next_hop: node_at(frame, 10),
        };

        let (signed, tag) = frame.split_at(frame.len() - TAG_LEN);
        let trailer_at = signed.len() - (TRAILER_LEN - TAG_LEN);
        let trailer = Trailer {
            index: u32_at(frame, trailer_at),
            counter: u32_at(frame, trailer_at + 4),
            tag: tag.try_into().expect("split at TAG_LEN"),
        };

        let body = &frame[HEADER_LEN..trailer_at];
        let body = match kind {
            FrameKind::Control => Body::Control(Tlvs(body)),
            FrameKind::Data => {
                if body.len() < DATA_HEADER_LEN {
                    return Err(Error::Truncated);
                }
                Body::Data {
                    header: DataHeader {
                        origin: node_at(body, 0),
                        dst: node_at(body, 8),
                        ttl: body[16],
                    },
                    payload: &body[DATA_HEADER_LEN..],
                }
            }
        };
        Ok(Frame {
            header,
            body,
            trailer,
            signed,
        })
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
    /// "Prove this boot index is live: echo this nonce."
    ChallengeRequest { nonce: u64 },
    /// The echo of a [`Tlv::ChallengeRequest`] nonce.
    ChallengeReply { nonce: u64 },
}

impl Tlv {
    const HELLO: u8 = 1;
    const IHU: u8 = 2;
    const UPDATE: u8 = 3;
    const ROUTE_REQUEST: u8 = 4;
    const SEQNO_REQUEST: u8 = 5;
    const CHALLENGE_REQUEST: u8 = 6;
    const CHALLENGE_REPLY: u8 = 7;

    /// Bytes this TLV takes in a frame, including its type and length.
    pub fn encoded_len(&self) -> usize {
        2 + self.body_len()
    }

    fn body_len(&self) -> usize {
        match self {
            Tlv::Hello { .. } => 4,
            Tlv::Ihu { .. } => 12,
            Tlv::Update { .. } => 16,
            Tlv::RouteRequest { .. } => 8,
            Tlv::SeqnoRequest { .. } => 11,
            Tlv::ChallengeRequest { .. } | Tlv::ChallengeReply { .. } => 8,
        }
    }

    fn kind(&self) -> u8 {
        match self {
            Tlv::Hello { .. } => Self::HELLO,
            Tlv::Ihu { .. } => Self::IHU,
            Tlv::Update { .. } => Self::UPDATE,
            Tlv::RouteRequest { .. } => Self::ROUTE_REQUEST,
            Tlv::SeqnoRequest { .. } => Self::SEQNO_REQUEST,
            Tlv::ChallengeRequest { .. } => Self::CHALLENGE_REQUEST,
            Tlv::ChallengeReply { .. } => Self::CHALLENGE_REPLY,
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
            Tlv::ChallengeRequest { nonce } | Tlv::ChallengeReply { nonce } => {
                buf.extend_from_slice(&nonce.to_be_bytes());
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
                need(12)?;
                Tlv::Ihu {
                    neighbor: node_at(body, 0),
                    rxcost: u16_at(body, 8),
                    interval: interval_at(body, 10),
                }
            }
            Self::UPDATE => {
                need(16)?;
                Tlv::Update {
                    dest: node_at(body, 0),
                    seqno: u16_at(body, 8),
                    metric: u32_at(body, 10),
                    interval: interval_at(body, 14),
                }
            }
            Self::ROUTE_REQUEST => {
                need(8)?;
                Tlv::RouteRequest {
                    dest: node_at(body, 0),
                }
            }
            Self::SEQNO_REQUEST => {
                need(11)?;
                Tlv::SeqnoRequest {
                    dest: node_at(body, 0),
                    seqno: u16_at(body, 8),
                    hop_count: body[10],
                }
            }
            Self::CHALLENGE_REQUEST => {
                need(8)?;
                Tlv::ChallengeRequest {
                    nonce: u64_at(body, 0),
                }
            }
            Self::CHALLENGE_REPLY => {
                need(8)?;
                Tlv::ChallengeReply {
                    nonce: u64_at(body, 0),
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
    NodeId(
        buf[at..at + NodeId::LEN]
            .try_into()
            .expect("slice is 8 bytes"),
    )
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(buf[at..at + 4].try_into().expect("slice is 4 bytes"))
}

fn u64_at(buf: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(buf[at..at + 8].try_into().expect("slice is 8 bytes"))
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

    const A: NodeId = NodeId::from_u64(0x0102_0304_0506_0708);
    const B: NodeId = NodeId::from_u64(0x0a0b_0c0d_0e0f_1011);

    fn finish(mut frame: Vec<u8>) -> Vec<u8> {
        Trailer::encode_signed_part(0xaabb_ccdd, 42, &mut frame);
        frame.extend_from_slice(&[0x55; TAG_LEN]);
        frame
    }

    fn header(kind: FrameKind, sender: NodeId, next_hop: NodeId) -> Vec<u8> {
        let mut frame = Vec::new();
        Header {
            kind,
            key_id: 9,
            sender,
            next_hop,
        }
        .encode(&mut frame);
        frame
    }

    fn control_frame(tlvs: &[Tlv]) -> Vec<u8> {
        let mut frame = header(FrameKind::Control, A, NodeId::BROADCAST);
        for tlv in tlvs {
            tlv.encode(&mut frame);
        }
        finish(frame)
    }

    fn tlvs(frame: &Frame<'_>) -> Vec<Tlv> {
        let Body::Control(tlvs) = frame.body.clone() else {
            panic!("expected a control frame");
        };
        tlvs.collect::<Result<_>>().unwrap()
    }

    #[test]
    fn control_frames_round_trip() {
        let sent = [
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
            Tlv::ChallengeRequest {
                nonce: 0x0123_4567_89ab_cdef,
            },
            Tlv::ChallengeReply { nonce: 1 },
        ];
        let bytes = control_frame(&sent);
        let expected_len = CONTROL_OVERHEAD + sent.iter().map(Tlv::encoded_len).sum::<usize>();
        assert_eq!(bytes.len(), expected_len);

        let frame = Frame::decode(&bytes).unwrap();
        assert_eq!(
            frame.header,
            Header {
                kind: FrameKind::Control,
                key_id: 9,
                sender: A,
                next_hop: NodeId::BROADCAST,
            }
        );
        assert_eq!(
            frame.trailer,
            Trailer {
                index: 0xaabb_ccdd,
                counter: 42,
                tag: [0x55; TAG_LEN],
            }
        );
        assert_eq!(frame.signed, &bytes[..bytes.len() - TAG_LEN]);
        assert_eq!(tlvs(&frame), sent);
    }

    #[test]
    fn unknown_tlvs_are_skipped() {
        let hello = Tlv::Hello {
            seqno: 1,
            interval: Duration::from_secs(4),
        };
        let mut frame = header(FrameKind::Control, A, B);
        frame.extend_from_slice(&[200, 3, 0xaa, 0xbb, 0xcc]);
        hello.encode(&mut frame);
        let frame = finish(frame);

        assert_eq!(tlvs(&Frame::decode(&frame).unwrap()), vec![hello]);
    }

    #[test]
    fn bad_tlvs_are_reported() {
        for (body, err) in [
            (&[Tlv::HELLO, 4, 0, 1][..], Error::Truncated),
            (&[Tlv::HELLO, 2, 0, 1][..], Error::Malformed),
        ] {
            let mut frame = header(FrameKind::Control, A, B);
            frame.extend_from_slice(body);
            let frame = finish(frame);
            let decoded = Frame::decode(&frame).unwrap();
            let Body::Control(mut tlvs) = decoded.body else {
                panic!("expected a control frame");
            };
            assert_eq!(tlvs.next(), Some(Err(err)));
            assert_eq!(tlvs.next(), None);
        }
    }

    #[test]
    fn data_frames_round_trip() {
        let data = DataHeader {
            origin: A,
            dst: NodeId::from_u64(9),
            ttl: 16,
        };
        let mut frame = header(FrameKind::Data, B, A);
        data.encode(&mut frame);
        frame.extend_from_slice(b"hi");
        let frame = finish(frame);
        assert_eq!(frame.len(), DATA_OVERHEAD + 2);

        let decoded = Frame::decode(&frame).unwrap();
        assert_eq!(decoded.header.sender, B);
        let Body::Data { header, payload } = decoded.body else {
            panic!("expected a data frame");
        };
        assert_eq!((header, payload), (data, &b"hi"[..]));
    }

    #[test]
    fn rejects_bad_headers() {
        let mut frame = control_frame(&[]);
        assert!(matches!(
            Frame::decode(&frame[..CONTROL_OVERHEAD - 1]),
            Err(Error::Truncated)
        ));
        frame[0] = 1 << 4 | FrameKind::Control as u8;
        assert!(matches!(
            Frame::decode(&frame),
            Err(Error::UnsupportedVersion(1))
        ));
        frame[0] = VERSION << 4 | 9;
        assert!(matches!(
            Frame::decode(&frame),
            Err(Error::UnknownFrameKind(9))
        ));
    }
}
