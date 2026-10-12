//! What the node keeps in flash: its identity, its settings and its boot count.
//!
//! Everything lives in the `spectramesh` data partition (see
//! `partitions.csv`), one 4 KiB sector per record:
//!
//! | Sector | Record                                                        |
//! |--------|---------------------------------------------------------------|
//! | 0      | Identity: a magic number, the 32-byte secret, and its bitwise complement |
//! | 1, 2   | Settings ([`spectramesh_core::settings`]), written to each in turn |
//! | 3      | Boot count: a magic number, the count, and its complement     |
//!
//! The partition starts where ESP-IDF's `nvs` partition does, and boards
//! flashed with the default partition table kept their identity in its first
//! sector, so they keep it now. (Flashing an ESP-IDF app afterwards erases it.)
//!
//! The partition is flagged `encrypted`. Until flash encryption is turned on
//! in the chip's eFuses, that changes nothing; once it is, the records are
//! stored encrypted, and a stolen board's flash gives away no keys.

use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, FlashRegion, FlashStorage, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_hal::rng::Trng;
use log::{info, warn};
use spectramesh_core::Identity;
use spectramesh_core::settings::{MAX_RECORD_LEN, Settings};

pub use partitions::Error;

const LABEL: &str = "spectramesh";
const SECTOR_LEN: u32 = 4096;

const IDENTITY: u32 = 0;
const SETTINGS: [u32; 2] = [SECTOR_LEN, 2 * SECTOR_LEN];
const BOOTS: u32 = 3 * SECTOR_LEN;

const IDENTITY_MAGIC: [u8; 8] = *b"SMESHID1";
const IDENTITY_LEN: usize = IDENTITY_MAGIC.len() + 32 + 32;
const BOOTS_MAGIC: [u8; 8] = *b"SMESHBC1";

pub struct Store {
    flash: FlashStorage<'static>,
}

impl Store {
    pub fn new(flash: FLASH<'static>) -> Self {
        Store {
            flash: FlashStorage::new(flash),
        }
    }

    /// Runs `f` on the partition. Boards flashed with the default partition
    /// table have `nvs` where `spectramesh` would be, so that's used instead.
    fn with_region<T>(
        &mut self,
        f: impl FnOnce(&mut FlashRegion<'_, 'static>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut table = [0u8; PARTITION_TABLE_MAX_LEN];
        let table = partitions::read_partition_table(&mut self.flash, &mut table)?;
        let entry = match table.iter().find(|p| p.label_as_str() == LABEL) {
            Some(entry) => entry,
            None => table
                .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))?
                .ok_or(Error::Invalid)?,
        };
        let mut region = entry.as_flash_region(&mut self.flash);
        f(&mut region)
    }

    /// Reads this node's identity, or creates and stores one on first boot.
    ///
    /// `rng` is the true random number generator.
    pub fn load_or_create_identity(&mut self, rng: &Trng) -> Result<Identity, Error> {
        self.with_region(|region| {
            let mut record = [0u8; IDENTITY_LEN];
            region.read(IDENTITY, &mut record)?;
            if let Some(secret) = with_complement(&IDENTITY_MAGIC, &record) {
                return Ok(Identity::from_secret(secret));
            }
            let mut secret = [0u8; 32];
            rng.read(&mut secret);
            let identity = Identity::from_secret(secret);
            let mut record = complemented(&IDENTITY_MAGIC, identity.secret());
            let written = region.write(IDENTITY, &record);
            record.fill(0); // It held the secret.
            written?;
            info!("created a new identity for node {}", identity.node_id());
            Ok(identity)
        })
    }

    /// The newest intact settings, and their generation.
    fn read_settings(&mut self) -> Result<Option<(u32, Settings)>, Error> {
        self.with_region(|region| {
            let mut newest: Option<(u32, Settings)> = None;
            let mut record = [0u8; MAX_RECORD_LEN];
            for offset in SETTINGS {
                region.read(offset, &mut record)?;
                if let Some((generation, settings)) = Settings::decode(&record)
                    && newest.as_ref().is_none_or(|(g, _)| generation > *g)
                {
                    newest = Some((generation, settings));
                }
            }
            record.fill(0); // It held keys.
            Ok(newest)
        })
    }

    /// The stored settings, or the defaults (no keys) if there are none.
    pub fn load_settings(&mut self) -> Settings {
        match self.read_settings() {
            Ok(settings) => settings.map(|(_, s)| s).unwrap_or_default(),
            Err(err) => {
                warn!("couldn't read settings from flash: {err:?}");
                Settings::default()
            }
        }
    }

    /// Stores `settings` over the older of the two records, so the newer one
    /// survives if power fails while writing.
    pub fn save_settings(&mut self, settings: &Settings) -> Result<(), Error> {
        let current = self.read_settings()?;
        let generation = current.as_ref().map_or(1, |(g, _)| g.wrapping_add(1));
        let record = settings.encode(generation);
        self.with_region(|region| {
            // Generations alternate between the two places: odd in the first.
            region.write(SETTINGS[(generation % 2 == 0) as usize], &record)
        })
    }

    /// Erases the settings, keeping the identity.
    pub fn erase_settings(&mut self) -> Result<(), Error> {
        self.with_region(|region| {
            for offset in SETTINGS {
                region.erase(offset, offset + SECTOR_LEN)?;
            }
            Ok(())
        })
    }

    /// Counts this boot and returns the count, starting at 1.
    pub fn count_boot(&mut self) -> Result<u32, Error> {
        self.with_region(|region| {
            let mut record = [0u8; 16];
            region.read(BOOTS, &mut record)?;
            let previous =
                with_complement::<4>(&BOOTS_MAGIC, &record).map_or(0, u32::from_be_bytes);
            let count = previous.wrapping_add(1).max(1);
            region.write(BOOTS, &complemented(&BOOTS_MAGIC, &count.to_be_bytes()))?;
            Ok(count)
        })
    }
}

/// A record: `magic`, `value`, and `value`'s bitwise complement, so an
/// erased or damaged sector is never mistaken for a value.
fn complemented<const N: usize>(magic: &[u8; 8], value: &[u8; N]) -> alloc::vec::Vec<u8> {
    let mut record = alloc::vec::Vec::with_capacity(8 + 2 * N);
    record.extend_from_slice(magic);
    record.extend_from_slice(value);
    record.extend(value.iter().map(|b| !b));
    record
}

fn with_complement<const N: usize>(magic: &[u8; 8], record: &[u8]) -> Option<[u8; N]> {
    let (found, rest) = record.split_at_checked(8)?;
    let value = rest.get(..N)?;
    let check = rest.get(N..2 * N)?;
    let intact = found == magic && value.iter().zip(check).all(|(v, c)| *v == !c);
    intact.then(|| value.try_into().expect("N bytes"))
}
