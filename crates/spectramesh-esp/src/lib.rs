//! SpectraMesh firmware for ESP32 boards.
//!
//! [`mesh`] runs the `spectramesh-core` router as an embassy task. Each hue
//! (so far, [`espnow`]) runs its own receive and send tasks and talks to the
//! router through channels, so adding a radio means adding a module like
//! [`espnow`] and registering it in `main`.
//!
//! [`storage`] keeps the node's identity, settings and boot count in flash,
//! and [`console`] lets a host set them over USB. [`reset`] restarts the
//! node, after a panic too, and tells why it last started.

#![no_std]

extern crate alloc;

pub mod console;
pub mod espnow;
pub mod mesh;
pub mod reset;
pub mod storage;
