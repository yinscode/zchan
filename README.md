# zchan

Payment channels for AI agents on Zcash. An agent funds **one** channel to a solver and pays any merchant through it. The solver keeps **one** channel to each merchant, and every agent's purchases from that merchant advance it. Merchants get paid over HTTP 402, as [x402 v2](docs/x402-zchannel.md) or [MPP](docs/mpp-zchannel.md), or through a QR checkout. In proxy mode a merchant puts an existing API behind payment without changing its code. No transaction is broadcast per payment.

```
agent-1 ──channel──▶ ┌────────┐ ──channel──▶ merchant-a
agent-2 ──channel──▶ │ solver │ ──channel──▶ merchant-b
```

Each channel is a unidirectional (Spilman) channel on a transparent Zcash v6 P2SH 2-of-2 output. Its time-locked refund is signed by both sides before the funding transaction is signed. Closes are enforced by the chain; the solver is trusted with one payment per agent at a time; the two legs are not atomic. Funded from Ironwood notes and closed cooperatively, a channel shows the chain only its capacity and its open and close ([docs/privacy.md](docs/privacy.md)). Read [docs/design.md](docs/design.md) for the flow and the full trust model.

**Try it:** [zchan.cash/play](https://zchan.cash/play) (Zcash testnet, no install) · **Docs:** [zchan.cash/docs](https://zchan.cash/docs/) · **Install:** `cargo install --locked --git https://github.com/yinscode/zchan zchan --bins`

## Status

| | |
|---|---|
| Channel layer (refund before funding, states, verifier, closes) | regtest gate passes ([gate0](docs/spikes/gate0-regtest.json)) |
| Solver, merchants, agents; idempotent payments, credits, crash recovery | regtest end-to-end test passes ([e2e](docs/spikes/e2e-regtest.json)) |
| x402 v2 `zchannel` route and client | regtest e2e: paid once; replayed, mismatched, forged, malformed and expired credentials stay unpaid |
| MPP `zchannel` charge (`WWW-Authenticate: Payment`) | regtest e2e: paid once; replayed, tampered, wrong-path and malformed credentials stay unpaid |
| Proxy mode (gate any HTTP API) | testnet run (x402 + MPP through the proxy) and regtest e2e: free paths pass through; priced paths paid over either protocol; payment headers not forwarded; an upstream failure releases the invoice |
| QR checkout and phone approval page | regtest e2e: altered, wrong-merchant, expired and reused invoices fail |
| Privacy: shielded funding from Ironwood notes, cooperative close paying both sides to Ironwood, fresh change per channel ([docs/privacy.md](docs/privacy.md)) | testnet runs (blocks 4,526,560–4,526,623: fundings with 0 transparent inputs; merchant and agent cooperative closes with 0 transparent outputs) and regtest e2e |
| Testnet run | **done** October 21, 2026: channels, x402 and QR payments, Ironwood and transparent closes, a refund ([docs/testnet.md](docs/testnet.md)) |
| Demo video | `scripts/record.sh` records it from a live run (testnet and regtest cuts made) |
| Mainnet | refused by the code |

## Demo

| | |
|---|---|
| Local, scripted (~1 min) | `scripts/demo-regtest.sh` |
| Local, live walkthrough | `KEEP=1 scripts/demo-regtest.sh` |
| Testnet | `ZCHAN_NODE_URL=<Zakura RPC> scripts/demo-testnet.sh` |
| **Judge playground**: anyone tries shield in → channel → x402/MPP payments → shield out with an agent of their own | `scripts/playground.sh [testnet]`, then open `http://<host>:8080/` |
| Two-minute video | `scripts/record.sh` |

Every demo funds channels from Ironwood notes, pays three merchants (one in proxy mode) over x402 and MPP, and closes cooperatively with both sides paid to Ironwood. Instructions, expected output and hosting notes: [docs/demo.md](docs/demo.md).

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
| `zchan-agent` | `deposit [--fresh]`, `shield`, `balance`, `send`, `open [--shielded]`, `close`, `get URL [--mpp]`, `pay-ref`, `resume`, `status`, `watch`, `serve` (approval page) |
| `zchan-playground` | the judge playground page (`docs/demo.md` §3) |
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
