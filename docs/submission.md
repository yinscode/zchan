# Zecathon submission

The repository, demo and description are ready; the video needs re-recording. The submission itself is made from your Zecathon account.

## Checklist

| Item | Where |
|---|---|
| Repository | https://github.com/yinscode/zchan |
| Demo | https://zchan.cash/play (hosted on testnet, no install); `scripts/demo.sh` runs the stack locally on regtest |
| Testnet evidence | `docs/testnet.md`, `docs/spikes/testnet-run.json` |
| Two-minute video | `video/testnet/zchan-demo-testnet.mp4` (not in git, upload it); it predates MPP, proxy mode, shielded closes and MCP, so re-record before submitting |
| Description | below |

## Description (ready to paste)

1,969 characters, under the form's 2,000-character limit even if line breaks count as two (1,998). Paste the block as plain text.

```text
# Zchan

Payment channels on Zcash for x402 and MPP: fast payments for agents and people.
Demo: https://zchan.cash/play
Code: https://github.com/yinscode/zchan

## What it does

An agent or user funds one channel to a solver and pays any merchant behind it. A payment is a newly signed channel state, not a transaction: 17 ms median, no block wait, no per-payment fee. Services charge via x402 v2 or MPP, or put Zchan's proxy in front of an existing API. Two transactions carry any number of payments. Agents pay from a CLI or MCP server, under a price cap and budget the model cannot raise.

## What stays private

Channels are funded from Ironwood shielded notes and can close cooperatively with both sides paid to shielded addresses. The chain sees a channel's capacity and when it opened and closed, not the source of funds, the split, individual payments, or which merchant was paid. The solver sees route, amount and resource, but not response content.

## How funds are protected

Spilman channels on a v6 P2SH 2-of-2. The solver signs a time-locked refund before the channel is funded. If the solver disappears, the agent's watcher recovers the balance at the lock height.

## Real on testnet

- Every shield-in, open and close in the Playground is a real testnet transaction, shown with its ID.
- PCZT construction, Ironwood proofs, watcher refunds and shielded closes are exercised on testnet. Crash recovery and idempotency are tested end to end.

## Demo simplifications

Merchants return demo JSON and the proxy targets a stub API; the payments are real. The Playground runs each visitor's agent server-side. A house wallet grants each session 160,000 testnet zat.

## Current limits

The agent and merchant legs are not yet atomic: until hash locks land, the solver is trusted with at most one payment in flight per agent. The solver sees which agent paid which merchant. Refunds and unilateral closes are transparent. Mainnet waits for a security review.
```
