# spectramesh-esp

SpectraMesh firmware for ESP32 boards. It runs the [`spectramesh-core`](../spectramesh-core) router on the board, with **ESP-NOW** as its first hue.

Built on Espressif's Rust stack ([`esp-hal`](https://github.com/esp-rs/esp-hal), `esp-radio` and `esp-rtos`) with [embassy](https://embassy.dev) for async tasks. No ESP-IDF or C toolchain is needed.

## Hardware

Any **ESP32-C6** dev board. You need at least two to see a mesh form, and three in a line to see one relay for another.

## Build and flash

You need [`espflash`](https://github.com/esp-rs/espflash) once:

```
cargo install espflash --locked
```

Every node in a mesh needs the same mesh key, which is built into the firmware for now. Make one with `spectrameshd --generate-mesh-key` (from [`spectramesh-wrt`](../spectramesh-wrt)), keep it somewhere safe, then, from this directory, with a board plugged in over USB:

```
SPECTRAMESH_MESH_KEY=smk1-... cargo run --release
```

That builds, flashes and opens the serial monitor. Firmware built without a key logs an error and doesn't start. Treat built firmware images like the key itself. `rust-toolchain.toml` makes rustup add the RISC-V target and Rust's source code on the first build.

This crate is excluded from the root workspace because it builds for the board, not your computer. Run cargo commands from this directory.

## What it does

On boot, each board:

1. Starts Wi-Fi and ESP-NOW on channel 1 (`ESPNOW_CHANNEL` in `src/bin/main.rs`). All boards must use the same channel.
2. Loads its secret from flash, or on first boot creates one with the hardware random number generator and stores it. Its node ID comes from that secret, so it survives reflashing the firmware (but not erasing the whole flash).
3. Runs the router, which sends hellos, measures links and builds routes.
4. Logs its neighbors and routes every 30 seconds:

```
INFO - SpectraMesh node !a1b2c3d4e5f60718 running on ESP-NOW channel 1
INFO - node !a1b2c3d4e5f60718: 2 neighbor links, 2 routes
INFO -   !0e0f101112131415 via !0e0f101112131415 on hue 0, cost 800 us
INFO -   !161718191a1b1c1d via !0e0f101112131415 on hue 0, cost 1600 us
```

## How it's organized

| File | Role |
|---|---|
| `src/bin/main.rs` | Starts the hardware, registers hues and spawns tasks |
| `src/mesh.rs` | The router task. It owns the `Router`, wakes on received frames and timers, and hands frames to each hue's send queue |
| `src/identity_store.rs` | Keeps the node's secret in the first sector of the `nvs` flash partition, which this firmware doesn't otherwise use |
| `src/espnow.rs` | The ESP-NOW hue: a receive task and a send task |

Each hue talks to the router only through channels. To add a radio, write a module like `espnow.rs` with a receive task that feeds `mesh::INBOX` and a send task that reads its own `Outbox`, then register it in `main.rs`.

## Other chips

The RISC-V chips (ESP32-C3, C5, C6) build with standard Rust. To switch, replace `esp32c6` in every feature list in `Cargo.toml` and in the `runner` line of `.cargo/config.toml`. The **ESP32-C5** adds 5 GHz Wi-Fi.

The Xtensa chips (the original ESP32 and the S3) need Espressif's toolchain from [`espup`](https://github.com/esp-rs/espup), a different target, and different build settings. Generating a project with [`esp-generate`](https://github.com/esp-rs/esp-generate) is the easiest way to get them right.

## Next steps

- **A sub-GHz hue**: an SX1262 driver over SPI for 915 MHz FSK and LoRa (Heltec WiFi LoRa 32 V3, LilyGO T-Beam)
- **Unicast ESP-NOW** to the next hop, for link-layer acknowledgements and retries
- **ESP-NOW v2**, raising the MTU from 250 to 1470 bytes
- **An application interface**, so code on the board can send and receive mesh data (today, received data is only logged)
- **Mesh keys provisioned over the serial console** and kept in flash, so one firmware image works for every mesh
- **Flash encryption**, so someone holding a board can't read its secret or the mesh key out of flash
