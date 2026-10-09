//! ESP-NOW as a SpectraMesh hue.
//!
//! ESP-NOW sends short frames between ESP32s over 2.4 GHz Wi-Fi without
//! joining a network. Every node must use the same Wi-Fi channel.

use esp_radio::esp_now::{
    BROADCAST_ADDRESS, ESP_NOW_MAX_DATA_LEN_V1, EspNowReceiver, EspNowSender,
};
use log::warn;
use spectramesh_core::{HueId, HueInfo, HueKind};

use crate::mesh::{INBOX, Outbox, Received};

/// ESP-NOW's default PHY rate.
const BITRATE_BPS: u64 = 1_000_000;

/// Describes the ESP-NOW hue for the router.
//
// TODO: ESP-NOW v2 carries up to 1470 bytes. Raise the MTU once that's been
// checked between real boards.
pub fn hue_info(id: HueId) -> HueInfo {
    HueInfo::new(
        id,
        HueKind::EspNow,
        ESP_NOW_MAX_DATA_LEN_V1 as u16,
        BITRATE_BPS,
    )
}

/// Passes every ESP-NOW frame received to the router.
pub async fn receive(hue: HueId, mut receiver: EspNowReceiver) -> ! {
    loop {
        let received = receiver.receive_async().await;
        INBOX
            .send(Received {
                hue,
                frame: received.data().to_vec(),
            })
            .await;
    }
}

/// Sends the router's frames for this hue.
///
/// Everything goes to the broadcast address; receivers use the next hop in
/// the SpectraMesh header to decide whether a frame is theirs.
//
// TODO: unicast frames with a specific next hop, learning MAC addresses with
// `packet::control_sender`, to get ESP-NOW's link-layer acknowledgements and
// retries.
pub async fn send(mut sender: EspNowSender, outbox: &'static Outbox) -> ! {
    loop {
        let tx = outbox.receive().await;
        if let Err(err) = sender.send_async(&BROADCAST_ADDRESS, &tx.frame).await {
            warn!("ESP-NOW send failed: {err:?}");
        }
    }
}
