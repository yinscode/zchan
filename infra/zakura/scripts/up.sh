#!/usr/bin/env bash
# Starts a fresh zchan regtest node (ephemeral state: a restart is a new chain).
set -euo pipefail
source "$(dirname "$0")/lib.sh"
"${COMPOSE[@]}" up -d
wait_rpc
echo "zchan-zakura RPC: $RPC_URL height=$(rpc getblockcount)"
