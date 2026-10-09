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

Each frame starts with a 2-byte length, because Ethernet pads short frames. Frames for a known neighbor are sent to its MAC address, which on Wi-Fi gets link-layer acknowledgements and retries. Everything else is broadcast.

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

### Channels

Channels carry encrypted group messages across the whole mesh. Make a key with `spectrameshd --generate-channel-key`, give it to every member, and list it in a file passed with `--channel-key-file` (one key per line). Every node relays channel messages, but only members can read them. For now the daemon logs the messages it receives; a local interface for programs to send and receive on channels is still to come.

## Trying it without hardware

`scripts/veth-demo.sh` runs four nodes, each in a throwaway network namespace of its own, joined by virtual Ethernet cables. Nodes 1 and 3 reach each other through node 2, and node 1 pings node 3's IPv6 address. Node 4 is plugged into node 2 too, but has a different mesh key. It needs no root.

```
cargo build -p spectramesh-wrt
crates/spectramesh-wrt/scripts/veth-demo.sh
```

```
[node 1 ping] 3 packets transmitted, 3 received, 0% packet loss, time 2003ms
[node 1] info:   !5aced5892ccbf39d via !9149fdc63f0b6b51 on a0, cost 2 us
[node 2] warning: dropped a frame on hue 2: frame tag doesn't verify with any mesh key
[node 4] info: node !8b6ff553f685778c: 0 neighbor links, 0 routes
```

Node IDs are random, because each run creates new keys.

The same scenario runs as a test: `cargo test -p spectramesh-wrt -- --ignored`.

## OpenWRT

> The package hasn't been built in the OpenWRT SDK or run on a router yet. Expect fixes.

### Building

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

Add `option tun 'smesh0'` to the `main` section to carry IPv6, and set `option ipv6_prefix` to keep addresses stable when the mesh key changes. Add `list channel_key 'smc1-...'` lines to join channels. Then `service spectramesh start`, and watch it with `logread -f`.

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

Encrypted mesh needs a full `wpad` package (such as `wpad-mbedtls`) instead of the default `wpad-basic`. Find the device name with `ip link` after `wifi reload`; recent releases call it something like `phy1-mesh0`.

## How it's organized

| File | Role |
|---|---|
| `src/main.rs` | Opens each device and starts the daemon |
| `src/config.rs` | Command-line options |
| `src/keys.rs` | The node's identity file, mesh key files, and random numbers from the kernel |
| `src/link.rs` | Raw Ethernet sockets. With `tun.rs`'s device setup and `keys.rs`'s call for random numbers, the only `unsafe` code in the project |
| `src/tun.rs` | The TUN device for IPv6, and mesh addresses |
| `src/daemon.rs` | One receive thread per hue and one for the TUN device, plus the router thread, which also learns neighbors' MAC addresses from frames the router has authenticated |
| `openwrt/` | The package Makefile, the procd init script and the default UCI config |
| `scripts/veth-demo.sh` | The four-node demo, with a ping over the mesh |

It uses plain threads, with no async runtime, to keep the binary small for routers.

## Next steps

- **A local interface** (such as a Unix socket) for programs to send and receive on channels
- **Routing a whole LAN's traffic**, so devices behind a router reach the mesh without running SpectraMesh themselves
- **A UDP hue**, for linking sites over the internet or networks SpectraMesh doesn't control
- **Status over ubus**, and a LuCI page
- **Registering an EtherType** before a public release (`0x88B5` is reserved for experiments)
