#!/usr/bin/env bash
# The full demo on Zcash testnet. Needs ZCHAN_NODE_URL (a Zakura testnet JSON-RPC
# endpoint) and testnet funds at the solver's address, which the script prints and waits
# for. KEEP=1 leaves the services running instead of closing.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ZCHAN_NODE_URL:?set ZCHAN_NODE_URL to a Zakura testnet JSON-RPC endpoint}"
if [ "${KEEP:-0}" = 1 ]; then KEEP=1 NETWORK=testnet scripts/demo.sh; else CLOSE=1 NETWORK=testnet scripts/demo.sh; fi
