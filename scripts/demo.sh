#!/usr/bin/env bash
# The zchan demo: one solver; merchants A and B, and D in proxy mode in front of an
# existing API (scripts/upstream.py); agents 1 and 2.
#
#   scripts/demo.sh                          regtest: fresh node, everything funded and mined
#   NETWORK=testnet ZCHAN_NODE_URL=<Zakura RPC> scripts/demo.sh
#                                            testnet: fund the solver's printed address
#
# PLAYGROUND=1 instead starts the judge playground (zchan-playground) on $HOST:8080: the
# solver and merchants with cheap prices and large channels, and a funded house wallet;
# judges bring no funds. It keeps running.
#
# SHIELDED=1 (default) funds every channel from Ironwood notes and closes every channel
# cooperatively with both sides paid to Ironwood; SHIELDED=0 uses transparent coins.
# KEEP=1 leaves the services running after the purchases (for the QR checkout and the
# video); CLOSE=1 settles every channel at the end; STEP=settle settles the channels of
# services left running by KEEP=1. Data goes to $DATA (default .demo/<network>), logs to
# $DATA/*.log.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NETWORK="${NETWORK:-regtest}"
SHIELDED="${SHIELDED:-1}"
PLAYGROUND="${PLAYGROUND:-0}"
[ "$PLAYGROUND" = 1 ] && KEEP=1 && SHIELDED=1
if [ "$PLAYGROUND" = 1 ]; then
  PRICE_A=5000; PRICE_B=4000; ROUTES_D='/api/search=3000,/api/*=2000'; CAP_MERCHANT=1000000
else
  PRICE_A=20000; PRICE_B=15000; ROUTES_D='/api/search=12000,/api/*=8000'; CAP_MERCHANT=0
fi
DATA="${DATA:-$ROOT/.demo/$NETWORK}"
BIN="${CARGO_TARGET_DIR:-$ROOT/target}/release"
HOST="${HOST:-127.0.0.1}"
SOLVER="http://127.0.0.1:7100"; A="http://$HOST:7201"; B="http://127.0.0.1:7202"; D="http://127.0.0.1:7204"

(cd "$ROOT" && cargo build --release -q -p zchan)

if [ "$NETWORK" = regtest ]; then
  export ZCHAN_NODE_URL="http://127.0.0.1:18272/"
  if [ "${STEP:-all}" != settle ]; then
    "$ROOT/infra/zakura/scripts/down.sh" >/dev/null 2>&1 || true
    "$ROOT/infra/zakura/scripts/up.sh" >/dev/null
    rm -rf "$DATA"
    "$BIN/zchan-dev" mine 210 >/dev/null
  fi
  COMMON=(--automine --poll-ms 500)
  SEED() { echo --dev-seed "$1"; }
  LOCK_MERCHANT=400; LOCK_AGENT=300
else
  : "${ZCHAN_NODE_URL:?set ZCHAN_NODE_URL to a Zakura testnet JSON-RPC endpoint}"
  COMMON=(--poll-ms 15000 --close-margin 40)
  SEED() { :; }
  LOCK_MERCHANT=3000; LOCK_AGENT=2000
fi
mkdir -p "$DATA"
# Admin routes need this token (required when a service listens beyond localhost).
# The token is kept in $DATA so STEP=settle can reach services started earlier.
[ -f "$DATA/admin-token" ] && [ "${STEP:-all}" = settle ] && ZCHAN_ADMIN_TOKEN="$(cat "$DATA/admin-token")"
export ZCHAN_ADMIN_TOKEN="${ZCHAN_ADMIN_TOKEN:-$(openssl rand -hex 12)}"
echo "$ZCHAN_ADMIN_TOKEN" > "$DATA/admin-token"; chmod 600 "$DATA/admin-token"
NODE=(--node "$ZCHAN_NODE_URL")
ADMIN=(-H "x-admin-token: $ZCHAN_ADMIN_TOKEN")
SH=$([ "$SHIELDED" = 1 ] && echo true || echo false)

json() { /usr/bin/python3 -c "
import json,sys
raw=sys.stdin.read()
try:
    d=json.loads(raw); print(eval(sys.argv[1]))
except Exception:
    sys.exit('unexpected answer: '+raw[:400])" "$1"; }
rpc() { curl -s -m 20 -H content-type:application/json --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":$2}" "$ZCHAN_NODE_URL"; }
# One line per transaction: its transparent inputs and outputs.
txline() {
  for _ in $(seq 1 80); do
    out=$(rpc getrawtransaction "[\"$2\",1]" | json '"%s" % (d["result"]["height"],) if d["result"].get("height") else ""' 2>/dev/null || true)
    [ -n "$out" ] && break; sleep 15
  done
  rpc getrawtransaction "[\"$2\",1]" | json '"  %-26s %s…  block %s  transparent in %d · out %s" % ("'"$1"'", d["result"]["txid"][:12], d["result"]["height"], len(d["result"]["vin"]), [o["valueZat"] for o in d["result"]["vout"]] or "none")'
}
up() { for _ in $(seq 1 120); do curl -sf "$1/info" >/dev/null && return 0; sleep 0.5; done; echo "$1 not up" >&2; exit 1; }
party() {
  local who=$1 dir="$DATA/$1"; shift
  [ "$who" = house ] && dir="$DATA/playground/house"
  "$BIN/zchan-agent" "$@" --data "$dir" "${NODE[@]}" --solver "$SOLVER" --name "$who" "${COMMON[@]}" $(SEED "$who")
}
settle() {
  echo "== settle: every channel closes cooperatively"
  for m in "merchant-a $A" "merchant-b $B" "merchant-d $D"; do
    set -- $m
    id=$(curl -s "$SOLVER/status.json" | json '[x["channel_id"] for x in d["merchants"] if x["id"]=="'$1'"][-1]')
    t=$(curl -s "${ADMIN[@]}" -X POST "$2/admin/channels/$id/coop-close" | json 'd["txid"]')
    txline "$1 closes" "$t"
  done
  for a in agent-1 agent-2; do
    t=$(party $a close)
    txline "$a closes" "$t"
  done
}
# STEP=settle settles the channels of services already running (from KEEP=1).
if [ "${STEP:-all}" = settle ]; then settle; exit 0; fi

PIDS=()
cleanup() { [ "${KEEP:-0}" = 1 ] || kill "${PIDS[@]}" 2>/dev/null || true; }
trap cleanup EXIT

python3 "$ROOT/scripts/upstream.py" 7400 >"$DATA/upstream.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-solver" --data "$DATA/solver" "${NODE[@]}" --listen 127.0.0.1:7100 --name solver --fee 100 "${COMMON[@]}" $(SEED solver) >"$DATA/solver.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-merchant" --data "$DATA/merchant-a" "${NODE[@]}" --listen "$HOST:7201" --url "$A" --name merchant-a --solver "$SOLVER" --price "$PRICE_A" "${COMMON[@]}" $(SEED merchant-a) >"$DATA/merchant-a.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-merchant" --data "$DATA/merchant-b" "${NODE[@]}" --listen 127.0.0.1:7202 --name merchant-b --solver "$SOLVER" --price "$PRICE_B" "${COMMON[@]}" $(SEED merchant-b) >"$DATA/merchant-b.log" 2>&1 & PIDS+=($!)
"$BIN/zchan-merchant" --data "$DATA/merchant-d" "${NODE[@]}" --listen 127.0.0.1:7204 --name merchant-d --solver "$SOLVER" \
  --upstream http://127.0.0.1:7400 --routes "$ROUTES_D" "${COMMON[@]}" $(SEED merchant-d) >"$DATA/merchant-d.log" 2>&1 & PIDS+=($!)
up "$SOLVER"; up "http://127.0.0.1:7201"; up "$B"; up "$D"

SOLVER_DEPOSIT=$(curl -s "$SOLVER/info" | json 'd["deposit"]')

echo "== funding"
if [ "$NETWORK" = regtest ]; then
  "$BIN/zchan-dev" fund "$SOLVER_DEPOSIT" 8000000 >/dev/null
  AGENTS=(agent-1 agent-2); [ "$PLAYGROUND" = 1 ] && AGENTS=(house)
  for a in "${AGENTS[@]}"; do "$BIN/zchan-dev" fund "$(party $a deposit)" 3000000 >/dev/null; done
  if [ "$SHIELDED" = 1 ]; then
    curl -sf "${ADMIN[@]}" -X POST "$SOLVER/admin/shield" | json '"  solver shields its coins   "+d["txid"][:12]+"…"'
    for a in "${AGENTS[@]}"; do echo "  $a shields its coins  $(party $a shield | cut -c1-12)…"; done
  fi
elif [ "$SHIELDED" = 1 ]; then
  until [ "$(party solver balance | json 'd["shielded"]+d["transparent"]')" -ge 1500000 ]; do
    echo "Fund the solver with at least 0.015 TAZ: $SOLVER_DEPOSIT"; sleep 30
  done
  if [ "$(party solver balance | json 'd["transparent"]')" -gt 0 ]; then
    curl -sf "${ADMIN[@]}" -X POST "$SOLVER/admin/shield" | json '"  solver shields its coins   "+d["txid"][:12]+"…"'
  fi
  if [ "$PLAYGROUND" = 1 ]; then
    if [ "$(party house balance | json 'd["shielded"]')" -lt 1000000 ]; then
      echo "  solver → house, shielded   $(party solver send-shielded "$(party house shielded-address)" 2000000 | cut -c1-12)…"
    fi
  else
    for a in agent-1 agent-2; do
      if [ "$(party $a balance | json 'd["shielded"]')" -lt 200000 ]; then
        echo "  solver → $a, shielded      $(party solver send-shielded "$(party $a shielded-address)" 300000 | cut -c1-12)…"
      fi
    done
  fi
else
  echo "Fund these testnet addresses, then wait for one confirmation:"
  echo "  solver   $SOLVER_DEPOSIT   ≥ 0.0050 TAZ"
  echo "  agent-1  $(party agent-1 deposit)   ≥ 0.0015 TAZ"
  echo "  agent-2  $(party agent-2 deposit)   ≥ 0.0015 TAZ"
  until [ "$(party solver balance | json 'd["transparent"]')" -ge 500000 ]; do sleep 20; done
fi

echo "== channels$([ "$SHIELDED" = 1 ] && echo ', funded from Ironwood notes')"
for m in "merchant-a $A 200000" "merchant-b $B 100000" "merchant-d $D 100000"; do
  set -- $m
  [ "$CAP_MERCHANT" -gt 0 ] && set -- "$1" "$2" "$CAP_MERCHANT"
  ch=$(curl -sf "${ADMIN[@]}" -X POST "$SOLVER/admin/merchants" -H 'content-type: application/json' \
    -d "{\"url\":\"$2\",\"capacity\":$3,\"lock_blocks\":$LOCK_MERCHANT,\"shielded\":$SH}" | json 'd["channel_id"]')
  txline "solver → $1" "${ch%%:*}"
done
if [ "$PLAYGROUND" = 1 ]; then
  "$BIN/zchan-playground" --data "$DATA/playground" "${NODE[@]}" --listen "$HOST:8080" --solver "$SOLVER" \
    --merchants "merchant-a=$A,merchant-b=$B,merchant-d=$D" --lock-blocks "$LOCK_AGENT" "${COMMON[@]}" \
    $([ "$NETWORK" = regtest ] && echo --house-dev-seed house) ${EXPLORER:+--explorer "$EXPLORER"} >"$DATA/playground.log" 2>&1 & PIDS+=($!)
  for _ in $(seq 1 60); do curl -sf "http://$HOST:8080/api/info" >/dev/null && break; sleep 1; done
  echo "== playground running: http://$HOST:8080/"
  exit 0
fi
for a in agent-1 agent-2; do
  ch=$(party $a open --capacity 100000 --lock-blocks "$LOCK_AGENT" $([ "$SHIELDED" = 1 ] && echo --shielded) | json 'd["id"]')
  txline "$a → solver" "${ch%%:*}"
done

echo "== purchases: no transaction per payment"
paid() { json '"  %-26s %s  %s" % ("'"$1"'", d["status"], d["body"]["data"] if "data" in d["body"] else json.dumps({k: v for k, v in d["body"].items() if k != "headers"}))'; }
party agent-1 get "$A/paid/weather" | paid "agent-1 → A (x402)"
party agent-1 get "$B/paid/news" | paid "agent-1 → B (x402)"
party agent-2 get "$A/paid/weather" | paid "agent-2 → A (x402)"
party agent-2 get "$D/api/search?q=zcash" --mpp | paid "agent-2 → D proxy (MPP)"
party agent-1 get "$D/api/echo" --mpp --method POST --body "hello from agent-1" | paid "agent-1 → D proxy (MPP)"
curl -s "$SOLVER/status.json" | json '"\n".join("  %-11s %-8s level %6d  reconciled %s" % (r["peer"], r["side"], r["cumulative"], r["ok"]) for r in d["reconcile"] if r["channel"] in {c["id"] for k in ("agent_channels","merchant_channels") for c in d[k] if c["status"]=="open"})'

if [ "${KEEP:-0}" = 1 ]; then
  party agent-1 serve --listen "$HOST:7300" --url "http://$HOST:7300" >"$DATA/agent-1-ui.log" 2>&1 & PIDS+=($!)
  sleep 1
  echo "== running (KEEP=1)"
  echo "  solver dashboard   $SOLVER/status"
  echo "  merchant-a POS     $A/checkout?amount=30000&item=coffee"
  echo "  agent-1 approvals  $(grep -o 'http[^ ]*/t/[^ ]*' "$DATA/agent-1-ui.log")"
fi

[ "${CLOSE:-0}" = 1 ] && settle
echo "done · logs in $DATA"
