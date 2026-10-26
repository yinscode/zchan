# Security

What the chain enforces, what is trusted, and who can see what. The details behind each line are in [Protocol](#design) and [Privacy](#privacy).

## Funds always come back

Before an agent funds a channel, the solver signs a time-locked refund that returns the whole channel, less its fee, to the agent. The refund is built from the funding transaction's id, which is known before the funding is signed. If the solver disappears, the agent's watcher (`zchan-agent watch`) broadcasts the refund at the channel's lock height. Solver channels to merchants carry the same guarantee for the solver.

## Enforced by the chain

| Exposure | Protected by | Bound |
|---|---|---|
| An agent's unspent channel balance | the presigned refund | returned at the lock height |
| What a merchant has acknowledged | the signed state the merchant holds | the merchant closes before the lock height |
| Solver liquidity in merchant channels | the presigned refund | returned at the lock height |

## Trusted today

| Exposure | Trust | Bound |
|---|---|---|
| Agent paid, solver does not forward | the solver | one payment in flight per agent |
| The agent leg and the merchant leg together | not atomic | hash-locked states are planned |
| Which agent paid which merchant | seen by the solver | never on chain |

## Who sees what

For a channel funded from shielded notes and closed cooperatively:

| What | Agent | Solver | Merchant | Chain |
|---|---|---|---|---|
| Each payment's amount | sees | sees | its own | hidden |
| Which merchant was paid | sees | sees | itself | hidden |
| What was bought (the resource path) | sees | sees | sees | hidden |
| The response content | sees | hidden | sees | hidden |
| Where the channel's funds came from | sees | hidden | hidden | hidden |
| How the channel split at close | sees | sees | hidden | hidden |
| Channel size, open and close times | sees | sees | hidden | sees |

**What is always transparent:**
- **Refunds and unilateral closes:** they are presigned transparent transactions (Ironwood has no time locks), and they reveal the split.
- **Network metadata:** merchants see the client's IP address and headers. Put TLS in front of public services.

## Keys and operations

- **Seeds:** each party's seed lives in its data directory, readable only by its owner (mode 600). Back them up: a lost seed loses the channel keys.
- **Admin token:** `/admin/*` routes (opening merchant channels, shielding, closes) need `X-Admin-Token`. A service listening beyond localhost refuses to start without one.
- **Mainnet:** refused by the code until an external review of the channel code and key handling.
- **Durability:** each channel state is committed before it is acted on. By default every commit is synced to disk; [Self-hosting](#deploy) describes the faster setting and what it risks.
