//! Radio settings, and how they become the chip's register values.

/// Which chip of the family. The SX1262 transmits up to +22 dBm, the SX1261
/// up to +15 dBm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Sx1261,
    Sx1262,
}

/// Supply voltage for a temperature-compensated crystal oscillator powered
/// from the radio's DIO3, as most modules have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TcxoVoltage {
    V1_6 = 0,
    V1_7 = 1,
    V1_8 = 2,
    V2_2 = 3,
    V2_4 = 4,
    V2_7 = 5,
    V3_0 = 6,
    V3_3 = 7,
}

/// How a board is wired, which its maker documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Board {
    pub chip: Chip,
    /// `None` for a plain crystal.
    pub tcxo: Option<TcxoVoltage>,
    /// Whether DIO2 drives the antenna switch between sending and receiving.
    pub dio2_rf_switch: bool,
    /// Whether the board has the inductor the chip's DC-DC regulator needs,
    /// which uses less power than its LDO.
    pub dc_dc: bool,
}

/// LoRa bandwidth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoRaBandwidth {
    Khz62_5,
    Khz125,
    Khz250,
    Khz500,
}

impl LoRaBandwidth {
    pub fn hz(self) -> u32 {
        match self {
            LoRaBandwidth::Khz62_5 => 62_500,
            LoRaBandwidth::Khz125 => 125_000,
            LoRaBandwidth::Khz250 => 250_000,
            LoRaBandwidth::Khz500 => 500_000,
        }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            LoRaBandwidth::Khz62_5 => 0x03,
            LoRaBandwidth::Khz125 => 0x04,
            LoRaBandwidth::Khz250 => 0x05,
            LoRaBandwidth::Khz500 => 0x06,
        }
    }
}

/// LoRa forward error correction: 4 data bits in every 5, 6, 7 or 8 sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CodingRate {
    Cr4_5 = 1,
    Cr4_6 = 2,
    Cr4_7 = 3,
    Cr4_8 = 4,
}

/// The receiver's bandwidth in GFSK. It should be at least the bitrate plus
/// twice the deviation, with room for crystal error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FskBandwidth {
    Khz58_6,
    Khz78_2,
    Khz117_3,
    Khz156_2,
    Khz234_3,
    Khz312,
    Khz373_6,
    Khz467,
}

impl FskBandwidth {
    pub(crate) fn code(self) -> u8 {
        // Datasheet table 13-47: the double-sideband bandwidths.
        match self {
            FskBandwidth::Khz58_6 => 0x0C,
            FskBandwidth::Khz78_2 => 0x1B,
            FskBandwidth::Khz117_3 => 0x0B,
            FskBandwidth::Khz156_2 => 0x1A,
            FskBandwidth::Khz234_3 => 0x0A,
            FskBandwidth::Khz312 => 0x19,
            FskBandwidth::Khz373_6 => 0x11,
            FskBandwidth::Khz467 => 0x09,
        }
    }
}

/// How bits become radio waves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modulation {
    /// Long range at low bitrates: each step up in spreading factor (5 to
    /// 12) about halves the bitrate and adds 2.5 dB of range.
    LoRa {
        spreading_factor: u8,
        bandwidth: LoRaBandwidth,
        coding_rate: CodingRate,
        preamble_symbols: u16,
        /// The chip's two sync word registers. 0x1424 is the "private
        /// network" default; LoRaWAN uses 0x3444.
        sync_word: u16,
    },
    /// Gaussian FSK, for fast links between well-placed nodes: 50 to 300
    /// kbit/s, with less range than LoRa.
    Gfsk {
        bitrate_bps: u32,
        deviation_hz: u32,
        bandwidth: FskBandwidth,
        preamble_bits: u16,
        sync_word: [u8; 4],
    },
}

impl Modulation {
    /// A SpectraMesh backbone link: GFSK at 250 kbit/s.
    pub const FAST_FSK: Modulation = Modulation::Gfsk {
        bitrate_bps: 250_000,
        deviation_hz: 125_000,
        bandwidth: FskBandwidth::Khz467,
        preamble_bits: 64,
        sync_word: [0x9E, 0x37, 0x52, 0xB1],
    };

    /// Long range: LoRa at SF9 and 125 kHz, about 1.8 kbit/s.
    pub const LONG_RANGE_LORA: Modulation = Modulation::LoRa {
        spreading_factor: 9,
        bandwidth: LoRaBandwidth::Khz125,
        coding_rate: CodingRate::Cr4_5,
        preamble_symbols: 16,
        sync_word: 0x1424,
    };

    /// The bitrate data travels at, for the router's link costs.
    pub fn bitrate_bps(&self) -> u32 {
        match *self {
            Modulation::LoRa {
                spreading_factor,
                bandwidth,
                coding_rate,
                ..
            } => {
                let sf = u64::from(spreading_factor);
                let cr = coding_rate as u64;
                (sf * u64::from(bandwidth.hz()) * 4 / ((1 << sf) * (4 + cr))) as u32
            }
            Modulation::Gfsk { bitrate_bps, .. } => bitrate_bps,
        }
    }

    /// Microseconds on air for a frame of `len` bytes: for LoRa at
    /// spreading factors 7 to 12, as the datasheet (section 6.1.4) works it
    /// out with an explicit header and CRC (5 and 6 take a little less); for
    /// GFSK, the bits over the bitrate.
    pub fn airtime_us(&self, len: usize) -> u64 {
        match *self {
            Modulation::LoRa {
                spreading_factor,
                bandwidth,
                coding_rate,
                preamble_symbols,
                ..
            } => {
                let sf = i64::from(spreading_factor);
                let ldro = i64::from(self.low_data_rate_optimize());
                let bits = 8 * len as i64 + 16 - 4 * sf + 8 + 20;
                let per_block = 4 * (sf - 2 * ldro);
                let blocks = if bits > 0 {
                    (bits + per_block - 1) / per_block
                } else {
                    0
                };
                let payload_symbols = 8 + blocks * (4 + coding_rate as i64);
                // Preamble, plus 4.25 symbols of sync word, in quarter symbols.
                let quarter_symbols = 4 * (i64::from(preamble_symbols) + payload_symbols) + 17;
                let symbol_us = (1_000_000u64 << sf) / u64::from(bandwidth.hz());
                quarter_symbols as u64 * symbol_us / 4
            }
            Modulation::Gfsk {
                bitrate_bps,
                preamble_bits,
                ..
            } => {
                // Preamble, 4-byte sync word, length byte, payload and CRC.
                let bits = u64::from(preamble_bits) + 8 * (4 + 1 + len as u64 + 2);
                bits * 1_000_000 / u64::from(bitrate_bps.max(1))
            }
        }
    }

    /// LoRa's low data rate optimization, which the datasheet requires when
    /// a symbol lasts 16 ms or more.
    pub(crate) fn low_data_rate_optimize(&self) -> u8 {
        match *self {
            Modulation::LoRa {
                spreading_factor,
                bandwidth,
                ..
            } => {
                let symbol_us = (1_000_000u64 << spreading_factor) / u64::from(bandwidth.hz());
                u8::from(symbol_us >= 16_380)
            }
            Modulation::Gfsk { .. } => 0,
        }
    }
}

/// Everything the radio needs to start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub board: Board,
    pub frequency_hz: u32,
    /// Transmit power in dBm: -9 to +22 on the SX1262, -17 to +15 on the
    /// SX1261. Mind your region's limits.
    pub power_dbm: i8,
    pub modulation: Modulation,
}

/// The chip's frequency register value: steps of 32 MHz / 2^25.
pub(crate) fn frequency_steps(hz: u32) -> u32 {
    ((u64::from(hz) << 25) / 32_000_000) as u32
}

/// The two bytes for image calibration around `hz` (datasheet table 9-2),
/// which tunes the receiver's image rejection for the band.
pub(crate) fn image_calibration(hz: u32) -> [u8; 2] {
    match hz {
        902_000_000.. => [0xE1, 0xE9],
        779_000_000..=901_999_999 => {
            if hz >= 863_000_000 {
                [0xD7, 0xDB]
            } else {
                [0xC1, 0xC5]
            }
        }
        470_000_000..=778_999_999 => [0x75, 0x81],
        _ => [0x6B, 0x6F],
    }
}

/// The 3-byte GFSK bitrate register: 32 times the 32 MHz crystal over the bitrate.
pub(crate) fn fsk_bitrate(bps: u32) -> [u8; 3] {
    let value = (32 * 32_000_000u64 / u64::from(bps.max(1))) as u32;
    let [_, a, b, c] = value.to_be_bytes();
    [a, b, c]
}

/// The 3-byte GFSK deviation register, in frequency steps.
pub(crate) fn fsk_deviation(hz: u32) -> [u8; 3] {
    let [_, a, b, c] = frequency_steps(hz).to_be_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_become_register_values() {
        // Worked examples from Semtech's reference code.
        assert_eq!(frequency_steps(915_000_000), 0x3930_0000);
        assert_eq!(frequency_steps(868_000_000), 0x3640_0000);
        assert_eq!(image_calibration(915_000_000), [0xE1, 0xE9]);
        assert_eq!(image_calibration(868_000_000), [0xD7, 0xDB]);
        assert_eq!(image_calibration(433_000_000), [0x6B, 0x6F]);
        // 250 kbit/s: 1,024,000,000 / 250,000 = 4096.
        assert_eq!(fsk_bitrate(250_000), [0x00, 0x10, 0x00]);
        // 125 kHz in steps of 32 MHz / 2^25: 131,072.
        assert_eq!(fsk_deviation(125_000), [0x02, 0x00, 0x00]);
    }

    #[test]
    fn lora_bitrates_and_airtimes_match_the_usual_figures() {
        assert_eq!(Modulation::LONG_RANGE_LORA.bitrate_bps(), 1757);
        // SF7, 125 kHz, CR 4/5: about 5.5 kbit/s.
        let sf7 = Modulation::LoRa {
            spreading_factor: 7,
            bandwidth: LoRaBandwidth::Khz125,
            coding_rate: CodingRate::Cr4_5,
            preamble_symbols: 8,
            sync_word: 0x1424,
        };
        assert_eq!(sf7.bitrate_bps(), 5468);
        // Semtech's calculator: 10 bytes at SF7/125 kHz with an 8-symbol
        // preamble, explicit header and CRC: 41.22 ms.
        assert_eq!(sf7.airtime_us(10), 41_216);
        // SF12 at 125 kHz needs low data rate optimization; SF9 doesn't.
        let sf12 = Modulation::LoRa {
            spreading_factor: 12,
            bandwidth: LoRaBandwidth::Khz125,
            coding_rate: CodingRate::Cr4_5,
            preamble_symbols: 8,
            sync_word: 0x1424,
        };
        assert_eq!(sf12.low_data_rate_optimize(), 1);
        assert_eq!(Modulation::LONG_RANGE_LORA.low_data_rate_optimize(), 0);
        // 250 bytes of GFSK at 250 kbit/s: 64 + 8 * 257 bits.
        assert_eq!(Modulation::FAST_FSK.airtime_us(250), 8_480);
    }
}
