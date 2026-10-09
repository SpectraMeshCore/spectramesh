//! Routing engine for SpectraMesh.
//!
//! SpectraMesh treats every radio a node has (2.4 GHz and 5.8 GHz Wi-Fi,
//! sub-GHz links such as 915 MHz HaLow, FSK or LoRa, ESP-NOW, plain IP) as one
//! network. Each of those is a **hue**. This crate decides which neighbor and
//! which hue each packet should take; it never touches a radio itself.
//!
//! The crate is `no_std` (it only needs an allocator) and sans-IO: the
//! platform crate feeds it received frames and the current time, and sends
//! whatever frames it hands back. See [`Router`] for the loop.
//!
//! Routing follows Babel (RFC 8966), a loop-free distance-vector protocol:
//!
//! - Nodes broadcast **hellos** on every hue and report back how well they
//!   hear each neighbor, so every link is measured in both directions
//!   ([`neighbor`]).
//! - Nodes tell their neighbors about the routes they use, adding their link
//!   cost each hop. Nothing is flooded across the whole mesh.
//! - The **feasibility condition** keeps routes loop-free, even while the
//!   network is changing ([`routing`]).
//!
//! A link's cost is its expected transmission time: how many tries a packet
//! takes, times how long one try takes on that hue. So routes prefer fast
//! hues, use slower long-range hues where nothing else reaches, and can mix
//! hues hop by hop.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod error;
pub mod hue;
pub mod neighbor;
pub mod node;
pub mod packet;
pub mod router;
pub mod routing;
pub mod time;

pub use error::{Error, Result};
pub use hue::{HueId, HueInfo, HueKind};
pub use node::NodeId;
pub use router::{Config, Delivery, Router, Transmit};
pub use routing::Route;
pub use time::Instant;
