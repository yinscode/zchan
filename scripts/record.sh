#!/usr/bin/env bash
# Records the two-minute demo video from a live run: the demo's terminal output and
# screenshots of the real pages (solver dashboard, point of sale, invoice hand-off, agent
# approval), captioned and joined into an MP4. Needs Google Chrome and ffmpeg.
#
#   scripts/record.sh                                         regtest (shielded run)
#   NETWORK=testnet ZCHAN_NODE_URL=<Zakura RPC> scripts/record.sh
#
# Writes $OUT (default video/<network>/): zchan-demo-<network>.mp4, frames and logs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NETWORK="${NETWORK:-regtest}"
OUT="${OUT:-$ROOT/video/$NETWORK}"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
SOLVER="http://127.0.0.1:7100"; A="http://127.0.0.1:7201"
rm -rf "$OUT"; mkdir -p "$OUT/shots" "$OUT/frames"
export ZCHAN_ADMIN_TOKEN="${ZCHAN_ADMIN_TOKEN:-$(openssl rand -hex 12)}"

json() { /usr/bin/python3 -c "import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))" "$1"; }
shot() { perl -e 'alarm shift; exec @ARGV' 45 "$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-dark-mode --window-size="${3:-1280,860}" \
  --virtual-time-budget=5000 --screenshot="$2" "$1" >/dev/null 2>&1; }
stop() { pkill -f "$ROOT/.demo/$NETWORK" 2>/dev/null || true; pkill -f "scripts/upstream.py 7400" 2>/dev/null || true; }
trap stop EXIT

KEEP=1 NETWORK="$NETWORK" "$ROOT/scripts/demo.sh" | tee "$OUT/demo.txt"
DATA="$ROOT/.demo/$NETWORK"
UI=$(grep -o 'http[^ ]*/t/[^ ]*' "$DATA/agent-1-ui.log")

# One unpaid request to the proxy: a single 402 offers x402 and MPP.
curl -s -D - -o /dev/null "http://127.0.0.1:7204/api/search?q=zcash" | tr -d '\r' \
  | grep -i -E "^HTTP|^payment-required|^www-authenticate" | cut -c1-118 > "$OUT/offer.txt"

INV=$(curl -s -X POST "$A/invoices" -H 'content-type: application/json' -d '{"amount":30000,"resource":"checkout:coffee"}' | json 'd["body"]["id"]')
INV_URL="$A/invoices/$INV"
shot "$SOLVER/status?still" "$OUT/shots/dashboard-1.png" 1280,1100
shot "$A/checkout?invoice=$INV" "$OUT/shots/checkout-waiting.png" 520,900
shot "$INV_URL" "$OUT/shots/handoff.png" 520,700
shot "${UI}pay?ref=$INV_URL" "$OUT/shots/approve.png" 520,800
QUOTE=$(curl -s -X POST "${UI}api/resolve" -H 'content-type: application/json' -d "{\"ref\":\"$INV_URL\"}" | json 'json.dumps(d["quote"])')
curl -s -X POST "${UI}api/approve" -H 'content-type: application/json' -d "{\"quote\":$QUOTE}" | json '"approved: "+json.dumps(d["ack"]["body"])' | tee -a "$OUT/demo.txt"
shot "$A/checkout?invoice=$INV" "$OUT/shots/checkout-paid.png" 520,900

STEP=settle NETWORK="$NETWORK" "$ROOT/scripts/demo.sh" | tee "$OUT/close.txt"
# Wait until the solver has seen every close.
for _ in $(seq 1 120); do
  open=$(curl -s "$SOLVER/status.json" | json 'sum(1 for k in ("agent_channels","merchant_channels") for c in d[k] if c["status"] not in ("closed","expired"))')
  [ "$open" = 0 ] && break
  sleep 10
done
shot "$SOLVER/status?still" "$OUT/shots/dashboard-2.png" 1280,1100
curl -s "$SOLVER/status.json" > "$OUT/solver-status.json"

/usr/bin/python3 "$ROOT/scripts/slides.py" "$OUT" "$NETWORK"
: > "$OUT/concat.txt"
n=0
while IFS=$'\t' read -r html secs; do
  png="$OUT/frames/$(printf %02d $n).png"
  shot "file://$html" "$png" 1920,1080
  printf "file '%s'\nduration %s\n" "$png" "$secs" >> "$OUT/concat.txt"
  last="$png"; n=$((n+1))
done < "$OUT/slides.txt"
printf "file '%s'\n" "$last" >> "$OUT/concat.txt"
ffmpeg -y -loglevel error -f concat -safe 0 -i "$OUT/concat.txt" -vf "fps=30,format=yuv420p" -t 120 -c:v libx264 -crf 20 "$OUT/zchan-demo-$NETWORK.mp4"
echo "video: $OUT/zchan-demo-$NETWORK.mp4"
