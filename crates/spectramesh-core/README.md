# spectramesh-core

The routing engine for [SpectraMesh](https://github.com/SpectraMeshCore/spectramesh). It decides which neighbor, and which **hue** (2.4 GHz or 5.8 GHz Wi-Fi, a sub-GHz link such as 915 MHz HaLow, FSK or LoRa, ESP-NOW, Ethernet or fiber, or IP), each packet should take.

- **`no_std`**: needs only an allocator, so the same code runs on ESP32, OpenWRT routers and desktops.
- **Sans-IO**: no radios, sockets, threads or clocks. Platform crates pass in received frames and the current time, and send the frames it returns.
- **No dependencies.**

## How it routes

It follows [Babel](https://www.rfc-editor.org/rfc/rfc8966) (RFC 8966), a loop-free distance-vector protocol:

1. **Measuring links.** Every node broadcasts numbered hellos on each hue. Receivers report back how many arrived in IHU ("I heard you") messages, so each link is measured in both directions.
2. **Sharing routes.** Each node advertises a route to itself, numbered with a sequence number (seqno) that only it can raise. Neighbors pass on the routes they select, adding their own link cost. Nothing is flooded across the whole mesh, so a node's overhead depends on its neighbors, not on the size of the mesh.
3. **Staying loop-free.** A node only selects a route that's strictly better than the best it has advertised: a newer seqno, or a lower metric. That rules out routes that lead back through itself, so loops can't form even while links are failing. When a node has no usable route left, it asks the destination for a newer seqno.

### Link cost

A link's cost is its **expected transmission time** in microseconds: how many tries a packet takes, counting both directions, times how long a 100-byte frame takes on that hue. A gigabit Ethernet or fiber hop costs 1, a perfect 20 Mbit/s Wi-Fi hop 40 and a 250 kbit/s sub-GHz hop 3,200. Routes prefer fast hues, use long-range ones where nothing else reaches, and can mix hues hop by hop.

Routing only looks at a hue's bitrate, MTU, timers and link model, never its band. A 915 MHz radio running HaLow or FSK is simply a slower backbone link.

### Wired links

Ethernet, fiber and IP hues use the **reliable** link model, as Babel does for wired links. Radios use the **lossy** one.

| | Lossy (radio) | Reliable (wired) |
|---|---|---|
| Cost | Rises with the share of hellos lost | Fixed while the link is up |
| Down after | 4 hellos in a row are lost | 2 of the last 3 hellos are lost |
| One lost hello | Raises the cost | No effect |

So cabled nodes always route over the cable where it reaches, a single dropped hello never moves traffic, and a cut cable is noticed within about 10 seconds, after which traffic falls back to radio.

On switched networks, the platform can unicast instead of broadcasting: every frame the router hands back names its next hop, and `handle_frame` returns the sender of every frame it accepts, so the platform learns which link-layer address belongs to which node.

## Security

All three phases of the [security design](../../docs/design/wire-format-v2.md) are in place:

- **Node IDs come from keys.** Each node keeps a 32-byte secret, from which it derives Ed25519 and X25519 key pairs. Its 8-byte node ID is the start of a hash of the public keys, so claiming another node's ID means finding keys that hash to it ([`identity.rs`](src/identity.rs)).
- **Every frame is authenticated** with an 8-byte tag keyed with the shared mesh key. Nodes without the key can't inject or alter anything ([`auth.rs`](src/auth.rs)).
- **Replays are rejected.** Each frame carries the sender's random boot index and a counter. A neighbor with an unknown boot index must answer a challenge before its frames count.
- **Keys can change without downtime.** A node can accept several mesh keys while sending with one.
- **Unicast data is encrypted end to end** ([`session.rs`](src/session.rs)), so relays and other members can't read or alter it. Nodes find each other's public keys by asking along the route, then set up a session with one round trip of the Noise IK handshake, as WireGuard does. Sessions are replaced every 2 minutes and recover on their own when the other side restarts.
- **Deliveries say who sent them.** Each delivery carries the sender's public keys as proven by the handshake, so an application can compare the full identity hash with the one it expects.

- **Members can't hijack other nodes' routes.** Every node signs the sequence numbers it issues for its own route, and nodes ignore newer sequence numbers that come without a valid signature ([`router/proofs.rs`](src/router/proofs.rs)). A member can still advertise a falsely low cost, which no practical distance-vector protocol prevents.
- **Channels** carry encrypted group messages across the whole mesh, like Meshtastic channels ([`channel.rs`](src/channel.rs)). Members share a channel key; every node relays channel messages, but only members can read them.

Broadcasts meant only for neighbors are protected by the mesh key alone.

## Fragmentation

Data too big for a hue is split into fragments and reassembled by the next node ([`fragment.rs`](src/fragment.rs)), so a route can mix Ethernet and 250-byte radio hops. Payloads of up to 1,553 bytes go anywhere. Hues must carry at least 166-byte frames, which every radio SpectraMesh targets does.

## Using it

```rust
use spectramesh_core::{Config, HueId, HueInfo, HueKind, Identity, KeyRing, MeshKey, Router};

// Stored secrets, and 32 fresh random bytes on every start.
let identity = Identity::from_secret(stored_secret);
let mesh_key = MeshKey::from_text("smk1-...")?;
let mut router = Router::new(identity, KeyRing::new(&mesh_key), Config::default(), random_seed);
// Timers are picked from the bitrate; change the fields to override them.
router.add_hue(HueInfo::new(HueId(0), HueKind::Wifi { freq_mhz: 2437 }, 1400, 20_000_000));
router.add_hue(HueInfo::new(HueId(1), HueKind::Fsk { freq_khz: 915_000 }, 255, 250_000));
router.add_hue(HueInfo::new(HueId(2), HueKind::Ethernet, 1500, 1_000_000_000));

// In the platform's event loop:
// router.handle_frame(hue, &frame, now)?;     for every frame a radio receives
// router.poll(now);                          at or after router.next_wakeup()
// while let Some(tx) = router.poll_transmit() { /* send tx.frame on tx.hue, to tx.next_hop */ }
// while let Some(packet) = router.poll_delivery() { /* hand to the app */ }
// router.send(dst, &data, now)?;               encrypted end to end
```

## Wire format

Version 2: an 18-byte header (version and frame kind, mesh key ID, sender, next hop), a body, and a 16-byte trailer (boot index, counter, tag):

- **Control frames**: Babel-style TLVs (hello, IHU, update, route and seqno requests, challenges), several per frame.
- **Data frames**: origin, destination and TTL, then the payload: for unicast, an end-to-end message (a handshake, or encrypted data), and for broadcasts, a neighbor message or a channel message. 81 bytes of overhead in all for encrypted unicast data, 97 for a channel message.
- **Fragment frames**: a piece of a data frame too big for the hue.

See [`packet.rs`](src/packet.rs) and the [design document](../../docs/design/wire-format-v2.md). The format is SpectraMesh's own, so it doesn't exchange routes with `babeld`.

## Not done yet

Each is marked with a `TODO` in the code.

- **Slow or airtime-limited hues** (LoRa, EU 868 MHz duty cycles) should use on-demand routing within an airtime budget instead of Babel on slow timers.
- Measured throughput per neighbor instead of a configured bitrate
- Hysteresis, so near-equal routes don't flap
- Split horizon on wired hues, to send smaller updates on switched networks
- Telling 1 Gbit/s from 10 Gbit/s links apart (both cost 1)
- Choosing relays for channel messages, as OLSR's multipoint relays do, instead of every node rebroadcasting each one
