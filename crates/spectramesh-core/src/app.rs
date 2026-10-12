//! What end-to-end payloads carry.
//!
//! The router moves bytes; it doesn't care what they mean. So that one node
//! can carry both IP traffic and messages for programs, platforms start each
//! end-to-end payload ([`Router::send`](crate::Router::send)) with a byte
//! saying what follows:
//!
//! | First byte    | Payload                                                  |
//! |---------------|----------------------------------------------------------|
//! | `0x01`        | A message for programs, such as text; the bytes follow   |
//! | `0x60`–`0x6f` | An IPv6 packet, whole: its first byte holds its version  |
//!
//! Channel messages ([`Router::send_to_channel`](crate::Router::send_to_channel))
//! are always messages for programs, so they carry no type byte.

/// The first byte of a message for programs.
pub const MESSAGE: u8 = 0x01;

/// Wraps `data` as a message for programs.
pub fn message(data: &[u8]) -> alloc::vec::Vec<u8> {
    let mut payload = alloc::vec::Vec::with_capacity(1 + data.len());
    payload.push(MESSAGE);
    payload.extend_from_slice(data);
    payload
}

/// What an end-to-end payload holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload<'a> {
    /// A message for programs.
    Message(&'a [u8]),
    /// An IPv6 packet.
    Ipv6(&'a [u8]),
    /// Something this version doesn't know.
    Unknown,
}

impl<'a> Payload<'a> {
    pub fn classify(payload: &'a [u8]) -> Self {
        match payload.first() {
            Some(&MESSAGE) => Payload::Message(&payload[1..]),
            Some(byte) if byte >> 4 == 6 => Payload::Ipv6(payload),
            _ => Payload::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_say_what_they_carry() {
        assert_eq!(Payload::classify(&message(b"hi")), Payload::Message(b"hi"));
        let packet = [0x60, 0, 0, 0];
        assert_eq!(Payload::classify(&packet), Payload::Ipv6(&packet));
        assert_eq!(Payload::classify(&[0x45, 0]), Payload::Unknown);
        assert_eq!(Payload::classify(&[]), Payload::Unknown);
    }
}
