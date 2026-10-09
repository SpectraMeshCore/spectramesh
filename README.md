# SpectraMesh

[![CI](https://github.com/SpectraMeshCore/spectramesh/actions/workflows/ci.yml/badge.svg)](https://github.com/SpectraMeshCore/spectramesh/actions/workflows/ci.yml)

A multi-band mesh networking stack for ESP32, OpenWRT, desktops and laptops.

SpectraMesh routes across every radio a node has as one network, using Babel-style loop-free routing. Each radio link is a **hue**:

- 2.4 GHz and 5.8 GHz Wi-Fi
- Sub-GHz links such as 915 MHz HaLow, FSK or LoRa
- ESP-NOW
- Ethernet and fiber
- Plain IP, for desktops and linking sites over other networks

Routes pick the fastest hue that reaches, and can mix hues hop by hop. Nodes cabled together use the cable wherever it reaches and fall back to radio when it's cut.

## Components

| Package | Runs on |
|---|---|
| [`spectramesh-core`](crates/spectramesh-core) | Routing engine, shared by every platform (`no_std`, Rust) |
| [`spectramesh-esp`](crates/spectramesh-esp) | ESP32 firmware, with ESP-NOW (`no_std`, Rust, embassy) |
| [`spectramesh-wrt`](crates/spectramesh-wrt) | Routing daemon for OpenWRT and other Linux systems, with its OpenWRT package |

## Status

Early development. `spectramesh-core` routes between simulated nodes in its tests, `spectramesh-esp` runs it on ESP32-C6 boards over ESP-NOW, and `spectramesh-wrt` runs it on Linux over Ethernet, fiber and Wi-Fi mesh devices.

Every frame is authenticated with a shared mesh key, replays are rejected, and node IDs come from each node's own keys. End-to-end encryption is next; see the [security design](docs/design/wire-format-v2.md).

```
cargo test                                         # core and daemon tests
crates/spectramesh-wrt/scripts/veth-demo.sh        # daemons on virtual cables (after cargo build)
SPECTRAMESH_MESH_KEY=smk1-... cargo run --release  # flash a board, from crates/spectramesh-esp
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
