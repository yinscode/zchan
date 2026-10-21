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
