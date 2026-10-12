//! `spectramesh-modem`: turns an ESP32 into a USB radio modem.
//!
//! It runs no router. It passes every frame its radio hears to the host over
//! USB, and sends whatever the host asks it to, using the protocol in
//! `spectramesh_core::modem`. The host (`spectrameshd` with a `kind=modem`
//! hue) is the mesh node, with its own identity and keys, so the modem holds
//! no secrets and needs no mesh key.
//!
//! It doesn't log either: the USB serial link carries only the protocol.
//!
//! The radio is ESP-NOW, or 802.15.4 when built with `--no-default-features
//! --features ieee802154`; the modem tells the host which. A computer with
//! one of each joins both hues, and routes between them.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

use alloc::vec::Vec;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embedded_io_async::{Read, Write};
use esp_backtrace as _;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
#[cfg(feature = "xiao-esp32c6")]
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx};
#[cfg(feature = "espnow")]
use esp_radio::esp_now::EspNowReceiver;
#[cfg(feature = "ieee802154")]
use esp_radio::ieee802154::Ieee802154;
#[cfg(feature = "espnow")]
use esp_radio::wifi::{Interface, WifiController};
use spectramesh_core::modem::{
    FromModem, ModemInfo, ModemRadio, PROTOCOL_VERSION, SlipDecoder, ToModem, slip_encode,
};
// Also linked for its panic handling: restart rather than hang.
#[cfg(feature = "espnow")]
use spectramesh_esp::espnow::{self, Peers};
#[cfg(feature = "ieee802154")]
use spectramesh_esp::ieee802154::{self, Link};

esp_bootloader_esp_idf::esp_app_desc!();

/// Transmit power in quarter dBm: 80 is 20 dBm.
#[cfg(feature = "espnow")]
const TX_POWER_QUARTER_DBM: i8 = 80;

/// The Wi-Fi channel ESP-NOW uses; it must match the mesh's ESP32 nodes.
#[cfg(feature = "espnow")]
const ESPNOW_CHANNEL: u8 = 1;
/// ESP-NOW's default PHY rate.
#[cfg(feature = "espnow")]
const BITRATE_BPS: u32 = 1_000_000;

/// The 802.15.4 channel; it must match the mesh's 802.15.4 nodes.
#[cfg(feature = "ieee802154")]
const IEEE802154_CHANNEL: u8 = spectramesh_esp::radio::DEFAULT_CHANNEL;

/// The largest frame the radio carries.
#[cfg(feature = "espnow")]
const MTU: usize = espnow::MTU;
#[cfg(feature = "ieee802154")]
const MTU: usize = ieee802154::MTU;

/// Frames from the host for the 802.15.4 task, which owns the radio.
#[cfg(feature = "ieee802154")]
static TO_RADIO: Channel<CriticalSectionRawMutex, Vec<u8>, 8> = Channel::new();

/// SLIP-framed messages waiting to go to the host. When the host isn't
/// reading, frames are dropped rather than holding up the radio.
static TO_HOST: Channel<CriticalSectionRawMutex, Vec<u8>, 32> = Channel::new();

fn to_host(message: FromModem<'_>) {
    let mut framed = Vec::new();
    slip_encode(&message.encode(), &mut framed);
    let _ = TO_HOST.try_send(framed);
}

#[cfg(feature = "espnow")]
#[embassy_executor::task]
async fn radio_to_host(mut receiver: EspNowReceiver) -> ! {
    loop {
        let received = receiver.receive_async().await;
        to_host(FromModem::Received {
            src: received.info.src_address,
            rssi: received.info.rx_control.rssi.clamp(-128, 127) as i8,
            frame: received.data(),
        });
    }
}

/// Runs the 802.15.4 radio: what it hears goes to the host, and what the
/// host sends goes out.
#[cfg(feature = "ieee802154")]
#[embassy_executor::task]
async fn ieee802154_radio(mut link: Link) -> ! {
    use embassy_futures::select::{Either, select};
    loop {
        match select(link.wait(), TO_RADIO.receive()).await {
            Either::First(()) => {
                for incoming in link.take_received() {
                    // The host keys neighbors by 6-byte addresses: use the
                    // low 6 bytes of the sender's 8.
                    let mut src = [0u8; 6];
                    if let Some(address) = incoming.src {
                        src.copy_from_slice(&address.to_be_bytes()[2..]);
                    }
                    to_host(FromModem::Received {
                        src,
                        rssi: incoming.rssi,
                        frame: &incoming.frame,
                    });
                }
            }
            Either::Second(frame) => link.send(frame).await,
        }
    }
}

#[embassy_executor::task]
async fn write_to_host(mut tx: UsbSerialJtagTx<'static, Async>) -> ! {
    loop {
        let framed = TO_HOST.receive().await;
        // Errors only mean no host is listening; the frame is dropped.
        let _ = tx.write_all(&framed).await;
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // See main.rs: the XIAO ESP32-C6's antenna switch.
    #[cfg(feature = "xiao-esp32c6")]
    let _antenna = (
        Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default()),
    );

    #[cfg(feature = "espnow")]
    let (_wifi, mut sender, mut peers, info) = {
        let mut wifi = WifiController::new(peripherals.WIFI, Default::default())
            .expect("failed to start Wi-Fi");
        // esp-radio starts the radio at 5 dBm. Mesh links need range, so use
        // 20 dBm (units are quarter dBm), the most 2.4 GHz rules allow
        // without a high-gain antenna.
        wifi.set_max_tx_power(TX_POWER_QUARTER_DBM)
            .expect("failed to set transmit power");
        let station = Interface::station();
        let esp_now = wifi.esp_now();
        esp_now
            .set_channel(ESPNOW_CHANNEL)
            .expect("failed to set the ESP-NOW channel");
        let (manager, sender, receiver) = esp_now.split();
        spawner.spawn(radio_to_host(receiver).expect("task already running"));
        let info = ModemInfo {
            version: PROTOCOL_VERSION,
            mtu: MTU as u16,
            bitrate_bps: BITRATE_BPS,
            channel: ESPNOW_CHANNEL,
            address: station.mac_address(),
            radio: ModemRadio::EspNow,
        };
        (wifi, sender, Peers::new(manager), info)
    };
    #[cfg(feature = "ieee802154")]
    let info = {
        let mac = *esp_hal::efuse::base_mac_address()
            .as_bytes()
            .first_chunk::<6>()
            .expect("6 bytes");
        let link = Link::new(
            Ieee802154::new(peripherals.IEEE802154),
            ieee802154::eui64(mac),
            IEEE802154_CHANNEL,
        );
        spawner.spawn(ieee802154_radio(link).expect("task already running"));
        ModemInfo {
            version: PROTOCOL_VERSION,
            mtu: MTU as u16,
            bitrate_bps: 250_000,
            channel: IEEE802154_CHANNEL,
            address: mac,
            radio: ModemRadio::Ieee802154,
        }
    };

    let (mut rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();
    spawner.spawn(write_to_host(tx).expect("task already running"));
    let info = FromModem::Info(info);
    to_host(info);

    // Frames from the host can be as big as the radio allows, plus framing.
    let mut decoder = SlipDecoder::new(MTU + 16);
    let mut buf = [0u8; 256];
    loop {
        // Reading from the USB serial link can't fail.
        let Ok(len) = rx.read(&mut buf).await;
        for &byte in &buf[..len] {
            let Some(message) = decoder.push(byte) else {
                continue;
            };
            match ToModem::decode(&message) {
                // ESP-NOW unicasts to a neighbor the host names, or
                // broadcasts. See espnow.rs.
                #[cfg(feature = "espnow")]
                Ok(ToModem::Send { dst, frame }) => {
                    let _ = espnow::send_frame(&mut sender, &mut peers, Some(dst), frame).await;
                }
                // 802.15.4 broadcasts everything. See ieee802154.rs.
                #[cfg(feature = "ieee802154")]
                Ok(ToModem::Send { frame, .. }) => {
                    if TO_RADIO.try_send(frame.to_vec()).is_err() {
                        // The radio is behind; drop the frame, as a radio would.
                    }
                }
                Ok(ToModem::InfoRequest) => to_host(info),
                Err(_) => {}
            }
        }
    }
}
