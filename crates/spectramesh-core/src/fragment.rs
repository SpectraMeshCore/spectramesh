//! Splitting frames too big for a hue, and putting them back together.
//!
//! Fragmentation is per hop: a node splits a data frame's body, or a control
//! frame's routing messages, into fragment frames sized for the next hue, and the next node reassembles it
//! before routing it further, perhaps splitting it again for a smaller hue.
//! Each fragment carries its own link tag, so nodes outside the mesh can't
//! inject fragments.
//!
//! A fragment frame's body is a 6-byte fragment header and a chunk:
//!
//! | Bytes | Field                                                     |
//! |-------|-----------------------------------------------------------|
//! | 0..2  | Datagram ID, counted by the sender                        |
//! | 2..4  | Offset of this chunk in the reassembled body              |
//! | 4..6  | Length of the reassembled body; the top bit is set if the body is routing messages (a control frame's) rather than data |
//! | 6..   | The chunk                                                 |
//!
//! Reassembly is bounded: at most [`MAX_DATAGRAM`] bytes per datagram, at most
//! [`MAX_PENDING`] datagrams at once, and each must complete within
//! [`REASSEMBLY_TIMEOUT`].

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

use crate::error::{Error, Result};
use crate::hue::HueId;
use crate::node::NodeId;
use crate::time::Instant;

/// Bytes the fragment header adds to each fragment.
pub const FRAGMENT_HEADER_LEN: usize = 6;
/// The largest data frame body, reassembled.
pub const MAX_DATAGRAM: usize = 1600;
/// How many datagrams can be partly reassembled at once.
pub const MAX_PENDING: usize = 8;
/// How long a datagram may take to arrive in full.
pub const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The bit of the length field that marks routing messages.
const CONTROL_BIT: u16 = 0x8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FragmentHeader {
    pub datagram: u16,
    pub offset: u16,
    pub total: u16,
    /// Whether the reassembled body is routing messages, as a control
    /// frame's body, rather than a data frame's.
    pub control: bool,
}

impl FragmentHeader {
    pub fn encode(&self, buf: &mut Vec<u8>) {
        let total = if self.control {
            self.total | CONTROL_BIT
        } else {
            self.total
        };
        buf.extend_from_slice(&self.datagram.to_be_bytes());
        buf.extend_from_slice(&self.offset.to_be_bytes());
        buf.extend_from_slice(&total.to_be_bytes());
    }

    /// Splits a fragment body into its header and chunk.
    pub fn decode(body: &[u8]) -> Result<(Self, &[u8])> {
        if body.len() < FRAGMENT_HEADER_LEN {
            return Err(Error::Truncated);
        }
        let field = |at: usize| u16::from_be_bytes([body[at], body[at + 1]]);
        let header = FragmentHeader {
            datagram: field(0),
            offset: field(2),
            total: field(4) & !CONTROL_BIT,
            control: field(4) & CONTROL_BIT != 0,
        };
        Ok((header, &body[FRAGMENT_HEADER_LEN..]))
    }
}

/// Splits `body` into chunks of at most `chunk_len` bytes, each with its
/// header. `control` says whether it's a control frame's body.
pub fn split(
    body: &[u8],
    datagram: u16,
    chunk_len: usize,
    control: bool,
) -> Vec<(FragmentHeader, &[u8])> {
    body.chunks(chunk_len.max(1))
        .enumerate()
        .map(|(i, chunk)| {
            let header = FragmentHeader {
                datagram,
                offset: (i * chunk_len) as u16,
                total: body.len() as u16,
                control,
            };
            (header, chunk)
        })
        .collect()
}

struct Partial {
    started: Instant,
    control: bool,
    data: Vec<u8>,
    /// One bit per byte of `data`: whether it has arrived.
    arrived: Vec<u8>,
    remaining: usize,
}

/// Datagrams being reassembled, by (sender, hue, datagram ID).
#[derive(Default)]
pub struct Reassembly {
    pending: BTreeMap<(NodeId, HueId, u16), Partial>,
}

impl Reassembly {
    /// Adds a fragment. Returns the whole body once every byte has arrived;
    /// it's a control frame's body if `header.control` is set.
    ///
    /// Fragments that disagree about the length or kind, run past the end,
    /// or would exceed the limits are rejected; repeated bytes are ignored.
    pub fn add(
        &mut self,
        sender: NodeId,
        hue: HueId,
        header: FragmentHeader,
        chunk: &[u8],
        now: Instant,
    ) -> Result<Option<Vec<u8>>> {
        let total = usize::from(header.total);
        let offset = usize::from(header.offset);
        if total > MAX_DATAGRAM || offset + chunk.len() > total || chunk.is_empty() {
            return Err(Error::BadFragment);
        }
        let key = (sender, hue, header.datagram);
        if !self.pending.contains_key(&key) {
            self.expire(now);
            if self.pending.len() >= MAX_PENDING {
                // Make room by dropping the oldest.
                let oldest = self
                    .pending
                    .iter()
                    .min_by_key(|(_, p)| p.started)
                    .map(|(key, _)| *key);
                if let Some(oldest) = oldest {
                    self.pending.remove(&oldest);
                }
            }
            self.pending.insert(
                key,
                Partial {
                    started: now,
                    control: header.control,
                    data: vec![0; total],
                    arrived: vec![0; total.div_ceil(8)],
                    remaining: total,
                },
            );
        }
        let partial = self.pending.get_mut(&key).expect("inserted above");
        if partial.data.len() != total || partial.control != header.control {
            self.pending.remove(&key);
            return Err(Error::BadFragment);
        }
        for (i, &byte) in chunk.iter().enumerate() {
            let at = offset + i;
            let (index, bit) = (at / 8, 1 << (at % 8));
            if partial.arrived[index] & bit == 0 {
                partial.arrived[index] |= bit;
                partial.data[at] = byte;
                partial.remaining -= 1;
            }
        }
        if partial.remaining == 0 {
            return Ok(self.pending.remove(&key).map(|p| p.data));
        }
        Ok(None)
    }

    /// Drops datagrams that took too long.
    pub fn expire(&mut self, now: Instant) {
        self.pending
            .retain(|_, p| now < p.started + REASSEMBLY_TIMEOUT);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: NodeId = NodeId::from_u64(1);
    const HUE: HueId = HueId(0);
    const NOW: Instant = Instant::from_millis(0);

    fn body(len: usize) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    #[test]
    fn reassembles_in_any_order() {
        let data = body(1000);
        let mut fragments = split(&data, 7, 210, false);
        assert_eq!(fragments.len(), 5);
        fragments.reverse();

        let mut reassembly = Reassembly::default();
        let (last, rest) = fragments.split_last().unwrap();
        for (header, chunk) in rest {
            assert_eq!(reassembly.add(A, HUE, *header, chunk, NOW), Ok(None));
            // Duplicates are harmless.
            assert_eq!(reassembly.add(A, HUE, *header, chunk, NOW), Ok(None));
        }
        assert_eq!(reassembly.add(A, HUE, last.0, last.1, NOW), Ok(Some(data)));
        assert!(reassembly.is_empty());
    }

    #[test]
    fn rejects_fragments_that_break_the_rules() {
        let mut reassembly = Reassembly::default();
        let header = |offset, total| FragmentHeader {
            datagram: 1,
            offset,
            total,
            control: false,
        };
        let too_big = header(0, MAX_DATAGRAM as u16 + 1);
        assert_eq!(
            reassembly.add(A, HUE, too_big, &[1], NOW),
            Err(Error::BadFragment)
        );
        let past_end = header(99, 100);
        assert_eq!(
            reassembly.add(A, HUE, past_end, &[1, 2], NOW),
            Err(Error::BadFragment)
        );

        assert_eq!(reassembly.add(A, HUE, header(0, 100), &[1], NOW), Ok(None));
        let changed_length = header(1, 200);
        assert_eq!(
            reassembly.add(A, HUE, changed_length, &[2], NOW),
            Err(Error::BadFragment)
        );
        assert!(reassembly.is_empty());

        // A fragment that changes the body's kind spoils its datagram.
        assert_eq!(reassembly.add(A, HUE, header(0, 100), &[1], NOW), Ok(None));
        let control = FragmentHeader {
            control: true,
            ..header(1, 100)
        };
        assert_eq!(
            reassembly.add(A, HUE, control, &[2], NOW),
            Err(Error::BadFragment)
        );
    }

    #[test]
    fn headers_say_what_kind_of_body_they_carry() {
        for control in [false, true] {
            let header = FragmentHeader {
                datagram: 0xbeef,
                offset: 70,
                total: MAX_DATAGRAM as u16,
                control,
            };
            let mut encoded = Vec::new();
            header.encode(&mut encoded);
            encoded.push(9);
            assert_eq!(FragmentHeader::decode(&encoded), Ok((header, &[9][..])));
        }
    }

    #[test]
    fn incomplete_datagrams_expire_and_are_capped() {
        let mut reassembly = Reassembly::default();
        for datagram in 0..20 {
            let header = FragmentHeader {
                datagram,
                offset: 0,
                total: 100,
                control: false,
            };
            reassembly.add(A, HUE, header, &[1], NOW).unwrap();
        }
        assert_eq!(reassembly.len(), MAX_PENDING);
        reassembly.expire(NOW + REASSEMBLY_TIMEOUT);
        assert!(reassembly.is_empty());
    }
}
