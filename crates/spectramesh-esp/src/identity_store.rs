//! Keeps the node's identity secret in flash.
//!
//! The secret lives in the first sector of the `nvs` data partition, which
//! every ESP-IDF partition table has. This firmware doesn't use ESP-IDF's NVS,
//! so the partition is free. (Flashing an ESP-IDF app afterwards will see it
//! as corrupt and reformat it, erasing the identity.)
//!
//! The record is a magic number, the 32-byte secret and its bitwise
//! complement, so an erased or damaged sector is never mistaken for a secret.

use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, FlashStorage, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_hal::rng::Rng;
use log::info;
use spectramesh_core::Identity;

const MAGIC: [u8; 8] = *b"SMESHID1";
const RECORD_LEN: usize = MAGIC.len() + 32 + 32;
const SECTOR_LEN: u32 = 4096;

/// Reads this node's identity from flash, or creates and stores one on first boot.
///
/// `rng` must be a true random source, which it is once Wi-Fi is running.
pub fn load_or_create(flash: FLASH<'_>, rng: &Rng) -> Result<Identity, partitions::Error> {
    let mut storage = FlashStorage::new(flash);
    let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
    let nvs = partitions::read_partition_table(&mut storage, &mut table)?
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))?
        .ok_or(partitions::Error::Invalid)?;
    let mut region = nvs.as_flash_region(&mut storage);

    let mut record = [0u8; RECORD_LEN];
    region.read(0, &mut record)?;
    if let Some(secret) = parse(&record) {
        return Ok(Identity::from_secret(secret));
    }

    let mut secret = [0u8; 32];
    rng.read(&mut secret);
    let identity = Identity::from_secret(secret);
    region.erase(0, SECTOR_LEN)?;
    region.write(0, &encode(identity.secret()))?;
    info!("created a new identity for node {}", identity.node_id());
    Ok(identity)
}

fn encode(secret: &[u8; 32]) -> [u8; RECORD_LEN] {
    let mut record = [0u8; RECORD_LEN];
    record[..8].copy_from_slice(&MAGIC);
    record[8..40].copy_from_slice(secret);
    for (out, byte) in record[40..].iter_mut().zip(secret) {
        *out = !byte;
    }
    record
}

fn parse(record: &[u8; RECORD_LEN]) -> Option<[u8; 32]> {
    let (magic, rest) = record.split_at(8);
    let (secret, check) = rest.split_at(32);
    let intact = magic == MAGIC && secret.iter().zip(check).all(|(s, c)| *s == !c);
    intact.then(|| secret.try_into().expect("32 bytes"))
}
