//! The daemon's own parsers: command lines, control socket commands, web
//! requests, key files, ICMPv6 errors and the Ethernet length prefix. The
//! first byte picks one; the rest is its input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::{ChannelKey, MeshKey};
use spectramesh_wrt::{config, control, icmp, keys, link, tun, web};

fuzz_target!(|data: &[u8]| {
    let Some((&which, input)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(input);
    match which % 6 {
        0 => {
            let _ = config::parse(text.split(' ').map(String::from));
        }
        1 => {
            let _ = control::parse(&text);
            assert_eq!(control::unhex(&control::hex(input)).unwrap(), input);
            let _ = control::unhex(&text);
            let printable = control::printable(input);
            assert!(!printable.chars().any(char::is_control));
        }
        2 => {
            let _ = web::route(&text);
            // Node names reach the page through this: nothing may escape the string.
            let quoted = web::quoted(&text);
            let inner = &quoted[1..quoted.len() - 1];
            assert!(quoted.starts_with('"') && quoted.ends_with('"'));
            assert!(!inner.chars().any(|c| c < ' ' || c == '<' || c == '>'));
            let mut escaped = false;
            for c in inner.chars() {
                assert!(escaped || c != '"', "{quoted}");
                escaped = !escaped && c == '\\';
            }
            assert!(!escaped, "{quoted}");
        }
        3 => {
            let own = [0xfd; 16];
            if let Some(error) = icmp::no_route(&own, input) {
                assert!(error.len() <= tun::IPV6_MIN_MTU);
                let (src, dst) = tun::addresses(&error).expect("a valid IPv6 packet");
                assert_eq!(src, own);
                assert_eq!(dst, <[u8; 16]>::try_from(&input[8..24]).unwrap());
                // Never an error about an error.
                assert!(icmp::no_route(&own, &error).is_none());
            }
            let _ = tun::addresses(input);
        }
        4 => {
            let _ = link::unwrap(input);
            if input.len() <= usize::from(u16::MAX) {
                assert_eq!(link::unwrap(&link::wrap(input)), Some(input));
            }
        }
        _ => {
            let _ = keys::parse_lines(&text, MeshKey::from_text);
            let _ = keys::parse_lines(&text, ChannelKey::from_text);
        }
    }
});
