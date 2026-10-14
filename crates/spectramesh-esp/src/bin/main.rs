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
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx};
#[cfg(feature = "espnow")]
use esp_radio::esp_now::{EspNowManager, EspNowReceiver, EspNowSender};
#[cfg(feature = "ieee802154")]
use esp_radio::ieee802154::Ieee802154;
#[cfg(feature = "espnow")]
use esp_radio::wifi::WifiController;
use log::{info, warn};
#[cfg(feature = "ieee802154")]
use spectramesh_core::NodeId;
use spectramesh_core::{Config, HueId, KeyRing, Router};
use spectramesh_esp::console::{self, Context};
#[cfg(feature = "espnow")]
use spectramesh_esp::espnow;
#[cfg(feature = "ieee802154")]
use spectramesh_esp::ieee802154;
use spectramesh_esp::mesh::{self, Outbox};
use spectramesh_esp::radio::Radio;
use spectramesh_esp::reset;
use spectramesh_esp::storage::Store;

extern crate alloc;

use alloc::boxed::Box;

// The app descriptor the ESP-IDF bootloader expects.
esp_bootloader_esp_idf::esp_app_desc!();

/// Transmit power in quarter dBm: 80 is 20 dBm.
#[cfg(feature = "espnow")]
const TX_POWER_QUARTER_DBM: i8 = 80;

/// The Wi-Fi channel every node's ESP-NOW hue uses.
#[cfg(feature = "espnow")]
const ESPNOW_CHANNEL: u8 = 1;

/// The built-in radio's hue: ESP-NOW or 802.15.4, as the settings say.
const RADIO_HUE: HueId = HueId(0);

static RADIO_OUTBOX: Outbox = Outbox::new();
static OUTBOXES: [(HueId, &Outbox); 1] = [(RADIO_HUE, &RADIO_OUTBOX)];

#[embassy_executor::task]
async fn mesh_task(router: Box<Router>) -> ! {
    mesh::run(router, &OUTBOXES).await
}

#[embassy_executor::task]
async fn console_task(rx: UsbSerialJtagRx<'static, Async>, store: Store, context: Context) -> ! {
    console::run(rx, store, context).await
}

#[cfg(feature = "espnow")]
#[embassy_executor::task]
async fn espnow_receive_task(receiver: EspNowReceiver) -> ! {
    espnow::receive(RADIO_HUE, receiver).await
}

#[cfg(feature = "espnow")]
#[embassy_executor::task]
async fn espnow_send_task(sender: EspNowSender, manager: EspNowManager) -> ! {
    espnow::send(sender, manager, &RADIO_OUTBOX).await
}

#[cfg(feature = "ieee802154")]
#[embassy_executor::task]
async fn ieee802154_task(radio: Ieee802154<'static>, node: NodeId, channel: u8) -> ! {
    ieee802154::run(radio, node, RADIO_HUE, channel, &RADIO_OUTBOX).await
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

    // The RAM the bootloader used, and more besides: the router keeps
    // sessions, handshakes, fragments and the mesh map on the heap, and
    // 64 KiB ran out with two neighbors.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 65536);
    esp_alloc::heap_allocator!(size: 96 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut store = Store::new(peripherals.FLASH);
    let mut settings = store.load_settings();
    let radio = Radio::from_setting(&settings.radio).unwrap_or_else(|err| {
        warn!(
            "radio setting {:?}: {err}; using the default",
            settings.radio
        );
        Radio::from_setting("").expect("the default is valid")
    });

    // Random numbers for the identity and the router's seed, from the ADC's
    // noise, before any radio starts (as ESP-IDF does), so they're truly
    // random whichever radio runs.
    let trng_source = TrngSource::new(peripherals.RNG, peripherals.ADC1);
    let trng = Trng::try_new().expect("the TRNG source is on");
    // Timed, as a measure of the elliptic-curve work on this chip: deriving
    // both public keys, and below, the router's first Ed25519 signature.
    let started = embassy_time::Instant::now();
    let identity = store
        .load_or_create_identity(&trng)
        .expect("failed to read or store this node's identity in flash");
    let identity_ms = started.elapsed().as_millis();
    let mut seed = [0u8; 32];
    trng.read(&mut seed);
    drop(trng);
    if trng_source.try_disable().is_err() {
        warn!("couldn't turn the TRNG source off");
    }

    let id = identity.node_id();
    let boots = store
        .count_boot()
        .inspect_err(|err| warn!("couldn't count this boot: {err:?}"))
        .ok();
    info!(
        "node {id}, boot {}, last start: {reset_cause:?}, radio {radio}",
        boots.unwrap_or(0)
    );

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
    #[cfg(feature = "espnow")]
    router.add_hue(espnow::hue_info(RADIO_HUE));
    #[cfg(feature = "ieee802154")]
    let Radio::Ieee802154 { channel } = radio else {
        unreachable!("802.15.4 firmware has an 802.15.4 radio")
    };
    #[cfg(feature = "ieee802154")]
    router.add_hue(ieee802154::hue_info(RADIO_HUE, channel));
    router.set_name(&settings.name);
    router.set_boot_info(boots, reset_cause);
    for key in settings.channel_keys.drain(..) {
        info!("joined channel {}", key.id());
        router.join_channel(key);
    }
    // The keys are in the router now; drop the copies.
    drop(settings);

    // Kept alive for as long as main runs, which is forever.
    #[cfg(feature = "espnow")]
    let _wifi = {
        let mut wifi = WifiController::new(peripherals.WIFI, Default::default())
            .expect("failed to start Wi-Fi");
        // esp-radio starts the radio at 5 dBm. Mesh links need range, so use
        // 20 dBm (units are quarter dBm), the most 2.4 GHz rules allow
        // without a high-gain antenna.
        wifi.set_max_tx_power(TX_POWER_QUARTER_DBM)
            .expect("failed to set transmit power");
        let esp_now = wifi.esp_now();
        esp_now
            .set_channel(ESPNOW_CHANNEL)
            .expect("failed to set the ESP-NOW channel");
        let (manager, sender, receiver) = esp_now.split();
        spawner.spawn(espnow_receive_task(receiver).expect("task already running"));
        spawner.spawn(espnow_send_task(sender, manager).expect("task already running"));
        info!("SpectraMesh node {id} running on ESP-NOW channel {ESPNOW_CHANNEL}");
        wifi
    };
    #[cfg(feature = "ieee802154")]
    {
        let ieee802154 = Ieee802154::new(peripherals.IEEE802154);
        spawner.spawn(ieee802154_task(ieee802154, id, channel).expect("task already running"));
        info!("SpectraMesh node {id} running on 802.15.4 channel {channel}");
    }
    // Boxed: the router is too big to pass on a microcontroller's stack.
    spawner.spawn(mesh_task(Box::new(router)).expect("task already running"));

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
