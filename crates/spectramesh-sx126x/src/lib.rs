//! An async driver for Semtech's SX1261 and SX1262 sub-GHz radios, in LoRa
//! and GFSK, written for SpectraMesh's 915 MHz (and 868 MHz) hues.
//!
//! It's built on the `embedded-hal` 1.0 traits, so it runs on any
//! microcontroller with an async HAL: an SPI device, the reset pin, and the
//! BUSY and DIO1 inputs. The radio is half duplex, so a hue built on it
//! receives continuously and stops to send:
//!
//! ```ignore
//! radio.init().await?;
//! radio.start_receiving().await?;
//! loop {
//!     // Race the radio's interrupt against frames waiting to be sent.
//!     match select(radio.wait_for_interrupt(), outbox.receive()).await {
//!         Either::First(_) => {
//!             if let Some(packet) = radio.read_packet(&mut buf).await? { /* ... */ }
//!         }
//!         Either::Second(frame) => {
//!             radio.transmit(&frame).await?;
//!             radio.start_receiving().await?;
//!         }
//!     }
//! }
//! ```
//!
//! Frames are up to 255 bytes in both modes, with the radio's own CRC, so
//! damaged frames are dropped before they reach the router.
//!
//! **Regulations.** In the US, 902-928 MHz is unlicensed under FCC part 15,
//! but transmitting at high power on one channel, as this driver does, is
//! allowed only for wide-band signals (a 500 kHz 6 dB bandwidth or more,
//! such as GFSK at 250 kbit/s); narrower LoRa settings at high power need
//! frequency hopping. Europe's 868 MHz bands limit power and duty cycle.
//! Check your region's rules before choosing power and modulation.

#![no_std]
#![forbid(unsafe_code)]

mod config;

pub use config::{
    Board, Chip, CodingRate, Config, FskBandwidth, LoRaBandwidth, Modulation, TcxoVoltage,
};

use config::{frequency_steps, fsk_bitrate, fsk_deviation, image_calibration};
use embedded_hal::digital::OutputPin;
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::{Operation, SpiDevice};

/// The largest frame, in bytes.
pub const MAX_PACKET: usize = 255;

/// Opcodes, from the datasheet's section 13.
mod op {
    pub const SET_STANDBY: u8 = 0x80;
    pub const SET_RX: u8 = 0x82;
    pub const SET_TX: u8 = 0x83;
    pub const SET_RF_FREQUENCY: u8 = 0x86;
    pub const SET_PACKET_TYPE: u8 = 0x8A;
    pub const SET_MODULATION_PARAMS: u8 = 0x8B;
    pub const SET_PACKET_PARAMS: u8 = 0x8C;
    pub const SET_TX_PARAMS: u8 = 0x8E;
    pub const SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
    pub const CALIBRATE: u8 = 0x89;
    pub const SET_PA_CONFIG: u8 = 0x95;
    pub const SET_REGULATOR_MODE: u8 = 0x96;
    pub const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
    pub const CALIBRATE_IMAGE: u8 = 0x98;
    pub const SET_DIO2_AS_RF_SWITCH_CTRL: u8 = 0x9D;
    pub const SET_DIO_IRQ_PARAMS: u8 = 0x08;
    pub const CLEAR_IRQ_STATUS: u8 = 0x02;
    pub const CLEAR_DEVICE_ERRORS: u8 = 0x07;
    pub const GET_IRQ_STATUS: u8 = 0x12;
    pub const GET_RX_BUFFER_STATUS: u8 = 0x13;
    pub const GET_PACKET_STATUS: u8 = 0x14;
    pub const WRITE_REGISTER: u8 = 0x0D;
    pub const READ_REGISTER: u8 = 0x1D;
    pub const WRITE_BUFFER: u8 = 0x0E;
    pub const READ_BUFFER: u8 = 0x1E;
}

/// Registers this driver touches.
mod reg {
    pub const FSK_CRC_INIT: u16 = 0x06BC;
    pub const FSK_CRC_POLYNOMIAL: u16 = 0x06BE;
    pub const FSK_SYNC_WORD: u16 = 0x06C0;
    pub const LORA_IQ_POLARITY: u16 = 0x0736;
    pub const LORA_SYNC_WORD: u16 = 0x0740;
    pub const TX_MODULATION: u16 = 0x0889;
    pub const RX_GAIN: u16 = 0x08AC;
    pub const TX_CLAMP: u16 = 0x08D8;
    pub const OCP: u16 = 0x08E7;
}

/// Interrupt flags.
pub mod irq {
    pub const TX_DONE: u16 = 1 << 0;
    pub const RX_DONE: u16 = 1 << 1;
    pub const PREAMBLE_DETECTED: u16 = 1 << 2;
    pub const SYNC_WORD_VALID: u16 = 1 << 3;
    pub const HEADER_VALID: u16 = 1 << 4;
    pub const HEADER_ERROR: u16 = 1 << 5;
    pub const CRC_ERROR: u16 = 1 << 6;
    pub const TIMEOUT: u16 = 1 << 9;
    pub const ALL: u16 = 0x03FF;
}

const PACKET_TYPE_GFSK: u8 = 0;
const PACKET_TYPE_LORA: u8 = 1;
/// Standby on the internal RC oscillator.
const STANDBY_RC: u8 = 0;
/// Receive until told otherwise.
const RX_CONTINUOUS: [u8; 3] = [0xFF, 0xFF, 0xFF];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<E> {
    Spi(E),
    /// A GPIO (reset, BUSY or DIO1) failed.
    Pin,
    /// The radio reported a timeout, so a send didn't finish.
    Timeout,
    /// The frame is longer than [`MAX_PACKET`].
    TooLong,
}

/// A frame that arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub len: usize,
    /// Signal strength in dBm.
    pub rssi_dbm: i16,
    /// LoRa's signal-to-noise ratio in dB, which goes below zero: LoRa
    /// receives under the noise floor.
    pub snr_db: Option<i8>,
}

/// The radio, and the pins it needs.
pub struct Sx126x<SPI, RESET, BUSY, DIO1, DELAY> {
    spi: SPI,
    reset: RESET,
    busy: BUSY,
    dio1: DIO1,
    delay: DELAY,
    config: Config,
}

impl<SPI, RESET, BUSY, DIO1, DELAY> Sx126x<SPI, RESET, BUSY, DIO1, DELAY>
where
    SPI: SpiDevice,
    RESET: OutputPin,
    BUSY: Wait,
    DIO1: Wait,
    DELAY: DelayNs,
{
    pub fn new(
        spi: SPI,
        reset: RESET,
        busy: BUSY,
        dio1: DIO1,
        delay: DELAY,
        config: Config,
    ) -> Self {
        Sx126x {
            spi,
            reset,
            busy,
            dio1,
            delay,
            config,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Resets the radio and sets it up as configured. It's left in standby.
    pub async fn init(&mut self) -> Result<(), Error<SPI::Error>> {
        self.reset.set_low().map_err(|_| Error::Pin)?;
        self.delay.delay_ms(1).await;
        self.reset.set_high().map_err(|_| Error::Pin)?;
        self.delay.delay_ms(10).await;
        self.command(op::SET_STANDBY, &[STANDBY_RC]).await?;

        let board = self.config.board;
        if let Some(voltage) = board.tcxo {
            // Power the oscillator and give it 5 ms (in 15.625 µs steps) to settle.
            self.command(
                op::SET_DIO3_AS_TCXO_CTRL,
                &[voltage as u8, 0x00, 0x01, 0x40],
            )
            .await?;
            // Calibrate everything again now that the clock runs, and clear
            // the oscillator error that starting without it leaves.
            self.command(op::CALIBRATE, &[0x7F]).await?;
            self.delay.delay_ms(5).await;
            self.command(op::CLEAR_DEVICE_ERRORS, &[0, 0]).await?;
        }
        self.command(op::SET_REGULATOR_MODE, &[u8::from(board.dc_dc)])
            .await?;
        self.command(op::SET_BUFFER_BASE_ADDRESS, &[0, 0]).await?;

        let packet_type = match self.config.modulation {
            Modulation::LoRa { .. } => PACKET_TYPE_LORA,
            Modulation::Gfsk { .. } => PACKET_TYPE_GFSK,
        };
        self.command(op::SET_PACKET_TYPE, &[packet_type]).await?;
        self.command(
            op::SET_RF_FREQUENCY,
            &frequency_steps(self.config.frequency_hz).to_be_bytes(),
        )
        .await?;
        self.command(
            op::CALIBRATE_IMAGE,
            &image_calibration(self.config.frequency_hz),
        )
        .await?;

        // Power amplifier settings for the chip's full power (datasheet 13.1.14).
        let (pa_config, ocp, max_dbm, min_dbm) = match board.chip {
            Chip::Sx1262 => ([0x04, 0x07, 0x00, 0x01], 0x38, 22, -9),
            Chip::Sx1261 => ([0x06, 0x00, 0x01, 0x01], 0x18, 15, -17),
        };
        self.command(op::SET_PA_CONFIG, &pa_config).await?;
        let power = self.config.power_dbm.clamp(min_dbm, max_dbm);
        // Ramp the amplifier up over 200 µs.
        self.command(op::SET_TX_PARAMS, &[power as u8, 0x04])
            .await?;
        // Over-current protection: 140 mA (SX1262) or 60 mA (SX1261).
        self.write_register(reg::OCP, &[ocp]).await?;
        if board.chip == Chip::Sx1262 {
            // Errata 15.2: better tolerance of a mismatched antenna.
            let clamp = self.read_register(reg::TX_CLAMP).await?;
            self.write_register(reg::TX_CLAMP, &[clamp | 0x1E]).await?;
        }

        self.set_modulation().await?;
        self.set_packet_params(MAX_PACKET as u8).await?;

        let all = irq::ALL.to_be_bytes();
        let dio1 =
            (irq::TX_DONE | irq::RX_DONE | irq::TIMEOUT | irq::CRC_ERROR | irq::HEADER_ERROR)
                .to_be_bytes();
        self.command(
            op::SET_DIO_IRQ_PARAMS,
            &[all[0], all[1], dio1[0], dio1[1], 0, 0, 0, 0],
        )
        .await?;
        if board.dio2_rf_switch {
            self.command(op::SET_DIO2_AS_RF_SWITCH_CTRL, &[1]).await?;
        }
        // Boosted receiver gain: a few dB more sensitivity for a little more current.
        self.write_register(reg::RX_GAIN, &[0x96]).await?;
        Ok(())
    }

    async fn set_modulation(&mut self) -> Result<(), Error<SPI::Error>> {
        let ldro = self.config.modulation.low_data_rate_optimize();
        match self.config.modulation {
            Modulation::LoRa {
                spreading_factor,
                bandwidth,
                coding_rate,
                sync_word,
                ..
            } => {
                self.command(
                    op::SET_MODULATION_PARAMS,
                    &[spreading_factor, bandwidth.code(), coding_rate as u8, ldro],
                )
                .await?;
                self.write_register(reg::LORA_SYNC_WORD, &sync_word.to_be_bytes())
                    .await?;
                // Errata 15.1: 500 kHz needs the modulation quality bit
                // cleared, other bandwidths set.
                let quality = self.read_register(reg::TX_MODULATION).await?;
                let quality = if bandwidth == LoRaBandwidth::Khz500 {
                    quality & !0x04
                } else {
                    quality | 0x04
                };
                self.write_register(reg::TX_MODULATION, &[quality]).await?;
                // Errata 15.4: with standard IQ, set bit 2 of the polarity register.
                let iq = self.read_register(reg::LORA_IQ_POLARITY).await?;
                self.write_register(reg::LORA_IQ_POLARITY, &[iq | 0x04])
                    .await?;
            }
            Modulation::Gfsk {
                bitrate_bps,
                deviation_hz,
                bandwidth,
                sync_word,
                ..
            } => {
                let [b0, b1, b2] = fsk_bitrate(bitrate_bps);
                let [d0, d1, d2] = fsk_deviation(deviation_hz);
                // Gaussian filter with BT 0.5.
                self.command(
                    op::SET_MODULATION_PARAMS,
                    &[b0, b1, b2, 0x09, bandwidth.code(), d0, d1, d2],
                )
                .await?;
                self.write_register(reg::FSK_SYNC_WORD, &sync_word).await?;
                // CRC-16/CCITT.
                self.write_register(reg::FSK_CRC_INIT, &[0x1D, 0x0F])
                    .await?;
                self.write_register(reg::FSK_CRC_POLYNOMIAL, &[0x10, 0x21])
                    .await?;
            }
        }
        Ok(())
    }

    /// Sets the packet format, with `len` as the length to send (or the most
    /// to accept, when receiving).
    async fn set_packet_params(&mut self, len: u8) -> Result<(), Error<SPI::Error>> {
        match self.config.modulation {
            Modulation::LoRa {
                preamble_symbols, ..
            } => {
                let [p0, p1] = preamble_symbols.to_be_bytes();
                // Explicit header, CRC on, standard IQ.
                self.command(op::SET_PACKET_PARAMS, &[p0, p1, 0, len, 1, 0])
                    .await
            }
            Modulation::Gfsk { preamble_bits, .. } => {
                let [p0, p1] = preamble_bits.to_be_bytes();
                // Detect 16 preamble bits; 32-bit sync word; no address
                // filtering; variable length; 2-byte CRC; whitening.
                self.command(
                    op::SET_PACKET_PARAMS,
                    &[p0, p1, 0x05, 32, 0, 1, len, 0x02, 0x01],
                )
                .await
            }
        }
    }

    /// Sends a frame and waits until it's gone. The radio is in standby
    /// afterwards; call [`start_receiving`](Self::start_receiving) to listen again.
    pub async fn transmit(&mut self, frame: &[u8]) -> Result<(), Error<SPI::Error>> {
        if frame.len() > MAX_PACKET {
            return Err(Error::TooLong);
        }
        self.command(op::SET_STANDBY, &[STANDBY_RC]).await?;
        self.set_packet_params(frame.len() as u8).await?;
        self.write_buffer(0, frame).await?;
        self.clear_irq(irq::ALL).await?;
        // Give up after twice the frame's airtime and a margin, in 15.625 µs steps.
        let airtime_us = self.config.modulation.airtime_us(frame.len());
        let steps = ((2 * airtime_us + 100_000) * 64 / 1000).min(0x00FF_FFFF) as u32;
        let [_, t0, t1, t2] = steps.to_be_bytes();
        self.command(op::SET_TX, &[t0, t1, t2]).await?;
        self.dio1.wait_for_high().await.map_err(|_| Error::Pin)?;
        let flags = self.irq_status().await?;
        self.clear_irq(irq::ALL).await?;
        if flags & irq::TX_DONE != 0 {
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// Starts listening, until the next [`transmit`](Self::transmit).
    pub async fn start_receiving(&mut self) -> Result<(), Error<SPI::Error>> {
        self.set_packet_params(MAX_PACKET as u8).await?;
        self.clear_irq(irq::ALL).await?;
        self.command(op::SET_RX, &RX_CONTINUOUS).await
    }

    /// Waits for the radio to finish receiving something, and reads it into
    /// `buf`. `None` if it was damaged (a bad CRC or header).
    pub async fn wait_for_packet(
        &mut self,
        buf: &mut [u8],
    ) -> Result<Option<Packet>, Error<SPI::Error>> {
        self.wait_for_interrupt().await?;
        self.read_packet(buf).await
    }

    /// Waits for the radio's interrupt (DIO1): something arrived, or failed
    /// to. Only waits on a pin, so it's safe to race against other futures
    /// and drop; follow it with [`read_packet`](Self::read_packet).
    pub async fn wait_for_interrupt(&mut self) -> Result<(), Error<SPI::Error>> {
        self.dio1.wait_for_high().await.map_err(|_| Error::Pin)
    }

    /// Reads what the radio received, after
    /// [`wait_for_interrupt`](Self::wait_for_interrupt). `None` if it was
    /// damaged (a bad CRC or header), or nothing arrived.
    pub async fn read_packet(
        &mut self,
        buf: &mut [u8],
    ) -> Result<Option<Packet>, Error<SPI::Error>> {
        let flags = self.irq_status().await?;
        self.clear_irq(flags).await?;
        if flags & irq::RX_DONE == 0 || flags & (irq::CRC_ERROR | irq::HEADER_ERROR) != 0 {
            return Ok(None);
        }
        let mut status = [0u8; 2];
        self.read_command(op::GET_RX_BUFFER_STATUS, &[], &mut status)
            .await?;
        let [len, start] = status;
        let len = usize::from(len).min(buf.len());
        self.read_command(op::READ_BUFFER, &[start], &mut buf[..len])
            .await?;
        let mut packet = [0u8; 3];
        self.read_command(op::GET_PACKET_STATUS, &[], &mut packet)
            .await?;
        let (rssi_dbm, snr_db) = match self.config.modulation {
            // Packet RSSI and SNR, in -1/2 dBm and 1/4 dB.
            Modulation::LoRa { .. } => (-i16::from(packet[0]) / 2, Some((packet[1] as i8) / 4)),
            // RSSI averaged over the packet.
            Modulation::Gfsk { .. } => (-i16::from(packet[2]) / 2, None),
        };
        Ok(Some(Packet {
            len,
            rssi_dbm,
            snr_db,
        }))
    }

    /// Whether a frame is arriving right now: the radio has locked onto a
    /// sync word or header. Sending now would collide with it.
    pub async fn is_receiving(&mut self) -> Result<bool, Error<SPI::Error>> {
        let flags = self.irq_status().await?;
        let started = irq::SYNC_WORD_VALID | irq::HEADER_VALID;
        Ok(flags & started != 0 && flags & irq::RX_DONE == 0)
    }

    /// Forgets a frame that started arriving but never finished, so
    /// [`is_receiving`](Self::is_receiving) stops reporting it.
    pub async fn clear_receiving(&mut self) -> Result<(), Error<SPI::Error>> {
        self.clear_irq(irq::SYNC_WORD_VALID | irq::HEADER_VALID | irq::PREAMBLE_DETECTED)
            .await
    }

    async fn irq_status(&mut self) -> Result<u16, Error<SPI::Error>> {
        let mut flags = [0u8; 2];
        self.read_command(op::GET_IRQ_STATUS, &[], &mut flags)
            .await?;
        Ok(u16::from_be_bytes(flags))
    }

    async fn clear_irq(&mut self, flags: u16) -> Result<(), Error<SPI::Error>> {
        self.command(op::CLEAR_IRQ_STATUS, &flags.to_be_bytes())
            .await
    }

    async fn write_register(&mut self, address: u16, data: &[u8]) -> Result<(), Error<SPI::Error>> {
        let [a0, a1] = address.to_be_bytes();
        self.wait_until_ready().await?;
        self.spi
            .transaction(&mut [
                Operation::Write(&[op::WRITE_REGISTER, a0, a1]),
                Operation::Write(data),
            ])
            .await
            .map_err(Error::Spi)
    }

    async fn read_register(&mut self, address: u16) -> Result<u8, Error<SPI::Error>> {
        let mut value = [0u8];
        self.read_command(op::READ_REGISTER, &address.to_be_bytes(), &mut value)
            .await?;
        Ok(value[0])
    }

    async fn write_buffer(&mut self, offset: u8, data: &[u8]) -> Result<(), Error<SPI::Error>> {
        self.wait_until_ready().await?;
        self.spi
            .transaction(&mut [
                Operation::Write(&[op::WRITE_BUFFER, offset]),
                Operation::Write(data),
            ])
            .await
            .map_err(Error::Spi)
    }

    /// Sends a command and its parameters.
    async fn command(&mut self, opcode: u8, params: &[u8]) -> Result<(), Error<SPI::Error>> {
        self.wait_until_ready().await?;
        self.spi
            .transaction(&mut [Operation::Write(&[opcode]), Operation::Write(params)])
            .await
            .map_err(Error::Spi)
    }

    /// Sends a command and its arguments, then reads its answer, which
    /// follows a status byte.
    async fn read_command(
        &mut self,
        opcode: u8,
        args: &[u8],
        out: &mut [u8],
    ) -> Result<(), Error<SPI::Error>> {
        self.wait_until_ready().await?;
        let mut status = [0u8];
        self.spi
            .transaction(&mut [
                Operation::Write(&[opcode]),
                Operation::Write(args),
                Operation::Read(&mut status),
                Operation::Read(out),
            ])
            .await
            .map_err(Error::Spi)
    }

    /// The radio takes no commands while BUSY is high.
    async fn wait_until_ready(&mut self) -> Result<(), Error<SPI::Error>> {
        self.busy.wait_for_low().await.map_err(|_| Error::Pin)
    }
}

#[cfg(test)]
mod tests;
