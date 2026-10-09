#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{EspNowReceiver, EspNowSender};
use esp_radio::wifi::{Interface, WifiController};
use log::info;
use spectramesh_core::{Config, HueId, Router};
use spectramesh_esp::mesh::{self, Outbox};
use spectramesh_esp::{espnow, node_id};

extern crate alloc;

// The app descriptor the ESP-IDF bootloader expects.
esp_bootloader_esp_idf::esp_app_desc!();

/// The Wi-Fi channel every node's ESP-NOW hue uses.
const ESPNOW_CHANNEL: u8 = 1;

const ESPNOW_HUE: HueId = HueId(0);

static ESPNOW_OUTBOX: Outbox = Outbox::new();
static OUTBOXES: [(HueId, &Outbox); 1] = [(ESPNOW_HUE, &ESPNOW_OUTBOX)];

#[embassy_executor::task]
async fn mesh_task(router: Router) -> ! {
    mesh::run(router, &OUTBOXES).await
}

#[embassy_executor::task]
async fn espnow_receive_task(receiver: EspNowReceiver) -> ! {
    espnow::receive(ESPNOW_HUE, receiver).await
}

#[embassy_executor::task]
async fn espnow_send_task(sender: EspNowSender) -> ! {
    espnow::send(sender, &ESPNOW_OUTBOX).await
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let wifi =
        WifiController::new(peripherals.WIFI, Default::default()).expect("failed to start Wi-Fi");
    let station = Interface::station();
    let id = node_id(station.mac_address());

    let esp_now = wifi.esp_now();
    esp_now
        .set_channel(ESPNOW_CHANNEL)
        .expect("failed to set the ESP-NOW channel");
    let (_manager, sender, receiver) = esp_now.split();

    let mut router = Router::new(id, Config::default());
    router.add_hue(espnow::hue_info(ESPNOW_HUE));

    spawner.spawn(espnow_receive_task(receiver).expect("task already running"));
    spawner.spawn(espnow_send_task(sender).expect("task already running"));
    spawner.spawn(mesh_task(router).expect("task already running"));
    info!("SpectraMesh node {id} running on ESP-NOW channel {ESPNOW_CHANNEL}");

    // Wi-Fi stays up while `wifi`, `station` and `_manager` live, so main never returns.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}
