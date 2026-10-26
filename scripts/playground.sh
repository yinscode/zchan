#!/usr/bin/env bash
# The judge playground: solver, merchants and zchan-playground on $HOST:$PORT (default 8080).
#
#   scripts/playground.sh                     regtest (local; blocks are mined instantly)
#   ZCHAN_NODE_URL=<Zakura RPC> scripts/playground.sh testnet
#
# HOST=0.0.0.0 to serve beyond this machine (then put TLS in front). EXPLORER=<url
# prefix> adds explorer links to transactions.
set -euo pipefail
cd "$(dirname "$0")/.."
PLAYGROUND=1 NETWORK="${1:-regtest}" scripts/demo.sh
