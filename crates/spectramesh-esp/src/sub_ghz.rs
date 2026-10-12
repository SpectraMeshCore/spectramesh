//! A Semtech SX1262 radio as a hue: 915 MHz (or 868 MHz) links, in GFSK for
//! fast backbone links or LoRa for long, slow ones (see
//! `spectramesh-sx126x`).
//!
//! The radio is half duplex. It listens all the time, and stops to send
//! when the router has a frame for it, first waiting for any frame already
//! arriving, so it doesn't talk over a neighbor. Frames are at most 255
//! bytes; the router fragments anything bigger. The radio has no link-layer
//! addresses, so everything is broadcast; receivers use the next hop in the
//! SpectraMesh header.
//!
//! Board wiring (SPI pins, reset, BUSY, DIO1, oscillator and antenna switch)
//! differs between boards, so `main` creates the driver; this module only
//! runs it.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_hal::digital::OutputPin;
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::digital::Wait;
use embedded_hal_async::spi::SpiDevice;
use log::{info, warn};
use spectramesh_core::{HueId, HueInfo, HueKind};
use spectramesh_sx126x::{Config, MAX_PACKET, Modulation, Sx126x};

use crate::mesh::{INBOX, Outbox, Received};

/// Describes the radio's hue for the router.
pub fn hue_info(id: HueId, config: &Config) -> HueInfo {
    let freq_khz = config.frequency_hz / 1000;
    let kind = match config.modulation {
        Modulation::Gfsk { .. } => HueKind::Fsk { freq_khz },
        Modulation::LoRa { .. } => HueKind::LoRa { freq_khz },
    };
    HueInfo::new(
        id,
        kind,
        MAX_PACKET as u16,
        config.modulation.bitrate_bps().into(),
    )
}

/// Runs the radio forever: receives into the router's inbox and sends what
/// the router queues on `outbox`.
pub async fn run<SPI, RESET, BUSY, DIO1, DELAY>(
    mut radio: Sx126x<SPI, RESET, BUSY, DIO1, DELAY>,
    hue: HueId,
    outbox: &'static Outbox,
) -> !
where
    SPI: SpiDevice,
    SPI::Error: core::fmt::Debug,
    RESET: OutputPin,
    BUSY: Wait,
    DIO1: Wait,
    DELAY: DelayNs,
{
    // The longest a frame can take on air, which is how long to wait for
    // one that has started arriving.
    let longest = Duration::from_micros(radio.config().modulation.airtime_us(MAX_PACKET));
    let mut buf = [0u8; MAX_PACKET];
    loop {
        // (Re)start the radio, and keep going until something fails.
        let result: Result<(), spectramesh_sx126x::Error<SPI::Error>> = async {
            radio.init().await?;
            radio.start_receiving().await?;
            info!(
                "sub-GHz radio on hue {} at {} kHz",
                hue.0,
                radio.config().frequency_hz / 1000
            );
            loop {
                match select(radio.wait_for_interrupt(), outbox.receive()).await {
                    Either::First(interrupt) => {
                        interrupt?;
                        deliver(&mut radio, hue, &mut buf).await?;
                    }
                    Either::Second(outgoing) => {
                        // Listen before talking: let a frame that's arriving finish.
                        if radio.is_receiving().await? {
                            match with_timeout(longest, radio.wait_for_interrupt()).await {
                                Ok(interrupt) => {
                                    interrupt?;
                                    deliver(&mut radio, hue, &mut buf).await?;
                                }
                                Err(_) => radio.clear_receiving().await?,
                            }
                        }
                        if let Err(err) = radio.transmit(&outgoing.frame).await {
                            warn!("sub-GHz send failed: {err:?}");
                        }
                        radio.start_receiving().await?;
                    }
                }
            }
        }
        .await;
        if let Err(err) = result {
            warn!("sub-GHz radio failed: {err:?}; restarting it");
            Timer::after(Duration::from_secs(1)).await;
        }
    }
}

/// Passes what the radio received to the router.
async fn deliver<SPI, RESET, BUSY, DIO1, DELAY>(
    radio: &mut Sx126x<SPI, RESET, BUSY, DIO1, DELAY>,
    hue: HueId,
    buf: &mut [u8; MAX_PACKET],
) -> Result<(), spectramesh_sx126x::Error<SPI::Error>>
where
    SPI: SpiDevice,
    RESET: OutputPin,
    BUSY: Wait,
    DIO1: Wait,
    DELAY: DelayNs,
{
    if let Some(packet) = radio.read_packet(buf).await? {
        INBOX
            .send(Received {
                hue,
                frame: buf[..packet.len].to_vec(),
                src: None,
                rssi: Some(packet.rssi_dbm.clamp(-128, 127) as i8),
            })
            .await;
    }
    Ok(())
}
