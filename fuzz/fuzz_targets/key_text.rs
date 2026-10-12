//! Parses arbitrary text as mesh and channel keys, and checks that keys
//! round-trip through their text form.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::{ChannelKey, MeshKey};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(key) = MeshKey::from_text(text) {
        let again = MeshKey::from_text(&key.to_text()).expect("own text parses");
        assert_eq!(again.id(), key.id());
        assert_eq!(again.ipv6_prefix(), key.ipv6_prefix());
    }
    if let Ok(key) = ChannelKey::from_text(text) {
        let again = ChannelKey::from_text(&key.to_text()).expect("own text parses");
        assert_eq!(again.id(), key.id());
    }
});
