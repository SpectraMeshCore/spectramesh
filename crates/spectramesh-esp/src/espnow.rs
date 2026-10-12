//! ESP-NOW as a SpectraMesh hue.
//!
//! ESP-NOW sends short frames between ESP32s over 2.4 GHz Wi-Fi without
//! joining a network. Every node must use the same Wi-Fi channel.
//!
//! Frames for one neighbor are unicast to its MAC address, once the router
//! task has learned it from a frame the router accepted, so ESP-NOW
//! acknowledges and retries them. Everything else is broadcast; receivers use
//! the next hop in the SpectraMesh header to decide whether a frame is theirs.

use alloc::vec::Vec;

use esp_radio::esp_now::{
    BROADCAST_ADDRESS, ESP_NOW_MAX_DATA_LEN_V2, EspNowError, EspNowManager, EspNowReceiver,
    EspNowSender, EspNowWifiInterface, PeerInfo,
};
use log::{debug, warn};
use spectramesh_core::{HueId, HueInfo, HueKind};

use crate::mesh::{INBOX, Outbox, Received};

/// ESP-NOW's default PHY rate.
const BITRATE_BPS: u64 = 1_000_000;

/// The largest frame: ESP-NOW v2's 1,470 bytes. (Version 1 carried 250.)
pub const MTU: usize = ESP_NOW_MAX_DATA_LEN_V2;

/// How many neighbors to keep on ESP-NOW's peer list, which unicast needs.
/// The list holds 20, including the broadcast address.
const MAX_PEERS: usize = 16;

/// Describes the ESP-NOW hue for the router.
pub fn hue_info(id: HueId) -> HueInfo {
    HueInfo::new(id, HueKind::EspNow, MTU as u16, BITRATE_BPS)
}

/// Passes every ESP-NOW frame received to the router.
pub async fn receive(hue: HueId, mut receiver: EspNowReceiver) -> ! {
    loop {
        let received = receiver.receive_async().await;
        INBOX
            .send(Received {
                hue,
                frame: received.data().to_vec(),
                src: Some(received.info.src_address),
                rssi: Some(received.info.rx_control.rssi.clamp(-128, 127) as i8),
            })
            .await;
    }
}

/// Sends the router's frames for this hue.
pub async fn send(mut sender: EspNowSender, manager: EspNowManager, outbox: &'static Outbox) -> ! {
    let mut peers = Peers::new(manager);
    loop {
        let outgoing = outbox.receive().await;
        if let Err(err) = send_frame(&mut sender, &mut peers, outgoing.dst, &outgoing.frame).await {
            // Unicast failures mean the neighbor didn't acknowledge, even
            // after ESP-NOW's retries; routing notices from the missing hellos.
            match (outgoing.dst, err) {
                (Some(dst), EspNowError::SendFailed) => {
                    debug!("no acknowledgement from {}", Mac(dst))
                }
                _ => warn!("ESP-NOW send failed: {err:?}"),
            }
        }
    }
}

/// Sends a frame to `dst` if given, which must be a neighbor's MAC address,
/// or broadcasts it.
pub async fn send_frame(
    sender: &mut EspNowSender,
    peers: &mut Peers,
    dst: Option<[u8; 6]>,
    frame: &[u8],
) -> Result<(), EspNowError> {
    let dst = match dst {
        Some(dst) if dst != BROADCAST_ADDRESS => match peers.ensure(dst) {
            Ok(()) => dst,
            // Without a place on the list, broadcast instead.
            Err(_) => BROADCAST_ADDRESS,
        },
        _ => BROADCAST_ADDRESS,
    };
    sender.send_async(&dst, frame).await
}

/// ESP-NOW's peer list, which a neighbor must be on to be unicast to. When
/// it's full, the neighbor sent to least recently makes way.
pub struct Peers {
    manager: EspNowManager,
    /// Peers this module added, least recently used first.
    recent: Vec<[u8; 6]>,
}

impl Peers {
    pub fn new(manager: EspNowManager) -> Self {
        Peers {
            manager,
            recent: Vec::with_capacity(MAX_PEERS),
        }
    }

    fn ensure(&mut self, mac: [u8; 6]) -> Result<(), EspNowError> {
        if let Some(i) = self.recent.iter().position(|&m| m == mac) {
            let mac = self.recent.remove(i);
            self.recent.push(mac);
            return Ok(());
        }
        if self.recent.len() >= MAX_PEERS {
            let oldest = self.recent.remove(0);
            let _ = self.manager.remove_peer(&oldest);
        }
        if !self.manager.peer_exists(&mac) {
            self.manager.add_peer(PeerInfo {
                interface: EspNowWifiInterface::Station,
                peer_address: mac,
                lmk: None,
                // The current channel.
                channel: None,
                // SpectraMesh does its own authentication and encryption.
                encrypt: false,
            })?;
        }
        self.recent.push(mac);
        Ok(())
    }
}

/// A MAC address, for logs.
pub struct Mac(pub [u8; 6]);

impl core::fmt::Display for Mac {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02x}:{b:02x}:{c:02x}:{d:02x}:{e:02x}:{g:02x}")
    }
}
