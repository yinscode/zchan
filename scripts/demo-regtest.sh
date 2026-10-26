#!/usr/bin/env bash
# The full demo on a fresh local regtest chain: services, shielded channels, x402 and MPP
# purchases, then every channel closed cooperatively to Ironwood. KEEP=1 leaves the
# services running for the QR checkout instead of closing.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${KEEP:-0}" = 1 ]; then KEEP=1 NETWORK=regtest scripts/demo.sh; else CLOSE=1 NETWORK=regtest scripts/demo.sh; fi
