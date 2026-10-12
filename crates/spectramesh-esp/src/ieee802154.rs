//! IEEE 802.15.4 as a SpectraMesh hue: the 2.4 GHz radio under Thread and
//! Zigbee, built into the ESP32-C6 and H2.
//!
//! 802.15.4 sends 250 kbit/s in frames of at most 127 bytes. After the MAC
//! header (15 bytes, with PAN ID compression, a short broadcast destination
//! and an extended source) and the checksum (2), 110 bytes are left for a
//! SpectraMesh frame; the router fragments anything bigger, routing messages
//! included.
//!
//! Frames are MAC data frames on SpectraMesh's PAN, [`PAN_ID`], so Thread
//! and Zigbee networks on the same channel ignore them, and the radio
//! ignores theirs. Each node's extended address is its node ID. Everything
//! is broadcast, without acknowledgements; receivers use the next hop in the
//! SpectraMesh header.
//!
//! On the ESP32-C6, 802.15.4 can't share the radio with Wi-Fi yet (esp-radio
//! won't build both), so firmware runs either this hue or ESP-NOW, as built
//! (see [`radio`](crate::radio)). Nodes with both, such as a computer with
//! two USB modems, join the two.

use alloc::vec::Vec;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, with_timeout};
use esp_radio::ieee802154::{Config, Frame, Ieee802154};
use ieee802154::mac::{
    Address, ExtendedAddress, FrameContent, FrameType, FrameVersion, Header, PanId, ShortAddress,
};
use log::{debug, warn};
use spectramesh_core::{HueId, HueInfo, HueKind, NodeId};

use crate::mesh::{INBOX, Outbox, Received};

/// SpectraMesh's PAN ID: "SM".
pub const PAN_ID: u16 = 0x534D;

/// Room for a SpectraMesh frame: 127 bytes, less the MAC header and checksum.
pub const MTU: usize = 127 - 15 - 2;

const BITRATE_BPS: u64 = 250_000;

/// Transmit power in dBm: the C6's most for 802.15.4.
const TX_POWER_DBM: i8 = 20;

static RECEIVED: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// Whether the last send went out (true) or failed, such as when the
/// channel stayed busy.
static SENT: Signal<CriticalSectionRawMutex, bool> = Signal::new();

fn on_received() {
    RECEIVED.signal(());
}

fn on_sent() {
    SENT.signal(true);
}

fn on_send_failed() {
    SENT.signal(false);
}

/// Describes the hue for the router.
pub fn hue_info(id: HueId, channel: u8) -> HueInfo {
    HueInfo::new(id, HueKind::Ieee802154 { channel }, MTU as u16, BITRATE_BPS)
}

/// The radio, sending SpectraMesh frames in 802.15.4 data frames.
pub struct Link {
    radio: Ieee802154<'static>,
    /// This end's extended address.
    address: u64,
    seq: u8,
}

/// A frame the radio received.
pub struct Incoming {
    pub frame: Vec<u8>,
    pub rssi: i8,
    /// The sender's extended address, if it gave one.
    pub src: Option<u64>,
}

impl Link {
    /// Starts the radio on `channel`, with `address` as this end's
    /// extended address, listening.
    pub fn new(mut radio: Ieee802154<'static>, address: u64, channel: u8) -> Self {
        radio.set_config(Config {
            channel,
            txpower: TX_POWER_DBM,
            pan_id: Some(PAN_ID),
            ext_addr: Some(address),
            // Listen whenever not sending.
            rx_when_idle: true,
            ..Config::default()
        });
        radio.set_rx_available_callback_fn(on_received);
        radio.set_tx_done_callback_fn(on_sent);
        radio.set_tx_failed_callback_fn(on_send_failed);
        radio.start_receive();
        Link {
            radio,
            address,
            seq: 0,
        }
    }

    /// Waits until something has arrived. Safe to race and drop.
    pub async fn wait(&self) {
        RECEIVED.wait().await
    }

    /// Everything that has arrived.
    pub fn take_received(&mut self) -> Vec<Incoming> {
        let mut frames = Vec::new();
        while let Some(received) = self.radio.received() {
            match received {
                Ok(received) if received.frame.content == FrameContent::Data => {
                    let src = match received.frame.header.source {
                        Some(Address::Extended(_, ExtendedAddress(address))) => Some(address),
                        _ => None,
                    };
                    frames.push(Incoming {
                        frame: received.frame.payload,
                        rssi: received.rssi,
                        src,
                    });
                }
                Ok(_) => {}
                Err(err) => debug!("undecodable 802.15.4 frame: {err:?}"),
            }
        }
        frames
    }

    /// Broadcasts a frame, after checking the channel is clear, and waits
    /// until it's gone. Frames longer than [`MTU`] are dropped.
    pub async fn send(&mut self, payload: Vec<u8>) {
        if payload.len() > MTU {
            warn!(
                "dropped a {}-byte frame: 802.15.4 carries {MTU}",
                payload.len()
            );
            return;
        }
        self.seq = self.seq.wrapping_add(1);
        let frame = Frame {
            header: Header {
                frame_type: FrameType::Data,
                frame_pending: false,
                ack_request: false,
                pan_id_compress: true,
                seq_no_suppress: false,
                ie_present: false,
                version: FrameVersion::Ieee802154_2006,
                seq: self.seq,
                destination: Some(Address::Short(PanId(PAN_ID), ShortAddress::BROADCAST)),
                source: Some(Address::Extended(
                    PanId(PAN_ID),
                    ExtendedAddress(self.address),
                )),
                auxiliary_security_header: None,
            },
            content: FrameContent::Data,
            payload,
            footer: [0; 2],
        };
        SENT.reset();
        if let Err(err) = self.radio.transmit(&frame, true) {
            warn!("802.15.4 send failed: {err:?}");
        } else {
            // A 127-byte frame takes 4 ms; anything much longer is stuck.
            match with_timeout(Duration::from_millis(50), SENT.wait()).await {
                Ok(true) => {}
                Ok(false) => debug!("802.15.4 channel busy; frame dropped"),
                Err(_) => warn!("802.15.4 send didn't finish"),
            }
        }
        self.radio.start_receive();
    }
}

/// Runs the hue forever: receives into the router's inbox and sends what
/// the router queues on `outbox`. The radio is half duplex, so one task does
/// both. The node's extended address is its node ID.
pub async fn run(
    radio: Ieee802154<'static>,
    node: NodeId,
    hue: HueId,
    channel: u8,
    outbox: &'static Outbox,
) -> ! {
    let mut link = Link::new(radio, node.to_u64(), channel);
    loop {
        match select(link.wait(), outbox.receive()).await {
            Either::First(()) => {
                for incoming in link.take_received() {
                    INBOX
                        .send(Received {
                            hue,
                            frame: incoming.frame,
                            src: None,
                            rssi: Some(incoming.rssi),
                        })
                        .await;
                }
            }
            Either::Second(outgoing) => link.send(outgoing.frame).await,
        }
    }
}

/// An EUI-64 for 802.15.4 from a 48-bit MAC address, as IEEE describes:
/// `ff:fe` in the middle.
pub fn eui64(mac: [u8; 6]) -> u64 {
    let [a, b, c, d, e, f] = mac;
    u64::from_be_bytes([a, b, c, 0xff, 0xfe, d, e, f])
}
