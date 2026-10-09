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
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::esp_now::{EspNowReceiver, EspNowSender};
use esp_radio::wifi::WifiController;
use log::{error, info};
use spectramesh_core::{Config, HueId, KeyRing, MeshKey, Router};
use spectramesh_esp::mesh::{self, Outbox};
use spectramesh_esp::{espnow, identity_store};

extern crate alloc;

use alloc::boxed::Box;

// The app descriptor the ESP-IDF bootloader expects.
esp_bootloader_esp_idf::esp_app_desc!();

/// The mesh key, set when building. Every node in the mesh needs the same one:
///
/// ```text
/// SPECTRAMESH_MESH_KEY=smk1-... cargo run --release
/// ```
//
// TODO: provision the key over the serial console and keep it in flash, so
// one firmware image works for every mesh.
const MESH_KEY: Option<&str> = option_env!("SPECTRAMESH_MESH_KEY");

/// The Wi-Fi channel every node's ESP-NOW hue uses.
const ESPNOW_CHANNEL: u8 = 1;

const ESPNOW_HUE: HueId = HueId(0);

static ESPNOW_OUTBOX: Outbox = Outbox::new();
static OUTBOXES: [(HueId, &Outbox); 1] = [(ESPNOW_HUE, &ESPNOW_OUTBOX)];

#[embassy_executor::task]
async fn mesh_task(router: Box<Router>) -> ! {
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

    let mesh_key = match MESH_KEY.map(MeshKey::from_text) {
        Some(Ok(key)) => key,
        Some(Err(err)) => halt(format_args!("SPECTRAMESH_MESH_KEY is invalid: {err}")).await,
        None => {
            halt(format_args!(
                "built without a mesh key; rebuild with SPECTRAMESH_MESH_KEY set \
                 (make one with `spectrameshd --generate-mesh-key`)"
            ))
            .await
        }
    };

    let wifi =
        WifiController::new(peripherals.WIFI, Default::default()).expect("failed to start Wi-Fi");
    // With Wi-Fi running, the hardware RNG is a true random source.
    let rng = Rng::new();
    let identity = identity_store::load_or_create(peripherals.FLASH, &rng)
        .expect("failed to read or store this node's identity in flash");
    let id = identity.node_id();
    let mut seed = [0u8; 32];
    rng.read(&mut seed);

    let esp_now = wifi.esp_now();
    esp_now
        .set_channel(ESPNOW_CHANNEL)
        .expect("failed to set the ESP-NOW channel");
    let (_manager, sender, receiver) = esp_now.split();

    let mut router = Router::new(identity, KeyRing::new(&mesh_key), Config::default(), seed);
    router.add_hue(espnow::hue_info(ESPNOW_HUE));

    spawner.spawn(espnow_receive_task(receiver).expect("task already running"));
    spawner.spawn(espnow_send_task(sender).expect("task already running"));
    // Boxed: the router is too big to pass on a microcontroller's stack.
    spawner.spawn(mesh_task(Box::new(router)).expect("task already running"));
    info!("SpectraMesh node {id} running on ESP-NOW channel {ESPNOW_CHANNEL}");

    // Wi-Fi stays up while `wifi` and `_manager` live, so main never returns.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

/// Logs why the node can't start, then waits forever.
async fn halt(reason: core::fmt::Arguments<'_>) -> ! {
    loop {
        error!("{reason}");
        Timer::after(Duration::from_secs(10)).await;
    }
}
