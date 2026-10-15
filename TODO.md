# Outstanding

Work that's written but waiting on hardware, or on a decision.

## Waiting on you

- [x] **Hardware tests on the XIAO ESP32-C6 boards** (2026-10-12 to 14, xiao-1 and the USB modem). Working: keys in flash and `--provision`, the console, the new partition table keeping identities, reset cause and boot count on the map, 600-byte messages over ESP-NOW v2, direct and channel messages both ways over ESP-NOW and over 802.15.4, and a node on the new firmware meshing with xiao-2's older firmware. Fixed along the way: proofs lost on 802.15.4, bursts lost on 802.15.4, settings not saving, `--provision` not being heard, and the daemon not reopening a modem after the laptop slept.
- [x] **xiao-2 reflashed and provisioned** (2026-10-14). Its first direct message crashed it: the router asked for a 9 KB block for its handshake table, and the 64 KiB heap was nearly full. Handshakes and sessions are now boxed and the heap is 160 KiB; `info` shows what's free. The crash also showed restart-after-panic working, and "Panic" as the last start.
- [x] **xiao-1 reflashed** with the larger heap (2026-10-14); its keys stayed in flash. All three boards now run the current firmware.
- [ ] **A fourth board, to bridge ESP-NOW and 802.15.4 on hardware**: the laptop with one modem of each kind, plus a node on each hue. Three boards can't show it (tests/multi_hue.rs shows it with UDP hues).
- [x] **An OpenWRT router** (2026-10-14): a HiLink HLK-7621 kit (MT7621, OpenWRT 24.10.1) on the laptop's Ethernet. `spectrameshd`, cross-built with the SDK's toolchain (`crates/spectramesh-wrt/scripts/cross-build.sh`; CI builds it too), runs under procd with the package's init script and UCI config, over an 802.11s interface on its Netgear WNA1100 (AR9271, `kmod-ath9k-htc`), the LAN bridge and UDP. It reached the laptop over UDP, swapped direct and channel messages with it both ways, served the map, and came back after a reboot and after `wifi down`/`wifi up`. Fixed along the way: hues dying when their device was taken down or created again, and procd giving up when the Wi-Fi interface appeared late at boot.
- [ ] **HaLow on the HT-HC01P.** The card can't work in the HLK-7621's mini PCIe slot: it carries SPI/SDIO on reserved mini PCIe pins (45 to 51, and 10), with no PCIe or USB, so the router sees no card. It needs an SPI host with Morse Micro's driver: for example a Raspberry Pi with Heltec's HAT for the card, running Morse Micro's OpenWRT build for the MM6108 with the card's board configuration file (`bcf_HC01P-V2.bin` for V2 cards). Then `spectrameshd` runs a `kind=halow` hue on its 802.11s interface. A second HaLow radio is needed to see a link.
- [ ] **A second 802.11s node** to test the router's Wi-Fi hue over the air: another OpenWRT router, or any Linux machine with a mesh-capable card (a second AR9271 adapter works). The laptop's Intel card can't do mesh mode.
- [ ] **Choose a 915 MHz board.** The SX1262 driver (`crates/spectramesh-sx126x`) and its hue (`crates/spectramesh-esp/src/sub_ghz.rs`) need the board's pins. Most ESP32 boards with an SX1262 (Heltec WiFi LoRa 32 V3, LilyGO T-Beam S3) use the ESP32-S3, which needs Espressif's Xtensa toolchain (`espup`).

## Also waiting

- [ ] **A message sent just after the other end restarted can be lost**: once, after the laptop's daemon restarted, xiao-1's first message to it went on the old session and never arrived; later ones did. To look into.

- [ ] **Flash encryption**: burns eFuses, which can't be undone. Needs a go-ahead and a spare board. Plan: `crates/spectramesh-esp/README.md`, "Protecting the keys on a board". It also needs the `spectramesh` partition flagged `encrypted` again, which waits on a fix to esp-bootloader-esp-idf: 0.6.0 reads the `encrypted` flag bit as read-only (see `crates/spectramesh-esp/partitions.csv`).
- [ ] **The OpenWRT package in the SDK**: `openwrt/Makefile` hasn't been built through the SDK's feed yet (it builds Rust itself, about an hour). CI cross-builds the daemon for `ramips/mt7621` instead.
- [ ] **The laptop on the router's Ethernet hue**: it links over UDP for now, because raw Ethernet sockets need root or `CAP_NET_RAW` there (`sudo setcap cap_net_raw,cap_net_admin+ep target/release/spectrameshd`).
- [ ] **ESP-NOW and 802.15.4 on one ESP32-C6**: esp-radio can't run both yet. Until it can, a node with both is a computer with two USB modems.
