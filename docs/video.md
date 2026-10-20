# Two-minute video script

`scripts/record.sh` records this video automatically from a live run, as captioned frames of the real terminal output and pages, joined with ffmpeg (Chrome and ffmpeg required). It writes `video/<network>/zchan-demo-<network>.mp4`; `NETWORK=testnet ZCHAN_NODE_URL=… scripts/record.sh` records the testnet cut. A voice-over or a live phone scan can be added on top.

Record on testnet if the testnet run is done (docs/testnet.md); otherwise on regtest, and say so on screen. Before recording, run `KEEP=1 scripts/demo.sh` (add `NETWORK=testnet ZCHAN_NODE_URL=…` for testnet) and open three windows: the solver dashboard, merchant A's checkout page, and a terminal.

| Time | Screen | Say |
|---|---|---|
| 0:00–0:12 | README diagram (agents → solver → merchants) | "zchan: an AI agent funds one Zcash payment channel and pays any merchant through a solver. No transaction per payment." |
| 0:12–0:30 | Terminal: the demo's channel-open lines | "The solver opens one channel to each merchant; each agent opens one channel to the solver. Every channel's refund is signed before its funding is broadcast, so funds can always come back." |
| 0:30–0:55 | Terminal: `zchan-agent get …/paid/weather`, then `…/paid/news`; show the 200 bodies | "The merchant answers HTTP 402 with an x402 v2 offer. The agent gets a quote, signs its next channel state, and the solver pays the merchant on its own channel. The merchant signs a receipt; the agent sends it back and gets the data." |
| 0:55–1:10 | Solver dashboard: two agents, merchant A's channel level, reconciled ✓ | "Two agents bought from merchant A. Both purchases moved the same solver-to-A channel. The ledger reconciles every channel." |
| 1:10–1:30 | Phone scans the checkout QR → agent approval page → Approve → POS shows Paid ✓ | "At a counter, the merchant shows a QR code. It carries only an invoice reference. The phone opens the agent, shows amount and fee, and pays on approval." |
| 1:30–1:45 | Terminal: `CLOSE=1` settle lines; block explorer for the Ironwood close txid | "At the end of the day the merchant closes cooperatively and its share goes to a shielded Ironwood address." |
| 1:45–2:00 | design.md trust table | "Honest limits: the two legs are not atomic, the solver is trusted with one payment per agent at a time, and funding is transparent. Refunds and closes are enforced by the chain." |

Keep the receipts for the description: channel ids and txids are in `.demo/<network>/*.log` and on each service's `/status.json`.
