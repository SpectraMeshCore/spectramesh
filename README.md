# SpectraMesh

A multi-band mesh networking stack for ESP32, OpenWRT, desktops and laptops.

SpectraMesh routes across every radio a node has as one network, in the spirit of OLSR and Meshtastic. Each band is a **hue**:

- 2.4 GHz and 5.8 GHz Wi-Fi
- 915 MHz LoRa
- ESP-NOW
- ...and more to come

## Components

| Package | Runs on |
|---|---|
| `spectramesh-core` | Routing engine, shared by every platform |
| `spectramesh-esp` | ESP32 firmware |
| `spectramesh-wrt` | OpenWRT package |

## Status

Early development. Nothing to run yet.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
