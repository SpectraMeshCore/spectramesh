//! Hues: the radio links a node can send on.
//!
//! The core doesn't drive radios. Platform crates (`spectramesh-esp`,
//! `spectramesh-wrt`, ...) register each hue with the router, then move frames
//! between the router and the radio.
//!
//! Routing only looks at a hue's numbers (bitrate, MTU, timers and
//! [`LinkModel`]), never at its band. A 915 MHz radio running HaLow or FSK is
//! a backbone link, and the same radio running LoRa is a slow one; the bitrate
//! is what tells them apart. Ethernet and fiber are hues too, and being fast
//! and reliable, they win over radio wherever they reach.

use core::time::Duration;

pub use crate::neighbor::LinkModel;

/// Identifies one hue on this node. Chosen by the platform; only meaningful locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HueId(pub u8);

/// What kind of link a hue is. Informational: routing uses [`HueInfo`]'s numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HueKind {
    /// 802.11, including HaLow (802.11ah) below 1 GHz.
    Wifi {
        freq_mhz: u16,
    },
    EspNow,
    /// FSK or GFSK packet radio, such as an SX126x or CC13xx in FSK mode.
    Fsk {
        freq_khz: u32,
    },
    LoRa {
        freq_khz: u32,
    },
    /// Ethernet, including fiber and anything else that carries Ethernet
    /// frames. SpectraMesh frames travel directly in Ethernet frames, with no IP.
    Ethernet,
    /// UDP over an existing IP network, for desktops and for linking sites
    /// over networks SpectraMesh doesn't control.
    Ip,
}

impl HueKind {
    /// Whether this kind of link loses packets often enough that its cost
    /// should rise with loss. Radios do; wired links and IP don't.
    pub fn default_link_model(self) -> LinkModel {
        match self {
            HueKind::Ethernet | HueKind::Ip => LinkModel::Reliable,
            _ => LinkModel::Lossy,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HueInfo {
    pub id: HueId,
    pub kind: HueKind,
    /// Largest frame the hue can carry, in bytes, including the SpectraMesh header.
    pub mtu: u16,
    /// Typical usable bitrate. Sets the link cost, so slow hues cost more per hop.
    //
    // TODO: measure throughput per neighbor, as B.A.T.M.A.N. V does, instead
    // of trusting a configured figure.
    pub bitrate_bps: u64,
    pub hello_interval: Duration,
    /// How often the full route table is sent. Changes are sent straight away.
    pub update_interval: Duration,
    pub link_model: LinkModel,
}

/// Size of the frame that link costs are based on: 100 bytes.
const REFERENCE_FRAME_BITS: u64 = 800;

impl HueInfo {
    /// Describes a hue, picking timers from its bitrate: hellos every 4 s at
    /// 1 Mbit/s and up, 10 s at 50 kbit/s and up, and 60 s below that. Full
    /// updates go out every four hellos. The link model comes from `kind`.
    /// Change the fields afterwards to override any of these.
    //
    // TODO: hues below about 50 kbit/s, or with a duty-cycle limit, should
    // switch to on-demand routing within an airtime budget instead of running
    // Babel on slow timers.
    pub fn new(id: HueId, kind: HueKind, mtu: u16, bitrate_bps: u64) -> Self {
        let hello_interval = Duration::from_secs(match bitrate_bps {
            1_000_000.. => 4,
            50_000.. => 10,
            _ => 60,
        });
        HueInfo {
            id,
            kind,
            mtu,
            bitrate_bps,
            hello_interval,
            update_interval: hello_interval * 4,
            link_model: kind.default_link_model(),
        }
    }

    /// Microseconds to send a 100-byte frame on this hue, at least 1.
    ///
    /// 1 for gigabit Ethernet and faster, 8 for 100 Mbit/s Ethernet, about 40
    /// for 20 Mbit/s Wi-Fi, 800 for ESP-NOW, 3,200 for 250 kbit/s FSK and
    /// 160,000 for 5 kbit/s LoRa.
    pub fn airtime_us(&self) -> u32 {
        let us = REFERENCE_FRAME_BITS * 1_000_000 / self.bitrate_bps.max(1);
        us.clamp(1, u32::MAX.into()) as u32
    }
}
