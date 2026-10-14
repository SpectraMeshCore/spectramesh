# Outstanding

Work that's written but waiting on hardware, or on a decision.

## Waiting on you

- [x] **Hardware tests on the XIAO ESP32-C6 boards** (2026-10-12 to 14, xiao-1 and the USB modem). Working: keys in flash and `--provision`, the console, the new partition table keeping identities, reset cause and boot count on the map, 600-byte messages over ESP-NOW v2, direct and channel messages both ways over ESP-NOW and over 802.15.4, and a node on the new firmware meshing with xiao-2's older firmware. Fixed along the way: proofs lost on 802.15.4, bursts lost on 802.15.4, settings not saving, `--provision` not being heard, and the daemon not reopening a modem after the laptop slept.
- [x] **xiao-2 reflashed and provisioned** (2026-10-14). Its first direct message crashed it: the router asked for a 9 KB block for its handshake table, and the 64 KiB heap was nearly full. Handshakes and sessions are now boxed and the heap is 160 KiB; `info` shows what's free. The crash also showed restart-after-panic working, and "Panic" as the last start.
- [x] **xiao-1 reflashed** with the larger heap (2026-10-14); its keys stayed in flash. All three boards now run the current firmware.
- [ ] **A fourth board, to bridge ESP-NOW and 802.15.4 on hardware**: the laptop with one modem of each kind, plus a node on each hue. Three boards can't show it (tests/multi_hue.rs shows it with UDP hues).
- [ ] **Choose a 915 MHz board.** The SX1262 driver (`crates/spectramesh-sx126x`) and its hue (`crates/spectramesh-esp/src/sub_ghz.rs`) need the board's pins. Most ESP32 boards with an SX1262 (Heltec WiFi LoRa 32 V3, LilyGO T-Beam S3) use the ESP32-S3, which needs Espressif's Xtensa toolchain (`espup`).

## Also waiting

- [ ] **A message sent just after the other end restarted can be lost**: once, after the laptop's daemon restarted, xiao-1's first message to it went on the old session and never arrived; later ones did. To look into.

- [ ] **Flash encryption**: burns eFuses, which can't be undone. Needs a go-ahead and a spare board. Plan: `crates/spectramesh-esp/README.md`, "Protecting the keys on a board". It also needs the `spectramesh` partition flagged `encrypted` again, which waits on a fix to esp-bootloader-esp-idf: 0.6.0 reads the `encrypted` flag bit as read-only (see `crates/spectramesh-esp/partitions.csv`).
- [ ] **OpenWRT**: the router models, for an SDK build in CI and a test of the init script.
- [ ] **ESP-NOW and 802.15.4 on one ESP32-C6**: esp-radio can't run both yet. Until it can, a node with both is a computer with two USB modems.
