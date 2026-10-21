#!/usr/bin/env bash
# The two-merchant demo: one solver, merchants A and B, agents 1 and 2.
#
#   scripts/demo.sh                          regtest: fresh node, everything funded and mined
#   NETWORK=testnet ZCHAN_NODE_URL=<Zakura RPC> scripts/demo.sh
#                                            testnet: fund the printed deposit addresses
#
# KEEP=1 leaves the services running after the purchases (for the QR checkout and the
# video); CLOSE=1 settles every channel on chain at the end. Data goes to $DATA
# (default .demo/<network>), logs to $DATA/*.log.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NETWORK="${NETWORK:-regtest}"
DATA="${DATA:-$ROOT/.demo/$NETWORK}"
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/release"
HOST="${HOST:-127.0.0.1}"
SOLVER="http://127.0.0.1:7100"; A="http://127.0.0.1:7201"; B="http://127.0.0.1:7202"

(cd "$ROOT" && cargo build --release -q -p zchan)

if [ "$NETWORK" = regtest ]; then
  "$ROOT/infra/zakura/scripts/down.sh" >/dev/null 2>&1 || true
  "$ROOT/infra/zakura/scripts/up.sh"
  export ZCHAN_NODE_URL="http://127.0.0.1:18272/"
  rm -rf "$DATA"
  "$BIN/zchan-dev" mine 210 >/dev/null
  COMMON=(--automine --poll-ms 500)
  SEED() { echo --dev-seed "$1"; }
  LOCK_MERCHANT=400; LOCK_AGENT=300; MARGIN=10
else
  : "${ZCHAN_NODE_URL:?set ZCHAN_NODE_URL to a Zakura testnet JSON-RPC endpoint}"
  COMMON=(--poll-ms 15000 --close-margin 40)
  SEED() { :; }
  LOCK_MERCHANT=3000; LOCK_AGENT=2000; MARGIN=40
fi
mkdir -p "$DATA"
# Admin routes need this token (required when a service listens beyond localhost).
export ZCHAN_ADMIN_TOKEN="${ZCHAN_ADMIN_TOKEN:-$(openssl rand -hex 12)}"
NODE=(--node "$ZCHAN_NODE_URL")

json() { /usr/bin/python3 -c "
import json,sys
raw=sys.stdin.read()
try:
    d=json.loads(raw); print(eval(sys.argv[1]))
except Exception:
    sys.exit('unexpected answer: '+raw[:400])" "$1"; }
up() { for _ in $(seq 1 120); do curl -sf "$1/info" >/dev/null && return 0; sleep 0.5; done; echo "$1 not up" >&2; exit 1; }
PIDS=()
cleanup() { [ "${KEEP:-0}" = 1 ] || kill "${PIDS[@]}" 2>/dev/null || true; }
trap cleanup EXIT

"$BIN/zchan-solver" --data "$DATA/solver" "${NODE[@]}" --listen 127.0.0.1:7100 --name solver --fee 100 "${COMMON[@]}" $(SEED solver) >"$DATA/solver.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-merchant" --data "$DATA/merchant-a" "${NODE[@]}" --listen "$HOST:7201" --url "http://$HOST:7201" --name merchant-a --solver "$SOLVER" --price 20000 "${COMMON[@]}" $(SEED merchant-a) >"$DATA/merchant-a.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-merchant" --data "$DATA/merchant-b" "${NODE[@]}" --listen 127.0.0.1:7202 --name merchant-b --solver "$SOLVER" --price 5000 "${COMMON[@]}" $(SEED merchant-b) >"$DATA/merchant-b.log" 2>&1 & PIDS+=($!)
up "$SOLVER"; up "http://127.0.0.1:7201"; up "$B"

agent() { local who=$1; shift; "$BIN/zchan-agent" "$@" --data "$DATA/$who" "${NODE[@]}" --solver "$SOLVER" --name "$who" "${COMMON[@]}" $(SEED "$who"); }
SOLVER_DEPOSIT=$(curl -s "$SOLVER/info" | json 'd["deposit"]')
A1_DEPOSIT=$(agent agent-1 deposit); A2_DEPOSIT=$(agent agent-2 deposit)

if [ "$NETWORK" = regtest ]; then
  "$BIN/zchan-dev" fund "$SOLVER_DEPOSIT" 5000000 >/dev/null
  "$BIN/zchan-dev" fund "$A1_DEPOSIT" 2000000 >/dev/null
  "$BIN/zchan-dev" fund "$A2_DEPOSIT" 1000000 >/dev/null
else
  echo "Fund these testnet addresses (TAZ), then wait for one confirmation:"
  echo "  solver   $SOLVER_DEPOSIT   ≥ 0.0040 TAZ"
  echo "  agent-1  $A1_DEPOSIT   ≥ 0.0012 TAZ"
  echo "  agent-2  $A2_DEPOSIT   ≥ 0.0006 TAZ"
  until [ "$(curl -s "$SOLVER/status.json" | json 'd["info"]["balance"]')" -ge 400000 ]; do sleep 20; done
fi

echo "== solver opens a channel to each merchant"
curl -sf -H "x-admin-token: $ZCHAN_ADMIN_TOKEN" -X POST "$SOLVER/admin/merchants" -H 'content-type: application/json' -d "{\"url\":\"$A\",\"capacity\":200000,\"lock_blocks\":$LOCK_MERCHANT}" | json '"  merchant-a  "+d["channel_id"]'
curl -sf -H "x-admin-token: $ZCHAN_ADMIN_TOKEN" -X POST "$SOLVER/admin/merchants" -H 'content-type: application/json' -d "{\"url\":\"$B\",\"capacity\":100000,\"lock_blocks\":$LOCK_MERCHANT}" | json '"  merchant-b  "+d["channel_id"]'
echo "== each agent opens one channel to the solver"
agent agent-1 open --capacity 100000 --lock-blocks "$LOCK_AGENT" | json '"  agent-1  "+d["id"]'
agent agent-2 open --capacity 50000 --lock-blocks "$LOCK_AGENT" | json '"  agent-2  "+d["id"]'

echo "== agent-1 buys from A and B through its one channel; agent-2 buys from A"
agent agent-1 get "$A/paid/weather" | json '"  agent-1 → A  "+str(d["status"])+"  "+d["body"]["data"]'
agent agent-1 get "$B/paid/news"    | json '"  agent-1 → B  "+str(d["status"])+"  "+d["body"]["data"]'
agent agent-2 get "$A/paid/weather" | json '"  agent-2 → A  "+str(d["status"])+"  "+d["body"]["data"]'
curl -s "$SOLVER/status.json" | json '"\n".join("  %-10s %-8s level %6d  reconciled %s" % (r["peer"], r["side"], r["cumulative"], r["ok"]) for r in d["reconcile"])'

if [ "${KEEP:-0}" = 1 ]; then
  agent agent-1 serve --listen "$HOST:7300" --url "http://$HOST:7300" >"$DATA/agent-1-ui.log" 2>&1 & PIDS+=($!)
  sleep 1
  echo "== running (KEEP=1)"
  echo "  solver dashboard   $SOLVER/status"
  echo "  merchant-a POS     http://$HOST:7201/checkout?amount=3000&item=coffee"
  echo "  agent-1 approvals  $(grep -o 'http[^ ]*/t/[^ ]*' "$DATA/agent-1-ui.log")"
fi

if [ "${CLOSE:-0}" = 1 ]; then
  echo "== settle: A closes to Ironwood, B closes transparently, the solver closes agent channels"
  ID_A=$(curl -s "$SOLVER/status.json" | json '[m["channel_id"] for m in d["merchants"] if m["id"]=="merchant-a"][0]')
  ID_B=$(curl -s "$SOLVER/status.json" | json '[m["channel_id"] for m in d["merchants"] if m["id"]=="merchant-b"][0]')
  curl -s -H "x-admin-token: $ZCHAN_ADMIN_TOKEN" -X POST "$A/admin/channels/$ID_A/coop-close" | json '"  merchant-a coop close  "+d["txid"]'
  curl -s -H "x-admin-token: $ZCHAN_ADMIN_TOKEN" -X POST "$B/admin/channels/$ID_B/close" | json '"  merchant-b close       "+d["txid"]'
  for id in $(curl -s "$SOLVER/status.json" | json '" ".join(c["id"] for c in d["agent_channels"])'); do
    curl -s -H "x-admin-token: $ZCHAN_ADMIN_TOKEN" -X POST "$SOLVER/admin/channels/$id/close" | json '"  agent channel close   "+d["txid"]'
  done
fi
echo "done · logs in $DATA"
