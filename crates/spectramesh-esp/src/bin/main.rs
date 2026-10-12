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
use esp_hal::Async;
use esp_hal::clock::CpuClock;
#[cfg(feature = "xiao-esp32c6")]
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx};
use esp_radio::esp_now::{EspNowReceiver, EspNowSender};
use esp_radio::wifi::WifiController;
use log::{info, warn};
use spectramesh_core::{Config, HueId, KeyRing, Router};
use spectramesh_esp::console::{self, Context};
use spectramesh_esp::mesh::{self, Outbox};
use spectramesh_esp::storage::Store;
use spectramesh_esp::{espnow, reset};

extern crate alloc;

use alloc::boxed::Box;

// The app descriptor the ESP-IDF bootloader expects.
esp_bootloader_esp_idf::esp_app_desc!();

/// Transmit power in quarter dBm: 80 is 20 dBm.
const TX_POWER_QUARTER_DBM: i8 = 80;

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
async fn console_task(rx: UsbSerialJtagRx<'static, Async>, store: Store, context: Context) -> ! {
    console::run(rx, store, context).await
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
    let reset_cause = reset::cause();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // The XIAO ESP32-C6 routes its antenna through an RF switch: GPIO3 low
    // powers the switch, and GPIO14 low selects the built-in antenna (high
    // selects the external connector). Seeed's Arduino support does the same.
    // Kept alive for as long as main runs, which is forever.
    #[cfg(feature = "xiao-esp32c6")]
    let _antenna = (
        Output::new(peripherals.GPIO3, Level::Low, OutputConfig::default()),
        Output::new(peripherals.GPIO14, Level::Low, OutputConfig::default()),
    );

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut wifi =
        WifiController::new(peripherals.WIFI, Default::default()).expect("failed to start Wi-Fi");
    // esp-radio starts the radio at 5 dBm. Mesh links need range, so use
    // 20 dBm (units are quarter dBm), the most 2.4 GHz rules allow without
    // a high-gain antenna.
    wifi.set_max_tx_power(TX_POWER_QUARTER_DBM)
        .expect("failed to set transmit power");
    // With Wi-Fi running, the hardware RNG is a true random source.
    let rng = Rng::new();
    // Timed, as a measure of the elliptic-curve work on this chip: deriving
    // both public keys, and below, the router's first Ed25519 signature.
    let started = embassy_time::Instant::now();
    let mut store = Store::new(peripherals.FLASH);
    let identity = store
        .load_or_create_identity(&rng)
        .expect("failed to read or store this node's identity in flash");
    let identity_ms = started.elapsed().as_millis();
    let id = identity.node_id();
    let boots = store
        .count_boot()
        .inspect_err(|err| warn!("couldn't count this boot: {err:?}"))
        .ok();
    info!(
        "node {id}, boot {}, last start: {reset_cause:?}",
        boots.unwrap_or(0)
    );
    let mut settings = store.load_settings();

    // The console runs whether or not there's a mesh key: it's how one is set.
    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();
    let routing = !settings.mesh_keys.is_empty();
    let context = Context { node: id, routing };
    spawner.spawn(console_task(usb_rx, store, context).expect("task already running"));
    let Some(mesh_key) = settings.mesh_keys.first() else {
        halt(format_args!(
            "node {id} has no mesh key; give it one over USB with \
             `spectrameshd --provision PORT --mesh-key-file FILE`"
        ))
        .await
    };
    let mut seed = [0u8; 32];
    rng.read(&mut seed);

    let esp_now = wifi.esp_now();
    esp_now
        .set_channel(ESPNOW_CHANNEL)
        .expect("failed to set the ESP-NOW channel");
    let (_manager, sender, receiver) = esp_now.split();

    let started = embassy_time::Instant::now();
    let mut keys = KeyRing::new(mesh_key);
    for key in &settings.mesh_keys[1..] {
        keys.accept(key);
    }
    let mut router = Router::new(identity, keys, Config::default(), seed);
    let router_ms = started.elapsed().as_millis();
    info!(
        "startup: identity {identity_ms} ms (flash and key derivation), router {router_ms} ms (signing)"
    );
    router.add_hue(espnow::hue_info(ESPNOW_HUE));
    router.set_name(&settings.name);
    router.set_boot_info(boots, reset_cause);
    for key in settings.channel_keys.drain(..) {
        info!("joined channel {}", key.id());
        router.join_channel(key);
    }
    // The keys are in the router now; drop the copies.
    drop(settings);

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

/// Logs why the node can't start, every so often, while the console waits
/// for whatever it needs.
async fn halt(reason: core::fmt::Arguments<'_>) -> ! {
    loop {
        warn!("{reason}");
        Timer::after(Duration::from_secs(10)).await;
    }
}
