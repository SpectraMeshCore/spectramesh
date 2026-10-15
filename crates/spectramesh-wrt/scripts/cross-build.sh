#!/bin/sh
# Builds spectrameshd for an OpenWRT router with the C toolchain from that
# router's OpenWRT SDK, without building the SDK's own Rust: quicker than the
# package for trying a build on a router. See ../README.md, "Building without
# the package".
#
#   scripts/cross-build.sh SDK_DIR [CARGO_ARGS...]
#
# SDK_DIR is an unpacked SDK, such as openwrt-sdk-24.10.1-ramips-mt7621_...
# The stripped binary lands in target/openwrt/spectrameshd-<arch>.
#
# MIPS targets are tier 3 in Rust, without a prebuilt standard library, so
# they build it from source with a nightly toolchain (RUST_NIGHTLY, default
# "nightly", with the rust-src component).
set -eu

if [ $# -lt 1 ]; then
	echo "usage: $0 SDK_DIR [CARGO_ARGS...]" >&2
	exit 2
fi
sdk=$(cd "$1" && pwd)
shift

toolchain=
for dir in "$sdk"/staging_dir/toolchain-*; do
	toolchain=$dir
done
if [ ! -d "$toolchain" ]; then
	echo "$0: no staging_dir/toolchain-* in $sdk" >&2
	exit 1
fi
# toolchain-mipsel_24kc_gcc-13.3.0_musl: the arch is what precedes _gcc.
arch=${toolchain##*/toolchain-}
arch=${arch%%_gcc-*}

nightly=
case "$arch" in
mipsel_*) target=mipsel-unknown-linux-musl nightly=1 ;;
mips_*) target=mips-unknown-linux-musl nightly=1 ;;
aarch64_*) target=aarch64-unknown-linux-musl ;;
arm_*) target=armv7-unknown-linux-musleabihf ;;
x86_64) target=x86_64-unknown-linux-musl ;;
*)
	echo "$0: no Rust target known for OpenWRT arch $arch" >&2
	exit 1
	;;
esac

gcc=
for file in "$toolchain"/bin/*-openwrt-linux-musl*-gcc "$toolchain"/bin/*-openwrt-linux-gcc; do
	[ -x "$file" ] && gcc=$file && break
done
if [ -z "$gcc" ]; then
	echo "$0: no gcc in $toolchain/bin" >&2
	exit 1
fi

root=$(cd "$(dirname "$0")/../../.." && pwd)
env_target=$(echo "$target" | tr a-z- A-Z_)
export STAGING_DIR="$toolchain"
export "CARGO_TARGET_${env_target}_LINKER=$gcc"
# Link against the router's own musl, as OpenWRT's packages do.
export "CARGO_TARGET_${env_target}_RUSTFLAGS=-C link-self-contained=no -C target-feature=-crt-static"

cd "$root"
if [ -n "$nightly" ]; then
	cargo "+${RUST_NIGHTLY:-nightly}" build --release --locked -p spectramesh-wrt \
		--target "$target" -Zbuild-std=std,panic_abort "$@"
else
	cargo build --release --locked -p spectramesh-wrt --target "$target" "$@"
fi

target_dir=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$target_dir/openwrt"
out="$target_dir/openwrt/spectrameshd-$arch"
"${gcc%gcc}strip" -o "$out" "$target_dir/$target/release/spectrameshd"
echo "$out"
