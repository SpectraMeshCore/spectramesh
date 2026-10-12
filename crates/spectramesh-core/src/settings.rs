//! A node's settings as one record, for platforms that keep them in raw
//! flash, such as the ESP32 firmware.
//!
//! | Bytes   | Field                                          |
//! |---------|------------------------------------------------|
//! | 0..8    | Magic: `SMESHST1`                              |
//! | 8..12   | Generation: of two valid records, the higher wins |
//! | 12..14  | Length of the entries                          |
//! | then    | Entries: type (1), length (1), value           |
//! | then 4  | CRC-32 of everything before it                 |
//!
//! | Entry | Value                                                       |
//! |-------|-------------------------------------------------------------|
//! | 1     | A mesh key (32). Repeatable: the first is sent with, the others are also accepted |
//! | 2     | This node's name, UTF-8, for the map                        |
//! | 3     | A channel key to join (32). Repeatable                      |
//! | 4     | Which radio to use, as text the platform defines, such as `802.15.4:25` (at most 16 bytes) |
//!
//! Unknown entries are skipped, so later versions can add settings.
//! Platforms write the record to one of two places in turn, with the
//! generation one higher each time, so losing power while writing leaves
//! the previous record intact.
//!
//! The record holds keys in the clear: keep it somewhere only this node can
//! read, such as flash encrypted by the chip.

use alloc::string::String;
use alloc::vec::Vec;
use zeroize::Zeroizing;

use crate::auth::MeshKey;
use crate::channel::ChannelKey;
use crate::report::clip_name;

const MAGIC: [u8; 8] = *b"SMESHST1";
const HEADER_LEN: usize = 14;
const CRC_LEN: usize = 4;

const MESH_KEY: u8 = 1;
const NAME: u8 = 2;
const CHANNEL_KEY: u8 = 3;
const RADIO: u8 = 4;

/// The longest radio setting, in bytes.
pub const MAX_RADIO_LEN: usize = 16;

/// The most mesh keys a record holds: one to send with, and a few more
/// accepted while keys are changed.
pub const MAX_MESH_KEYS: usize = 4;
/// The most channels a record joins.
pub const MAX_CHANNEL_KEYS: usize = 8;
/// The longest record [`Settings::encode`] makes.
pub const MAX_RECORD_LEN: usize =
    HEADER_LEN + (MAX_MESH_KEYS + MAX_CHANNEL_KEYS) * 34 + 2 + 32 + 2 + MAX_RADIO_LEN + CRC_LEN;

/// What a node keeps between restarts, apart from its identity.
#[derive(Debug, Default)]
pub struct Settings {
    /// The first is sent with; the others are also accepted. At most
    /// [`MAX_MESH_KEYS`] are kept.
    pub mesh_keys: Vec<MeshKey>,
    /// At most 32 bytes are kept.
    pub name: String,
    /// At most [`MAX_CHANNEL_KEYS`] are kept.
    pub channel_keys: Vec<ChannelKey>,
    /// Which radio to use, for platforms with a choice; empty for their
    /// default. At most [`MAX_RADIO_LEN`] bytes are kept.
    pub radio: String,
}

impl Settings {
    /// The record, with the given generation. It holds keys, so it's wiped
    /// when dropped.
    pub fn encode(&self, generation: u32) -> Zeroizing<Vec<u8>> {
        let mut record = Zeroizing::new(Vec::with_capacity(MAX_RECORD_LEN));
        record.extend_from_slice(&MAGIC);
        record.extend_from_slice(&generation.to_be_bytes());
        record.extend_from_slice(&[0, 0]); // Length, filled in below.
        for key in self.mesh_keys.iter().take(MAX_MESH_KEYS) {
            entry(&mut record, MESH_KEY, &key.0);
        }
        let name = clip_name(&self.name);
        if !name.is_empty() {
            entry(&mut record, NAME, name.as_bytes());
        }
        for key in self.channel_keys.iter().take(MAX_CHANNEL_KEYS) {
            entry(&mut record, CHANNEL_KEY, &key.0);
        }
        let radio = clip(&self.radio, MAX_RADIO_LEN);
        if !radio.is_empty() {
            entry(&mut record, RADIO, radio.as_bytes());
        }
        let len = (record.len() - HEADER_LEN) as u16;
        record[12..14].copy_from_slice(&len.to_be_bytes());
        let crc = crc32(&record);
        record.extend_from_slice(&crc.to_be_bytes());
        record
    }

    /// Reads a record from the start of `bytes`, which may run on past its
    /// end. Returns its generation and settings, or `None` if `bytes` holds
    /// no intact record (such as erased flash).
    pub fn decode(bytes: &[u8]) -> Option<(u32, Settings)> {
        let header = bytes.get(..HEADER_LEN)?;
        if header[..8] != MAGIC {
            return None;
        }
        let generation = u32::from_be_bytes(header[8..12].try_into().ok()?);
        let len = usize::from(u16::from_be_bytes([header[12], header[13]]));
        let end = HEADER_LEN.checked_add(len)?;
        let signed = bytes.get(..end)?;
        let crc = bytes.get(end..end + CRC_LEN)?;
        if crc32(signed).to_be_bytes() != crc {
            return None;
        }

        let mut settings = Settings::default();
        let mut entries = &signed[HEADER_LEN..];
        while let [kind, len, rest @ ..] = entries {
            let value = rest.get(..usize::from(*len))?;
            entries = &rest[value.len()..];
            match (*kind, <[u8; 32]>::try_from(value)) {
                (MESH_KEY, Ok(key)) if settings.mesh_keys.len() < MAX_MESH_KEYS => {
                    settings.mesh_keys.push(MeshKey::from_bytes(key));
                }
                (CHANNEL_KEY, Ok(key)) if settings.channel_keys.len() < MAX_CHANNEL_KEYS => {
                    settings.channel_keys.push(ChannelKey::from_bytes(key));
                }
                (NAME, _) => {
                    settings.name = String::from(clip_name(&String::from_utf8_lossy(value)));
                }
                (RADIO, _) => {
                    let radio = String::from_utf8_lossy(value);
                    settings.radio = String::from(clip(&radio, MAX_RADIO_LEN));
                }
                _ => {}
            }
        }
        if !entries.is_empty() {
            return None;
        }
        Some((generation, settings))
    }
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn clip(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn entry(record: &mut Vec<u8>, kind: u8, value: &[u8]) {
    record.extend_from_slice(&[kind, value.len() as u8]);
    record.extend_from_slice(value);
}

/// CRC-32 (IEEE 802.3), bit by bit: records are small and rarely read.
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample() -> Settings {
        Settings {
            mesh_keys: vec![MeshKey::from_bytes([1; 32]), MeshKey::from_bytes([2; 32])],
            name: "roof".into(),
            channel_keys: vec![ChannelKey::from_bytes([3; 32])],
            radio: "802.15.4:25".into(),
        }
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn settings_round_trip() {
        let mut record = sample().encode(7).to_vec();
        // Erased flash after the record doesn't matter.
        record.resize(4096, 0xff);
        let (generation, settings) = Settings::decode(&record).unwrap();
        assert_eq!(generation, 7);
        assert_eq!(settings.name, "roof");
        assert_eq!(settings.radio, "802.15.4:25");
        let ids = |keys: &[MeshKey]| keys.iter().map(MeshKey::id).collect::<Vec<_>>();
        assert_eq!(ids(&settings.mesh_keys), ids(&sample().mesh_keys));
        assert_eq!(settings.channel_keys[0].id(), sample().channel_keys[0].id());
    }

    #[test]
    fn damaged_or_erased_records_are_ignored() {
        assert!(Settings::decode(&[0xff; 4096]).is_none());
        assert!(Settings::decode(&[]).is_none());
        let record = sample().encode(1);
        for i in 0..record.len() {
            let mut damaged = record.to_vec();
            damaged[i] ^= 0x10;
            assert!(Settings::decode(&damaged).is_none(), "byte {i}");
        }
        assert!(Settings::decode(&record[..record.len() - 1]).is_none());
    }

    #[test]
    fn records_have_room_for_everything() {
        let full = Settings {
            mesh_keys: (0..9).map(|i| MeshKey::from_bytes([i; 32])).collect(),
            name: "x".repeat(40),
            channel_keys: (0..9).map(|i| ChannelKey::from_bytes([i; 32])).collect(),
            radio: "r".repeat(20),
        };
        let record = full.encode(1);
        assert_eq!(record.len(), MAX_RECORD_LEN);
        let (_, settings) = Settings::decode(&record).unwrap();
        assert_eq!(settings.mesh_keys.len(), MAX_MESH_KEYS);
        assert_eq!(settings.channel_keys.len(), MAX_CHANNEL_KEYS);
        assert_eq!(settings.name.len(), 32);
        assert_eq!(settings.radio.len(), MAX_RADIO_LEN);
    }
}
