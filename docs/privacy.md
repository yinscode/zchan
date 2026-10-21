# Privacy

What zchan hides depends on how a channel is funded and closed. The private path is: fund from Ironwood notes, pay off chain, close cooperatively with both sides paid to Ironwood.

## What a chain observer sees

| Item | Transparent funding, transparent close | Shielded funding, shielded cooperative close |
|---|---|---|
| Individual payments: amounts, merchants, timing | hidden (off chain) | hidden (off chain) |
| Where the channel's money came from | the funding inputs' addresses | **hidden**: no transparent inputs, change stays shielded |
| Channel capacity | visible | visible (the 2-of-2 output's value) |
| That it was a 2-of-2 channel | visible when spent | visible when spent (the redeem script) |
| How much the payee earned | visible (close outputs) | **hidden**: both outputs are Ironwood notes; only capacity minus fee enters the pool |
| Where each side's money went | visible addresses | **hidden**: notes to per-channel Ironwood addresses |
| When the channel opened and closed | visible | visible |
| Links between a party's channels | change addresses: each channel uses its own (not the deposit address), but funding inputs can still join coins | none from funding or closes |

## What is always transparent

- **Refunds and unilateral closes.** Both are presigned transparent transactions: Ironwood has no time locks, so a refund cannot be shielded. They reveal the channel's split.
- **The channel output.** Its capacity, and its 2-of-2 script once spent.

## Off chain

- **The solver** sees every payment it routes: which agent channel paid which merchant, how much, and when. zchan does not blind this.
- **Merchants** see their own invoices and the client's network metadata (IP address, headers). Put TLS in front of public services; the demo uses plain HTTP.

## How each piece works

- **Shielded funding** (`zchan-agent shield`, then `open --shielded`; for the solver, `POST /admin/shield`, then `POST /admin/merchants` with `"shielded": true`):
  - The funding transaction spends the party's Ironwood notes, witnessed from the node's tree state.
  - It pays the channel output and returns change to the account's internal Ironwood address.
  - Its txid is known before signing, so the refund is still signed first.
- **Shielded cooperative close:**
  - At opening, the payer gives a fresh Ironwood address for that channel (a diversified address of its account).
  - The payee builds the close with both outputs shielded and pays any extra fee from its share. Two Ironwood outputs cost 20,000 zat against 10,000 for a state.
  - The payer co-signs only after checking that the transaction spends only the channel output, pays nothing transparently, and contains one output to its address whose note commitment matches and which its key decrypts to exactly its change.
- **Fresh transparent change:** with transparent funding, coins are chosen fewest first, and change goes to a new address for that channel, never back to the deposit address. `zchan-agent deposit --fresh` gives an unused deposit address.

## Claims this supports

- **Shielded funding and cooperative closes:** the chain shows a channel's capacity and when it opened and closed, but not where its money came from, how it was split, or any payment in between.
- **Not supported:** "anonymous", "untraceable", or "the solver cannot see payments". Refunds and unilateral closes are transparent, and the solver sees the payment graph.
