# Demoing zchan

There are four ways to show zchan, from a five-minute local run to a page judges use themselves.

| Way | Command | Needs | Time |
|---|---|---|---|
| Local demo, scripted | `scripts/demo-regtest.sh` | Docker, Rust | ~1 min |
| Local demo, live walkthrough | `KEEP=1 scripts/demo-regtest.sh` | same | 5 min |
| Testnet demo | `scripts/demo-testnet.sh` | a Zakura testnet RPC, ~0.015 TAZ | 15–25 min (block times) |
| Judge playground | `scripts/playground.sh [testnet]` | same as the network chosen | judges: ~4 min each |
| Recorded video | `scripts/record.sh` (regtest) or `NETWORK=testnet scripts/record.sh` | Chrome, ffmpeg | as above |

Common setup: Rust 1.97.1 (pinned), Docker (regtest node), `python3`, `openssl`. Build once with `cargo build --release`. Set `CARGO_TARGET_DIR` if you build elsewhere.

## What every demo runs

| Party | Port | Role |
|---|---|---|
| solver | 7100 | payee of agent channels, payer of merchant channels; dashboard at `/status` |
| merchant-a | 7201 | demo resources (`/paid/…`), QR checkout at `/checkout` |
| merchant-b | 7202 | demo resources |
| merchant-d | 7204 | **proxy mode** in front of `scripts/upstream.py` (port 7400): `/api/search`, `/api/echo` |
| agent-1, agent-2 | (CLI) | one channel each to the solver |

By default every channel is funded from Ironwood notes and closed cooperatively, with both sides paid to Ironwood (`SHIELDED=0` for the transparent variant). Each funding and close is printed with its transparent inputs and outputs.

## 1. Local demo (regtest)

```bash
scripts/demo-regtest.sh
```

It starts a fresh regtest node, funds and shields every party, opens five channels, makes five purchases, and closes everything. The purchases are:
- agent-1 → A and B over x402;
- agent-2 → A over x402;
- agent-2 → D (proxied) over MPP;
- agent-1 → D (proxied, POST) over MPP.

Expected output, abridged:

```
== channels, funded from Ironwood notes
  solver → merchant-a        2ccfe1d887e0…  block 217  transparent in 0 · out [200000]
  …
== purchases: no transaction per payment
  agent-2 → D proxy (MPP)    200  {"q": "zcash"}
  …
== settle: every channel closes cooperatively
  merchant-a closes          36a55c2daa13…  block 222  transparent in 1 · out none
  …
```

### Live walkthrough

```bash
KEEP=1 scripts/demo-regtest.sh
```

Leave the services running and walk through them:

1. **Funding lines.** Every channel was funded from shielded notes: `transparent in 0`.
2. **One 402, two protocols.** Run `curl -si 'http://127.0.0.1:7204/api/search?q=zcash'`. The response carries both `PAYMENT-REQUIRED` (x402 v2) and `WWW-Authenticate: Payment …` (MPP) for the same invoice.
3. **Dashboard.** At `http://127.0.0.1:7100/status`: one agent paid three merchants, two agents share the channel to A, and every channel shows reconciled ✓.
4. **QR checkout.** Open `http://127.0.0.1:7201/checkout?amount=30000&item=coffee`. Scan it with a phone (start with `HOST=<LAN IP>` so the phone can reach it). Paste the printed agent link into the invoice page, approve, and the point of sale shows Paid.
5. **Settle.** Run `STEP=settle scripts/demo.sh`. Every close shows `transparent out none`.

## 2. Testnet demo

```bash
ZCHAN_NODE_URL=<Zakura testnet RPC> scripts/demo-testnet.sh
```

On the first run:
1. The script prints the solver's address and waits for at least 0.015 TAZ.
2. The solver shields its coins.
3. The solver funds the agents with `send-shielded`, so no agent needs funds of its own.

Seeds go to `.demo/testnet/<party>/seed` (mode 600); back them up. Every open and close waits for a block. The txids to cite are printed, and [testnet.md](testnet.md) lists earlier runs.

## 3. Judge playground

```bash
scripts/playground.sh
```

```bash
ZCHAN_NODE_URL=<Zakura testnet RPC> HOST=0.0.0.0 EXPLORER=<tx URL prefix> scripts/playground.sh testnet
```

The first command runs on regtest. The second runs on testnet, reachable from other machines.

This starts the solver and the three merchants. Prices are low (2,000–5,000 zat a call) and each merchant channel holds 1,000,000 zat, so many judges can pay. The script funds the house wallet:
- **regtest:** from the miner.
- **testnet:** 2,000,000 zat sent shielded by the solver.

It then serves the playground on port 8080 (`PORT=` to change it). Each visitor gets an agent of their own, and the page walks through four steps:

| Step | What the judge does | What the page shows |
|---|---|---|
| 1 · Shield in | one click: the house sends 160,000 zat as an Ironwood note | the amount received and the balance; "Chain sees: nothing" |
| 2 · Open a channel | one click: a channel of up to 100,000 zat funded from that note | the channel size and the block the safety refund unlocks; "Chain sees: only the channel size" |
| 3 · Pay merchants | pick x402 or MPP for each of four calls (two proxied, one POST) | each payment's trace in milliseconds: 402 offers, quote, signed state, merchant receipt, retry, 200 with `PAYMENT-RESPONSE` or `Payment-Receipt`; the "On-chain payments" counter stays 0 |
| 4 · Shield out | one click, after more than 10,000 zat of payments | the change note, found with the agent's own viewing key; "Chain sees: nothing about who got what" |

Each on-chain step links its transaction in small type underneath; opens and closes each wait for one block (Zcash targets 75 s; testnet blocks recently averaged about 25 s), and the playground checks for it every 3 s.

**Operating notes:**
- **Limits:** one grant per session and 100 sessions (`--max-sessions`). Sessions survive a restart, and a session can be reopened with `/?session=<id>`.
- **House wallet:** it lives in `.demo/<network>/playground/house`. Top it up with `zchan-agent send-shielded "$(zchan-agent shielded-address --data .demo/<network>/playground/house)" ZAT --data .demo/<network>/solver …`.
- **Public hosting:**
  - Put TLS in front of port 8080, for example with a reverse proxy.
  - The merchants and the solver can stay on localhost; only the playground needs to be public.
  - Each judge's payments cost the house the grant (160,000 zat) plus fees. 2,000,000 zat serves about 12 judges; send more for more.

### Serving it from a laptop

A Cloudflare quick tunnel gives a public HTTPS link with no account or open ports. The playground stays bound to localhost:

```bash
PORT=8088 ZCHAN_NODE_URL=<Zakura testnet RPC> scripts/playground.sh testnet
```

```bash
cloudflared tunnel --url http://127.0.0.1:8088
```

cloudflared prints a `https://….trycloudflare.com` link. If `~/.cloudflared/config.yml` exists (from another tunnel), its routing rules apply to the quick tunnel too, and every request gets an empty 404. Pass an empty config instead: `cloudflared tunnel --config /path/to/empty.yml --url http://127.0.0.1:8088`. The machine must stay awake and online while judges use it. A quick tunnel's link changes each time cloudflared restarts; for a fixed name, use a named tunnel on a Cloudflare account.

**First run against a busy network:** each wallet reads every block since its starting height once. Testnet adds about 11 Ironwood commitments per block, so a wallet funded days ago takes a while to catch up over a remote RPC. Blocks are fetched 16 at a time. After that, each step reads only new blocks.

## 4. Recorded video

```bash
scripts/record.sh
```

```bash
NETWORK=testnet ZCHAN_NODE_URL=<Zakura testnet RPC> scripts/record.sh
```

This runs the demo, takes screenshots of the real pages, and writes a captioned two-minute MP4 to `video/<network>/zchan-demo-<network>.mp4`. The storyboard and captions are in `scripts/slides.py`.

## 5. Failure paths (for technical judges)

```bash
infra/zakura/scripts/down.sh; infra/zakura/scripts/up.sh
```

```bash
cargo test --release -p zchan --test e2e -- --ignored --nocapture
```

About 50 seconds. It kills the solver mid-payment, takes a merchant offline, replays and forges credentials, lets a channel refund at its lock height, and checks the books after each step.
