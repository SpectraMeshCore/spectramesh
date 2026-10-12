//! `spectramesh-modem`: turns an ESP32 into a USB radio modem.
//!
//! It runs no router. It passes every ESP-NOW frame it hears to the host over
//! USB, and sends whatever the host asks it to, using the protocol in
//! `spectramesh_core::modem`. The host (`spectrameshd` with a `kind=modem`
//! hue) is the mesh node, with its own identity and keys, so the modem holds
//! no secrets and needs no mesh key.
//!
//! It doesn't log either: the USB serial link carries only the protocol.

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
use esp_radio::esp_now::{BROADCAST_ADDRESS, ESP_NOW_MAX_DATA_LEN_V1, EspNowReceiver};
use esp_radio::wifi::{Interface, WifiController};
use spectramesh_core::modem::{
    FromModem, ModemInfo, PROTOCOL_VERSION, SlipDecoder, ToModem, slip_encode,
};

esp_bootloader_esp_idf::esp_app_desc!();

/// The Wi-Fi channel ESP-NOW uses; it must match the mesh's ESP32 nodes.
const ESPNOW_CHANNEL: u8 = 1;
/// ESP-NOW's default PHY rate.
const BITRATE_BPS: u32 = 1_000_000;

/// SLIP-framed messages waiting to go to the host. When the host isn't
/// reading, frames are dropped rather than holding up the radio.
static TO_HOST: Channel<CriticalSectionRawMutex, Vec<u8>, 32> = Channel::new();

fn to_host(message: FromModem<'_>) {
    let mut framed = Vec::new();
    slip_encode(&message.encode(), &mut framed);
    let _ = TO_HOST.try_send(framed);
}

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

    let wifi =
        WifiController::new(peripherals.WIFI, Default::default()).expect("failed to start Wi-Fi");
    let station = Interface::station();
    let esp_now = wifi.esp_now();
    esp_now
        .set_channel(ESPNOW_CHANNEL)
        .expect("failed to set the ESP-NOW channel");
    let (_manager, mut sender, receiver) = esp_now.split();

    let (mut rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();
    spawner.spawn(radio_to_host(receiver).expect("task already running"));
    spawner.spawn(write_to_host(tx).expect("task already running"));

    let info = FromModem::Info(ModemInfo {
        version: PROTOCOL_VERSION,
        mtu: ESP_NOW_MAX_DATA_LEN_V1 as u16,
        bitrate_bps: BITRATE_BPS,
        channel: ESPNOW_CHANNEL,
        address: station.mac_address(),
    });
    to_host(info);

    // Frames from the host can be as big as ESP-NOW allows, plus framing.
    let mut decoder = SlipDecoder::new(ESP_NOW_MAX_DATA_LEN_V1 + 16);
    let mut buf = [0u8; 256];
    loop {
        // Reading from the USB serial link can't fail.
        let Ok(len) = rx.read(&mut buf).await;
        for &byte in &buf[..len] {
            let Some(message) = decoder.push(byte) else {
                continue;
            };
            match ToModem::decode(&message) {
                // Everything goes to the broadcast address; receivers check
                // the next hop in the SpectraMesh header. See espnow.rs.
                Ok(ToModem::Send { frame, .. }) => {
                    let _ = sender.send_async(&BROADCAST_ADDRESS, frame).await;
                }
                Ok(ToModem::InfoRequest) => to_host(info),
                Err(_) => {}
            }
        }
    }
}
