//! SpectraMesh firmware for ESP32 boards.
//!
//! [`mesh`] runs the `spectramesh-core` router as an embassy task. Each hue
//! (so far, [`espnow`]) runs its own receive and send tasks and talks to the
//! router through channels, so adding a radio means adding a module like
//! [`espnow`] and registering it in `main`.

#![no_std]

extern crate alloc;

pub mod espnow;
pub mod identity_store;
pub mod mesh;
