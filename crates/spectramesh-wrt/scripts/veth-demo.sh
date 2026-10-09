#!/bin/sh
# Runs three spectrameshd nodes joined by virtual Ethernet cables, in a
# throwaway network namespace:
#
#   node 1 [a0] ---- [b0] node 2 [b1] ---- [c1] node 3
#
# Nodes 1 and 3 can only reach each other through node 2. Needs no root, only
# unprivileged user namespaces (enabled on most Linux distributions).
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

# Inside the namespace: show its own devices in /sys, wire up the veth pairs,
# run the nodes, then stop them.
exec unshare --user --map-root-user --net --mount sh -eu -c '
	bin=$1 seconds=$2
	mount -t sysfs sysfs /sys
	ip link add a0 type veth peer name b0
	ip link add b1 type veth peer name c1
	for dev in a0 b0 b1 c1; do ip link set "$dev" up; done

	report=$((seconds - 1))
	"$bin" --node-id 00000001 --hue a0 --report-interval "$report" 2>&1 | sed "s/^/[node 1] /" &
	"$bin" --node-id 00000002 --hue b0 --hue b1 --report-interval "$report" 2>&1 | sed "s/^/[node 2] /" &
	"$bin" --node-id 00000003 --hue c1 --report-interval "$report" 2>&1 | sed "s/^/[node 3] /" &

	sleep "$seconds"
	pkill -x spectrameshd
	wait
' demo "$bin" "$seconds"
