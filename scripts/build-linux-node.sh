#!/usr/bin/env bash
# build-linux-node.sh
#
# Native Linux build of mossraven-node (x86_64 musl, statically linked).
#
# Run this from a Linux machine or WSL. This is the recommended path —
# cross-compiling LuaJIT from Windows is fragile (needs zig + GNU make +
# ar/strip shims; documented below for reference).
#
# Prereqs (Ubuntu/Debian):
#   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # if rust not installed
#   rustup target add x86_64-unknown-linux-musl
#   sudo apt-get install -y musl-tools build-essential
#
# Usage:
#   ./scripts/build-linux-node.sh
#   -> produces dist/mossraven-node-linux-x86_64

set -euo pipefail

cd "$(dirname "$0")/.."

export CC_x86_64_unknown_linux_musl=musl-gcc
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc

cargo build -p mossraven-node \
    --release \
    --target x86_64-unknown-linux-musl

mkdir -p dist
cp target/x86_64-unknown-linux-musl/release/mossraven-node dist/mossraven-node-linux-x86_64
chmod +x dist/mossraven-node-linux-x86_64

echo
echo "Built: dist/mossraven-node-linux-x86_64"
file dist/mossraven-node-linux-x86_64 || true
ls -lh dist/mossraven-node-linux-x86_64

#
# WINDOWS CROSS-COMPILE NOTE (experimental, not used by default):
#
# `cargo zigbuild` with `pip install ziglang` works for plain Rust crates,
# but `mlua` with the `luajit` + `vendored` features pulls in luajit-src,
# whose build script calls Linux toolchain binaries (`ar`, `strip`, `make`)
# directly via `which::which`. zig provides `zig ar` / `zig cc` / `zig
# objcopy` and the cargo-zigbuild wrapper handles `cc` and `cxx` — but `ar`,
# `strip`, and especially `make` need their own shims/installs.
#
# scripts/ar.bat and scripts/strip.bat shim the first two. The remaining
# gap is `make`: install via `choco install make` and re-run, or just use
# WSL/Linux. Net: WSL is ~5 minutes to set up and 100% reliable. Save the
# zig adventure for crates with simpler build scripts.
