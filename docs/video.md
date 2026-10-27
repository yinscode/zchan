# Two-minute video script

Seven shots, about 265 words of voice-over (two minutes at ElevenLabs' default pace). Record the screen first, then fit the voice-over to it. Cut every block wait; the voice-over says the steps wait for a block, the video doesn't need to show it.

## Before recording

- **Playground** (shots 3, 4, 6): open https://zchan.cash/play in a fresh window and start a session. Shield in and open each wait for a testnet block (a minute or two); record them and cut the wait.
- **Landing** (shots 1, 2, 7): https://zchan.cash, dark theme. In shot 2, press Play on the topology diagram.
- **Assistant** (shot 5): the hosted solver isn't open to outside agents, so this shot runs locally. Start `KEEP=1 scripts/demo-regtest.sh`, add the MCP server with the README's `claude mcp add` command (agent-2, `--automine`), and ask Claude Code: *"Fetch http://127.0.0.1:7204/api/search?q=zcash and summarize it."* Show the `zchan_fetch` call and its result, with `paid_zat` and `budget_left_zat`.

## Shots

| Time | Screen | Voice-over |
|---|---|---|
| 0:00–0:12 | Landing hero | AI agents are starting to pay for things: an API call, a search, a tool. On most chains, every payment is a transaction, and every transaction is public. Zchan fixes both. |
| 0:12–0:27 | Landing topology diagram, playing | An agent funds one payment channel on Zcash, to a solver. The solver keeps one channel to each merchant. A payment is a newly signed channel state, not a transaction. |
| 0:27–0:45 | Playground: shield in, then open (waits cut) | This is the live playground on Zcash testnet. My agent shields in funds, then opens a channel from its shielded Ironwood notes. These are real testnet transactions, and the only ones until the end. |
| 0:45–1:05 | Playground: pay merchant-a over x402, merchant-b over MPP, merchant-d `/api/search`; open one trace | Now it pays. The merchant answers HTTP 402, over x402 or MPP. The agent gets a quote, signs the next state, and gets the data, in milliseconds. No block wait, no network fee. This merchant runs in proxy mode: an ordinary API behind payment, with no code changes. |
| 1:05–1:25 | Claude Code calling `zchan_fetch` | Agents don't need a shell. Zchan ships an MCP server, so an assistant like Claude can pay as it works. The person who runs it sets a price cap and a budget, and nothing the model reads can raise them. |
| 1:25–1:42 | Playground: shield out, then the close's transaction facts | When the work is done, the agent closes cooperatively, and both sides are paid back to shielded addresses. The chain saw a channel open and close, and its size. Not who paid whom, and none of the payments in between. |
| 1:42–2:00 | Landing "The public chain sees two transactions" section, then the final "Pay per request. Settle once." call to action | If the solver disappears, a refund signed before the channel was funded returns the agent's money at the lock height. Zchan: instant, private payments for agents, on Zcash. Try it at zchan.cash. |

## Voice-over for ElevenLabs

Paste this as one block. Acronyms and numbers are spelled the way they should be spoken, and the `<break>` tags leave room for shot changes. If the voice says "Zchan" wrongly, replace it with a phonetic spelling such as "Zee-chan".

```text
AI agents are starting to pay for things: an API call, a search, a tool. On most chains, every payment is a transaction, and every transaction is public. Zchan fixes both. <break time="0.8s" />

An agent funds one payment channel on Zcash, to a solver. The solver keeps one channel to each merchant. A payment is a newly signed channel state, not a transaction. <break time="0.8s" />

This is the live playground on Zcash testnet. My agent shields in funds, then opens a channel from its shielded Ironwood notes. These are real testnet transactions, and the only ones until the end. <break time="0.8s" />

Now it pays. The merchant answers H-T-T-P four-oh-two, over x four-oh-two or M-P-P. The agent gets a quote, signs the next state, and gets the data, in milliseconds. No block wait, no network fee. This merchant runs in proxy mode: an ordinary A-P-I behind payment, with no code changes. <break time="0.8s" />

Agents don't need a shell. Zchan ships an M-C-P server, so an assistant like Claude can pay as it works. The person who runs it sets a price cap and a budget, and nothing the model reads can raise them. <break time="0.8s" />

When the work is done, the agent closes cooperatively, and both sides are paid back to shielded addresses. The chain saw a channel open and close, and its size. Not who paid whom, and none of the payments in between. <break time="0.8s" />

If the solver disappears, a refund signed before the channel was funded returns the agent's money at the lock height. <break time="0.5s" /> Zchan: instant, private payments for agents, on Zcash. Try it at zchan dot cash.
```

## Older automated cut

`scripts/record.sh` still records the earlier captioned cut from a live run (Chrome and ffmpeg required) into `video/<network>/`. It predates MPP, proxy mode, shielded closes and MCP.
