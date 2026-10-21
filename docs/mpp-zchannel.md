# MPP method: `zchannel` (intent `charge`)

**Status: a local method.** `zchannel` is not registered in the HTTP Payment Methods registry. It follows the MPP core and charge drafts (`draft-httpauth-payment-01`, `draft-payment-intent-charge-00` at paymentauth.org, and the mpp.dev protocol pages, read 2026-10-21). Only zchan's merchant gate (`crates/zchan/src/gate.rs`) and agent (`crates/zchan/src/agent.rs`) speak it.

Every 402 from a zchan merchant offers the same invoice twice: as x402 v2 ([x402-zchannel.md](x402-zchannel.md)) and as this MPP challenge. A client pays with either one, and the invoice is served once whichever protocol presents it.

## Challenge

```http
HTTP/1.1 402 Payment Required
Cache-Control: no-store
WWW-Authenticate: Payment id="…", realm="merchant.example:7204", method="zchannel", intent="charge",
    request="<base64url JCS>", expires="2026-10-21T05:55:57Z", description="/api/search from merchant-d"
```

The decoded `request` (JCS, base64url without padding):

```json
{
  "amount": "3000",
  "currency": "zec",
  "description": "/api/search from merchant-d",
  "externalId": "<invoice id>",
  "methodDetails": {
    "invoice": { "body": { "id": "…", "merchant_id": "merchant-d", "amount": 3000, "resource": "/api/search", "expires_at": … },
                 "signer": "<merchant identity key>", "sig": "…" },
    "network": "zcash:regtest",
    "solver": "http://solver.example:7100"
  },
  "recipient": "merchant-d"
}
```

- **Amount:** in zatoshis.
- **Expiry:** `expires` is the invoice's expiry; the request does not repeat it, as the charge draft requires.
- **Binding:** the challenge `id` is `base64url(HMAC-SHA256(k, realm|method|intent|request|expires||))`, where `k` is derived from the merchant's identity key. Digest and opaque are not used.

## Credential

`Authorization: Payment <base64url JSON>`:

```json
{
  "challenge": { "id": "…", "realm": "…", "method": "zchannel", "intent": "charge",
                 "request": "…", "expires": "…", "description": "…" },
  "payload": { "ack": { "body": { "invoice_id": "…", "merchant_id": "merchant-d",
                                  "channel_id": "<funding txid>:0", "cumulative": 6000, "amount": 3000 },
                        "signer": "…", "sig": "…" } }
}
```

The client gets the acknowledgment the same way as under x402:
1. it verifies the invoice in `methodDetails`;
2. it gets a quote from `methodDetails.solver`;
3. it signs its next channel state;
4. the solver pays the merchant on its own channel and relays the merchant's acknowledgment.

## Verification and errors

| Check | Response on failure |
|---|---|
| `method` is `zchannel` and `intent` is `charge` | 400, `method-unsupported` |
| the realm is the merchant's, and the HMAC binding matches the echoed parameters | 402, fresh challenge, `invalid-challenge` |
| `expires` (plus the merchant's grace period) has not passed | 402, `payment-expired` |
| `request` decodes | 402, `malformed-credential` |
| the acknowledgment names the challenge's invoice, is signed by this merchant, and pays that invoice for this path | 402, `verification-failed` |
| the invoice has not been served | 402, `invalid-challenge` (already used) |
| the credential parses at all | 402, `malformed-credential` |

Failures carry an RFC 9457 body (`application/problem+json`, `type` = `https://paymentauth.org/problems/<code>`).

## Receipt

On success: `Payment-Receipt: <base64url JSON>`:

```json
{ "status": "success", "method": "zchannel", "timestamp": "2026-10-21T05:50:57Z",
  "reference": "<invoice id>", "channelId": "<funding txid>:0", "cumulative": 6000 }
```

`channelId` and `cumulative` are method-specific additions, which the core draft allows.

## Not implemented

- The `session` and `subscription` intents.
- `digest`, which binds a request body to the challenge. The draft says servers SHOULD use it for POST; proxy mode accepts POST without it.
- The `Payment-Authorization` header variant.
- Registering the method.
