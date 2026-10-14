# Outstanding

Work that's written but waiting on hardware, or on a decision.

## Waiting on you

- [ ] **Plug in the XIAO ESP32-C6 boards for hardware tests.** These changes have only run in tests and CI, not on a board:
  - keys stored in flash and set over USB (`spectrameshd --provision`), and the serial console
  - the `spectramesh` partition table, keeping each board's existing identity
  - restart after a panic, and the reset cause and boot count in reports
  - ESP-NOW v2 (1,470-byte frames) and unicast to known neighbors, on nodes and the USB modem
  - the 802.15.4 hue, on nodes and the USB modem, and a laptop routing between ESP-NOW and 802.15.4 with one modem of each kind

  Boards flashed before 2026-10-12 still have the mesh key built in; after reflashing they need `--provision`.
- [ ] **Choose a 915 MHz board.** The SX1262 driver (`crates/spectramesh-sx126x`) and its hue (`crates/spectramesh-esp/src/sub_ghz.rs`) need the board's pins. Most ESP32 boards with an SX1262 (Heltec WiFi LoRa 32 V3, LilyGO T-Beam S3) use the ESP32-S3, which needs Espressif's Xtensa toolchain (`espup`).

## Also waiting

- [ ] **Flash encryption**: burns eFuses, which can't be undone. Needs a go-ahead and a spare board. Plan: `crates/spectramesh-esp/README.md`, "Protecting the keys on a board". It also needs the `spectramesh` partition flagged `encrypted` again, which waits on a fix to esp-bootloader-esp-idf: 0.6.0 reads the `encrypted` flag bit as read-only (see `crates/spectramesh-esp/partitions.csv`).
- [ ] **OpenWRT**: the router models, for an SDK build in CI and a test of the init script.
- [ ] **ESP-NOW and 802.15.4 on one ESP32-C6**: esp-radio can't run both yet. Until it can, a node with both is a computer with two USB modems.
