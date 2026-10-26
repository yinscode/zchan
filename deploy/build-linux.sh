#!/usr/bin/env bash
# Builds static x86_64 Linux binaries of the zchan services into deploy/dist/, in Docker.
# Static (musl) binaries run on any Linux droplet with no Rust or libraries installed.
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLCHAIN=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
docker run --rm --platform linux/amd64 \
  -v "$PWD":/src -v zchan-cargo-registry:/usr/local/cargo/registry -v zchan-linux-target:/target \
  -e CARGO_TARGET_DIR=/target -w /src "rust:$TOOLCHAIN" bash -ec '
    apt-get update -qq && apt-get install -y -qq musl-tools >/dev/null
    rustup target add x86_64-unknown-linux-musl
    cargo build --release --locked --target x86_64-unknown-linux-musl \
      --bin zchan-solver --bin zchan-merchant --bin zchan-agent --bin zchan-playground
    mkdir -p deploy/dist
    cp /target/x86_64-unknown-linux-musl/release/zchan-{solver,merchant,agent,playground} deploy/dist/'
ls -la deploy/dist
