//! SpectraMesh firmware for ESP32 boards.
//!
//! [`mesh`] runs the `spectramesh-core` router as an embassy task. Each hue
//! (so far, [`espnow`]) runs its own receive and send tasks and talks to the
//! router through channels, so adding a radio means adding a module like
//! [`espnow`] and registering it in `main`.

#![no_std]

extern crate alloc;

pub mod espnow;
pub mod mesh;

use spectramesh_core::NodeId;

/// This node's mesh address: the last four bytes of its Wi-Fi MAC address.
///
/// Espressif MACs differ in their last three bytes from device to device, so
/// these are unique in practice.
//
// TODO: derive from a key pair stored in flash once packets are signed.
pub fn node_id(mac: [u8; 6]) -> NodeId {
    NodeId([mac[2], mac[3], mac[4], mac[5]])
}
