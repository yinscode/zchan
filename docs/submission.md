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

Under the form's 2,000-character limit (1975 characters). Paste the block as plain text.

```text
Zchan: payment channels for AI agents on Zcash. Live on testnet: https://165-22-48-127.sslip.io

WHAT IT DOES
An agent funds one channel to a solver and pays any merchant behind it. A payment is a newly signed channel state, not a transaction: 17 ms median, no block wait, 0 zat network fee. Services charge per request over x402 v2 or MPP, or put Zchan's proxy in front of an existing API. Two transactions settle any number of payments.

WHY IT DOES NOT LEAK
Channels are funded from Ironwood shielded notes and closed cooperatively with both sides paid back to Ironwood. The chain sees a channel's size and when it opened and closed. It does not see where the funds came from, how they were split, or any payment in between. Each merchant sees only its own payments, and the solver never sees response content.

WHY FUNDS ARE SAFE
Spilman channels on a v6 P2SH 2-of-2. The refund is signed before the funding transaction, so if the solver disappears the agent's watcher takes the balance back at the lock height.

REAL
- Every shield-in, open and close in the playground is a Zcash testnet transaction, with its txid and block shown on the page.
- Channel signing, pczt builds, Ironwood proofs, crash recovery, idempotent payments, ledger reconciliation.
- A watcher-broadcast refund, transparent closes and shielded closes, all exercised on testnet (docs/testnet.md).

MOCK OR SIMPLIFIED
- The three merchants return demo JSON, and the proxy target is a small stub API. The payments for them are real channel states.
- The playground server holds each visitor's agent keys so you can try it without installing. A real agent keeps its own seed.
- A house wallet grants each session 160,000 testnet zat.

LIMITS
The agent and merchant legs are not atomic yet, so the solver is trusted with one payment in flight per agent (hash locks are planned). The solver sees which agent paid which merchant. Refunds and unilateral closes are transparent. Mainnet is disabled in code.
```
