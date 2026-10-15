# spectramesh-wrt

`spectrameshd`, the SpectraMesh routing daemon for OpenWRT routers and other Linux systems, plus its OpenWRT package. It runs the [`spectramesh-core`](../spectramesh-core) router over Linux network devices.

## Hues on Linux

SpectraMesh frames travel directly inside Ethernet frames (EtherType `0x88B5`), with no IP. So one driver covers every device Linux presents as Ethernet:

| Link | Linux device | `--hue` example |
|---|---|---|
| Ethernet | `eth1`, `lan2`, ... | `eth1` |
| Fiber (SFP) | `sfp0`, `eth2`, ... | `sfp0,bitrate=10G` |
| 2.4 or 5.8 GHz Wi-Fi | an 802.11s mesh interface | `phy1-mesh0,kind=wifi,freq=5805,bitrate=100M` |
| HaLow (915 MHz) | an 802.11s mesh interface on a HaLow radio | `wlan2,kind=halow,bitrate=4M` |

Wired links use the reliable link model and radio links the lossy one (see the core README), so cabled routers prefer the cable and fall back to radio when it's cut.

**ESP-NOW or 802.15.4, through a USB radio modem.** An ESP32 running `spectramesh-modem` (see `spectramesh-esp`) gives any Linux machine an ESP-NOW or 802.15.4 hue, so a laptop can join a mesh of ESP32 nodes: `--hue /dev/serial/by-id/...,kind=modem`. The modem reports its radio, MTU and bitrate. With one modem of each kind, the laptop joins both hues and routes between them. Serial ports need membership of the `dialout` group, not root.

**A laptop's own Wi-Fi.** A laptop connected to a router's ordinary Wi-Fi network can reach a router running `spectrameshd` on that network's bridge with an Ethernet-style hue on its Wi-Fi interface (`--hue wlp164s0,kind=wifi,bitrate=100M`): SpectraMesh frames travel inside the Wi-Fi network like Ethernet frames. Joining an 802.11s mesh directly needs a card whose driver supports mesh mode; Intel cards' generally doesn't.

**UDP, across networks SpectraMesh doesn't control.** To link sites over the internet or a VPN, a UDP hue sends frames in UDP datagrams: `--hue [::]:4790,kind=udp,peer=hub.example.org:4790`. It has no broadcast, so broadcast frames go to each `peer` (repeatable) and to every neighbor learned on the hue. A node on a public address can list no peers and learn them from the frames they send, which also suits nodes behind NAT: their hellos, every 4 seconds, keep the NAT's mapping open. The mesh key protects UDP frames as it does every frame, so the port can face the internet. Peer names are looked up again every five minutes. The default MTU is 1,200 bytes, to cross any path without IP fragmentation; larger packets are fragmented by SpectraMesh. UDP links count as reliable and 10 Mbit/s unless told otherwise (`bitrate=`), so routes prefer local links where both exist.

Each Ethernet frame starts with a 2-byte length, because Ethernet pads short frames. Frames for a known neighbor are sent to its MAC address, which on Wi-Fi gets link-layer acknowledgements and retries. Everything else is broadcast.

Hues follow their device by name. If it's taken down, or deleted and created again (as `wifi up` and `ifup` do to Wi-Fi interfaces), the hue waits for it and carries on, logging once rather than for every frame. A device must exist when the daemon starts; on OpenWRT, procd restarts the daemon until it does.

## Running on any Linux machine

```
cargo build --release -p spectramesh-wrt
sudo mkdir -p /etc/spectramesh
./target/release/spectrameshd --generate-mesh-key | sudo tee /etc/spectramesh/mesh.key >/dev/null
sudo chmod 600 /etc/spectramesh/mesh.key
sudo ./target/release/spectrameshd --mesh-key-file /etc/spectramesh/mesh.key --hue eth0
```

Every node in a mesh needs the same mesh key, so copy that file to the others. On first start, each node also creates its own secret in `/etc/spectramesh/node.key` (change it with `--identity`). Its node ID comes from that secret, so back it up to keep the ID when reinstalling.

Raw sockets need root or the `CAP_NET_RAW` capability. To run without `sudo`, give the binary the capability and point `--identity` and `--mesh-key-file` at files your user can read:

```
sudo setcap cap_net_raw+ep target/release/spectrameshd
```

Run `spectrameshd --help` for every option. Each node logs its neighbors and routes every 30 seconds, and warns about frames from outside the mesh.

### Changing the mesh key

List more than one key in the mesh key file, one per line. Nodes send with the first and accept all of them. To change keys without downtime: add the new key as a second line everywhere, then move it to the first line everywhere, then remove the old one. Each node logs the key IDs it sends with and accepts when it starts, so you can check every node has the same keys at each stage.

### IPv6 over the mesh

With `--tun smesh0`, the daemon creates a TUN device and gives this node an IPv6 address: the mesh's `/64` prefix followed by its node ID. Any program can then reach any other node by address, encrypted end to end:

```
$ spectrameshd --node-info --mesh-key-file /etc/spectramesh/mesh.key
node ID:       !5aced5892ccbf39d
identity hash: 5aced5892ccbf39d...
IPv6 address:  fd72:a1fd:63c4:0:5ace:d589:2ccb:f39d
$ ping fd72:a1fd:63c4:0:5ace:d589:2ccb:f39d
```

The prefix comes from the first mesh key, so changing the key changes every address. To keep addresses stable, pick a prefix and pass it to every node with `--ipv6-prefix fd12:3456:789a::`.

The TUN device needs root or `CAP_NET_ADMIN`. Its MTU is the smallest hue's MTU less SpectraMesh's 81 bytes of overhead, so packets usually cross the mesh whole. If a hue is too small for that, as a 250-byte radio link is, the MTU is IPv6's minimum of 1,280 and SpectraMesh fragments packets on the small links.

Packets arriving from the mesh are only passed to the system if their source address belongs to the node that sent them. Packets to an address in the mesh that no node has are answered straight away with an ICMPv6 "destination unreachable" error.

### The mesh map

`--web 127.0.0.1:8470` serves a live map of the mesh at that address: every node and link, drawn as a graph you can drag around, with link types colored, plus tables of nodes and of this node's routes. Radio links show their signal strength: each end reports how strongly it hears the other, the label shows the weaker direction, and weak links (below −85 dBm) are highlighted. Hover over a link for both directions. It updates every 2 seconds and needs no internet connection. `--name` labels this node on it.

The map comes from **neighbor reports**: every node, ESP32s included, floods its name and links once a minute, and again shortly after it meets a new neighbor. The page is served without authentication and shows the mesh's layout, so keep it on `127.0.0.1` unless you mean to share it. `GET /api/mesh` returns the same data as JSON.

### Channels

Channels carry encrypted group messages across the whole mesh. Make a key with `spectrameshd --generate-channel-key`, give it to every member, and list it in a file passed with `--channel-key-file` (one key per line). Nodes relay channel messages, but only members can read them.

### Sending and receiving messages

Programs on the same host use the mesh through the daemon's control socket, `/var/run/spectramesh.sock` (change it with `--control PATH`; only the daemon's user can open it). From the shell:

```
spectrameshd --channels                                  # the joined channels' IDs
spectrameshd --send-channel 0e01 "hello everyone"        # the start of a channel ID is enough
spectrameshd --send '!4c6137c0a0eb06a1' "hello you"      # one node, encrypted end to end
spectrameshd --listen                                    # print messages as they arrive
```

`--listen` prints each message with its sender and channel, or `direct` for messages to this node alone. Only direct messages prove who sent them; any member of a channel could claim to be another.

Programs can speak the socket's line protocol directly (`socat - UNIX-CONNECT:/var/run/spectramesh.sock`): `send NODE HEX`, `channel CHANNEL HEX`, `channels` and `listen`, each answered with `ok` or `error REASON`; after `listen`, arriving messages follow as `message FROM CHANNEL HEX`. See `src/control.rs`.

## Trying it without hardware

`scripts/veth-demo.sh` runs four nodes, each in a throwaway network namespace of its own, joined by virtual Ethernet cables. Nodes 1 and 3 reach each other through node 2, and node 1 pings node 3's IPv6 address. Node 4 is plugged into node 2 too, but has a different mesh key. Nodes 1 and 3 also share a channel that node 2 relays without its key, and node 1 sends node 3 a message on it and another directly, through its control socket. It needs no root.

```
cargo build -p spectramesh-wrt
crates/spectramesh-wrt/scripts/veth-demo.sh
```

```
[node 1 ping] 3 packets transmitted, 3 received, 0% packet loss, time 2003ms
[node 3 listen] !4c6137c0a0eb06a1 0e01a144bf6c6eb0: hello channel
[node 3 listen] !4c6137c0a0eb06a1 direct: hello node 3
[node 1] info:   !5aced5892ccbf39d via !9149fdc63f0b6b51 on a0, cost 2 us
[node 2] warning: dropped a frame on hue 2: frame tag doesn't verify with any mesh key
[node 4] info: node !8b6ff553f685778c: 0 neighbor links, 0 routes
```

Node IDs are random, because each run creates new keys.

The same scenario runs as a test: `cargo test -p spectramesh-wrt -- --ignored`.

`tests/udp.rs` runs three daemons linked by UDP on this host, with no privileges: two spokes that only know a hub's address, and the hub, which learns them. The spokes route to each other through the hub and exchange a message. `tests/multi_hue.rs` (ignored by default; about 20 seconds) gives the hub two hues, one to each of two nodes that also share a slow third hue: their routes change hue at the hub, and fall back to the slow hue when the hub stops. `tests/reattach.rs` (ignored; about 35 seconds; user namespaces) joins two daemons with a veth pair, deletes it and creates it again, then takes one end down and up, and checks the route returns each time.

## OpenWRT

> Tested on a HiLink HLK-7621 (MT7621, `ramips/mt7621`, OpenWRT 24.10.1) with the binary built as below and the init script and UCI config from `openwrt/files`: it runs under procd in about 2.5 MB of RAM, over an 802.11s interface on a USB Wi-Fi adapter, the LAN bridge and UDP. The package Makefile hasn't been built in the SDK yet.

### Building without the package

`scripts/cross-build.sh` builds `spectrameshd` with the C toolchain from your router's [OpenWRT SDK](https://downloads.openwrt.org/releases/), skipping the SDK's own Rust build, which takes an hour. Download and unpack the SDK for your router's target, then:

```
crates/spectramesh-wrt/scripts/cross-build.sh ~/openwrt-sdk-24.10.1-ramips-mt7621_gcc-13.3.0_musl.Linux-x86_64
```

It prints where the stripped binary landed (`target/openwrt/spectrameshd-<arch>`). MIPS routers need a nightly Rust with `rust-src` (`rustup toolchain install nightly --component rust-src`), because Rust has no prebuilt standard library for MIPS; ARM and x86 routers build with stable. CI builds one for `ramips/mt7621` on every push, as the `openwrt-ramips-mt7621` artifact, with the init script and config.

To install it by hand, copy the binary to `/usr/sbin/spectrameshd`, `files/spectramesh.init` to `/etc/init.d/spectramesh` and `files/spectramesh.config` to `/etc/config/spectramesh`, then configure it as below and run `/etc/init.d/spectramesh enable`. Use `scp -O` with OpenWRT's dropbear, which has no SFTP server.

### Building the package

Add this repository as a feed in your OpenWRT tree's `feeds.conf`. The `packages` feed is needed too, for Rust:

```
src-git spectramesh https://github.com/SpectraMeshCore/spectramesh.git
```

Then:

```
./scripts/feeds update -a
./scripts/feeds install spectramesh
make menuconfig    # Network > Routing and Redirection > spectramesh
make package/spectramesh/compile
```

The Makefile lives in `openwrt/` and builds from `main`. For release builds, pin `PKG_SOURCE_VERSION` to a commit.

### Configuring

Edit `/etc/config/spectramesh`. It ships disabled:

```
config spectramesh 'main'
	option enabled '1'
	list mesh_key 'smk1-...'

config hue
	option device 'eth1'

config hue
	option device 'phy1-mesh0'
	option kind 'wifi'
	option freq '5805'
	option bitrate '100M'
```

Set `option name` for the map, and `option web '192.168.1.1:8470'` to serve the map on the LAN. A UDP hue takes its address as `device`, with `option kind 'udp'` and a `list peer 'HOST:PORT'` for each peer. Add `option tun 'smesh0'` to the `main` section to carry IPv6 (it needs `kmod-tun`), and set `option ipv6_prefix` to keep addresses stable when the mesh key changes. Add `list channel_key 'smc1-...'` lines to join channels. Then `service spectramesh start`, and watch it with `logread -f`.

The TUN device isn't in any firewall zone until you add it. To control what can reach the router over the mesh, put `smesh0` in a zone: for example, an interface with `option device 'smesh0'` and `option proto 'none'` in `/etc/config/network`, added to a zone in `/etc/config/firewall`.

Make the mesh key with `spectrameshd --generate-mesh-key`, on the router or any other machine, and use the same key on every node. The init script copies the keys to a root-only file in RAM for the daemon. The node's own secret is created in `/etc/spectramesh/node.key` on first start; keep it in your backups to keep the node's ID.

### Wi-Fi mesh interfaces

SpectraMesh does the routing, so set up 802.11s with its own forwarding turned off. In `/etc/config/wireless`, with every node on the same channel:

```
config wifi-iface 'spectramesh_5g'
	option device 'radio1'
	option mode 'mesh'
	option mesh_id 'spectramesh'
	option mesh_fwding '0'
	option encryption 'sae'
	option key 'change-this-passphrase'
	option network 'spectramesh'
```

and in `/etc/config/network`, an interface that leaves the device unmanaged:

```
config interface 'spectramesh'
	option proto 'none'
```

USB adapters need their driver: a Netgear WNA1100 or other Atheros AR9271 adapter, for example, needs `kmod-ath9k-htc` (which brings its firmware), and supports mesh mode. Encrypted mesh needs a full `wpad` package (such as `wpad-mbedtls`) instead of the default `wpad-basic`. Find the device name with `ip link` after `wifi reload`; recent releases call it something like `phy1-mesh0`.

## How it's organized

| File | Role |
|---|---|
| `src/main.rs` | Opens each device and starts the daemon |
| `src/lib.rs` | The modules below, as a library, so they can be tested and fuzzed on their own |
| `src/config.rs` | Command-line options |
| `src/keys.rs` | The node's identity file, mesh key files, and random numbers from the kernel |
| `src/udp.rs` | UDP hues |
| `src/link.rs` | Raw Ethernet sockets. With `tun.rs`'s device setup and `keys.rs`'s call for random numbers, the only `unsafe` code in the project |
| `src/tun.rs` | The TUN device for IPv6, and mesh addresses |
| `src/provision.rs` | `--provision`: gives an ESP32 node its keys and name over USB |
| `src/control.rs` | The control socket, and the `--send`, `--listen` and `--channels` client commands |
| `src/daemon.rs` | One receive thread per hue and one for the TUN device, plus the router thread, which also learns neighbors' MAC addresses from frames the router has authenticated |
| `openwrt/` | The package Makefile, the procd init script and the default UCI config |
| `scripts/veth-demo.sh` | The four-node demo, with a ping over the mesh |
| `scripts/cross-build.sh` | Builds the daemon for a router with its OpenWRT SDK's toolchain |

It uses plain threads, with no async runtime, to keep the binary small for routers.

## Next steps

- **Routing a whole LAN's traffic**, so devices behind a router reach the mesh without running SpectraMesh themselves
- **Status over ubus**, and a LuCI page
- **Registering an EtherType** before a public release (`0x88B5` is reserved for experiments)
