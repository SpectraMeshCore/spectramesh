//! SpectraMesh firmware for ESP32 boards.
//!
//! [`mesh`] runs the `spectramesh-core` router as an embassy task. Each hue
//! ([`espnow`] or [`ieee802154`], as [`radio`] chooses) runs its own tasks and talks to the
//! router through channels, so adding a radio means adding a module like
//! [`espnow`] and registering it in `main`. [`sub_ghz`] is the next: an
//! SX1262 radio, waiting for a board to wire it to.
//!
//! [`storage`] keeps the node's identity, settings and boot count in flash,
//! and [`console`] lets a host set them over USB. [`reset`] restarts the
//! node, after a panic too, and tells why it last started.

#![no_std]

#[cfg(all(feature = "espnow", feature = "ieee802154"))]
compile_error!(
    "esp-radio can't build ESP-NOW (Wi-Fi) and 802.15.4 into one image: \
     for 802.15.4, build with --no-default-features --features ieee802154"
);
#[cfg(not(any(feature = "espnow", feature = "ieee802154")))]
compile_error!("choose a radio: the espnow feature (the default) or ieee802154");

extern crate alloc;

pub mod console;
#[cfg(feature = "espnow")]
pub mod espnow;
#[cfg(feature = "ieee802154")]
pub mod ieee802154;
pub mod mesh;
pub mod radio;
pub mod reset;
pub mod storage;
pub mod sub_ghz;
