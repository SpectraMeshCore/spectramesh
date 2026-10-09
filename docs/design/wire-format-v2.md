# Wire format v2 and security

**Status:** Accepted, with the recommended answer to every open question. All three phases are implemented.

SpectraMesh v1 has no security: anyone in radio range can read traffic, inject routes, and impersonate any node. Its 4-byte node IDs can't be tied to keys either. This document proposes the second version of the wire format, built around security, before anything is deployed and while the format is still cheap to change.

## Summary

- **Identity.** Each node has its own key pair, created on first boot. Its node ID is derived from its public keys, so nobody can claim an ID without holding the matching private key. IDs grow from 4 to **8 bytes**.
- **Layer 1: link authentication**, hop by hop. Every frame carries a short tag computed with the shared **mesh key**, plus a counter. Outsiders can't inject or replay anything, and relays drop forged traffic before forwarding it. This follows Babel's own authentication design (RFC 8967).
- **Layer 2: end-to-end encryption** for data. Two nodes set up a session with a Noise handshake, the pattern WireGuard uses, so relays and other mesh members can't read their traffic.
- **Cost:** a data frame's overhead grows from 14 bytes to 81. That still leaves 169 bytes of payload in a 250-byte ESP-NOW frame, and 1,417 bytes on Ethernet.

The [decisions](#decisions) at the end record the choices made.

## Goals and non-goals

**Goals**

1. Outsiders without the mesh key can't inject, alter or replay any frame, or read data.
2. Mesh members can't read or forge traffic between two other nodes.
3. Nobody can claim another node's ID without its private key.
4. It works on every hue, including 250-byte frames at 250 kbit/s, and on ESP32-C6 microcontrollers.
5. It stays `no_std` and sans-IO in `spectramesh-core`.

**Non-goals, for now**

- **Jamming.** No protocol stops someone flooding the radio band.
- **Hiding who talks to whom** (traffic analysis). Headers stay readable so relays can route.
- **Stopping a malicious member from lying about routes.** See [Routing integrity](#routing-integrity) for what is and isn't covered.
- **Babel wire compatibility.** It was already dropped in v1.

## Threat model

| Attacker | Has | Can, after v2 | Can't, after v2 |
|---|---|---|---|
| **Outsider** | A radio, and recordings of old traffic | Jam. See who talks to whom | Read data. Inject or replay any frame. Learn the mesh key from traffic |
| **Member** | The mesh key | Everything an outsider can. Advertise false routes and drop what it attracts | Read or forge other nodes' end-to-end traffic. Impersonate another node |
| **Stolen device** | A member's keys | Everything a member can, as that node | Decrypt sessions recorded before the theft (forward secrecy) |

The mesh key works like a Wi-Fi password: it decides who may join. Anyone who has it can disrupt routing. Removing a member means changing the key; see [key rotation](#key-rotation).

## Identity

### Keys

Each node generates two key pairs on first boot and stores them: in flash (NVS) on ESP32, and in `/etc/spectramesh/node.key` (mode 0600) on Linux.

- **Ed25519**, for signatures (used in phase 3)
- **X25519**, for key agreement in end-to-end sessions

Two separate pairs avoid converting one key type into the other, which is safe but easy to get wrong.

### Node IDs

```
identity hash = BLAKE2s-256("SpectraMesh identity v1" || ed25519_public || x25519_public)
node ID       = first 8 bytes of the identity hash
```

Node IDs are printed as `!` followed by 16 hex digits, extending today's 8-digit form.

Because the ID is derived from the keys, an ID is **self-certifying**. Anyone can check that a set of public keys belongs to an ID by hashing them, with no certificates or authority involved.

#### How long should IDs be?

The ID's length sets how hard it is to *grind* one: generate key pairs until one hashes to a victim's ID. As a rough guide, assume an attacker can try 10⁹ keys a second on GPUs.

| Length | Grinding a specific victim's ID | Header cost per data frame (src, dst, next hop) |
|---|---|---|
| 4 bytes (today) | **Seconds** | 12 bytes |
| **8 bytes** | About 600 GPU-years | 24 bytes |
| 16 bytes | Out of reach | 48 bytes |

**Recommendation: 8-byte IDs on the air, with the full 32-byte identity hash used wherever impersonation matters:**

- **Routing** uses 8-byte IDs. The worst a successful grind achieves there is confusing routes to one node, which a member could do anyway.
- **End-to-end sessions** are opened to a *full* identity hash, like a WireGuard peer configured by its public key. A ground ID then gets an attacker nowhere, because the handshake proves the other side holds the keys behind the full 32-byte hash.
- **IPv6:** 8 bytes is exactly the size of an IPv6 interface identifier, which makes [IP addressing](#ip-addressing) straightforward.

### Identity discovery

A node needs another node's public keys to open a session with it. It asks for them hop by hop instead of flooding:

1. A node wants keys for `!a1b2…` and sends an **IdentityRequest** towards it, along the route.
2. Any node on the way that has the keys cached replies with an **Identity** TLV. Otherwise the request continues to the destination.
3. The requester checks that the keys hash to the ID, so a relay can't substitute other keys. It then caches them.

Every node caches identities it has seen, up to a size limit, so popular nodes are answered nearby.

## Layer 1: link authentication

This layer stops outsiders. Every frame on every hue carries a 16-byte trailer:

| Field | Size | Contents |
|---|---|---|
| Boot index | 4 bytes | Random, chosen each time the sender starts |
| Counter | 4 bytes | Increases with every frame the sender sends, on any hue |
| Tag | 8 bytes | Keyed BLAKE2s over the whole frame before it, truncated to 8 bytes |

The tag key is derived from the mesh key: `BLAKE2s(mesh key, "SpectraMesh link v1")`.

**Why an 8-byte tag?** Forging a frame then takes about 2⁶⁴ attempts, and every attempt has to go over the air, where each one costs real time. 802.15.4 makes the same trade-off on slow links. A 16-byte tag would add 8 bytes to every frame, which matters at 250 bytes.

**Why authenticate and not encrypt?** Data is already encrypted end to end. Routing messages only reveal topology, which an outsider can largely work out from radio traffic anyway. A tag also fails more gracefully than encryption: if a counter were ever reused, the result is a replayed frame, not a broken key.

### Replay protection

This follows RFC 8967, Babel's authentication extension:

- Each node picks a random 4-byte **boot index** whenever it starts, and puts it in every frame's trailer.
- Receivers remember each neighbor's index and the counters it has used recently, on each hue, and drop any frame they've seen before. A window of the last 64 counters lets frames arrive slightly out of order.
- When a frame arrives with an index the receiver doesn't know, the receiver drops it and sends a **ChallengeRequest** with a random nonce. The neighbor echoes it in a **ChallengeReply**, which proves the new index is live and not a recording. Two nodes meeting for the first time challenge each other.
- Once verified, the receiver says hello, sends its routes, and asks the neighbor for its routes, in case the neighbor dropped this node's frames before verifying it in turn. A new index also means the neighbor restarted, so its hello history is reset.

A recording can only be replayed if a node later picks the same boot index again: for each restart, about one chance in four billion per recorded boot.

This avoids writing counters to flash, which would wear it out on ESP32.

### Key rotation

Each frame carries a 1-byte **key ID**. During a rotation, nodes accept both the old and the new key, and send with the new one once it's configured. To remove a member, distribute a new key to everyone else and retire the old ID.

Distributing keys is manual for now: a config option on Linux, and a build setting on ESP32. The Linux daemon logs the IDs it sends and accepts at startup, so a node left behind during a change is easy to spot.

## Layer 2: end-to-end encryption

This layer stops other members. Unicast data is encrypted between source and destination with a **Noise IK** handshake, the same pattern WireGuard uses:

1. The initiator already knows the responder's static X25519 key, from identity discovery.
2. **One round trip** sets up a session: 133 bytes there and 57 back. The initiator's message carries its Ed25519 key, encrypted, so the responder can check that the keys hash to the node ID the frame claims. Both sides authenticate, and the session gets forward secrecy.
3. Data then travels as: the receiver's session index (4 bytes), a counter (8 bytes), ciphertext, and a 16-byte tag (ChaCha20-Poly1305). Counters let messages arrive out of order; a 64-message window rejects repeats.
4. As in WireGuard, the responder doesn't send on a session until the initiator has used it, which proves the handshake wasn't a replay. An initiator with no data waiting sends a keepalive for that.
5. Sessions are replaced when data is sent on one older than 2 minutes (or after 2⁶⁰ messages), and dropped at 3 minutes. Idle sessions just expire.
6. Two timers, also from WireGuard, notice when the other side has lost the session, for example by restarting: a node that received data but has sent nothing for 10 seconds sends a keepalive, and a node that sent data but has heard nothing for 15 seconds starts a new handshake.

The first byte inside the ciphertext names what the payload is (an IP packet, an application message, and so on), so even that is hidden from relays.

**Alternatives considered**

| Option | Overhead per message | Forward secrecy | Setup | Fits |
|---|---|---|---|---|
| **Noise IK sessions** (recommended) | 30 bytes | Yes | 1 round trip | Ongoing traffic, IP |
| Ephemeral key per message (like Reticulum) | 48 bytes | Partial | None | One-off messages on slow links |
| Static keys, random nonce | 40 bytes | No | None | Simple, but weakest |

For the future LoRa tier, where a round trip can take seconds, one-off messages could use per-message ephemeral keys instead. That can be added as a second payload type later.

### Channels

**Group messages** use channels, like Meshtastic's. A channel is a 32-byte key its members share, written as `smc1-` followed by 55 base32 characters, with a checksum, like a mesh key.

- **Encryption:** XChaCha20-Poly1305 under a key derived from the channel key, with a random 24-byte nonce, so nothing has to be counted or stored across restarts. The origin node and a random message ID are authenticated with the data, so relays can't change them.
- **Flooding:** every node rebroadcasts each new channel message once, until its TTL runs out, and remembers recent (origin, message ID) pairs for a minute, so copies arriving by other paths are dropped. Nodes relay every channel's messages, including ones they can't read.
- **Limit:** messages aren't signed per sender, so any member of a channel can claim another member's node as a message's origin. Members of a channel trust each other with it; nobody else can read or forge its messages.
- **Cost:** 46 bytes per message on top of a data frame's 51. Blind flooding sends every message once per node; choosing relays, as OLSR's multipoint relays do, would reduce that on slow hues.

Broadcasts meant only for neighbors stay as they were: protected by the mesh key alone, and never relayed.

## Routing integrity

Link authentication keeps outsiders out of routing entirely. Against a member:

| Attack | Effect | Covered? |
|---|---|---|
| Forging a newer seqno for another node | Pulls all traffic for that node through the attacker | **Yes**, phase 3: origins sign their seqnos |
| Advertising a falsely low metric | Pulls nearby traffic through the attacker | No. This is an open research problem for distance-vector protocols (hash-chain schemes such as SEAD exist, but are expensive) |
| Dropping traffic it relays | Black hole | No. End-to-end acknowledgements can at least detect it |
| Reading or changing relayed data | None: the data is encrypted and authenticated end to end | **Yes**, layer 2 |

**Signed seqnos:** every node signs each seqno it issues for its own route with its Ed25519 key, over `"SpectraMesh seqno v1"`, its node ID and the seqno. The signature travels as a **SeqnoProof**, which also carries the node's public keys, so a receiver can check it without first asking for them. Proofs go out in triggered updates and in the first full update to a newly verified neighbor, and nodes forward the best proof they hold for each destination.

A node ignores any update whose seqno is newer than the newest one it has a valid proof for, and asks that neighbor for the destination's route, which comes with its proof. A replayed old proof can only claim a seqno the destination really issued; if the destination has since restarted and started again from 0, it raises its seqno past the replayed one as soon as neighbors ask, which they do when its updates look old.

**Reboots:** a rebooted node restarts its seqno at 0, which its neighbors see as old. The seqno-request mechanism recovers: neighbors request a newer seqno, and the node jumps to it and signs it. A test confirms routes recover after a restart.

## Wire format v2

### Common header (18 bytes)

| Bytes | Field |
|---|---|
| 0 | Version `2` (high 4 bits), frame kind (low 4 bits) |
| 1 | Mesh key ID |
| 2..10 | Sender: the node that transmitted this frame |
| 10..18 | Next hop node ID, or broadcast |

Every frame ends with the 16-byte [trailer](#layer-1-link-authentication): the boot index, a counter and a tag.

### Control frames

The common header, then TLVs, then the trailer. Changes from v1:

| TLV | Change |
|---|---|
| IHU, Update, RouteRequest, SeqnoRequest | Node IDs grow to 8 bytes |
| **ChallengeRequest**, **ChallengeReply** | New: 8-byte nonce |
| **IdentityRequest**, **Identity** | New: node ID, then 64 bytes of public keys |
| **SeqnoProof** | New in phase 3: the node's public keys (64 bytes), seqno and a 64-byte signature |

### Data frames

| Bytes | Field |
|---|---|
| 0..18 | Common header |
| 18..26 | Origin: the node that created the packet |
| 26..34 | Destination node ID |
| 34 | TTL |
| 35.. | End-to-end message: type (1), receiver's session index (4), counter (8), ciphertext, tag (16) |
| last 16 | Trailer |

The header's sender changes at every hop, and is what the trailer is checked against. The origin stays the same end to end.

Noise handshake messages travel as data frames, with their own payload kind. Broadcast data frames start with a kind byte instead: 0 for neighbors only, 1 for a channel message.

### Fragment frames

A data frame too big for the next hue is split into **fragment frames** (kind 3), and reassembled by the next node before it's routed further: per hop, as 6LoWPAN does in mesh networks, because a route can mix hues of different sizes. Each fragment carries the common header, a 6-byte fragment header (datagram ID, offset, total length), a chunk of the data frame's body, and its own trailer, so nodes outside the mesh can't inject fragments. Reassembled bodies are at most 1,600 bytes; a node keeps at most 8 partial datagrams, for 5 seconds each.

Control frames aren't fragmented, so every hue must carry at least 166 bytes, the size of a frame with one SeqnoProof. Every radio SpectraMesh targets does.

### Overhead

| | v1 | v2 |
|---|---|---|
| Hello frame | 15 bytes | 40 bytes |
| Data overhead per frame | 14 bytes | 81 bytes (52 for broadcasts to neighbors, 97 for channel messages) |
| Overhead per fragment | | 40 bytes |
| Largest end-to-end payload | | 1,553 bytes, fragmented where needed |
| Payload in a 250-byte ESP-NOW v1 frame | 236 | **169** |
| Payload in a 255-byte sub-GHz FSK frame | 241 | **174** |
| Payload in a 1,470-byte ESP-NOW v2 frame | 1,456 | **1,389** |
| Payload on Ethernet (1,498 bytes) | 1,484 | **1,417** |

On a 250 kbit/s link with 10-second hellos, the extra 25 bytes per hello is under 1 ms of airtime every 10 seconds.

## IP addressing

The Linux daemon's TUN device (`spectrameshd --tun`) gives each node an IPv6 address derived from its identity:

```
fdXX:XXXX:XXXX:0000 : <8-byte node ID>
└── /64 prefix from a hash of the mesh key
```

A packet to that address routes straight to the node, with no address assignment or lookup table. Its keys are found with identity discovery. IPv4 could be carried later with configured mappings.

**No spoofing:** a packet from the mesh is only written to the TUN device if its source address belongs to the node that sent it, as proven by the session.

**Stable addresses:** the prefix comes from the first mesh key, so changing the key changes every address. A mesh that wants stable addresses sets its prefix explicitly (`--ipv6-prefix`).

**MTU:** IPv6 needs at least 1,280 bytes per packet. The TUN device's MTU is the smallest hue's MTU less the 81 bytes of overhead: 1,417 on Ethernet. That fits on Ethernet, Wi-Fi and ESP-NOW v2, but not on 250-byte hues, which will need a fragmentation layer to carry IP. Application messages that fit in one frame don't need it.

## Implementation notes

**Cryptography:** the WireGuard set of algorithms: X25519, ChaCha20-Poly1305 and BLAKE2s, plus Ed25519 for signatures. The Noise handshake and transport come from the [`snow`](https://crates.io/crates/snow) crate; the other primitives from the RustCrypto and dalek crates, at the versions `snow` uses, so firmware carries one copy of each. All are pure Rust and work in `no_std`.

**Keeping the core sans-IO:** the platform passes the router a 32-byte seed from its hardware or OS random number generator at startup (the ESP32's RNG, or `getrandom` on Linux), the same way it supplies time. Nonces, boot indexes and handshake keys come from a generator keyed with that seed. Key storage stays in the platform crates.

**ESP32-C6 performance:** expect X25519 and Ed25519 operations to take milliseconds in software. They're only needed for handshakes, roughly one per peer every 2 minutes. ChaCha20 and BLAKE2s are fast in software, and the C6's AES and SHA accelerators aren't needed. **To be measured** on the XIAO boards.

**Linux routers:** ChaCha20-Poly1305 gives WireGuard-class throughput. Exact numbers for MIPS and ARM routers are **to be measured**.

## Phases

| Phase | Delivers | Unlocks |
|---|---|---|
| **1. Format and link security** | 8-byte key-derived IDs, key storage, the v2 header, link tags, replay protection and challenges, key IDs | Outsiders locked out. Fixes the "IDs from MACs" TODOs |
| **2. End-to-end** | Identity discovery, Noise IK sessions, encrypted payloads, and the TUN device on Linux | Private traffic between nodes; IP over the mesh |
| **3. Routing integrity and groups** | Signed seqnos, group keys for broadcast, key rotation tools | Members can't hijack other nodes' routes; secure channels |

Phase 1 changes every frame, so all nodes must be upgraded together. There are no deployed nodes yet, so this is the moment to do it.

## Changes during phase 1

Implementing phase 1 showed two gaps in the original draft:

1. **Data frames need an origin as well as a sender.** Trailers have to be checked against the node that transmitted a frame, but a data frame's source was the node that created it, possibly hops away. Data frames now carry both, which costs 8 bytes.
2. **The boot index belongs in every frame, not only in hellos.** Otherwise a node couldn't check frames from a neighbor that arrived before that neighbor's next hello. Moving the index into the trailer makes every frame checkable on its own, and verification identical for every frame. It costs 4 bytes per frame, offset by using a 4-byte index instead of 8.

Overheads above include both changes.

## Changes during phase 2

1. **Transport messages name the receiver's session with a 4-byte index**, as WireGuard does, rather than a 1-byte epoch. Each side picks its own index, so simultaneous handshakes and rekeys can't collide. It costs 4 more bytes per message (30 instead of 26).
2. **The handshake init carries the initiator's Ed25519 key.** The responder needs both public keys to check them against the claimed node ID, and Noise only carries the X25519 one.
3. **Liveness timers** (keepalive after 10 seconds, new handshake after 15 seconds of silence) were added, so a session recovers quickly when the other side restarts.
4. **Identity answers only count if someone asked for them**, so a member can't fill other nodes' caches with keys nobody wanted.
5. **Broadcasts to neighbors stay protected only by the mesh key.** Phase 3 adds channels for group messages that need more.

## Changes during phase 3

1. **Proofs carry the signer's public keys**, so receivers can verify them straight away instead of waiting for identity discovery. That makes a SeqnoProof 130 bytes, and sets the minimum hue size at 166 bytes.
2. **Proofs go out only when they matter**: in triggered updates, and in the first full update to a new neighbor. A node that can't accept an update asks for the destination's route, which brings its proof.
3. **Channels use random 192-bit nonces (XChaCha20)** instead of counters, so nodes never have to persist anything to keep nonces unique.
4. **Fragmentation and ICMPv6 errors** were added alongside phase 3, closing the gaps phase 2 left: IPv6 now crosses 250-byte hues, and the Linux daemon answers undeliverable packets with "no route" errors.

## Decisions

Decided on 2026-10-09, taking the recommendation in each case:

1. **Node IDs are 8 bytes.** Sessions are opened to the full 32-byte identity hash.
2. **Link tags are 8 bytes.**
3. **The mesh key is a random 32-byte key**, shared as text (and later as a QR code). No passphrases.
4. **End-to-end encryption will be required** for unicast data. **Every hue uses link tags**, including wired ones.
5. **Membership is a shared mesh key.** Per-node certificates may come later.
