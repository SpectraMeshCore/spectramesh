//! The link between a host and a radio modem.
//!
//! A radio modem is a microcontroller, such as an ESP32 running the
//! `spectramesh-modem` firmware, that only moves frames between its radio and
//! a USB serial link. The host, a laptop or a router without the radio, runs
//! the router itself and uses the modem as a hue.
//!
//! Messages travel in SLIP frames (RFC 1055) over the serial link. Each
//! starts with a type byte:
//!
//! | Type | Direction     | Message                                                   |
//! |------|---------------|-----------------------------------------------------------|
//! | 0x01 | host to modem | Send: destination link address (6), then the frame        |
//! | 0x02 | host to modem | Info request                                              |
//! | 0x81 | modem to host | Received: source link address (6), RSSI in dBm (1, signed), then the frame |
//! | 0x82 | modem to host | Info: protocol version (1), MTU (2), bitrate (4), channel (1), link address (6) |
//!
//! The modem sends its info when it starts and whenever it's asked.

use alloc::vec::Vec;

use crate::error::{Error, Result};

pub const PROTOCOL_VERSION: u8 = 1;

const SEND: u8 = 0x01;
const INFO_REQUEST: u8 = 0x02;
const RECEIVED: u8 = 0x81;
const INFO: u8 = 0x82;

/// A message from the host to the modem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToModem<'a> {
    Send { dst: [u8; 6], frame: &'a [u8] },
    InfoRequest,
}

/// A message from the modem to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FromModem<'a> {
    Received {
        src: [u8; 6],
        rssi: i8,
        frame: &'a [u8],
    },
    Info(ModemInfo),
}

/// What a modem tells the host about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModemInfo {
    pub version: u8,
    /// The largest frame the radio carries.
    pub mtu: u16,
    pub bitrate_bps: u32,
    pub channel: u8,
    /// The modem's own link-layer address.
    pub address: [u8; 6],
}

impl ToModem<'_> {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToModem::Send { dst, frame } => {
                let mut message = Vec::with_capacity(7 + frame.len());
                message.push(SEND);
                message.extend_from_slice(dst);
                message.extend_from_slice(frame);
                message
            }
            ToModem::InfoRequest => alloc::vec![INFO_REQUEST],
        }
    }
}

impl<'a> ToModem<'a> {
    pub fn decode(message: &'a [u8]) -> Result<Self> {
        match message.split_first() {
            Some((&SEND, rest)) if rest.len() >= 6 => Ok(ToModem::Send {
                dst: rest[..6].try_into().expect("6 bytes"),
                frame: &rest[6..],
            }),
            Some((&INFO_REQUEST, _)) => Ok(ToModem::InfoRequest),
            Some((&SEND, _)) | None => Err(Error::Truncated),
            Some((&other, _)) => Err(Error::UnknownMessage(other)),
        }
    }
}

impl FromModem<'_> {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            FromModem::Received { src, rssi, frame } => {
                let mut message = Vec::with_capacity(8 + frame.len());
                message.push(RECEIVED);
                message.extend_from_slice(src);
                message.push(*rssi as u8);
                message.extend_from_slice(frame);
                message
            }
            FromModem::Info(info) => {
                let mut message = Vec::with_capacity(15);
                message.extend_from_slice(&[INFO, info.version]);
                message.extend_from_slice(&info.mtu.to_be_bytes());
                message.extend_from_slice(&info.bitrate_bps.to_be_bytes());
                message.push(info.channel);
                message.extend_from_slice(&info.address);
                message
            }
        }
    }
}

impl<'a> FromModem<'a> {
    pub fn decode(message: &'a [u8]) -> Result<Self> {
        match message.split_first() {
            Some((&RECEIVED, rest)) if rest.len() >= 7 => Ok(FromModem::Received {
                src: rest[..6].try_into().expect("6 bytes"),
                rssi: rest[6] as i8,
                frame: &rest[7..],
            }),
            Some((&INFO, rest)) if rest.len() >= 14 => Ok(FromModem::Info(ModemInfo {
                version: rest[0],
                mtu: u16::from_be_bytes([rest[1], rest[2]]),
                bitrate_bps: u32::from_be_bytes(rest[3..7].try_into().expect("4 bytes")),
                channel: rest[7],
                address: rest[8..14].try_into().expect("6 bytes"),
            })),
            Some((&RECEIVED | &INFO, _)) | None => Err(Error::Truncated),
            Some((&other, _)) => Err(Error::UnknownMessage(other)),
        }
    }
}

const END: u8 = 0xc0;
const ESC: u8 = 0xdb;
const ESC_END: u8 = 0xdc;
const ESC_ESC: u8 = 0xdd;

/// Wraps a message in a SLIP frame. The frame starts with END too, which
/// flushes any noise the other side received before it.
pub fn slip_encode(message: &[u8], out: &mut Vec<u8>) {
    out.reserve(message.len() + 2);
    out.push(END);
    for &byte in message {
        match byte {
            END => out.extend_from_slice(&[ESC, ESC_END]),
            ESC => out.extend_from_slice(&[ESC, ESC_ESC]),
            byte => out.push(byte),
        }
    }
    out.push(END);
}

/// Collects SLIP frames from a byte stream.
pub struct SlipDecoder {
    buf: Vec<u8>,
    escaped: bool,
    /// The current frame grew past `max_len`; it's dropped at its end.
    overflowed: bool,
    max_len: usize,
}

impl SlipDecoder {
    pub fn new(max_len: usize) -> Self {
        SlipDecoder {
            buf: Vec::new(),
            escaped: false,
            overflowed: false,
            max_len,
        }
    }

    /// Takes one byte. Returns a frame when one ends. Empty frames, frames
    /// that grew too long and bad escapes are dropped.
    pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        if byte == END {
            let frame = core::mem::take(&mut self.buf);
            let complete = !self.overflowed && !self.escaped && !frame.is_empty();
            self.escaped = false;
            self.overflowed = false;
            return complete.then_some(frame);
        }
        let byte = if self.escaped {
            self.escaped = false;
            match byte {
                ESC_END => END,
                ESC_ESC => ESC,
                _ => {
                    self.overflowed = true; // A bad escape spoils the frame.
                    return None;
                }
            }
        } else if byte == ESC {
            self.escaped = true;
            return None;
        } else {
            byte
        };
        if self.buf.len() >= self.max_len {
            self.overflowed = true;
            self.buf.clear();
        } else if !self.overflowed {
            self.buf.push(byte);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn decode_all(decoder: &mut SlipDecoder, bytes: &[u8]) -> Vec<Vec<u8>> {
        bytes.iter().filter_map(|&b| decoder.push(b)).collect()
    }

    #[test]
    fn messages_round_trip() {
        let frame = [END, ESC, 1, 2, 3];
        let to_modem = ToModem::Send {
            dst: [0xff; 6],
            frame: &frame,
        };
        assert_eq!(ToModem::decode(&to_modem.encode()), Ok(to_modem));
        assert_eq!(
            ToModem::decode(&ToModem::InfoRequest.encode()),
            Ok(ToModem::InfoRequest)
        );

        let received = FromModem::Received {
            src: [1, 2, 3, 4, 5, 6],
            rssi: -67,
            frame: &frame,
        };
        assert_eq!(FromModem::decode(&received.encode()), Ok(received));
        let info = FromModem::Info(ModemInfo {
            version: PROTOCOL_VERSION,
            mtu: 250,
            bitrate_bps: 1_000_000,
            channel: 1,
            address: [9; 6],
        });
        assert_eq!(FromModem::decode(&info.encode()), Ok(info));
        assert_eq!(FromModem::decode(&[0x99]), Err(Error::UnknownMessage(0x99)));
        assert_eq!(FromModem::decode(&[RECEIVED, 1]), Err(Error::Truncated));
    }

    #[test]
    fn slip_frames_survive_special_bytes_and_noise() {
        let messages = [
            vec![END, ESC, 0, END],
            vec![1, 2, 3],
            vec![ESC_END, ESC_ESC],
        ];
        let mut stream = vec![0x55, 0x66]; // noise before the first frame
        for message in &messages {
            slip_encode(message, &mut stream);
        }
        let mut decoder = SlipDecoder::new(64);
        let frames = decode_all(&mut decoder, &stream);
        // The noise ends up as one junk frame, which the message decoders reject.
        assert_eq!(frames[1..], messages);
    }

    #[test]
    fn oversized_and_corrupt_frames_are_dropped() {
        let mut decoder = SlipDecoder::new(4);
        let mut stream = Vec::new();
        slip_encode(&[1, 2, 3, 4, 5], &mut stream);
        slip_encode(&[7], &mut stream);
        stream.extend_from_slice(&[END, ESC, 0x01, 9, END]); // bad escape
        stream.extend_from_slice(&[8, END]);
        assert_eq!(decode_all(&mut decoder, &stream), [vec![7], vec![8]]);
    }
}
