//! ICMPv6 error messages, so programs learn quickly when the mesh can't
//! deliver a packet (RFC 4443).
//!
//! Only "destination unreachable, no route" is needed: with fragmentation,
//! every packet the TUN device accepts is small enough for the mesh.

use crate::tun::IPV6_MIN_MTU;

const NEXT_HEADER_ICMPV6: u8 = 58;
const DESTINATION_UNREACHABLE: u8 = 1;
const NO_ROUTE: u8 = 0;

/// A "destination unreachable, no route" error from `own`, about `invoking`,
/// for the host that sent it. `None` if the rules say not to answer: the
/// packet isn't IPv6, or is itself an ICMPv6 error.
pub fn no_route(own: &[u8; 16], invoking: &[u8]) -> Option<Vec<u8>> {
    if invoking.len() < 40 || invoking[0] >> 4 != 6 {
        return None;
    }
    // Never answer an error with an error. (Types below 128 are errors.)
    if invoking[6] == NEXT_HEADER_ICMPV6 && invoking.get(40).is_some_and(|&t| t < 128) {
        return None;
    }
    let destination: [u8; 16] = invoking[8..24].try_into().expect("16 bytes");

    // As much of the invoking packet as fits in the minimum MTU.
    let quoted = &invoking[..invoking.len().min(IPV6_MIN_MTU - 40 - 8)];
    let length = 8 + quoted.len();

    let mut packet = Vec::with_capacity(40 + length);
    packet.extend_from_slice(&[0x60, 0, 0, 0]); // version 6, no traffic class or flow
    packet.extend_from_slice(&(length as u16).to_be_bytes());
    packet.push(NEXT_HEADER_ICMPV6);
    packet.push(64); // hop limit
    packet.extend_from_slice(own);
    packet.extend_from_slice(&destination);
    packet.extend_from_slice(&[DESTINATION_UNREACHABLE, NO_ROUTE, 0, 0, 0, 0, 0, 0]);
    packet.extend_from_slice(quoted);

    let checksum = checksum(own, &destination, &packet[40..]);
    packet[42..44].copy_from_slice(&checksum.to_be_bytes());
    Some(packet)
}

/// The ICMPv6 checksum: the ones' complement sum over the pseudo-header and
/// the message (with its checksum field zero).
fn checksum(src: &[u8; 16], dst: &[u8; 16], message: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut add = |bytes: &[u8]| {
        for pair in bytes.chunks(2) {
            let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
            sum += u32::from(word);
        }
    };
    add(src);
    add(dst);
    add(&(message.len() as u32).to_be_bytes());
    add(&[0, 0, 0, NEXT_HEADER_ICMPV6]);
    add(message);
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: [u8; 16] = [0xfd, 1, 2, 3, 4, 5, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1];
    const OTHER: [u8; 16] = [0xfd, 1, 2, 3, 4, 5, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2];

    /// An IPv6 packet from OWN to OTHER with `next_header` and a payload.
    fn packet(next_header: u8, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x60, 0, 0, 0];
        packet.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        packet.extend_from_slice(&[next_header, 64]);
        packet.extend_from_slice(&OWN);
        packet.extend_from_slice(&OTHER);
        packet.extend_from_slice(payload);
        packet
    }

    #[test]
    fn errors_go_back_to_the_sender_with_a_valid_checksum() {
        // An echo request (ICMPv6 type 128).
        let invoking = packet(NEXT_HEADER_ICMPV6, &[128, 0, 0, 0, 0, 1, 0, 1]);
        let error = no_route(&OTHER, &invoking).unwrap();
        assert_eq!(error[8..24], OTHER);
        assert_eq!(error[24..40], OWN);
        assert_eq!(&error[40..42], &[DESTINATION_UNREACHABLE, NO_ROUTE]);
        assert_eq!(&error[48..], &invoking[..]);
        // Summing a message with its checksum in place gives zero.
        assert_eq!(checksum(&OTHER, &OWN, &error[40..]), 0);
    }

    #[test]
    fn errors_stay_within_the_minimum_mtu() {
        let invoking = packet(17, &[0; 1400]);
        let error = no_route(&OTHER, &invoking).unwrap();
        assert_eq!(error.len(), IPV6_MIN_MTU);
        assert_eq!(checksum(&OTHER, &OWN, &error[40..]), 0);
    }

    #[test]
    fn errors_are_never_answered_with_errors() {
        let error = packet(NEXT_HEADER_ICMPV6, &[DESTINATION_UNREACHABLE, 0, 0, 0]);
        assert_eq!(no_route(&OTHER, &error), None);
        assert_eq!(no_route(&OTHER, &[0x45; 60]), None);
    }
}
