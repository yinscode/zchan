# Zecathon submission

Everything the submission asks for (repository, demo, description, two-minute video) is ready. The submission itself is made from your Zecathon account.

## Checklist

| Item | Where |
|---|---|
| Repository | this repo (push it to a public remote first; there is no remote yet) |
| Demo | `scripts/demo.sh` (regtest), `NETWORK=testnet ZCHAN_NODE_URL=… scripts/demo.sh` (testnet) |
| Testnet evidence | `docs/testnet.md`, `docs/spikes/testnet-run.json` |
| Two-minute video | `video/testnet/zchan-demo-testnet.mp4` (recorded from the live testnet run; not in git, upload it) |
| Description | below |

## Description (ready to paste)

**zchan: payment channels for AI agents on Zcash**

An agent funds one Zcash payment channel to a solver and pays any merchant through it. The solver keeps one channel to each merchant, and every agent's purchases from that merchant advance it. A payment is a newly signed channel state, not an on-chain transaction; settlement happens when a channel closes.

- **Channels:** unidirectional (Spilman) channels on transparent Zcash v6 P2SH 2-of-2 outputs, built through pczt against a Zakura node. Each refund is signed by both sides before the funding transaction is signed. Payee closes, payer refunds, and a refusal of terms that cross a network upgrade are all built in.
- **Payments over HTTP:** a local x402 v2 scheme, `zchannel`. The merchant answers 402 with a signed invoice. The agent signs its next state, and the solver pays the merchant on its own channel. The merchant's signed acknowledgment is the credential, and it is served once.
- **At a counter:** a QR checkout that carries only an invoice reference, and a phone approval page on the buyer's own agent.
- **Shielded payout:** the merchant's cooperative close pays its share to an Ironwood address.
- **Reliability:** idempotent payment ids, credits when a merchant refuses, and recovery from a solver crash mid-payment or a merchant outage. Everything is persisted before it is acknowledged, and a ledger reconciles every channel.

**Proven on Zcash testnet** (blocks 4,524,435–4,524,506): 4 channels; 3 x402 purchases and 1 QR checkout; one cooperative close to Ironwood; three transparent closes; and a watcher-broadcast refund. All txids are in `docs/testnet.md`. A regtest end-to-end suite runs the real binaries and covers every failure path.

**Honest limits:** the two legs are not atomic. The solver is trusted with one payment per agent at a time. Funding and transparent closes are public. Mainnet is disabled in code.
