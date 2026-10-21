//! The merchant's payment gate, shared by the demo routes and proxy mode.
//!
//! An unpaid request gets 402 with one fresh invoice offered twice: as x402 v2
//! (`PAYMENT-REQUIRED`) and as an MPP challenge (`WWW-Authenticate: Payment`). A paid
//! request carries either an x402 `PAYMENT-SIGNATURE` or an MPP `Authorization: Payment`
//! credential; both hold the merchant's own signed acknowledgment of the channel state
//! that paid the invoice. The invoice is served once, whichever protocol presents it.
//!
//! Proxy mode (`--upstream URL --routes "/a=PRICE,/b/*=PRICE"`): requests that match no
//! merchant route are forwarded to the upstream service. Priced paths are gated first;
//! other paths pass through free. If the upstream fails (5xx or unreachable), the
//! invoice is released so the same credential can be presented again.

use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use zchan_tx::keys;

use crate::{
    ctx::Ctx,
    mpp::{self, ChargeRequest, Challenge, Credential, MethodDetails, Receipt},
    store::{self, InvoiceRow},
    web,
    wire::{now, random_id, Ack, Invoice, Signed, ACK, INVOICE},
    x402::{self, PaymentPayload, PaymentRequired, PaymentRequirements, ResourceInfo, SettlementResponse, ZchannelExtra},
};

/// Largest request body proxy mode forwards.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

/// A request that has paid: the invoice, its acknowledgment, and the protocol's
/// response header (`PAYMENT-RESPONSE` or `Payment-Receipt`).
pub struct Paid {
    pub invoice: Signed<Invoice>,
    pub ack: Signed<Ack>,
    pub headers: HeaderMap,
}

pub fn issue(ctx: &Ctx, amount: u64, resource: &str) -> crate::Result<Signed<Invoice>> {
    let inv = Signed::sign(
        INVOICE,
        Invoice {
            id: random_id(),
            merchant_id: ctx.cfg.name.clone(),
            amount,
            resource: resource.into(),
            expires_at: now() + ctx.cfg.invoice_ttl,
        },
        &ctx.identity(),
    );
    ctx.store.tx(|c| store::invoice_put(c, &InvoiceRow { invoice: inv.clone(), status: "open".into(), ack: None }))?;
    Ok(inv)
}

fn hv(v: &str) -> HeaderValue {
    HeaderValue::from_str(v).expect("ASCII header value")
}

/// The key that binds MPP challenge ids (derived from the identity key).
fn mpp_secret(ctx: &Ctx) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"zchan/mpp/v1")
        .chain_update(ctx.identity().to_secret_bytes())
        .finalize()
        .into()
}

fn realm(ctx: &Ctx) -> String {
    let u = ctx.cfg.base_url.split("://").nth(1).unwrap_or(&ctx.cfg.base_url);
    u.split('/').next().unwrap_or(u).to_string()
}

pub struct Failure {
    /// x402 `errorReason`, for `PAYMENT-RESPONSE`.
    pub x402: Option<String>,
    /// MPP problem code and detail, for the body.
    pub mpp: Option<(String, String)>,
}

/// 402 with a fresh invoice for `path`, offered over x402 and MPP.
pub fn offer(ctx: &Ctx, path: &str, price: u64, failure: Option<Failure>) -> Response {
    let inv = match issue(ctx, price, path) {
        Ok(i) => i,
        Err(e) => return web::ApiError::from(e).into_response(),
    };
    let description = format!("{path} from {}", ctx.cfg.name);
    let x402_error = failure.as_ref().and_then(|f| f.x402.clone());
    let pr = PaymentRequired {
        x402_version: x402::VERSION,
        error: Some(x402_error.clone().unwrap_or_else(|| format!("{} header is required", x402::PAYMENT_SIGNATURE))),
        resource: ResourceInfo {
            url: format!("{}{path}", ctx.cfg.base_url),
            description: Some(description.clone()),
            mime_type: None,
        },
        accepts: vec![PaymentRequirements {
            scheme: x402::SCHEME.into(),
            network: ctx.network.clone(),
            amount: price.to_string(),
            asset: "ZEC".into(),
            pay_to: ctx.cfg.name.clone(),
            max_timeout_seconds: ctx.cfg.invoice_ttl,
            extra: ZchannelExtra { payment_flow: "upfront".into(), invoice: inv.clone(), solver: ctx.cfg.solver_url.clone() },
        }],
    };
    let charge = ChargeRequest {
        amount: price.to_string(),
        currency: "zec".into(),
        recipient: ctx.cfg.name.clone(),
        description: Some(description),
        external_id: inv.body.id.clone(),
        method_details: MethodDetails { invoice: inv.clone(), solver: ctx.cfg.solver_url.clone(), network: ctx.network.clone() },
    };
    let challenge = Challenge::new(&mpp_secret(ctx), &realm(ctx), &charge, &mpp::rfc3339(inv.body.expires_at));
    let mut h = HeaderMap::new();
    h.insert(x402::PAYMENT_REQUIRED, hv(&x402::encode(&pr)));
    h.insert(header::WWW_AUTHENTICATE, hv(&challenge.header()));
    h.insert(header::CACHE_CONTROL, hv("no-store"));
    if let Some(reason) = x402_error {
        let f = SettlementResponse { success: false, error_reason: Some(reason), transaction: String::new(), network: ctx.network.clone(), amount: None };
        h.insert(x402::PAYMENT_RESPONSE, hv(&x402::encode(&f)));
    }
    match failure.and_then(|f| f.mpp) {
        Some((code, detail)) => {
            h.insert(header::CONTENT_TYPE, hv("application/problem+json"));
            let body = mpp::problem(&code, &code.replace('-', " "), &detail);
            (StatusCode::PAYMENT_REQUIRED, h, body.to_string()).into_response()
        }
        None => (StatusCode::PAYMENT_REQUIRED, h, Json(json!({}))).into_response(),
    }
}

/// Marks the invoice an acknowledgment pays as served, if it is paid with exactly that
/// acknowledgment and is for `path`. Errors are x402-style reasons.
fn serve(ctx: &Ctx, path: &str, ack: &Signed<Ack>) -> Result<Signed<Invoice>, String> {
    if ack.verify_by(ACK, &keys::pubkey(&ctx.identity())).is_err() || ack.body.merchant_id != ctx.cfg.name {
        return Err("invalid_acknowledgment".into());
    }
    ctx.store
        .tx(|c| {
            let Some(row) = store::invoice_get(c, &ack.body.invoice_id)? else {
                return Ok(Err("unknown_invoice".to_string()));
            };
            if row.invoice.body.resource != path {
                return Ok(Err("resource_mismatch".into()));
            }
            match row.status.as_str() {
                "paid" if row.ack.as_ref() == Some(ack) => {
                    store::invoice_put(c, &InvoiceRow { status: "served".into(), ..row.clone() })?;
                    store::event(c, "resource_served", json!({"invoice": row.invoice.body.id, "resource": path}))?;
                    Ok(Ok(row.invoice))
                }
                "served" => Ok(Err("invoice_already_used".into())),
                _ => Ok(Err("unpaid".into())),
            }
        })
        .unwrap_or_else(|e| Err(format!("server_error: {e}")))
}

/// Releases a served invoice back to paid (the upstream failed to serve it).
fn release(ctx: &Ctx, invoice_id: &str) {
    let _ = ctx.store.tx(|c| {
        if let Some(row) = store::invoice_get(c, invoice_id)? {
            if row.status == "served" {
                store::invoice_put(c, &InvoiceRow { status: "paid".into(), ..row })?;
                store::event(c, "invoice_released", json!({"invoice": invoice_id}))?;
            }
        }
        Ok(())
    });
}

fn x402_paid(ctx: &Ctx, path: &str, pp: &PaymentPayload) -> Result<Signed<Invoice>, String> {
    if pp.x402_version != x402::VERSION {
        return Err("unsupported_version".into());
    }
    if pp.accepted.scheme != x402::SCHEME || pp.accepted.network != ctx.network {
        return Err("unsupported_scheme_or_network".into());
    }
    if pp.accepted.extra.invoice.body.id != pp.payload.ack.body.invoice_id {
        return Err("invoice_mismatch".into());
    }
    serve(ctx, path, &pp.payload.ack)
}

/// MPP checks; errors are (problem code, detail).
fn mpp_paid(ctx: &Ctx, path: &str, cred: &Credential) -> Result<Signed<Invoice>, (String, String)> {
    let c = &cred.challenge;
    if c.method != mpp::METHOD || c.intent != mpp::INTENT {
        return Err(("method-unsupported".into(), format!("{}/{} is not offered", c.method, c.intent)));
    }
    if c.realm != realm(ctx) || c.binding(&mpp_secret(ctx)) != c.id {
        return Err(("invalid-challenge".into(), "challenge does not match its binding".into()));
    }
    let expires = c.expires.as_deref().and_then(mpp::parse_rfc3339).unwrap_or(0);
    if now() > expires + ctx.cfg.invoice_grace {
        return Err(("payment-expired".into(), "challenge expired".into()));
    }
    let charge = c.charge().map_err(|e| ("malformed-credential".to_string(), e.to_string()))?;
    let ack = &cred.payload.ack;
    if charge.external_id != ack.body.invoice_id || charge.method_details.invoice.body.id != ack.body.invoice_id {
        return Err(("verification-failed".into(), "acknowledgment is for another invoice".into()));
    }
    serve(ctx, path, ack).map_err(|r| match r.as_str() {
        "invoice_already_used" => ("invalid-challenge".into(), "challenge already used".into()),
        _ => ("verification-failed".into(), r),
    })
}

/// Checks the request's payment for `path` at `price`: `Ok` with the response headers to
/// add, or the 402 (or 400) response to send.
pub fn check(ctx: &Ctx, path: &str, price: u64, headers: &HeaderMap) -> Result<Paid, Response> {
    if let Some(h) = headers.get(x402::PAYMENT_SIGNATURE) {
        let pp: PaymentPayload = match h.to_str().map_err(|e| e.to_string()).and_then(|s| x402::decode(s).map_err(|e| e.to_string())) {
            Ok(p) => p,
            Err(e) => {
                return Err((StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid payment payload: {e}")}))).into_response())
            }
        };
        return match x402_paid(ctx, path, &pp) {
            Ok(invoice) => {
                let mut h = HeaderMap::new();
                let s = SettlementResponse {
                    success: true,
                    error_reason: None,
                    transaction: String::new(),
                    network: ctx.network.clone(),
                    amount: Some(invoice.body.amount.to_string()),
                };
                h.insert(x402::PAYMENT_RESPONSE, hv(&x402::encode(&s)));
                Ok(Paid { invoice, ack: pp.payload.ack, headers: h })
            }
            Err(reason) => Err(offer(ctx, path, price, Some(Failure { x402: Some(reason), mpp: None }))),
        };
    }
    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).filter(|h| h.starts_with("Payment "));
    if let Some(a) = auth {
        let cred = match Credential::parse(a) {
            Ok(c) => c,
            Err(e) => {
                return Err(offer(ctx, path, price, Some(Failure { x402: None, mpp: Some(("malformed-credential".into(), e.to_string())) })))
            }
        };
        return match mpp_paid(ctx, path, &cred) {
            Ok(invoice) => {
                let ack = cred.payload.ack;
                let r = Receipt {
                    status: "success".into(),
                    method: mpp::METHOD.into(),
                    timestamp: mpp::rfc3339(now()),
                    reference: invoice.body.id.clone(),
                    channel_id: ack.body.channel_id.clone(),
                    cumulative: ack.body.cumulative,
                };
                let mut h = HeaderMap::new();
                h.insert(HeaderName::from_static("payment-receipt"), hv(&r.header()));
                Ok(Paid { invoice, ack, headers: h })
            }
            Err((code, detail)) if code == "method-unsupported" => {
                Err((StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/problem+json")], mpp::problem(&code, "method unsupported", &detail).to_string()).into_response())
            }
            Err(m) => Err(offer(ctx, path, price, Some(Failure { x402: None, mpp: Some(m) }))),
        };
    }
    Err(offer(ctx, path, price, None))
}

/// The price of `path` under `--routes` (first match; `*` at the end matches a prefix).
pub fn price_for(routes: &[(String, u64)], path: &str) -> Option<u64> {
    routes.iter().find_map(|(p, price)| {
        let hit = match p.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == p,
        };
        hit.then_some(*price)
    })
}

/// Parses `"/a=100,/b/*=5"`.
pub fn parse_routes(s: &str) -> Result<Vec<(String, u64)>, String> {
    s.split(',')
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            let (p, v) = r.trim().rsplit_once('=').ok_or_else(|| format!("route {r}: want PATH=PRICE"))?;
            let price: u64 = v.parse().map_err(|_| format!("route {r}: bad price"))?;
            if !p.starts_with('/') || price == 0 {
                return Err(format!("route {r}: path must start with / and price be positive"));
            }
            Ok((p.to_string(), price))
        })
        .collect()
}

const HOP: [&str; 9] =
    ["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade", "host"];

/// Proxy mode: the router's fallback. Gates priced paths, then forwards to the upstream.
pub async fn proxy(State(ctx): State<Arc<Ctx>>, req: Request) -> Response {
    let Some(upstream) = ctx.cfg.upstream.clone() else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response();
    };
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let mut extra = HeaderMap::new();
    let mut paid_invoice = None;
    if let Some(price) = price_for(&ctx.cfg.routes, &path) {
        match check(&ctx, &path, price, &parts.headers) {
            Ok(p) => {
                extra = p.headers;
                paid_invoice = Some(p.invoice.body.id);
            }
            Err(resp) => return resp,
        }
    }
    let bytes = match to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"error": e.to_string()}))).into_response(),
    };
    let url = format!("{}{}", upstream.trim_end_matches('/'), parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/"));
    let mut fwd = reqwest::header::HeaderMap::new();
    for (k, v) in &parts.headers {
        let n = k.as_str();
        let payment_auth = n == "authorization" && v.as_bytes().starts_with(b"Payment ");
        if HOP.contains(&n) || n == "content-length" || n == "payment-signature" || payment_auth {
            continue;
        }
        if let (Ok(k), Ok(v)) = (reqwest::header::HeaderName::from_bytes(k.as_ref()), reqwest::header::HeaderValue::from_bytes(v.as_bytes())) {
            fwd.append(k, v);
        }
    }
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET);
    let resp = match ctx.http.request(method, &url).headers(fwd).body(bytes.to_vec()).send().await {
        Ok(r) => r,
        Err(e) => {
            if let Some(id) = &paid_invoice {
                release(&ctx, id);
            }
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("upstream: {e}")}))).into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if status.is_server_error() {
        if let Some(id) = &paid_invoice {
            release(&ctx, id);
        }
    }
    let mut out = HeaderMap::new();
    for (k, v) in resp.headers() {
        if HOP.contains(&k.as_str()) || k.as_str() == "content-length" {
            continue;
        }
        if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_ref()), HeaderValue::from_bytes(v.as_bytes())) {
            out.append(k, v);
        }
    }
    if !status.is_server_error() {
        out.extend(extra);
    }
    let body = resp.bytes().await.unwrap_or_default();
    (status, out, Body::from(body)).into_response()
}

/// Whether the request may use `/admin/*`: always without `--admin-token`, otherwise only
/// with `X-Admin-Token: <token>`.
pub fn admin(ctx: &Ctx, headers: &HeaderMap) -> Result<(), web::ApiError> {
    match &ctx.cfg.admin_token {
        None => Ok(()),
        Some(t) if headers.get("x-admin-token").and_then(|v| v.to_str().ok()) == Some(t.as_str()) => Ok(()),
        Some(_) => Err(web::err(StatusCode::UNAUTHORIZED, "admin token required")),
    }
}

/// A demo resource body.
pub fn demo_body(ctx: &Ctx, path: &str, invoice: &Signed<Invoice>) -> Value {
    let name = path.trim_start_matches("/paid/");
    json!({
        "resource": path, "merchant": ctx.cfg.name, "invoice": invoice.body.id,
        "data": format!("{} · paid content from {} at {}", name, ctx.cfg.name, now()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_and_prices() {
        let r = parse_routes("/api/search=3000, /api/*=500").unwrap();
        assert_eq!(price_for(&r, "/api/search"), Some(3000));
        assert_eq!(price_for(&r, "/api/echo"), Some(500));
        assert_eq!(price_for(&r, "/free"), None);
        assert!(parse_routes("api=1").is_err());
        assert!(parse_routes("/a=0").is_err());
    }
}
