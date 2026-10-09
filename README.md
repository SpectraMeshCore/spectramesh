# SpectraMesh

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
| `spectramesh-esp` | ESP32 firmware |
| `spectramesh-wrt` | OpenWRT package |

## Status

Early development. `spectramesh-core` routes between simulated nodes in its tests; there are no radio drivers yet.

```
cargo test
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
