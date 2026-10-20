"""Builds the demo video's captioned slides from recorded output and screenshots.

Usage: python3 slides.py OUT_DIR NETWORK. Reads OUT_DIR/{demo,close}.txt and
OUT_DIR/shots/*.png; writes OUT_DIR/slides/NN.html and OUT_DIR/slides.txt (file,
seconds), which record.sh renders and joins.
"""

import html
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
network = sys.argv[2]
shots = out / "shots"
slides = out / "slides"
slides.mkdir(parents=True, exist_ok=True)

CSS = """
*{box-sizing:border-box}body{margin:0;width:1920px;height:1080px;background:#151513;color:#ecebe6;
font:28px/1.45 ui-sans-serif,system-ui,sans-serif;display:flex;flex-direction:column}
header{padding:26px 56px;font-size:24px;color:#a19f97;display:flex;justify-content:space-between}
header b{color:#fb923c}main{flex:1;display:flex;align-items:center;justify-content:center;padding:0 56px;min-height:0}
footer{padding:28px 56px 40px;font-size:34px;line-height:1.35;border-top:1px solid #2e2d29;min-height:170px}
img{max-width:100%;max-height:100%;border-radius:12px;border:1px solid #2e2d29}
pre{margin:0;width:100%;background:#0c0c0b;border:1px solid #2e2d29;border-radius:12px;padding:28px 34px;
font:22px/1.5 ui-monospace,Menlo,monospace;white-space:pre-wrap;word-break:break-all;color:#d6d3c8}
h1{font-size:96px;margin:0 0 18px}.sub{font-size:38px;color:#a19f97}.c{text-align:center}
table{border-collapse:collapse;font-size:30px}td,th{padding:14px 22px;border-bottom:1px solid #2e2d29;text-align:left}
th{color:#a19f97}.ok{color:#4ade80}.bad{color:#f87171}
"""


def page(body, caption):
    return (
        f"<!doctype html><html><head><meta charset=utf-8><style>{CSS}</style></head><body>"
        f"<header><span><b>zchan</b> · payment channels for agents on Zcash</span><span>{network}</span></header>"
        f"<main>{body}</main><footer>{caption}</footer></body></html>"
    )


def lines(name, start, stop=None):
    text = (out / name).read_text().splitlines()
    keep, on = [], False
    for l in text:
        if l.startswith(start):
            on = True
        if stop and l.startswith(stop):
            break
        if on:
            keep.append(l)
    return "\n".join(keep)


def term(text):
    return f"<pre>{html.escape(text)}</pre>"


def shot(name):
    return f"<img src='{(shots / name).resolve().as_uri()}'>"


deck = [
    (6, "<div class=c><h1>zchan</h1><div class=sub>One channel per agent. One channel per merchant.<br>Many payments, no transaction per payment.</div></div>",
     f"A Zecathon demo, recorded on Zcash {network}."),
    (12, term(lines("demo.txt", "== solver opens", "== agent-1 buys")),
     "The solver opens one channel to each merchant, and each agent opens one channel to the solver. "
     "Every refund is signed by both sides before its funding transaction is signed."),
    (16, term(lines("demo.txt", "== agent-1 buys", "== running")),
     "Each merchant answers HTTP 402 with an x402 v2 offer. The agent signs its next channel state; the solver pays the "
     "merchant on its own channel and relays the merchant's signed receipt. Agent-1 pays two merchants through one channel."),
    (14, shot("dashboard-1.png"),
     "The solver's dashboard: two agents bought from merchant A and advanced the same solver→A channel. Every channel reconciles."),
    (11, shot("checkout-waiting.png"),
     "At a counter, the merchant's point of sale shows a QR code. It carries only an invoice reference: no key, no bearer token."),
    (7, shot("handoff.png"),
     "The phone's camera opens the invoice page, which hands the invoice link to the buyer's own agent."),
    (12, shot("approve.png"),
     "The agent fetches the signed invoice, verifies it, gets a quote, and shows merchant, amount, solver fee and expiry before approval."),
    (7, shot("checkout-paid.png"),
     "Approved: the merchant has signed for the channel state that paid it, and the point of sale shows Paid."),
    (12, term(lines("close.txt", "== settle")),
     "End of day: merchant A closes cooperatively and its share goes to a shielded Ironwood address; "
     "merchant B and the agent channels close transparently. Each close is one transaction."),
    (6, shot("dashboard-2.png"), "All channels settled; the books still reconcile."),
    (13,
     "<table><tr><th>Exposure</th><th>Enforced by</th></tr>"
     "<tr><td>Agent's unspent capacity</td><td class=ok>chain: presigned refund</td></tr>"
     "<tr><td>What a merchant has acknowledged</td><td class=ok>chain: it holds the signed state</td></tr>"
     "<tr><td>Solver capital in merchant channels</td><td class=ok>chain: presigned refund</td></tr>"
     "<tr><td>Agent paid, solver does not forward</td><td class=bad>trust in the solver · one payment at a time</td></tr>"
     "<tr><td>The two legs together</td><td class=bad>not atomic</td></tr>"
     "<tr><td>Funding and transparent closes</td><td class=bad>public amounts and parties</td></tr></table>",
     "Honest limits: the legs are not atomic, the solver is trusted with one payment per agent at a time, and funding is "
     "transparent. Refunds and closes are enforced by the chain."),
    (4, "<div class=c><h1>zchan</h1><div class=sub>docs/design.md · docs/x402-zchannel.md · scripts/demo.sh</div></div>",
     "Source, tests and the full trust model are in the repository."),
]

index = []
for i, (secs, body, caption) in enumerate(deck):
    p = slides / f"{i:02d}.html"
    p.write_text(page(body, html.escape(caption)))
    index.append(f"{p}\t{secs}")
(out / "slides.txt").write_text("\n".join(index) + "\n")
print(sum(s for s, _, _ in deck), "seconds,", len(deck), "slides")
