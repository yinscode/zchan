# zchan

Payment channels for AI agents on Zcash. An agent funds **one** channel to a solver and pays any merchant through it. The solver keeps **one** channel to each merchant, and every agent's purchases from that merchant advance it. Merchants get paid over HTTP 402, as [x402 v2](docs/x402-zchannel.md) or [MPP](docs/mpp-zchannel.md), or through a QR checkout. In proxy mode a merchant puts an existing API behind payment without changing its code. No transaction is broadcast per payment.

```
agent-1 ──channel──▶ ┌────────┐ ──channel──▶ merchant-a
agent-2 ──channel──▶ │ solver │ ──channel──▶ merchant-b
```

Each channel is a unidirectional (Spilman) channel on a transparent Zcash v6 P2SH 2-of-2 output. Its time-locked refund is signed by both sides before the funding transaction is signed. Closes are enforced by the chain; the solver is trusted with one payment per agent at a time; the two legs are not atomic. Read [docs/design.md](docs/design.md) for the flow and the full trust model.

## Status

| | |
|---|---|
| Channel layer (refund before funding, states, verifier, closes) | regtest gate passes ([gate0](docs/spikes/gate0-regtest.json)) |
| Solver, merchants, agents; idempotent payments, credits, crash recovery | regtest end-to-end test passes ([e2e](docs/spikes/e2e-regtest.json)) |
| x402 v2 `zchannel` route and client | regtest e2e: paid once; replayed, mismatched, forged, malformed and expired credentials stay unpaid |
| MPP `zchannel` charge (`WWW-Authenticate: Payment`) | regtest e2e: paid once; replayed, tampered, wrong-path and malformed credentials stay unpaid |
| Proxy mode (gate any HTTP API) | testnet run (x402 + MPP through the proxy) and regtest e2e: free paths pass through; priced paths paid over either protocol; payment headers not forwarded; an upstream failure releases the invoice |
| QR checkout and phone approval page | regtest e2e: altered, wrong-merchant, expired and reused invoices fail |
| Cooperative close to Ironwood (shielded merchant payout) | regtest e2e |
| Testnet run | **done** October 21, 2026: channels, x402 and QR payments, Ironwood and transparent closes, a refund ([docs/testnet.md](docs/testnet.md)) |
| Demo video | `scripts/record.sh` records it from a live run (testnet and regtest cuts made) |
| Mainnet | refused by the code |

## Run the demo

Needs Rust 1.97.1 (pinned in `rust-toolchain.toml`), Docker, and `python3` for the script's JSON parsing.

```bash
KEEP=1 CLOSE=1 scripts/demo.sh
```

The script:
1. starts a fresh Zakura regtest node, the solver, and merchants A and B;
2. funds everyone, and opens the solver's channels to A and B and the agents' channels to the solver;
3. has agent-1 buy from A and B through its one channel, and agent-2 buy from A;
4. prints the reconciled levels.

With `KEEP=1` the services keep running and it prints the dashboard, POS and approval-page links. With `CLOSE=1` it settles every channel: A cooperatively to Ironwood, B and the agent channels transparently. For testnet see [docs/testnet.md](docs/testnet.md).

| Page | URL |
|---|---|
| Solver dashboard (channels, payments, reconciliation, events) | `http://127.0.0.1:7100/status` |
| Merchant A point of sale (QR checkout) | `http://127.0.0.1:7201/checkout?amount=3000&item=coffee` |
| Agent approval page (open it from the merchant's invoice page on a phone) | printed by the script |

For a phone on the same network, run with `HOST=<your LAN IP>`.

## Proxy mode

Put an existing HTTP service behind payment:

```bash
zchan-merchant --name my-api --solver http://127.0.0.1:7100 --upstream http://127.0.0.1:3000 --routes '/api/search=3000,/api/*=500' --listen 0.0.0.0:7204 --url https://my-api.example --admin-token "$TOKEN" --data ./data/my-api --node "$ZCHAN_NODE_URL"
```

Requests that match no merchant route go to the upstream service:
- **Priced paths** (first match wins; a trailing `*` matches a prefix) answer 402 until paid, over x402 or MPP.
- **Other paths** pass through free.

The merchant's own paths take precedence over the upstream's: `/info`, `/channels/…`, `/invoices…`, `/checkout`, `/paid/…`, `/status`, `/admin/…`. Payment headers are stripped before forwarding. A service that listens beyond localhost must set `--admin-token`; `/admin/*` then requires `X-Admin-Token`.

An agent pays and fetches with:

```bash
zchan-agent get https://my-api.example/api/search?q=zec --solver http://127.0.0.1:7100 --data ./data/agent
```

Add `--mpp` to pay over MPP, and `--method POST --body '…'` for other methods.

## Tests

```bash
cargo test --release --workspace
```

```bash
infra/zakura/scripts/down.sh; infra/zakura/scripts/up.sh
```

```bash
cargo test --release -p zchan --test regtest -- --ignored --nocapture
```

```bash
infra/zakura/scripts/down.sh; infra/zakura/scripts/up.sh
```

```bash
cargo test --release -p zchan --test e2e -- --ignored --nocapture
```

The two regtest suites each need a fresh chain. The e2e test runs the real `zchan-solver` and `zchan-merchant` binaries as processes; it kills and restarts them, injects a solver crash between signing and acknowledgment, and takes a merchant offline mid-payment. Set `ZCHAN_RESULTS=<path>` to write a suite's log.

## Binaries

| Binary | Role |
|---|---|
| `zchan-solver` | payee of agent channels, payer of merchant channels; `/quote`, `/pay`, `/status`, `/admin/merchants` |
| `zchan-merchant` | payee of the solver's channel; x402 routes `/paid/{resource}`, `/checkout`, `/invoices/{id}` |
| `zchan-agent` | `deposit`, `open`, `get URL` (x402), `pay-ref`, `resume`, `status`, `watch`, `serve` (approval page) |
| `zchan-dev` | regtest: `fund ADDRESS ZAT`, `mine N`, `tip` |

All take `--data DIR --node URL`, plus `--dev-seed LABEL` on regtest. Without a dev seed, a random seed is written to `DIR/seed`.

## Layout

| Path | What |
|---|---|
| `crates/zchan-tx` | v6 transactions through pczt, Zakura JSON-RPC, network parameters, seed wallet, refund watcher |
| `crates/zchan` | channels, signed messages, SQLite store, the three services, x402 |
| `infra/zakura` | regtest node (Zakura 1.6.0; NU7 at height 200) |
| `site/index.html` | landing page (static, no build step) |
| `docs/plan.md` | plan and schedule · `docs/design.md` design and trust model · `docs/x402-zchannel.md` scheme |
