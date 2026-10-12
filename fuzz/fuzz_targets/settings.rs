//! Settings records as firmware reads them from flash: never a panic, and
//! whatever decodes encodes back to the same settings.
//!
//! Random bytes almost never carry the magic number and a matching CRC, so
//! inputs starting with an even byte are wrapped in a valid header and CRC,
//! which sends them on to the entries.

#![no_main]

use libfuzzer_sys::fuzz_target;
use spectramesh_core::settings::Settings;
use spectramesh_core::{ChannelKey, MeshKey};

fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let record = if first % 2 == 0 && rest.len() <= usize::from(u16::MAX) {
        let mut record = b"SMESHST1".to_vec();
        record.extend_from_slice(&7u32.to_be_bytes());
        record.extend_from_slice(&(rest.len() as u16).to_be_bytes());
        record.extend_from_slice(rest);
        let crc = crc32(&record);
        record.extend_from_slice(&crc.to_be_bytes());
        record
    } else {
        data.to_vec()
    };
    if let Some((generation, settings)) = Settings::decode(&record) {
        let again = settings.encode(generation);
        let (again_generation, again) = Settings::decode(&again).expect("its own record");
        assert_eq!(again_generation, generation);
        assert_eq!(again.name, settings.name);
        let ids = |s: &Settings| {
            (
                s.mesh_keys.iter().map(MeshKey::id).collect::<Vec<_>>(),
                s.channel_keys
                    .iter()
                    .map(ChannelKey::id)
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(ids(&again), ids(&settings));
    }
});

/// CRC-32 (IEEE 802.3), as the record uses.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}
