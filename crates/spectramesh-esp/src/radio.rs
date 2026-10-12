//! Which radio a node uses for its built-in hue.
//!
//! The ESP32-C6 has both Wi-Fi (for ESP-NOW) and 802.15.4, but esp-radio
//! can't build both into one image, so firmware is built for one: the
//! `espnow` feature (the default) or `ieee802154`. For 802.15.4, settings
//! choose the channel, as `802.15.4:CHANNEL` (11 to 26).

use alloc::string::String;
use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Radio {
    EspNow,
    Ieee802154 { channel: u8 },
}

/// The channel used unless settings say otherwise. Channel 25 (2475 MHz)
/// sits clear of Wi-Fi channels 1, 6 and 11, and of ESP-NOW on channel 1.
pub const DEFAULT_CHANNEL: u8 = 25;

impl Radio {
    /// The radio this firmware was built for, with the channel the setting
    /// gives (empty for the default). Settings for another radio are errors.
    pub fn from_setting(text: &str) -> Result<Radio, &'static str> {
        let built_for_802154 = cfg!(feature = "ieee802154");
        match text.trim() {
            "" if built_for_802154 => Ok(Radio::Ieee802154 {
                channel: DEFAULT_CHANNEL,
            }),
            "" | "esp-now" if !built_for_802154 => Ok(Radio::EspNow),
            "esp-now" => Err("this firmware was built for 802.15.4"),
            _ if !built_for_802154 => Err(
                "this firmware was built for ESP-NOW; build with --features ieee802154 for 802.15.4",
            ),
            "802.15.4" => Ok(Radio::Ieee802154 {
                channel: DEFAULT_CHANNEL,
            }),
            other => {
                let channel = other
                    .strip_prefix("802.15.4:")
                    .ok_or("expected 802.15.4 or 802.15.4:CHANNEL")?;
                match channel.parse() {
                    Ok(channel @ 11..=26) => Ok(Radio::Ieee802154 { channel }),
                    _ => Err("802.15.4 channels are 11 to 26"),
                }
            }
        }
    }

    pub fn to_setting(self) -> String {
        alloc::format!("{self}")
    }
}

impl fmt::Display for Radio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Radio::EspNow => f.write_str("esp-now"),
            Radio::Ieee802154 { channel } => write!(f, "802.15.4:{channel}"),
        }
    }
}
