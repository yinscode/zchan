# Zchan

Payment channels on Zcash for AI agents and real-time commerce. An agent funds **one** channel and pays any merchant through it in milliseconds, over [x402 v2](docs/x402-zchannel.md) or [MPP](docs/mpp-zchannel.md). No transaction is broadcast per payment, and channels are funded from and settled to shielded Ironwood notes.

**[Playground](https://zchan.cash/play)** (try it on testnet, no install) · **[Docs](https://zchan.cash/docs/)** · **[Status](https://zchan.cash/status/)**

```
agent-1 ──channel──▶ ┌────────┐ ──channel──▶ merchant-a
agent-2 ──channel──▶ │ solver │ ──channel──▶ merchant-b
                     └────────┘ ──channel──▶ merchant-d (proxy in front of any HTTP API)
```

- **Fast.** A payment is a newly signed channel state, not a transaction: 17 ms median on testnet, no block wait, no network fee. Two transactions settle any number of payments.
- **Private.** Funded from Ironwood notes and closed cooperatively, a channel shows the chain only its capacity and its open and close. Not the source of funds, the split, or any payment ([privacy](docs/privacy.md)).
- **Safe.** Each channel is a Spilman channel on a v6 P2SH 2-of-2. Its time-locked refund is signed before the funding transaction, so if the solver disappears the agent takes its balance back at the lock height ([security](docs/security.md)).
- **Bounded.** Agents pay from a CLI or an MCP server, under a price cap and a budget the model cannot raise.

Testnet only; mainnet is refused by the code.

## Install

Two commands cover most uses: `zchan-agent` pays, `zchan-merchant` charges. Update Rust first (`rustup update stable`; tested with 1.97.1), then:

```bash
cargo install --locked --git https://github.com/yinscode/zchan zchan --bin zchan-agent --bin zchan-merchant
```

They land in `~/.cargo/bin`. Use `--bins` instead to also install `zchan-solver`, `zchan-playground` and `zchan-dev`. There are no prebuilt binaries yet.

Every command talks to a Zcash node over JSON-RPC (`--node`). For a local chain, clone the repository and run the regtest stack (needs Docker, `python3` and `openssl`):

```bash
git clone https://github.com/yinscode/zchan && cd zchan
```

```bash
KEEP=1 scripts/demo-regtest.sh
```

This builds everything, starts a regtest node, funds and shields every party, opens the channels, makes a few purchases and leaves the services running:

| Party | Address | Role |
|---|---|---|
| solver | `http://127.0.0.1:7100` | payee of agent channels, payer of merchant channels; dashboard at `/status` |
| merchant-a, merchant-b | `http://127.0.0.1:7201`, `:7202` | demo resources at `/paid/…`; QR checkout at `7201/checkout` |
| merchant-d | `http://127.0.0.1:7204` | proxy mode in front of a demo API: `/api/search`, `/api/echo` |
| agent-1, agent-2 | `.demo/regtest/agent-1`, `agent-2` | agents with open channels |

The examples below use that stack. The hosted solver at zchan.cash serves the playground only; it is not open to outside agents yet.

## Pay as an agent

Every `zchan-agent` command takes the agent's data directory, the node and the solver. A shell function keeps them in one place (it works in bash and zsh). For the local demo's agent-1:

```bash
agent() { zchan-agent "$@" --data .demo/regtest/agent-1 --node http://127.0.0.1:18272/ --solver http://127.0.0.1:7100 --dev-seed agent-1; }
```

Pay a 402 and fetch, over x402 or MPP, refusing any price above a cap:

```bash
agent get 'http://127.0.0.1:7204/api/search?q=zcash' --max-price 20000
```

```bash
agent get http://127.0.0.1:7204/api/echo --mpp --method POST --body hello --max-price 20000
```

`get` answers the 402, gets a quote from the solver, signs the channel's next state and retries with the receipt. Then settle, with both sides paid to Ironwood:

```bash
agent close
```

A new agent starts by asking its person for funds, then opens a channel:

| Step | Command |
|---|---|
| Ask for a budget | `agent request 120000 --message "API budget"` prints a ZIP 321 link any shielded wallet opens |
| Check it arrived | `agent balance` |
| Open | `agent open --capacity 100000 --lock-blocks 2000 --shielded` |
| Keep refunds safe | `agent watch` broadcasts the refund if the solver disappears |
| Give the rest back | `agent send-shielded <unified address> <zat>` after `close` |

`zchan-agent` with no arguments lists every command. [docs/agents.md](docs/agents.md) has the full flow and a system prompt for agents.

## Charge as a merchant

Proxy mode puts an existing HTTP API behind payment without changing its code. With your API on port 3000:

```bash
zchan-merchant --name my-api --upstream http://127.0.0.1:3000 --routes '/api/search=3000,/api/*=500' \
  --solver http://127.0.0.1:7100 --listen 127.0.0.1:7205 --admin-token "$TOKEN" \
  --data .demo/regtest/my-api --node http://127.0.0.1:18272/
```

Clients call port 7205 instead of 3000:
- **Priced paths** answer 402 until paid, over x402 v2 or MPP. Prices are in zatoshis; the first match wins and a trailing `*` matches a prefix. Query strings don't change the price.
- **Other paths** pass through free. Payment headers are stripped before forwarding, and an upstream 5xx releases the invoice so the client can retry.
- **Admin:** a merchant that listens beyond localhost must set `--admin-token`; `/admin/*` then requires `X-Admin-Token`. The merchant's own paths (`/info`, `/channels/…`, `/invoices…`, `/checkout`, `/paid/…`, `/status`, `/admin/…`) take precedence over the upstream's.

The merchant is paid on a channel from the solver. Ask the solver to open one (the demo's token is in `.demo/regtest/admin-token`):

```bash
curl -X POST http://127.0.0.1:7100/admin/merchants -H "X-Admin-Token: $(cat .demo/regtest/admin-token)" \
  -H 'content-type: application/json' -d '{"url":"http://127.0.0.1:7205","capacity":200000,"lock_blocks":400,"shielded":true}'
```

Without an upstream, `zchan-merchant --price ZAT` sells demo resources at `/paid/{name}` and runs a QR checkout for counters and vending at `/checkout`. The [Quickstart](docs/quickstart.md) walks through an Express API behind proxy mode.

## Use it from an assistant (MCP)

`zchan-agent mcp` serves the agent's wallet as an MCP server over stdio, so Claude Code, Claude Desktop, Cursor and other MCP clients can pay for HTTP resources without a shell. Add it to Claude Code with the local demo's agent-2, whose channel has about 50,000 zat left after the demo. Use absolute paths, since the client starts the server from its own directory:

```bash
claude mcp add zchan -- zchan-agent mcp --max-price 20000 --budget 100000 \
  --data "$PWD/.demo/regtest/agent-2" --node http://127.0.0.1:18272/ --solver http://127.0.0.1:7100 --dev-seed agent-2 --automine
```

Or in any client's `mcpServers` config:

```json
{
  "mcpServers": {
    "zchan": {
      "command": "zchan-agent",
      "args": ["mcp", "--max-price", "20000", "--budget", "100000",
               "--data", "/path/to/zchan/.demo/regtest/agent-2", "--node", "http://127.0.0.1:18272/",
               "--solver", "http://127.0.0.1:7100", "--dev-seed", "agent-2", "--automine"]
    }
  }
}
```

Then ask the assistant something like *"Fetch http://127.0.0.1:7204/api/search?q=zcash and summarize it."* It calls `zchan_fetch`, which pays the 402 and returns the response. When the channel runs low, it can close it with `zchan_close` and open a new one with `zchan_open`.

| Tool | What it does |
|---|---|
| `zchan_fetch` | fetches a URL; on a `zchannel` 402 (x402 or MPP), pays within the caps and returns the paid response |
| `zchan_status` | the channel's capacity, paid and remaining; the shielded balance; this session's spending |
| `zchan_request_funds` | a ZIP 321 payment link for the person to fund the agent |
| `zchan_open` | opens a channel from the agent's shielded funds and waits for its block |
| `zchan_close` | settles the channel, both sides paid to Ironwood |

The person who starts the server sets the limits, not the model:
- **`--max-price`** (default 5,000 zat) caps one request's price. A tool call can pass a lower `max_price`, never a higher one.
- **`--budget`** (default 50,000 zat) caps what the server spends, solver fees included, until it restarts.
- **`--lock-blocks`** (default 2,000) sets the refund lock of channels it opens.

Text on a web page cannot raise these, so a prompt injection can spend at most the budget. The server also runs the refund watcher while it is up. A good default is to allow `zchan_fetch` and keep approving `zchan_open` and `zchan_close` yourself. `--automine` is for regtest only: it mines a block whenever a transaction is sent, so `zchan_open` and `zchan_close` finish at once. On testnet, drop it and `--dev-seed`, and point `--node` at a testnet node and `--solver` at a testnet solver.

## How it works

An agent opens one channel to the solver. To pay, it asks the merchant for a price (an HTTP 402), gets a quote from the solver, and signs the next channel state for the price plus the solver's fee. The solver advances its own channel to that merchant by the price, and the merchant serves the request. Each channel is closed cooperatively at the end, or refunded at its lock height if the other side disappears.

The solver is trusted with one payment per agent in flight: the agent and merchant legs are not atomic yet (hash locks are planned). The solver sees which agent paid which merchant; the chain does not. Refunds and unilateral closes are transparent. [docs/design.md](docs/design.md) has the flow, the channel states and the full trust model.

## Status

| | |
|---|---|
| Channel layer (refund before funding, states, verifier, closes) | regtest gate passes ([gate0](docs/spikes/gate0-regtest.json)) |
| Solver, merchants, agents; idempotent payments, credits, crash recovery | regtest end-to-end test passes ([e2e](docs/spikes/e2e-regtest.json)) |
| x402 v2 and MPP `zchannel` | regtest e2e: paid once; replayed, tampered, mismatched, forged, malformed and expired credentials stay unpaid |
| Proxy mode | testnet run over x402 and MPP, and regtest e2e |
| QR checkout and phone approval page | regtest e2e: altered, wrong-merchant, expired and reused invoices fail |
| Shielded funding and closes | testnet: fundings with 0 transparent inputs, cooperative closes with 0 transparent outputs |
| MCP server | testnet: paid fetches over x402 and MPP, cap and budget refusals, a shielded close |
| Hosted playground | [zchan.cash/play](https://zchan.cash/play), every step a testnet transaction |
| Mainnet | refused by the code |

Every testnet run, with its transaction ids, is in [docs/testnet.md](docs/testnet.md).

## Demos and tests

| | |
|---|---|
| Local, scripted (~1 min) | `scripts/demo-regtest.sh` |
| Local, services left running | `KEEP=1 scripts/demo-regtest.sh` |
| Testnet | `ZCHAN_NODE_URL=<testnet RPC> scripts/demo-testnet.sh` |
| Your own playground | `scripts/playground.sh [testnet]`, then open `http://<host>:8080/` ([hosting](docs/deploy.md)) |

```bash
cargo test --release --workspace
```

The regtest suites each need a fresh chain:

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

The e2e test runs the real `zchan-solver` and `zchan-merchant` binaries as processes. It kills and restarts them, injects a solver crash between signing and acknowledgment, and takes a merchant offline mid-payment. Set `ZCHAN_RESULTS=<path>` to write a suite's log.

## Binaries

| Binary | Role |
|---|---|
| `zchan-agent` | the payer: `request`, `balance`, `open`, `get`, `pay-ref`, `close`, `send-shielded`, `status`, `watch`, `serve` (QR approval page), `mcp` |
| `zchan-merchant` | the payee: proxy mode (`--upstream`, `--routes`), demo resources (`--price`), QR checkout |
| `zchan-solver` | payee of agent channels, payer of merchant channels; `/quote`, `/pay`, `/status`, `/admin/merchants` |
| `zchan-playground` | the hosted try-it page |
| `zchan-dev` | regtest helpers: `fund ADDRESS ZAT`, `mine N`, `tip` |

All take `--data DIR --node URL`, plus `--dev-seed LABEL` on regtest. Without a dev seed, a random seed is written to `DIR/seed`.

## Repository

| Path | What |
|---|---|
| `crates/zchan-tx` | v6 transactions through pczt, node JSON-RPC, network parameters, seed wallet, refund watcher |
| `crates/zchan` | channels, signed messages, SQLite store, the services, x402, MPP, MCP |
| `infra/zakura` | regtest node (Zakura 1.6.0; NU7 at height 200) |
| `site/` | landing page, docs viewer and status page (static, no build step) |
| `deploy/` | the hosted setup: build, push, systemd units, Caddy |
| `docs/` | [quickstart](docs/quickstart.md) · [agents](docs/agents.md) · [use cases](docs/use-cases.md) · [design](docs/design.md) · [x402](docs/x402-zchannel.md) · [MPP](docs/mpp-zchannel.md) · [security](docs/security.md) · [privacy](docs/privacy.md) · [testnet runs](docs/testnet.md) · [hosting](docs/deploy.md) |
