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


def read(name):
    return (out / name).read_text()


def term(text):
    return f"<pre>{html.escape(text)}</pre>"


def shot(name):
    return f"<img src='{(shots / name).resolve().as_uri()}'>"


deck = [
    (5, "<div class=c><h1>zchan</h1><div class=sub>Payment channels for AI agents on Zcash.<br>Pay per request. Settle once. Shielded in, shielded out.</div></div>",
     f"A Zecathon demo, recorded live on Zcash {network}."),
    (12, term(lines("demo.txt", "== funding", "== purchases")),
     "The solver opens one channel to each merchant and each agent opens one channel to the solver. Every channel is "
     "funded from shielded Ironwood notes: zero transparent inputs, and each refund is signed before its funding."),
    (9, term(read("offer.txt")),
     "Merchant D runs in proxy mode in front of an existing API. One 402 response offers the same signed invoice "
     "two ways: x402 v2 and MPP."),
    (15, term(lines("demo.txt", "== purchases", "== running")),
     "Agents pay over x402 and MPP, including a POST through the proxy. Each payment is a new signed channel state; "
     "no transaction is broadcast. Agent-1 pays two merchants through one channel, and both agents share the channel to A."),
    (10, shot("dashboard-1.png"),
     "The solver's dashboard: every channel's level, every payment, and books that reconcile."),
    (9, shot("checkout-waiting.png"),
     "At a counter, the point of sale shows a QR code carrying only an invoice reference."),
    (5, shot("handoff.png"), "The phone opens the invoice and hands it to the buyer's own agent."),
    (8, shot("approve.png"), "The agent verifies the signed invoice, gets a quote, and shows the merchant, amount and fee."),
    (5, shot("checkout-paid.png"), "Approved. The merchant signed for the state that paid it."),
    (15, term(lines("close.txt", "== settle")),
     "Every channel closes cooperatively with both sides paid to Ironwood: zero transparent outputs. Agents start their "
     "own close and co-sign only after decrypting their change. The chain sees capacity and timing, not the split."),
    (6, shot("dashboard-2.png"), "All channels settled; the books still reconcile."),
    (16,
     "<table><tr><th>What the chain sees</th><th></th></tr>"
     "<tr><td>Individual payments</td><td class=ok>nothing: off chain</td></tr>"
     "<tr><td>Where channel money came from</td><td class=ok>hidden: funded from notes</td></tr>"
     "<tr><td>How a channel was split</td><td class=ok>hidden: both sides closed to Ironwood</td></tr>"
     "<tr><td>Channel capacity, open and close times</td><td class=bad>public</td></tr>"
     "<tr><td>Refunds and unilateral closes</td><td class=bad>transparent (no time locks in Ironwood)</td></tr>"
     "<tr><td>Agent ↔ merchant mapping</td><td class=bad>seen by the solver; legs not atomic</td></tr></table>",
     "Honest limits: the solver is trusted with one payment per agent at a time and sees the payment graph. "
     "Refunds stay transparent. Mainnet stays disabled until review."),
    (5, "<div class=c><h1>zchan</h1><div class=sub>docs/design.md · docs/privacy.md · docs/testnet.md · scripts/demo.sh</div></div>",
     "Every transaction in this video is in docs/testnet.md."),
]

index = []
for i, (secs, body, caption) in enumerate(deck):
    p = slides / f"{i:02d}.html"
    p.write_text(page(body, html.escape(caption)))
    index.append(f"{p}\t{secs}")
(out / "slides.txt").write_text("\n".join(index) + "\n")
print(sum(s for s, _, _ in deck), "seconds,", len(deck), "slides")
