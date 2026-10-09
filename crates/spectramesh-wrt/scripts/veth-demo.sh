#!/bin/sh
# Runs spectrameshd nodes joined by virtual Ethernet cables, in a throwaway
# network namespace:
#
#   node 1 [a0] ---- [b0] node 2 [b1] ---- [c1] node 3
#                         node 2 [b2] ---- [d2] node 4 (outsider: wrong mesh key)
#
# Nodes 1 and 3 can only reach each other through node 2. Node 4 is plugged in
# but has a different mesh key, so node 2 drops everything it sends. Needs no
# root, only unprivileged user namespaces (enabled on most Linux distributions).
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

keys=$(mktemp -d)
trap 'rm -rf "$keys"' EXIT
(umask 077 && "$bin" --generate-mesh-key >"$keys/mesh.key" && "$bin" --generate-mesh-key >"$keys/other.key")

# Inside the namespace: show its own devices in /sys, wire up the veth pairs,
# run the nodes, then stop them.
unshare --user --map-root-user --net --mount sh -eu -c '
	bin=$1 seconds=$2 keys=$3
	mount -t sysfs sysfs /sys
	ip link add a0 type veth peer name b0
	ip link add b1 type veth peer name c1
	ip link add b2 type veth peer name d2
	for dev in a0 b0 b1 c1 b2 d2; do ip link set "$dev" up; done

	node() {
		name=$1 key=$2
		shift 2
		"$bin" --identity "$keys/$name.id" --mesh-key-file "$keys/$key" \
			--report-interval "$((seconds - 1))" "$@" 2>&1 | sed "s/^/[$name] /" &
	}
	node "node 1" mesh.key --hue a0
	node "node 2" mesh.key --hue b0 --hue b1 --hue b2
	node "node 3" mesh.key --hue c1
	node "node 4" other.key --hue d2

	sleep "$seconds"
	pkill -x spectrameshd
	wait
' demo "$bin" "$seconds" "$keys"
