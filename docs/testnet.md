# Running on Zcash testnet

## Run of October 21, 2026 (blocks 4,524,435–4,524,506)

All transactions are v6. The full record is in [spikes/testnet-run.json](spikes/testnet-run.json); the video is made by `NETWORK=testnet scripts/record.sh`.

| Step | Txid | Height | Outputs (zat) |
|---|---|---|---|
| solver → merchant-a channel (funding) | `75f44ac0eaf454151e00e315af3b4af8b21d5e33dfb73545aac257a6a65bcdde` | 4,524,456 | 200,000 capacity |
| solver → merchant-b channel (funding) | `a1327aa74e504ad05f86722bd56a71bd5d2f5177183985208bc6d7f02eec3117` | 4,524,460 | 100,000 capacity |
| agent-1 → solver channel (funding) | `bacb81f57676cdb9e75d3024820dd9eb863b977eaac226532dd55eac3c0a9b19` | 4,524,466 | 100,000 capacity |
| agent-2 → solver channel (funding) | `a14cac0736282948eaa47f78005346a459ee1d661cbf6c5a8bb89668e49218ba` | 4,524,467 | 50,000 capacity |
| Payments: agent-1 → A (x402), agent-1 → B (x402), agent-2 → A (x402), agent-1 → A (QR checkout) | none: channel states only | — | A 70,000 · B 5,000 |
| merchant-a cooperative close to Ironwood | `2e82924da246f71243bddad100cfda5ada375b0afcd73a480a89b39d540863a7` | 4,524,474 | solver change 120,000; merchant 65,000 shielded |
| merchant-b close | `25aff53e030b213f49e9f9b320e14aae8d0913a35ef9cbe81853d564cb6054ce` | 4,524,474 | 5,000 · 85,000 |
| agent-1 channel close (by the solver) | `95dea063c92bcf84595c5a88e893c2b2a5dcb14911d0445d47ce71e3fe770136` | 4,524,474 | 55,300 · 34,700 |
| agent-2 channel close (by the solver) | `c35ca05aaff3c1a6886fc65be8966aa5a3e69f5fc5ce53286545d443b0a0e441` | 4,524,474 | 20,100 · 19,900 |
| agent-1 unused channel (lock 4,524,504), refunded by the watcher | `70e2e4147efaa7f8a3a1aec6085d78543cf6e300038838e0bfe8716da070d32e` | 4,524,506 | 40,000 (`nLockTime` 4,524,504, expiry 0) |

Every channel reconciled on the solver (`reconcile: ok` for all four). Funds came from the testnet faucet to the solver's deposit; `zchan-agent send` split them to the agents (`2a6990f2…`, `c152c955…`).

Everything that runs on regtest runs on testnet unchanged. Only the node, the funding and the lock lengths differ.

## Agent-initiated shielded close on testnet (blocks 4,526,607–4,526,623)

Agent-1 funded a channel from notes, made one payment, and closed it cooperatively. It asked the solver for a close offer, checked that the offer paid its own change to its Ironwood address at its own signed level, and co-signed. Merchant-a then closed its channel the same way. Both closes spend only the channel output and have **no transparent outputs**. The record is in [spikes/testnet-agent-close.json](spikes/testnet-agent-close.json).

| Step | Txid | Height | On chain |
|---|---|---|---|
| solver → merchant-a channel, funded from notes | `68d59e7aa846e6076173bca4775604504217c299f00c6f394a4e807375bac260` | 4,526,612 | 0 transparent inputs; channel output 100,000 |
| agent-1 → solver channel, funded from notes | `801e5a16b738869d36aabb4ffd509b77d2fe30b65d134a1884f2bc7f9cd6ab67` | 4,526,618 | 0 transparent inputs; channel output 50,000 |
| Payment: x402 `/paid/weather` (20,000 + 100 fee) | none | — | agent level 20,100; merchant level 20,000 |
| agent-1 channel: cooperative close, started by the agent | `05ac56266209177d7da7654e8776be08c4e0915e34510013b767aec175d92216` | 4,526,621 | 1 transparent input, **0 transparent outputs**; 30,000 into Ironwood |
| merchant-a channel: cooperative close | `4b4e658a377d1204734bcb1ee2a71b77148100840835f0e6ddc2b65587d15ebf` | 4,526,623 | 1 transparent input, **0 transparent outputs**; 80,000 into Ironwood |

Notes each owner found with its own viewing key afterwards:

| Close | Owner | Note | Why that amount |
|---|---|---|---|
| agent channel | agent-1 | 19,900 | its change: 50,000 − 10,000 − 20,100 |
| agent channel | solver | 10,100 | its 20,100, less the extra 10,000 fee of a shielded close |
| merchant channel | solver | 70,000 | its change: 100,000 − 10,000 − 20,000 |
| merchant channel | merchant-a | 10,000 | its 20,000, less the extra fee |

The solver's books reconciled after both closes.

## Shielded funding and a fully shielded cooperative close on testnet (blocks 4,526,560–4,526,579)

The solver and agent-1 moved their transparent coins into their shielded accounts. Each then funded a channel from Ironwood notes, so neither funding transaction has a transparent input. After two payments, merchant-a closed cooperatively with both sides paid to Ironwood: the close spends the channel output and has no transparent outputs. The record is in [spikes/testnet-shielded.json](spikes/testnet-shielded.json).

| Step | Txid | Height | On chain |
|---|---|---|---|
| solver shields its coins | `7fa054e841796d0f803b88cfab750b45488fb50e5604a17ab2f9191f5971b5db` | 4,526,563 | 1 transparent input, 0 transparent outputs; 11,240,000 into Ironwood |
| agent-1 shields its coins | `1b1fe705f771997b687819b07e82569943d84e8add344a1e76efdf0786c262bd` | 4,526,564 | 1 transparent input, 0 transparent outputs; 210,000 into Ironwood |
| solver → merchant-a channel, funded from notes | `d736ba0888188c4a78d55516ba887b0f7c488ec91909b3d6bc4c7ca9ad62334d` | 4,526,568 | **0 transparent inputs**; channel output 100,000; change 11,130,000 shielded |
| agent-1 → solver channel, funded from notes | `a1275d7705720b21f47b2627639e0b8b7a1df95c3b0f41b85636425ed0d4bf73` | 4,526,573 | **0 transparent inputs**; channel output 100,000; change 100,000 shielded |
| Payments: x402 `/paid/weather`, MPP `/paid/news` (20,000 each) | none | — | merchant level 40,000; agent level 40,200 |
| merchant-a cooperative close, both sides to Ironwood | `c70690f3c6b6c672b952c4159be980634e9be90b61d5ddbe53def7ae33454c85` | 4,526,577 | 1 transparent input (the channel), **0 transparent outputs**; 80,000 into Ironwood (fee 20,000) |
| agent-1 channel close (by the solver, unilateral) | `39a458cb35b1cf42b5fda7450155e568473c433e832a36806d4d1433ba03d3f8` | 4,526,579 | transparent: 40,200 · 49,800 |

Each party's shielded account, scanned with its own viewing key afterwards, holds the expected notes from the cooperative close:
- **merchant-a:** a 30,000 zat note (its 40,000 share, less the 10,000 extra fee for the shielded close).
- **solver:** a 50,000 zat note, exactly its change (100,000 − 10,000 − 40,000). The solver decrypted this output before it co-signed.

Neither amount appears in the close's transparent part. The chain shows only that 80,000 entered Ironwood.

The agent-1 channel was closed by the solver alone, so that close is transparent and shows its split. Agent channels have no cooperative close yet (see [privacy.md](privacy.md)).

## Proxy mode and MPP on testnet (blocks 4,524,592–4,524,606)

Merchant D ran in proxy mode in front of an existing HTTP API (`--upstream`, `--routes '/api/search=3000,/api/*=500'`, admin token set). Agent-1 opened a fresh channel and paid through the proxy once over x402 v2 and twice over MPP. Every 402 offered both protocols: `PAYMENT-REQUIRED` and `WWW-Authenticate: Payment … method="zchannel", intent="charge"`. The unpriced `/free` path passed through with no payment. The record is in [spikes/testnet-proxy-mpp.json](spikes/testnet-proxy-mpp.json).

| Step | Txid or invoice | Height | Result |
|---|---|---|---|
| solver → merchant-d channel (funding) | `7efa2dd0e6275829ee040d9f4b741293e0f7903338f2714e743d86d1c4b62739` | 4,524,602 | 100,000 capacity |
| agent-1 → solver channel (funding) | `54c85351af8bdaa4b54dede4dd923ce6b100c9d19ddc0987eba52a8fe31dd00a` | 4,524,605 | 100,000 capacity |
| x402 `GET /api/search?q=zec` (proxied) | invoice `2bbe8463…` | — | 200, `PAYMENT-RESPONSE` success, merchant level 3,000 |
| MPP `GET /api/search?q=mpp` (proxied) | invoice `a609491e…` | — | 200, `Payment-Receipt` success, level 6,000 |
| MPP `POST /api/echo` (proxied, body forwarded) | invoice `43f19810…` | — | 200, `Payment-Receipt` success, level 6,500 |
| merchant-d close | `95a278ab5f4c895f5c88a91d74d47aef7ec1bbaa14e963738bcdf9428dbd206f` | 4,524,606 | 6,500 · 83,500 |
| agent-1 channel close (by the solver) | `23c74660f42db0a9ead0d4beb9b45ed3b76397c603c34cddbc52eb7cce71ef3e` | 4,524,606 | 6,800 · 83,200 (6,500 + 3 × 100 fee) |

Both channels reconciled on the solver before closing.

## Needs

- A Zakura testnet JSON-RPC endpoint with `getaddressutxos`, `getrawtransaction` (verbose) and `z_gettreestate`, given as `ZCHAN_NODE_URL`. Never commit it.
- About 0.006 TAZ in total: solver 0.004, agent-1 0.0012, agent-2 0.0006.

## Run

```bash
NETWORK=testnet ZCHAN_NODE_URL=<endpoint> KEEP=1 CLOSE=1 scripts/demo.sh
```

The script prints three deposit addresses and waits until the solver's has 0.004 TAZ confirmed. Fund the two agent addresses at the same time. Seeds are created in `.demo/testnet/<party>/seed` (mode 600); back them up before funding.

On testnet the demo uses `--poll-ms 15000 --close-margin 40`, merchant locks of 3,000 blocks and agent locks of 2,000 blocks, so nothing nears a deadline during a demo. Each channel open waits for one confirmation.

## Exercising the refund on testnet

The regtest gates exercise refunds, deadline closes and Ironwood closes. To show a refund on testnet, open a short agent channel, for example `zchan-agent open --capacity 50000 --lock-blocks 25` with `--data .demo/testnet/agent-3`. Do not pay with it. Run `zchan-agent watch` with the same `--data`, `--node` and `--solver`: it broadcasts the refund once the lock height is reached and records the settlement (`zchan-agent status`).

## Record

Collect channel ids and txids from `curl -s http://127.0.0.1:7100/status.json` and the merchants' `/status.json`, and add them to the submission.
