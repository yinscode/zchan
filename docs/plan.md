# Zchan implementation plan (v2)

**Revised:** October 21, 2026
**Deadline:** Zecathon, October 28, 2026, 23:59 UTC (October 29, 06:59 Bangkok). Submit a repository, a demo, a description and a two-minute video. [Zecathon rules](https://thezecathon.com/)

## TL;DR

- An agent funds **one** channel to a solver. The solver keeps **one** channel to each merchant, shared by every agent that buys from that merchant.
- Each channel is a unidirectional Spilman channel on a transparent Zcash v6 P2SH 2-of-2 output. Its time-locked refund is fully signed before funding is broadcast.
- **The agent pays first.** The agent signs the solver's next state; the solver then pays the merchant and returns the merchant's signed acknowledgment. The agent presents that acknowledgment as its x402 credential.
- The solver risks only its capital, which is locked in merchant channels and recoverable by refund. It extends no credit to agents.
- The transaction crate (`crates/zchan-tx`) builds v6 transactions through pczt against a Zakura node. The refund-safety gate runs first, on regtest.

## Status (October 21, 2026)

| Gate (§7) | State | Evidence |
|---|---|---|
| Oct 21: refund before funding; rejected before lock, accepted at it; state accepted | **passed** (regtest) | `crates/zchan/tests/regtest.rs`, `docs/spikes/gate0-regtest.json` |
| Oct 22: open, update, close, refund, recovery after restart, payee close at deadline | **passed** (regtest) | `crates/zchan/tests/e2e.rs`, `docs/spikes/e2e-regtest.json` |
| Oct 23: one agent → A and B; two agents share solver→A; idempotent and concurrent retries; exhausted capacity; refusal → credit; reconciliation | **passed** (regtest) | same |
| Oct 24: x402 v2 route paid once; invalid, replayed, mismatched, expired stay unpaid | **passed** (regtest) | same |
| Oct 24: **first full testnet run** | **passed** (testnet, blocks 4,524,435–4,524,506): 4 channels, 4 payments incl. QR, Ironwood and transparent closes, a watcher refund | `docs/testnet.md`, `docs/spikes/testnet-run.json` |
| Oct 25: QR checkout; altered, wrong-merchant, expired, reused invoices fail | **passed** (regtest; phone scan not tested on a device) | e2e; `docs/video.md` |
| Oct 26: solver killed mid-payment, merchant down, status page, docs | **passed** (regtest) | e2e; `docs/design.md`, `/status` |
| Oct 27: record | **recorded**: testnet and regtest cuts, 2:00 each (`scripts/record.sh`) | `docs/video.md` |
| Oct 28: submit | **prepared**: description, checklist and video ready; the submission itself is made from the user's account | `docs/submission.md` |

P1 built: QR checkout, Ironwood cooperative close, second agent in the demo, status page. From P2, built after the gates: the MPP adapter (`docs/mpp-zchannel.md`) and proxy mode, covered by the regtest e2e test and run on testnet (blocks 4,524,592–4,524,606, `docs/testnet.md`).

## 1. Decisions

| # | Decision | Why |
|---|---|---|
| D1 | **Agent prepays** the solver for each payment | Removes solver credit exposure, the agent allowlist, exposure caps and the "agent refused to sign" state. The solver is already the trusted party in the MVP. |
| D2 | **One solver, allowlisted merchants, test funds only** | Permissionless solvers, collateral and reputation are post-submission work. |
| D3 | **"A state sent is a state paid"** | Once a party sends a signed channel state, it treats that amount as paid. Retries resend the identical stored state and never build a new one. |
| D4 | **Shared merchant channel needs a per-invoice acknowledgment** | On a shared channel, cumulative amounts can't identify which invoice they cover. The solver pushes each state directly to the merchant (one writer per channel), and the merchant signs `(invoice_id, channel_id, cumulative)`. |
| D5 | **Separate transaction crate** `crates/zchan-tx` (pczt v6 builder, P2SH 2-of-2, Zakura RPC) | Keeps channel logic apart from transaction plumbing; it is built and gated first (§6). |
| D6 | **Zakura node**: a regtest node for gates, testnet for the demo | One node implementation for every test. The RPC URL comes from the environment and is never committed. |
| D7 | **Single process per role, SQLite (WAL mode)** | One mutex per channel replaces a reservation table and an allocator. An append-only event table replaces a double-entry ledger. |
| D8 | **x402 v2 is the only adapter for submission.** MPP is deferred. | One adapter around one coordinator shows the design. The QR flow is more useful in the video than a second wire format. |

## 2. Trust model

| Exposure | Enforced by | Bound in the MVP |
|---|---|---|
| Agent's unspent channel capacity | **Chain**: presigned refund after `lock_height` | — |
| Merchant's received amount | **Chain**: merchant holds the payer-signed state and closes before `lock_height − margin` | Merchant must close in time |
| Solver capital in merchant channels | **Chain**: presigned refund | — |
| Agent pays, solver doesn't forward | **Trust in the solver** | One in-flight payment per agent channel × per-payment cap |
| Solver pays, merchant doesn't deliver | **Trust in the merchant** (allowlisted) | One invoice; the receipt is evidence for a dispute |
| Merchant rejects after the agent paid | **Solver ledger**: a credit to the agent | Credit is applied to the next quote, or to the cooperative close |

**Not atomic.** Nothing cryptographically links the two legs. Closing that gap would need hash-locked conditional states (deferred to P2).

**Privacy.** Funding, states and refunds are transparent: amounts and counterparties are visible on-chain. The solver sees which agent paid which merchant. This is **not** a shielded payment channel. P1 shields only the merchant's cooperative-close output.

## 3. Channel construction

Notation: payer P, payee Q, funding output F of value `capacity`.

| Transaction | Spends → pays | Lock | Expiry | Signed by |
|---|---|---|---|---|
| **Funding** | P's coins → F = P2SH(2-of-2 P, Q) | — | normal | P, after it holds a verified refund |
| **Refund** | F → P: `capacity − fee` | `nLockTime = lock_height`, inputs' sequence non-final | 0 | Q first, then P; **fully signed before funding is broadcast** |
| **State n** | F → Q: `C_n`; P: `capacity − fee − C_n` (left out below dust) | none | 0 | P signs and sends; Q adds its signature at close |

Rules:
- Before funding is signed, its txid comes from the **unsigned** pczt (`zchan_tx::txid`). v5 and later txids don't commit to signatures (ZIP 244), so the presigned refund stays valid. [ZIP 244](https://zips.z.cash/zip-0244)
- `state_fee` is computed once at open with the ZIP 317 rule (`zchan_tx::fee`) and stored. The usable capacity is `capacity − state_fee`.
- **Q verifies before storing:** correct funding outpoint, `C_n > C_{n−1}`, `C_n ≤ usable`, outputs and fee match the stored template, and P's signature is valid over the sighash.
- **Close deadline:** Q broadcasts its best state by `lock_height − margin`. P's refund watcher broadcasts once `lock_height` is reached and F is still unspent.
- **Upgrade guard:** refuse to open a channel whose `[open, lock_height]` window spans a network upgrade. Presigned transactions commit to the consensus branch ID (`zchan_tx::params::spans_upgrade`).
- **Defaults** (tune on testnet): agent channel `L = 2,000` blocks; merchant channel `L = 5,000`; `margin = 100`. Merchant channels are long-lived. Rotating a merchant channel before expiry is P2 work.
- **Keys:** one seed per process, with paths per channel and role.

## 4. Payment flow

```
Merchant          Agent                      Solver
   |-- 402 + signed invoice -->|                |
   |                           |-- quote ------>|  check invoice sig/expiry/unused, capacity, cap
   |                           |<-- signed quote|
   |                           |-- pay(payment_id, quote_id, agent state A+amt+fee−credit) -->|
   |                           |                |  verify + persist          [agent_signed]
   |<-- update(invoice_id, solver state M+amt) -|  under merchant-channel lock; persist state BEFORE push
   |-- ack(invoice_id, channel_id, M+amt) ---->|  persist                   [merchant_acked]
   |                           |<-- ack --------|                            [done]
   |<-- request + credential = ack ------------|
   |-- resource (served once per invoice) ----->|
```

1. **Invoice** (signed by the merchant): `merchant_id, amount_zat, resource_id, expiry, nonce`. `invoice_id = H(fields)`.
2. **Quote** (signed by the solver): `quote_id, invoice_id, amount, fee, credit_applied, expiry`. The solver performs **every check the merchant will perform**, so a valid push can't be refused for a reason the solver could have seen.
3. **Pay:** the agent's state must equal `prev + amount + fee − credit_applied` exactly. There is one in-flight payment per agent channel.
4. **Push:** the merchant's `update` is idempotent on `invoice_id`. A repeated push of the same state returns the stored acknowledgment. The merchant accepts a valid state for its own unpaid invoice until `expiry + grace`.
5. **Credential:** the merchant checks its own signature and the invoice binding, and serves each `invoice_id` once.
6. **Retries:** the same `payment_id` returns the stored result. On restart, any payment in `agent_signed` with a stored merchant state is pushed again with **the same bytes**.
7. **Failure:** the push is definitively refused, or the invoice passes `expiry + grace` before an acknowledgment arrives → `failed`, and a credit is recorded for the agent (`amount + fee`). The next solver→merchant state reuses the unconsumed cumulative level. A refused state is still "sent" (D3): the solver logs it as possible loss to that merchant.

Payment states: `agent_signed → merchant_acked → done`, plus `failed` (credit recorded). Every transition is written before the next message is sent.

## 5. Components

| Binary / crate | Owns | Invariant |
|---|---|---|
| `crates/zchan-tx` | pczt v6 builder: build, fee, txid, sign, append, extract, P2SH 2-of-2, keys, Zakura RPC, network params | Regtest gate tests for funding, refund and state transactions |
| `crates/zchan` | Channel types, state template, verifier, invoice/quote/ack types and signatures, SQLite store | Integer zatoshis only; never broadcast funding without a locally verified refund |
| `zchan-solver` | Quote, pay, merchant push, credits, event log, `GET /status` | Mutex per channel; persist, then send |
| `zchan-merchant` | Invoices, channel update endpoint, x402 middleware, demo paid route, `close-due` | Serve each invoice once; close before the deadline |
| `zchan-agent` | Agent channel, x402 client, refund watcher, approval web UI (QR) | One in-flight payment; refund kit kept on disk |
| `zchan-demo` | Scripted two-merchant scenario on regtest or testnet | Prints all txids and channel IDs |

SQLite tables (per process, as needed): `channels`, `states` (append-only), `payments`, `credits`, `events` (append-only). Reconciliation is a single query: for each merchant channel, `latest cumulative = Σ acked invoices + Σ logged sent-but-refused states`.

## 6. `zchan-tx` contents

| Module | Provides |
|---|---|
| `tx.rs` (`Spec` with `lock_height`/`expiry_height`, `fee`, `build`, `txid`, `sighash`, `sign`, `append`, `extract_with_script_sigs`) | Funding, refund and state transactions |
| `script.rs` (`multisig_2of2`, `p2sh`) | Channel output |
| `keys.rs`, `wallet.rs` | Keys derived from a seed |
| `node.rs`, `params.rs` (`from_node`, `spans_upgrade`) | Zakura RPC, network heights, upgrade guard |
| `watch.rs` | Block scanner for the refund watcher and `close-due` |
| `regtest.rs`, `infra/zakura` | Regtest helpers and the node setup |

Toolchain: Rust 1.97.1. Point `CARGO_TARGET_DIR` at an existing build of the pinned stack to save disk. Run regtest gates on fresh chains: on a long-running regtest chain, block rewards eventually halve too low and coinbase spends fail.

## 7. Schedule and gates

| Day | Build | Exit gate |
|---|---|---|
| **Oct 21** | `zchan-tx` crate; state template; verifier | `zchan-tx` regtest gate passes: a refund signed by both parties exists before funding and is **rejected before `lock_height` and accepted at it**. A state transaction is accepted. |
| **Oct 22** | Channel store, open/update/close, refund watcher, `close-due`, recovery after restart | On regtest, for both channel kinds: open, update twice, cooperative close, and a separate forced refund. Verifier rejects a decreasing amount, an amount over capacity, a wrong signature, and a wrong outpoint. Each process is killed and restarted, and it recovers its latest state and refund. |
| **Oct 23** | Solver: quote, pay, push, acknowledgment, credits, events, idempotency | Regtest: agent 1 pays merchants A and B from one channel; agents 1 and 2 both pay A, advancing **the same** solver→A channel. Duplicate and concurrent `payment_id` → paid at most once. Merchant refusal → credit, consumed by the next quote. Reconciliation query is clean. |
| **Oct 24** | x402 v2 middleware and client; **first full testnet run** | Local route: 402 → paid → resource served once. Invalid, expired, replayed and wrong-merchant credentials stay unpaid. The whole Oct 23 scenario runs on testnet, with txids recorded. |
| **Oct 25** | QR checkout; draft the video script | A second device scans the QR, sees the merchant, amount, fee and expiry, approves, and pays. An altered amount, wrong merchant, expired invoice, or reused nonce fails. The QR carries only an invoice reference: no key and no bearer token. |
| **Oct 26** | Fault injection, `GET /status` page, README, trust-model doc | Solver killed between `agent_signed` and the acknowledgment → restart pushes again → `done` with no double advance. Merchant down → retry. Testnet refund and close paths are exercised. |
| **Oct 27** | Record on testnet | Video shows: 2 merchants, the shared channel, crash recovery, QR, and an explorer link for each transaction |
| **Oct 28** | Buffer and submit | Demo runs from a clean checkout. Unfinished items are listed in the submission. |

If the Oct 21 gate fails, the channel engine runs on regtest only and the testnet demo is marked simulated. Real funds are never put in a channel whose refund path is unproven.

## 8. Priorities

- **P0 (needed for the claim):** both channel types on regtest and testnet with refunds proven; agent-prepays solver; 2 merchants; one merchant channel shared by 2 agents; idempotency and crash recovery; the x402 route; the trust-model doc.
- **P1 (cut first if P0 slips):** QR checkout; the merchant's cooperative close paid to a **shielded Ironwood** output (`zchan-tx` already builds Ironwood outputs); a second agent shown live; the status page.
- **P2 (after submission):** MPP adapter; NFC (an NDEF URI record carrying the same checkout link); rotating merchant channels before expiry; hash-locked conditional states for atomic legs; multiple solvers and routing; collateral and reputation; merchant self-service; shielded funding; mainnet.

## 9. Gates before any real value

1. **Refund safety** on the actual node stack: funding, sighash, `lock_height`, expiry 0, relay policy, both unilateral outcomes, and the upgrade guard.
2. **State safety:** persist before acknowledging; backup and restore; one writer per channel; test a refund racing a close around `lock_height`.
3. **Liquidity:** halt a merchant route when its channel can't cover the next payment. Reconcile confirmed funding against signed claims.
4. **Adapter truthfulness:** describe `scheme = zchannel` as a local x402 extension until an independent client and facilitator support it.

**Before mainnet (appendix, after submission):** review the channel code and key handling; exercise recovery under failure; document operating limits; decide whether the solver's custody and agent-credit risk is acceptable.

## 10. Open items

- ~~x402 v2 wire names~~ Resolved: taken from the spec at commit `faa1c694` (`PAYMENT-REQUIRED`, `PAYMENT-SIGNATURE`, `PAYMENT-RESPONSE`; `paymentFlow: upfront`). See `docs/x402-zchannel.md`.
- Lock-time defaults (§3) depend on testnet block timing as reported by the node.
- Credit policy: built as `amount + fee`. Change to `amount` only if the solver should keep its fee on a merchant refusal.
- The Ironwood close costs 15,000 zat against 10,000 for a transparent close; the merchant pays the difference, so it needs a share above 15,000 zat.
