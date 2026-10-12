# spectramesh-esp

SpectraMesh firmware for ESP32 boards. It runs the [`spectramesh-core`](../spectramesh-core) router on the board, with **ESP-NOW** or **802.15.4** as its hue.

Built on Espressif's Rust stack ([`esp-hal`](https://github.com/esp-rs/esp-hal), `esp-radio` and `esp-rtos`) with [embassy](https://embassy.dev) for async tasks. No ESP-IDF or C toolchain is needed.

## Hardware

Any **ESP32-C6** dev board. You need at least two to see a mesh form, and three in a line to see one relay for another.

On a **Seeed XIAO ESP32-C6**, build with `--features xiao-esp32c6`. The XIAO routes its antenna through an RF switch that firmware must power (GPIO3 low) and point at the built-in antenna (GPIO14 low), as Seeed's Arduino support does; without the feature, the radio may barely reach.

## Build and flash

You need [`espflash`](https://github.com/esp-rs/espflash) once:

```
cargo install espflash --locked
```

From this directory, with a board plugged in over USB:

```
cargo run --release                          # any ESP32-C6 board
cargo run --release --features xiao-esp32c6  # a Seeed XIAO ESP32-C6
```

That builds, flashes (with the partition table in `partitions.csv`) and opens the serial monitor. If the log stops at `waiting for download`, the chip is still in its bootloader (for example after starting it in BOOT mode): press RESET, or run `espflash reset --after watchdog-reset`. `rust-toolchain.toml` makes rustup add the RISC-V target and Rust's source code on the first build.

CI builds images too (the `firmware` artifact of each run). Flash one with `espflash flash --chip esp32c6 --partition-table partitions.csv spectramesh-esp-xiao-esp32c6`.

The firmware holds no keys, so one image works for every mesh. A new board starts without a mesh key and waits for one. Give it the mesh's keys, a name and any channels over USB with `spectrameshd` from [`spectramesh-wrt`](../spectramesh-wrt), using the same files as the daemon (make a mesh key with `spectrameshd --generate-mesh-key`):

```
spectrameshd --provision /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_<MAC>-if00 \
    --mesh-key-file ~/.config/spectramesh/mesh.key --name roof \
    [--channel-key-file ~/.config/spectramesh/channels.key]
```

```
/dev/serial/by-id/...: node !d1c6f35920e92ec6
  set mesh-keys
  set name
  restarting; it rejoins the mesh in a few seconds
```

The board keeps them in flash, through reflashing, and restarts to apply them. Run it again to change any of them; settings you leave out stay as they were.

### ESP-NOW or 802.15.4

The ESP32-C6 has two 2.4 GHz radios built in: Wi-Fi, which carries ESP-NOW (1 Mbit/s, 1,470-byte frames), and IEEE 802.15.4, the radio under Thread and Zigbee (250 kbit/s, 127-byte frames). esp-radio can't run both at once yet, and won't build both into one image, so firmware is built for one. ESP-NOW is the default; for 802.15.4:

```
cargo run --release --no-default-features --features ieee802154,xiao-esp32c6
```

802.15.4 nodes use channel 25 (2475 MHz, clear of Wi-Fi channels 1, 6 and 11) on their own PAN (`0x534D`), so they ignore Thread and Zigbee networks and vice versa. Give a node another channel with the console's `radio 802.15.4:CHANNEL` (11 to 26), then `restart`. All 802.15.4 nodes must share the channel. 802.15.4 frames leave 110 bytes for SpectraMesh, so the router fragments anything bigger, routing messages included.

ESP-NOW nodes and 802.15.4 nodes can't hear each other. A node with both radios joins them: today, a computer running `spectrameshd` with one USB modem of each kind (see below). Routes change hue at that node, and every node's map shows both.

### The serial console

`--provision` drives a console on the board's USB serial port, which you can also use by hand from `espflash monitor` or any serial terminal. Each command is answered with a line starting `ok` or `error`, between log messages:

| Command | Effect |
|---|---|
| `info` | Node ID, name, uptime, boot count, neighbors, channels and routes |
| `send NODE TEXT` | Sends a message to one node, encrypted end to end |
| `channel CHANNEL TEXT` | Sends a message on a joined channel (the start of its ID is enough) |
| `mesh-keys KEY...`, `name [NAME]`, `channel-keys [KEY...]` | Saves settings, applied at the next restart. Keys are never printed back |
| `radio 802.15.4:CHANNEL` | On 802.15.4 firmware, saves the channel (11 to 26) |
| `erase-settings` | Forgets the keys, name and channels, keeping the node's identity |
| `restart` | Restarts the board |

Messages for the node appear as `message FROM CHANNEL: TEXT` (`direct` for messages to this node alone), as `spectrameshd --listen` prints them.

This crate is excluded from the root workspace because it builds for the board, not your computer. Run cargo commands from this directory.

## What it does

On boot, each board:

1. Starts its radio: ESP-NOW on Wi-Fi channel 1 (`ESPNOW_CHANNEL` in `src/bin/main.rs`), or 802.15.4 on its channel. All boards on a hue must use the same channel.
2. Loads its secret from flash, or on first boot creates one with the hardware random number generator and stores it. Its node ID comes from that secret, so it survives reflashing the firmware (but not erasing the whole flash).
3. Counts the boot and notes why it started (power on, restart, crash, watchdog, brownout), and loads its settings. Without a mesh key, it waits for one on the console.
4. Runs the router, which sends hellos, measures links and builds routes, and every minute reports its links, uptime, boot count and last start reason to the whole mesh, for the map.
5. Logs its neighbors and routes every 30 seconds:

```
INFO - SpectraMesh node !a1b2c3d4e5f60718 running on ESP-NOW channel 1
INFO - node !a1b2c3d4e5f60718: 2 neighbor links, 2 routes
INFO -   !0e0f101112131415 via !0e0f101112131415 on hue 0, cost 800 us
INFO -   !161718191a1b1c1d via !0e0f101112131415 on hue 0, cost 1600 us
```

## As a USB radio modem

The crate's second firmware, `spectramesh-modem`, turns a board into a radio for a computer. It runs no router: it passes every frame its radio hears to the computer over USB, and sends whatever the computer asks it to. The computer runs `spectrameshd` with a `kind=modem` hue and is the mesh node, with its own identity and keys, so the modem needs no mesh key. The modem tells the computer which radio it has:

```
cargo run --release --bin spectramesh-modem --features xiao-esp32c6
spectrameshd --mesh-key-file ~/.config/spectramesh/mesh.key \
    --hue /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_<MAC>-if00,kind=modem
```

With one ESP-NOW modem and one 802.15.4 modem (built with `--no-default-features --features ieee802154,xiao-esp32c6`), the computer joins both hues and routes between them:

```
spectrameshd --mesh-key-file ~/.config/spectramesh/mesh.key \
    --hue /dev/serial/by-id/...<ESP-NOW modem>...,kind=modem \
    --hue /dev/serial/by-id/...<802.15.4 modem>...,kind=modem
```

The 802.15.4 modem uses channel 25.

The protocol (SLIP-framed messages over the USB serial link) is in `spectramesh_core::modem`.

## How it's organized

| File | Role |
|---|---|
| `src/bin/main.rs` | Starts the hardware, registers hues and spawns tasks |
| `src/mesh.rs` | The router task. It owns the `Router`, wakes on received frames and timers, and hands frames to each hue's send queue |
| `src/storage.rs` | Keeps the node's identity, settings and boot count in the `spectramesh` flash partition |
| `src/console.rs` | The serial console |
| `src/reset.rs` | Restarts, including after a panic, and why the board last started |
| `src/ieee802154.rs` | The 802.15.4 hue: one task, as the radio is half duplex, broadcasting SpectraMesh frames in 802.15.4 data frames |
| `src/radio.rs` | Which radio the firmware was built for, and the 802.15.4 channel setting |
| `src/espnow.rs` | The ESP-NOW hue: a receive task and a send task, which unicasts to neighbors whose MAC address the router task has learned (so ESP-NOW acknowledges and retries) and broadcasts the rest, in frames of up to 1,470 bytes (ESP-NOW v2) |
| `src/sub_ghz.rs` | An SX1262 radio as a hue, not yet wired to a board |
| `src/bin/modem.rs` | The USB radio modem firmware |

Each hue talks to the router only through channels. To add a radio, write a module like `espnow.rs` with a receive task that feeds `mesh::INBOX` and a send task that reads its own `Outbox`, then register it in `main.rs`.

## Other chips

The RISC-V chips (ESP32-C3, C5, C6) build with standard Rust. To switch, replace `esp32c6` in every feature list in `Cargo.toml` and in the `runner` line of `.cargo/config.toml`. The **ESP32-C5** adds 5 GHz Wi-Fi.

The Xtensa chips (the original ESP32 and the S3) need Espressif's toolchain from [`espup`](https://github.com/esp-rs/espup), a different target, and different build settings. Generating a project with [`esp-generate`](https://github.com/esp-rs/esp-generate) is the easiest way to get them right.

## Next steps

- **Wiring the sub-GHz hue to a board.** `src/sub_ghz.rs` runs an SX1262 (driver: [`spectramesh-sx126x`](../spectramesh-sx126x)) as a hue, in GFSK or LoRa. It needs a board's pins in `main.rs`. Most ESP32 boards with an SX1262 (Heltec WiFi LoRa 32 V3, LilyGO T-Beam S3) use the ESP32-S3, an Xtensa chip; see "Other chips" below
- **An application interface**, so code on the board can send and receive mesh data (today, messages go to the console)
- **Flash encryption on real boards**; see below

## Protecting the keys on a board

The node's secret and the mesh keys sit in the `spectramesh` partition. Anyone holding a board can read its flash over USB, so on its own the partition protects nothing. The ESP32-C6 can encrypt its flash with a key in its eFuses that software can't read, and the partition is already flagged `encrypted`. The firmware reads and writes it the same way either way, so nothing changes in the code when encryption is turned on.

Turning it on takes **eFuse burns, which can never be undone**. Try it on a spare board first. The plan, not yet tried on hardware:

1. Burn a flash encryption key made on the host: `espefuse.py --port PORT burn_key BLOCK_KEY0 key.bin XTS_AES_128_KEY`.
2. Encrypt the bootloader, partition table and app with that key (`espsecure.py encrypt_flash_data --aes_xts`), and flash the encrypted images.
3. Enable encryption by burning `SPI_BOOT_CRYPT_CNT`. In this **development mode**, boards can still be reflashed with images encrypted by the same key.
4. For deployed nodes, use **release mode**: also disable encrypted writes from download mode (`DIS_DOWNLOAD_MANUAL_ENCRYPT`) and the USB JTAG interface (`DIS_USB_JTAG`), which could otherwise read memory while the firmware runs. Without them, someone with the board could load their own firmware or attach a debugger and read the keys anyway. Secure Boot v2 goes further, letting the board run only firmware you signed.

Board identities are made on the board, so encrypting a board's flash for the first time also means giving it a new identity and provisioning it again.

The USB console never prints a key, so having it doesn't help someone read them. It does let anyone who holds the board change its settings.
