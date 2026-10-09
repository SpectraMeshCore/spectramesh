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

List more than one key in the mesh key file, one per line. Nodes send with the first and accept all of them. To change keys without downtime: add the new key as a second line everywhere, then move it to the first line everywhere, then remove the old one.

## Trying it without hardware

`scripts/veth-demo.sh` runs four nodes in a throwaway network namespace, joined by virtual Ethernet cables. Nodes 1 and 3 reach each other through node 2. Node 4 is plugged into node 2 too, but has a different mesh key. It needs no root.

```
cargo build -p spectramesh-wrt
crates/spectramesh-wrt/scripts/veth-demo.sh
```

```
[node 1] info:   !56fdc458df50ac8b via !2be57b5a26be4364 on a0, cost 2 us
[node 2] warning: dropped a frame on hue 2: frame tag doesn't verify with any mesh key
[node 4] info: node !db72f1c99a57484f: 0 neighbor links, 0 routes
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

Then `service spectramesh start`, and watch it with `logread -f`.

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
| `src/link.rs` | Raw Ethernet sockets. With `keys.rs`'s call for random numbers, the only `unsafe` code in the project |
| `src/daemon.rs` | One receive thread per hue, plus the router thread, which also learns neighbors' MAC addresses from frames the router has authenticated |
| `openwrt/` | The package Makefile, the procd init script and the default UCI config |
| `scripts/veth-demo.sh` | The four-node demo |

It uses plain threads, with no async runtime, to keep the binary small for routers.

## Next steps

- **Carrying IP traffic**, through a TUN device, so the mesh is useful to the rest of the router (today, mesh data is only logged)
- **A UDP hue**, for linking sites over the internet or networks SpectraMesh doesn't control
- **Status over ubus**, and a LuCI page
- **Registering an EtherType** before a public release (`0x88B5` is reserved for experiments)
