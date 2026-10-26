#!/usr/bin/env bash
# Starts one zchan service by name; systemd's zchan@NAME runs `run.sh NAME`.
# Same parties, ports and prices as `PLAYGROUND=1 NETWORK=testnet scripts/demo.sh`.
set -euo pipefail
: "${ZCHAN_NODE_URL:?set ZCHAN_NODE_URL in /etc/zchan/env}"
BIN=/opt/zchan/bin
DATA=/var/lib/zchan
SOLVER=http://127.0.0.1:7100
COMMON=(--node "$ZCHAN_NODE_URL" --poll-ms 15000 --close-margin 40)
case "${1:-}" in
  upstream) exec python3 /opt/zchan/upstream.py 7400 ;;
  solver) exec "$BIN/zchan-solver" --data "$DATA/solver" --listen 127.0.0.1:7100 --name solver --fee 100 "${COMMON[@]}" ;;
  merchant-a) exec "$BIN/zchan-merchant" --data "$DATA/merchant-a" --listen 127.0.0.1:7201 --url http://127.0.0.1:7201 \
    --name merchant-a --solver "$SOLVER" --price 5000 "${COMMON[@]}" ;;
  merchant-b) exec "$BIN/zchan-merchant" --data "$DATA/merchant-b" --listen 127.0.0.1:7202 \
    --name merchant-b --solver "$SOLVER" --price 4000 "${COMMON[@]}" ;;
  merchant-d) exec "$BIN/zchan-merchant" --data "$DATA/merchant-d" --listen 127.0.0.1:7204 \
    --name merchant-d --solver "$SOLVER" --upstream http://127.0.0.1:7400 --routes '/api/search=3000,/api/*=2000' "${COMMON[@]}" ;;
  # The playground checks for blocks every 3 s, not 15: judges are watching the wait.
  playground) exec "$BIN/zchan-playground" --data "$DATA/playground" --node "$ZCHAN_NODE_URL" --listen 127.0.0.1:8088 \
    --solver "$SOLVER" --merchants "merchant-a=http://127.0.0.1:7201,merchant-b=http://127.0.0.1:7202,merchant-d=http://127.0.0.1:7204" \
    --lock-blocks 2000 --poll-ms 3000 --close-margin 40 ${EXPLORER:+--explorer "$EXPLORER"} ;;
  *) echo "usage: run.sh upstream|solver|merchant-a|merchant-b|merchant-d|playground" >&2; exit 2 ;;
esac
