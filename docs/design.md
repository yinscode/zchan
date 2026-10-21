# zchan design

zchan lets an agent fund **one** payment channel to a solver and pay many merchants through it. The solver keeps **one** channel to each merchant, and every agent's purchases from that merchant advance it. Channels are unidirectional Spilman channels on transparent Zcash v6 P2SH 2-of-2 outputs.

## Parties and channels

```
agent-1 ──channel──▶ ┌────────┐ ──channel──▶ merchant-a
agent-2 ──channel──▶ │ solver │ ──channel──▶ merchant-b
agent-3 ──channel──▶ └────────┘ ──channel──▶ merchant-c
          (agent pays)            (solver pays)
```

| Party | Payer of | Payee of | Signs |
|---|---|---|---|
| Agent | its channel to the solver | — | channel states |
| Solver | one channel per merchant | every agent channel | quotes, merchant-channel states, refunds of agent channels |
| Merchant | — | the solver's channel to it | invoices, acknowledgments, refund of the solver's channel |

## Channel transactions

| Transaction | Spends → pays | Lock / expiry | Signed |
|---|---|---|---|
| Funding | payer coins → P2SH(2-of-2 payer, payee) + change | — / tip + 20 | by the payer, only after it has verified the payee's refund signature and stored the refund |
| Refund | channel → payer, `capacity − 10,000` | `nLockTime = lock_height` / 0 | by both before the funding transaction is signed |
| State n | channel → payee `C_n`; payer `capacity − 10,000 − C_n` | none / 0 | by the payer; the payee adds its signature at close |
| Ironwood close | channel → payee's Ironwood address; payer its change as in the latest state | none / tip + 40 | built and proved by the payee, checked and co-signed by the payer |

- **The refund comes first.** The funding txid is known before anyone signs it, because v5 and later txids do not commit to signatures (ZIP 244). `Payer::fund` refuses to sign funding until the refund signature verifies, and `payer::open` stores the refund before the funding transaction leaves the process.
- **A state on the wire is just an amount and a signature.** The payee rebuilds the state transaction from the terms (it is deterministic) and verifies the signature against it, so it never inspects a transaction the payer built.
- **Fixed shape and fee.** A state always has two outputs and the payer keeps at least 5,000 zat (`MIN_CHANGE`). Every state and refund therefore pays 10,000 zat under ZIP 317, and a channel can pay at most `capacity − 15,000`.
- **Upgrade guard.** Pre-signed transactions commit to a consensus branch ID, so terms whose refund window crosses a network upgrade are refused.
- **Close deadline.** The payee broadcasts its best state when the tip reaches `lock_height − close_margin`. The payer's watcher broadcasts the refund from `lock_height` if the output is still unspent.
- **Ironwood close.** It costs 15,000 zat (one more logical action). The merchant pays the difference from its share, and the solver checks only that its own change is exact.

## A payment

```
merchant ── 402 + signed invoice ──────────────▶ agent
agent ── POST /quote {invoice, agent channel} ─▶ solver   checks: merchant's signature, expiry, unused,
agent ◀──────────────────────── signed quote ─── solver           liquidity on both channels, credit
agent ── POST /pay {payment id, quote, state} ─▶ solver   under the agent lock, then the merchant lock:
                                                            verify the agent state; sign the merchant state;
                                                            commit both + payment `agent_signed`
solver ── POST /channels/{id}/update ──────────▶ merchant  verify: own invoice, unpaid, unexpired,
solver ◀──────────────── signed acknowledgment ── merchant         state = held + amount; commit; sign
agent ◀──────────────────────── done + ack ───── solver   payment `done`
agent ── request + PAYMENT-SIGNATURE(ack) ─────▶ merchant  serve once
```

| Situation | What happens |
|---|---|
| Retry with the same payment id | The stored result is returned; if still `agent_signed`, the identical stored merchant state is pushed again. |
| Two requests with the same payment id at once | Locks serialize them; the second sees the first's record. The merchant channel advances once. |
| Solver refuses `/pay` (quote expired, credit changed, liquidity, bad state) | It stored nothing. The agent restores its previous level. |
| Merchant refuses the state | Payment `failed`. The merchant channel's level is restored, and the agent is credited `amount + fee`, which applies to its next quote. The refused state was sent, so it is logged as exposure. |
| Merchant unreachable | Payment stays `agent_signed`. The solver re-pushes on every background pass and on restart; the agent's retries see `pending` until `done`. |
| Solver crashes after committing | On restart, `resume` pushes the stored state; the agent's retry completes. |

Every transition is committed to SQLite before the next message is sent.

## Trust model

| Exposure | Enforced by | Bound in this build |
|---|---|---|
| Agent's unspent capacity | **Chain**: presigned refund from `lock_height` | — |
| What a merchant has acknowledged | **Chain**: it holds the payer-signed state; it must close before `lock_height − margin` | close margin (default 10 blocks regtest, 40 on testnet in the demo) |
| Solver capital in merchant channels | **Chain**: presigned refund | — |
| Agent paid, solver does not pay the merchant | **Trust in the solver** | one payment in flight per agent channel; `max_payment` |
| Solver paid, merchant does not deliver | **Trust in the merchant** (registered by the solver) | one invoice; the acknowledgment is evidence |
| Merchant refuses a state it received | **Solver ledger**: agent credited; merchant could still close with the sent state | logged as `exposure` |

- **Not atomic.** Nothing on chain binds the agent's state to the merchant's.
- **Privacy.** Funding, states and transparent closes reveal amounts and counterparties on chain, and the solver sees which agent paid which merchant. This is not a shielded payment channel. The cooperative close can pay the merchant's share to Ironwood (shielded); funding and the payer's change stay transparent.

## Code map

| Module | Role |
|---|---|
| `zchan-tx` | v6 transactions through pczt, P2SH 2-of-2, Zakura JSON-RPC, network parameters and upgrade guard, seed wallet, refund watcher kit, regtest helpers |
| `zchan::channel` | terms, refund, states, verifier, close, Ironwood-close check (no I/O) |
| `zchan::wire` | signed messages (`Signed<T>` over SHA-256(domain ‖ JSON)) and request types |
| `zchan::store` | SQLite (WAL, synchronous FULL): channels, append-only states and events, invoices, quotes, payments, credits |
| `zchan::payer`, `zchan::payee` | opening, funding, refund watching; key slots, refund signing, funding checks, deadline closes |
| `zchan::solver`, `zchan::merchant`, `zchan::agent` | the three services and the agent library |
| `zchan::x402`, `zchan::mpp` | x402 v2 and MPP wire formats for `zchannel` (`docs/x402-zchannel.md`, `docs/mpp-zchannel.md`) |
| `zchan::gate` | the merchant's payment gate (one invoice offered over both protocols, served once) and proxy mode |

## Limits of this build

- Merchant channels are not rotated automatically. A merchant channel near its deadline is closed by the merchant; registering the merchant again (`POST /admin/merchants`) opens a new one.
- Admin endpoints (`/admin/...`) need `X-Admin-Token` when `--admin-token` is set, and a service refuses to listen beyond localhost without one. Peers are still reached over plain HTTP: put TLS in front of public services.
- The agent's approval page is protected only by a random path token; on a LAN, anyone with the link can approve payments.
- Mainnet is refused by the code (`Ctx::new`).
