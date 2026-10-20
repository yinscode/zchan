# Running on Zcash testnet

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
