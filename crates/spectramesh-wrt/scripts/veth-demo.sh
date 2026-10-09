#!/bin/sh
# Runs spectrameshd nodes, each in its own network namespace, joined by
# virtual Ethernet cables:
#
#   node 1 [a0] ---- [b0] node 2 [b1] ==== [c1] node 3
#                         node 2 [b2] ---- [d2] node 4 (outsider: wrong mesh key)
#
# Nodes 1 and 3 can only reach each other through node 2. The node 2 - node 3
# cable (====) is set up like a small radio link, carrying at most 250 bytes
# per frame. Each member node carries IPv6 through a TUN device, and node 1
# pings node 3's mesh address with small and 1,200-byte packets; the packets
# cross node 2 encrypted end to end, and the large ones are fragmented on the
# small link. Node 1 also pings an address no node has, which the mesh
# answers with "no route". Node 4 has a different mesh key, so node 2 drops
# everything it sends.
#
# Needs no root, only unprivileged user namespaces (enabled on most Linux
# distributions).
#
# Usage: veth-demo.sh [SECONDS]   (default 15)
# Set SPECTRAMESHD to use a binary other than the workspace's debug build.
set -eu

seconds=${1:-15}
bin=${SPECTRAMESHD:-$(cd "$(dirname "$0")/../../.." && pwd)/target/debug/spectrameshd}
if [ ! -x "$bin" ]; then
	echo "build first: cargo build -p spectramesh-wrt" >&2
	exit 1
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
(umask 077 && "$bin" --generate-mesh-key >"$dir/mesh.key" && "$bin" --generate-mesh-key >"$dir/other.key")
# Create node 3's identity now, so node 1 knows which address to ping.
address() {
	"$bin" --node-info --identity "$dir/$1.id" --mesh-key-file "$dir/mesh.key" |
		sed -n 's/^IPv6 address: *//p'
}
target=$(address "node 3")
# An address in the mesh that no running node has.
nowhere=$(address nobody)

unshare --user --map-root-user --net sh -eu -c '
	bin=$1 dir=$2 seconds=$3 target=$4 nowhere=$5

	# Starts a node in a network namespace of its own. It waits for its
	# cables, then runs the daemon, and pings `ping` if that is set.
	node() {
		name=$1 key=$2 ping=$3
		shift 3
		unshare --net --mount sh -eu -c "
			name=\$1 bin=\$2 dir=\$3 key=\$4 seconds=\$5 ping=\$6 nowhere=\$7
			shift 7
			mount -t sysfs sysfs /sys
			ip link set lo up
			hues=
			for hue in \"\$@\"; do
				dev=\${hue%%,*}
				while [ ! -e /sys/class/net/\$dev ]; do sleep 0.1; done
				ip link set \$dev up
				hues=\"\$hues --hue \$hue\"
			done
			\"\$bin\" --identity \"\$dir/\$name.id\" --mesh-key-file \"\$dir/\$key\" --tun smesh0 \
				--report-interval \$((seconds - 1)) \$hues 2>&1 | sed \"s/^/[\$name] /\" &
			if [ -n \"\$ping\" ]; then
				sleep \$((seconds - 9))
				ping -6 -c 3 -W 2 \$ping 2>&1 | sed \"s/^/[\$name ping] /\" || true
				ping -6 -c 3 -W 2 -s 1200 \$ping 2>&1 | sed \"s/^/[\$name big ping] /\" || true
				ping -6 -c 1 -W 2 \$nowhere 2>&1 | sed \"s/^/[\$name nowhere ping] /\" || true
			fi
			wait
		" node "$name" "$bin" "$dir" "$key" "$seconds" "$ping" "$nowhere" "$@" &
		pids="${pids:+$pids,}$!"
		eval "pid_$(echo "$name" | tr -d " ")=$!"
	}
	pids=
	# If anything below fails, stop the nodes rather than leave them waiting.
	trap '"'"'[ -z "$pids" ] || kill $(echo "$pids" | tr , " ") 2>/dev/null'"'"' EXIT
	node "node 1" mesh.key "$target" a0
	node "node 2" mesh.key "" b0 b1,mtu=250 b2
	node "node 3" mesh.key "" c1,mtu=250
	node "node 4" other.key "" d2

	# Wait for each node to be in its own namespace, then plug in the cables.
	here=$(readlink /proc/self/ns/net)
	for pid in $(echo "$pids" | tr , " "); do
		while [ "$(readlink /proc/$pid/ns/net)" = "$here" ]; do sleep 0.1; done
	done
	cable() {
		ip link add "$1" type veth peer name "$3"
		ip link set "$1" netns "$2"
		ip link set "$3" netns "$4"
	}
	cable a0 "$pid_node1" b0 "$pid_node2"
	cable b1 "$pid_node2" c1 "$pid_node3"
	cable b2 "$pid_node2" d2 "$pid_node4"

	sleep "$seconds"
	pkill -x -P "$pids" spectrameshd || true
	wait
	pids=
' demo "$bin" "$dir" "$seconds" "$target" "$nowhere"
