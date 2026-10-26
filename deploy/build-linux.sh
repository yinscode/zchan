#!/usr/bin/env bash
# Builds x86_64 Linux binaries of the zchan services into deploy/dist/.
# With cargo-zigbuild installed (`brew install zig && cargo install cargo-zigbuild`) it
# cross-compiles natively; otherwise it cross-compiles in a Docker container. Either way
# the binaries link glibc 2.36 or older, so they run on Debian 12+ and Ubuntu 23.04+.
set -euo pipefail
cd "$(dirname "$0")/.."
BINS=(--bin zchan-solver --bin zchan-merchant --bin zchan-agent --bin zchan-playground)
mkdir -p deploy/dist
if command -v cargo-zigbuild >/dev/null && command -v zig >/dev/null; then
  rustup target add x86_64-unknown-linux-gnu >/dev/null
  cargo zigbuild --release --locked --target x86_64-unknown-linux-gnu.2.36 "${BINS[@]}"
  T="${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release"
  cp "$T"/zchan-{solver,merchant,agent,playground} deploy/dist/
else
  TOOLCHAIN=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
  docker run --rm \
    -v "$PWD":/src -v zchan-cargo-registry:/usr/local/cargo/registry -v zchan-linux-target:/target \
    -e CARGO_TARGET_DIR=/target -w /src \
    -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    -e CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc -e AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
    "rust:$TOOLCHAIN-bookworm" bash -ec "
      if [ \"\$(uname -m)\" != x86_64 ]; then
        apt-get update -qq && apt-get install -y -qq gcc-x86-64-linux-gnu libc6-dev-amd64-cross >/dev/null
      fi
      rustup target add x86_64-unknown-linux-gnu
      cargo build --release --locked --target x86_64-unknown-linux-gnu ${BINS[*]}
      cp /target/x86_64-unknown-linux-gnu/release/zchan-{solver,merchant,agent,playground} deploy/dist/"
fi
ls -la deploy/dist
