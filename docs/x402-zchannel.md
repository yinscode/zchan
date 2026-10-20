# Scheme: `zchannel`

**Status: a local extension.** This scheme is not registered with x402. No public facilitator or client supports it; only zchan's merchant middleware (`crates/zchan/src/merchant.rs`) and agent client (`crates/zchan/src/agent.rs`) speak it. It follows the x402 v2 types and HTTP transport (x402-foundation/x402 `specs/x402-specification-v2.md` and `specs/transports-v2/http.md`, read at commit `faa1c694`, 2026-10-15).

## Summary

A merchant is paid through a payment channel that a solver keeps open to it. The agent pays the solver through its own channel. The x402 credential is the merchant's own signed acknowledgment that a channel state paid the invoice. No transaction is broadcast per payment: settlement happens later, when a channel closes.

The scheme's payment flow is `upfront` (spec §6.1). The channel state is committed, and the merchant has signed for it, before the resource runs. There is no facilitator `/verify` or `/settle` call: the merchant checks the acknowledgment itself.

## Use cases

- Agents paying per request (APIs, data, inference) in ZEC, without an on-chain transaction per request.
- One agent channel paying many merchants, and one merchant channel receiving from many agents (the solver nets them).

## `PaymentRequirements` (in the `PAYMENT-REQUIRED` header of a 402)

| Field | Value |
|---|---|
| `scheme` | `zchannel` |
| `network` | `zcash:regtest`, `zcash:testnet` (labels local to zchan; Zcash has no CAIP-2 namespace in x402) |
| `amount` | zatoshis, decimal string; equals the invoice amount |
| `asset` | `ZEC` |
| `payTo` | the merchant id |
| `maxTimeoutSeconds` | the invoice lifetime |
| `extra.paymentFlow` | `upfront` |
| `extra.invoice` | the signed invoice (`Signed<Invoice>`: `id`, `merchant_id`, `amount`, `resource`, `expires_at`, plus the merchant's key and signature) |
| `extra.solver` | base URL of the solver that pays this merchant |

Each 402 carries a fresh invoice. Its `id` (16 random bytes) is also its single-use nonce.

## `PaymentPayload` (in the `PAYMENT-SIGNATURE` header)

```json
{
  "x402Version": 2,
  "resource": { "url": "http://merchant/paid/weather", "mimeType": "application/json" },
  "accepted": { "...": "the PaymentRequirements chosen, unchanged" },
  "payload": {
    "ack": {
      "body": { "invoice_id": "…", "merchant_id": "merchant-a", "channel_id": "<funding txid>:0",
                "cumulative": 40000, "amount": 20000 },
      "signer": "<merchant identity key>", "sig": "<ECDSA over SHA-256('zchan/ack/v1' ‖ JSON(body))>"
    }
  }
}
```

## How the client gets the acknowledgment

1. The client verifies the invoice signature and that `amount` and `payTo` match it.
2. It asks `extra.solver` for a quote (`POST /quote`), then signs its next channel state for `amount + fee − credit` and sends it (`POST /pay`, with a payment id that makes retries idempotent).
3. The solver signs the next state of its channel to the merchant and pushes it (`POST /channels/{id}/update`). The merchant verifies it pays exactly the held level plus the invoice amount, stores it, and returns the acknowledgment.
4. The solver relays the acknowledgment; the client repeats the request with it.

## Merchant verification

The merchant serves the resource once if all of these hold. Otherwise it answers 402 with a fresh `PAYMENT-REQUIRED` and a failed `PAYMENT-RESPONSE` naming the reason:

| Check | `errorReason` on failure |
|---|---|
| `x402Version` is 2 | `unsupported_version` |
| `scheme` is `zchannel` and `network` is the merchant's | `unsupported_scheme_or_network` |
| the acknowledgment is signed by this merchant's identity key and names this merchant | `invalid_acknowledgment` |
| it names the invoice in `accepted.extra.invoice` | `invoice_mismatch` |
| the invoice exists | `unknown_invoice` |
| the invoice's resource is the requested path | `resource_mismatch` |
| the invoice is paid with exactly this acknowledgment | `unpaid` |
| it has not been served | `invoice_already_used` |

A payload that does not decode is answered with 400.

## `SettlementResponse` (in the `PAYMENT-RESPONSE` header)

`{"success": true, "transaction": "", "network": "zcash:regtest", "amount": "20000"}`. `transaction` is empty because a channel payment broadcasts nothing (spec §5.3.2).

## Appendix: what is and is not guaranteed

- **Merchant:** it holds a payer-signed state for every acknowledgment it gives, and can broadcast the highest one before the refund height. This is chain-enforced.
- **Agent:** it signs before the merchant is paid. The solver could keep the state and not pay the merchant; the loss is bounded by one payment, because each agent channel has one payment in flight at a time. This is trust in the solver, not chain-enforced.
- **Atomicity:** the agent's state and the merchant's state are separate transactions on separate channels. Nothing binds them on chain.
