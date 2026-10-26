# Use cases

A channel is worth opening wherever payments are smaller than one on-chain fee or come faster than one block. One agent channel pays every merchant behind its solver, and its capacity is a spending cap.

## For agents

| Use | Typical price | Why channels |
|---|---|---|
| **Paid APIs.** Search, market data, geocoding, inference. The provider prices each path; the agent pays only for the calls it makes. | ~100–5,000 zat a call | Per-call prices are far below one on-chain fee. |
| **Paid MCP tools.** MCP servers charge per tool call, so a specialized tool can earn without subscriptions or key management. | per tool call | One agent channel pays every tool server through the solver. |
| **Metered infrastructure.** GPU seconds, sandbox time and storage are billed as they are used, not in prepaid blocks. | per second, per MB, per unit of work | States update as often as the meter ticks, at no chain cost. |
| **Data with a budget.** A research agent buys articles, datasets and reports from many sources within a fixed spend. | per item | The channel's capacity is a hard cap the agent cannot exceed. |
| **Agents paying agents.** A planning agent hires specialist agents (translation, code review, image work) and pays each per task. | per task | A new counterparty needs no new channel, only a solver that pays it. |
| **Private revenue for providers.** Earnings arrive at a shielded address when the channel closes, and the payer's change goes back shielded too. | any | One close with no transparent outputs, instead of a public trail of every sale. |

## For commerce

| Use | Typical price | Why channels |
|---|---|---|
| **Counters and cafés.** The point of sale shows a QR code that carries only an invoice reference. The buyer's agent verifies the signed invoice and pays on approval. | per order | "Paid" in about a second instead of one block. |
| **Charging, Wi-Fi, vending, parking.** A machine meters usage and the buyer's channel advances as it runs; stopping the session stops the payments. | per kWh, per minute | Pay as you go, without prepaid credit or a card hold. |
| **Media and live events.** Pay per minute watched, per article read, or per tip during a stream. | per minute | Payments small enough to match the content. |
| **Suppliers and marketplaces.** A marketplace runs the solver, netting many buyers' payments into one channel per seller and settling daily. | daily settlement | One transaction per seller per day, with every payment reconciled. |

Start with the [Quickstart](#quickstart), or try the flow in the [Playground](https://165-22-48-127.sslip.io/play).
