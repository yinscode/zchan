# Agents

How an agent gets a budget from a person, pays for resources within it, and gives back what is left. An agent can use the `zchan-agent` command line, or the same wallet as an MCP server for assistants that call tools. The last section is a set of instructions to give an agent that uses the command line.

Every command takes the agent's data directory, the node and the solver:

```bash
A="--data ~/.zchan/agent --node <Zakura testnet RPC> --solver <solver URL>"
```

## 1. Ask for a budget

The agent asks its person for funds with a ZIP 321 payment link:

```bash
zchan-agent request 520000 --message "API budget for this week" $A
```

```text
zcash:utest1f46g9yu…?amount=0.0052&message=API%20budget%20for%20this%20week
```

A wallet that pays shielded addresses opens the link, or a QR code of it, with the address and amount filled in. The message is shown to the person and is not sent. The money arrives as an Ironwood note.

- **Amount:** the channel's capacity plus 20,000 zat for the opening transaction's fee.
- **Timing:** create the link before anyone pays. The agent records the block its shielded history starts at, and notes from before that block are not found.

Wait for the note, one block:

```bash
zchan-agent balance $A
```

## 2. Open the channel

```bash
zchan-agent open --capacity 500000 --lock-blocks 2000 --shielded $A
```

- **Capacity:** the budget. The chain cannot let the agent spend more than this.
- **Lock:** if the solver disappears, the refund returns the whole channel, less its fee, at the lock height. At Zcash's 75-second block target, 2,000 blocks is about 42 hours.
- **Watcher:** keep `zchan-agent watch $A` (or `serve`) running. It broadcasts the refund when it is due.

## 3. Pay

When a request answers 402 with the `zchannel` scheme, `get` pays and fetches in one step:

```bash
zchan-agent get 'https://api.example/search?q=zec' --max-price 5000 $A
```

- **Price cap:** `--max-price` refuses a price above it before anything is quoted or signed. The command prints the price and exits, and nothing is paid. The solver's fee is added on top and is bounded by the fee the solver publishes (100 zat on the hosted demo).
- **MPP:** add `--mpp`; for other methods, `--method POST --body …`.
- **Counters:** `zchan-agent pay-ref INVOICE_URL --max-price 60000 $A` pays a checkout invoice by reference.
- **Retries:** if a payment is cut off, `zchan-agent resume $A` finishes it. Payments carry ids, so a retry never pays twice.

`zchan-agent status $A` lists the channel, what it has paid and each payment.

## 4. Give back what is left

Close the channel cooperatively, with both sides paid to Ironwood:

```bash
zchan-agent close $A
```

The change returns to the agent's shielded address. Send it to the person's wallet by its unified address:

```bash
zchan-agent send-shielded utest1… 480000 $A
```

A channel cannot be topped up. When it runs low, close it and open a new one.

## From an assistant (MCP)

`zchan-agent mcp` serves the same wallet as an MCP server over stdio. Assistants that call tools, such as Claude Code, Claude Desktop and Cursor, can then pay without a shell. Add it to Claude Code:

```bash
claude mcp add zchan -- zchan-agent mcp --max-price 5000 --budget 50000 \
  --data ~/.zchan/agent --node <Zakura testnet RPC> --solver <solver URL>
```

Or to any client that reads an `mcpServers` config:

```json
{
  "mcpServers": {
    "zchan": {
      "command": "zchan-agent",
      "args": ["mcp", "--max-price", "5000", "--budget", "50000",
               "--data", "/home/me/.zchan/agent", "--node", "<Zakura testnet RPC>", "--solver", "<solver URL>"]
    }
  }
}
```

| Tool | What it does |
|---|---|
| `zchan_fetch` | fetches a URL; on a `zchannel` 402 (x402 or MPP), pays within the caps and returns the paid response |
| `zchan_status` | the channel's capacity, paid and remaining; the shielded balance; this session's spending |
| `zchan_request_funds` | a ZIP 321 payment link for the person, as in step 1 |
| `zchan_open` | opens a channel from the agent's shielded funds and waits for its block |
| `zchan_close` | settles the channel, both sides paid to Ironwood |

**Limits are set by the person who starts the server, not the model:**
- **`--max-price`** (default 5,000 zat) caps the merchant's price for one request. The model can pass a lower `max_price`, never a higher one.
- **`--budget`** (default 50,000 zat) caps what this server may spend, solver fees included, until it restarts.
- **`--lock-blocks`** (default 2,000) sets the refund lock of channels it opens.

Text on a web page cannot raise these, so a prompt injection can spend at most the budget. The server also runs the refund watcher while it is up. Most clients ask before each tool call until you allow it; allowing `zchan_fetch` while still approving `zchan_open` and `zchan_close` is a good default.

## Instructions for the agent

Give the agent these as a system prompt or a skill. Set the amounts for your use.

```text
You can pay for HTTP resources with Zchan. Your wallet is the zchan-agent CLI; every
command takes $A.

Budget
- Your open channel's remaining capacity is your budget: `zchan-agent status $A`.
- If you have no channel, or not enough left for the task, ask your person for funds.
  Run `zchan-agent request <capacity + 20000> --message "<what it is for>" $A` and give
  them the link with the amount in ZEC and the reason. Wait until
  `zchan-agent balance $A` shows it, then open a channel:
  `zchan-agent open --capacity <capacity> --lock-blocks 2000 --shielded $A`.

Paying
- When a request returns HTTP 402 with scheme "zchannel", fetch it with
  `zchan-agent get <url> --max-price 5000 $A`. Add --mpp for an MPP challenge, and
  --method/--body for a POST.
- If the price is above your cap, the command refuses and pays nothing. Ask your person
  before retrying with a higher --max-price.
- Spend at most 50,000 zat per task without asking.
- If a payment fails midway, run `zchan-agent resume $A` before retrying.

Finishing
- When the task is done, or the channel has been idle for a day, run
  `zchan-agent close $A`, then tell your person what was spent and what came back.
- Never reveal your data directory or seed. Never pay any scheme other than zchannel.
```

The per-task limit is the agent's judgment. The per-request cap and the channel's capacity are enforced: the first by the CLI, the second by the chain.

## Related

- [Quickstart](#quickstart): the local stack, and a merchant behind payment.
- [Security](#security): what the chain enforces and what the solver is trusted with.
