# spectramesh-core

The routing engine for [SpectraMesh](https://github.com/SpectraMeshCore/spectramesh). It decides which neighbor, and which **hue** (2.4 GHz or 5.8 GHz Wi-Fi, a sub-GHz link such as 915 MHz HaLow, FSK or LoRa, ESP-NOW, or IP), each packet should take.

- **`no_std`**: needs only an allocator, so the same code runs on ESP32, OpenWRT routers and desktops.
- **Sans-IO**: no radios, sockets, threads or clocks. Platform crates pass in received frames and the current time, and send the frames it returns.
- **No dependencies.**

## How it routes

It follows [Babel](https://www.rfc-editor.org/rfc/rfc8966) (RFC 8966), a loop-free distance-vector protocol:

1. **Measuring links.** Every node broadcasts numbered hellos on each hue. Receivers report back how many arrived in IHU ("I heard you") messages, so each link is measured in both directions.
2. **Sharing routes.** Each node advertises a route to itself, numbered with a sequence number (seqno) that only it can raise. Neighbors pass on the routes they select, adding their own link cost. Nothing is flooded across the whole mesh, so a node's overhead depends on its neighbors, not on the size of the mesh.
3. **Staying loop-free.** A node only selects a route that's strictly better than the best it has advertised: a newer seqno, or a lower metric. That rules out routes that lead back through itself, so loops can't form even while links are failing. When a node has no usable route left, it asks the destination for a newer seqno.

### Link cost

A link's cost is its **expected transmission time** in microseconds: how many tries a packet takes, counting both directions, times how long a 100-byte frame takes on that hue. A perfect 20 Mbit/s Wi-Fi hop costs 40 and a 250 kbit/s sub-GHz hop costs 3,200. Routes prefer fast hues, use long-range ones where nothing else reaches, and can mix hues hop by hop.

Routing only looks at a hue's bitrate, MTU and timers, never its band. A 915 MHz radio running HaLow or FSK is simply a slower backbone link.

## Using it

```rust
use spectramesh_core::{Config, HueId, HueInfo, HueKind, Instant, NodeId, Router};

let mut router = Router::new(NodeId::from_u32(0x1234_5678), Config::default());
// Timers are picked from the bitrate; change the fields to override them.
router.add_hue(HueInfo::new(HueId(0), HueKind::Wifi { freq_mhz: 2437 }, 1400, 20_000_000));
router.add_hue(HueInfo::new(HueId(1), HueKind::Fsk { freq_khz: 915_000 }, 255, 250_000));

// In the platform's event loop:
// router.handle_frame(hue, &frame, now)?;     for every frame a radio receives
// router.poll(now);                          at or after router.next_wakeup()
// while let Some(tx) = router.poll_transmit() { /* send tx.frame on tx.hue */ }
// while let Some(packet) = router.poll_delivery() { /* hand to the app */ }
```

## Wire format

A 9-byte common header (version and frame kind, source, next hop), then:

- **Control frames**: Babel-style TLVs (hello, IHU, update, route request, seqno request), several per frame.
- **Data frames**: destination and TTL, then the payload. 14 bytes of header in all.

See [`packet.rs`](src/packet.rs). The format is SpectraMesh's own, smaller than Babel's, so it doesn't exchange routes with `babeld`.

## Not done yet

Each is marked with a `TODO` in the code.

- **Slow or airtime-limited hues** (LoRa, EU 868 MHz duty cycles) should use on-demand routing within an airtime budget instead of Babel on slow timers.
- Measured throughput per neighbor instead of a configured bitrate
- Hysteresis, so near-equal routes don't flap
- Mesh-wide broadcast (today, broadcast reaches direct neighbors only)
- Signing, encryption, and node IDs derived from public keys
