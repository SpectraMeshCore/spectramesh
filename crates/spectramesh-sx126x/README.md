# spectramesh-sx126x

An async driver for Semtech's **SX1261 and SX1262** sub-GHz radios, in **LoRa** and **GFSK**, for SpectraMesh's 915 MHz and 868 MHz hues. It's `no_std`, needs no allocator, and is built on the `embedded-hal` 1.0 traits, so it runs on any microcontroller with an async HAL (`esp-hal`, `embassy-stm32`, `embassy-nrf`, ...).

Two presets cover SpectraMesh's two kinds of sub-GHz link:

| Preset | Modulation | Bitrate | For |
|---|---|---|---|
| `Modulation::FAST_FSK` | GFSK, 125 kHz deviation | 250 kbit/s | Backbone links between well-placed nodes |
| `Modulation::LONG_RANGE_LORA` | LoRa SF9, 125 kHz, 4/5 | 1.8 kbit/s | Long, slow links |

Any other LoRa or GFSK settings work too. `Modulation::bitrate_bps` and `airtime_us` give the router its link costs.

```rust
let config = Config {
    board: Board { chip: Chip::Sx1262, tcxo: Some(TcxoVoltage::V1_8), dio2_rf_switch: true, dc_dc: true },
    frequency_hz: 915_000_000,
    power_dbm: 22,
    modulation: Modulation::FAST_FSK,
};
let mut radio = Sx126x::new(spi_device, reset, busy, dio1, delay, config);
radio.init().await?;
radio.start_receiving().await?;
```

The board settings come from its schematic: whether DIO3 powers a TCXO (and at what voltage), whether DIO2 switches the antenna, and whether the DC-DC regulator's inductor is fitted. Heltec's WiFi LoRa 32 V3 and Seeed's Wio-SX1262 use a 1.8 V TCXO and a DIO2 antenna switch.

The driver applies the datasheet's errata: transmit clamping for antenna mismatch, the 500 kHz modulation quality bit, and LoRa IQ polarity.

Its tests run on any computer against a recording SPI bus, checking the bytes of each command against the datasheet: `cargo test -p spectramesh-sx126x`. It hasn't run against a real radio yet.

**Regulations.** In the US, 902-928 MHz is unlicensed under FCC part 15, but transmitting on one channel at high power is allowed only for wide-band signals (6 dB bandwidth of 500 kHz or more, such as the GFSK preset); narrow LoRa at high power needs frequency hopping, which this driver doesn't do. Europe's 868 MHz bands limit power and duty cycle. Check your region's rules.
