# Quickstart

Three ways in, from no install to your own API behind payment.

## 1. Try it in the browser

Open the [Zchan Playground](https://165-22-48-127.sslip.io/play). You get an agent of your own on Zcash testnet:
1. **Shield in:** the house wallet sends your agent 160,000 zat as an Ironwood note.
2. **Open:** your agent opens one channel to the solver.
3. **Pay:** pay three merchants over x402 or MPP and watch each payment's trace.
4. **Shield out:** close cooperatively, with both sides paid to Ironwood.

Opening and closing wait for testnet blocks. Payments don't.

## 2. Run the whole stack locally

Needs Rust 1.97.1 (pinned by `rust-toolchain.toml`), Docker for the regtest node, `python3` and `openssl`.

```bash
cargo build --release
```

```bash
KEEP=1 scripts/demo-regtest.sh
```

This starts a regtest node, then funds and shields every party. It opens the channels, makes five purchases and leaves the services running:

| Party | Address | Role |
|---|---|---|
| solver | `http://127.0.0.1:7100` | payee of agent channels, payer of merchant channels; dashboard at `/status` |
| merchant-a | `http://127.0.0.1:7201` | demo resources (`/paid/…`), QR checkout at `/checkout` |
| merchant-b | `http://127.0.0.1:7202` | demo resources |
| merchant-d | `http://127.0.0.1:7204` | proxy mode in front of a demo API: `/api/search`, `/api/echo` |

## 3. Pay an API from an agent

Every agent command takes the agent's data directory, the node and the solver. For the local demo's agent-1:

```bash
A="--data .demo/regtest/agent-1 --node http://127.0.0.1:18272/ --solver http://127.0.0.1:7100 --dev-seed agent-1"
```

Pay over x402 v2 and fetch:

```bash
target/release/zchan-agent get 'http://127.0.0.1:7204/api/search?q=zcash' $A
```

The same over MPP, with a POST:

```bash
target/release/zchan-agent get http://127.0.0.1:7204/api/echo --mpp --method POST --body hello $A
```

`get` answers the 402, gets a quote from the solver, signs the channel's next state and retries with the merchant's receipt. It prints the response.

Settle the channel, with both sides paid to Ironwood:

```bash
target/release/zchan-agent close $A
```

A fresh agent opens its own channel first, funded from shielded notes:

```bash
target/release/zchan-agent open --capacity 100000 --lock-blocks 2000 --shielded $A
```

The other agent commands (`balance`, `shield`, `status`, `watch`, `serve` and more) are listed by `zchan-agent` itself. [Agents](#agents) covers asking a person for a budget, price caps, and instructions to give an agent.

## 4. Put your API behind payment

Proxy mode needs no change to your service:

```bash
zchan-merchant --name my-api --upstream http://localhost:3000 \
  --routes '/api/search=3000,/api/*=500' \
  --solver http://127.0.0.1:7100 --data .demo/regtest/my-api \
  --node http://127.0.0.1:18272/ --listen 127.0.0.1:7205 --admin-token "$TOKEN"
```

- **Prices:** per path in zatoshis. `/api/*` matches by prefix; unpriced paths pass through free.
- **Requests:** your service sees them without payment headers.
- **Errors:** an upstream 5xx releases the invoice, so the client can retry.

The solver opens a channel to the new merchant when asked (`POST /admin/merchants` with the admin token; [Run the demo](#demo) shows the call).

## Next

- [Agents](#agents): budgets, price caps and instructions for an agent.
- [Protocol](#design): the flow, the channel states and the trust model.
- [x402 binding](#x402-zchannel) and [MPP binding](#mpp-zchannel): the wire formats.
- [Security](#security): who sees what, and what the chain enforces.
- [Run the demo](#demo) and [Self-hosting](#deploy): testnet runs and servers.
