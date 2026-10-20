//! A merchant: payee of the solver's channel, issuer of invoices, and an x402 resource
//! server. It serves a paid resource once per invoice, against its own signed
//! acknowledgment of the channel state that paid it.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use zchan_tx::{
    keys,
    tx::{self, Pay, Spec},
    txid_hex,
};

use crate::{
    channel::Terms,
    ctx::Ctx,
    payee,
    store::{self, InvoiceRow},
    web::{self, err, esc, page, zec, ApiResult},
    wire::{
        now, random_id, sig_from_hex, Ack, CoopCloseReq, CoopCloseResp, Info, Invoice, Signed, UpdateReq, ACK, INVOICE,
    },
    x402::{self, PaymentPayload, PaymentRequired, PaymentRequirements, ResourceInfo, SettlementResponse, ZchannelExtra},
    Error, Result,
};

pub fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .merge(payee::routes())
        .route("/info", get(info))
        .route("/channels/{id}/update", post(update))
        .route("/paid/{resource}", get(paid))
        .route("/invoices", post(new_invoice))
        .route("/invoices/{id}", get(invoice))
        .route("/checkout", get(checkout))
        .route("/admin/channels/{id}/close", post(admin_close))
        .route("/admin/channels/{id}/coop-close", post(admin_coop_close))
        .route("/status", get(status_html))
        .route("/status.json", get(status_json))
        .with_state(ctx)
}

pub async fn run(ctx: Arc<Ctx>) {
    loop {
        if let Err(e) = payee::tick(&ctx).await {
            eprintln!("payee tick: {e}");
        }
        tokio::time::sleep(ctx.cfg.poll).await;
    }
}

async fn info(State(ctx): State<Arc<Ctx>>) -> Json<Info> {
    Json(Info {
        role: "merchant".into(),
        id: ctx.cfg.name.clone(),
        pubkey: ctx.identity_hex(),
        network: ctx.network.clone(),
        deposit: ctx.deposit_address(),
        fee: 0,
        max_payment: 0,
    })
}

fn issue(ctx: &Ctx, amount: u64, resource: &str) -> Result<Signed<Invoice>> {
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
    ctx.store.tx(|c| {
        store::invoice_put(c, &InvoiceRow { invoice: inv.clone(), status: "open".into(), ack: None })
    })?;
    Ok(inv)
}

/// The solver pushes a state that pays one invoice; the answer is the signed
/// acknowledgment. Idempotent per invoice.
async fn update(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, Json(req): Json<UpdateReq>) -> ApiResult<Json<Signed<Ack>>> {
    if req.state.channel_id != id {
        return Err(err(StatusCode::BAD_REQUEST, "state is for another channel"));
    }
    if ctx.cfg.fault.as_deref() == Some("refuse_update") {
        return Err(err(StatusCode::CONFLICT, "refused (fault injection)"));
    }
    let lock = ctx.lock(&id);
    let _g = lock.lock().await;
    let (inv, row) = ctx.store.read(|c| Ok((store::invoice_get(c, &req.invoice_id)?, store::channel_get(c, &id)?)))?;
    let inv = inv.ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown invoice"))?;
    if let Some(ack) = &inv.ack {
        if ack.body.channel_id == id && ack.body.cumulative == req.state.cumulative {
            return Ok(Json(ack.clone()));
        }
        return Err(err(StatusCode::CONFLICT, "invoice already paid"));
    }
    if now() > inv.invoice.body.expires_at + ctx.cfg.invoice_grace {
        return Err(err(StatusCode::GONE, "invoice expired"));
    }
    let row = row
        .filter(|r| r.role == "payee" && r.status == "open")
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no open channel {id}")))?;
    let want = row.cumulative + inv.invoice.body.amount;
    if req.state.cumulative != want {
        return Err(err(StatusCode::BAD_REQUEST, format!("state must be {want}: the held {} plus the invoice", row.cumulative)));
    }
    payee::verify(&ctx, &row, req.state.cumulative, &req.state.sig)?;
    let ack = Signed::sign(
        ACK,
        Ack {
            invoice_id: req.invoice_id.clone(),
            merchant_id: ctx.cfg.name.clone(),
            channel_id: id.clone(),
            cumulative: want,
            amount: inv.invoice.body.amount,
        },
        &ctx.identity(),
    );
    ctx.store.tx(|c| {
        store::channel_update(c, &id, |r| {
            r.cumulative = want;
            r.sig = Some(req.state.sig.clone());
        })?;
        store::state_log(c, &id, want, &req.state.sig, &req.invoice_id)?;
        store::invoice_put(c, &InvoiceRow { invoice: inv.invoice.clone(), status: "paid".into(), ack: Some(ack.clone()) })?;
        store::event(c, "invoice_paid", json!({"invoice": req.invoice_id, "amount": inv.invoice.body.amount, "channel": id, "cumulative": want}))
    })?;
    Ok(Json(ack))
}

fn header(v: &str) -> HeaderValue {
    HeaderValue::from_str(v).expect("base64 is a valid header value")
}

/// A 402 with a fresh invoice for `path`.
fn required(ctx: &Ctx, path: &str, error: Option<String>, failure: Option<SettlementResponse>) -> Response {
    let inv = match issue(ctx, ctx.cfg.price, path) {
        Ok(i) => i,
        Err(e) => return web::ApiError::from(e).into_response(),
    };
    let body = PaymentRequired {
        x402_version: x402::VERSION,
        error,
        resource: ResourceInfo {
            url: format!("{}{path}", ctx.cfg.base_url),
            description: Some(format!("{} from {}", path.trim_start_matches("/paid/"), ctx.cfg.name)),
            mime_type: Some("application/json".into()),
        },
        accepts: vec![PaymentRequirements {
            scheme: x402::SCHEME.into(),
            network: ctx.network.clone(),
            amount: ctx.cfg.price.to_string(),
            asset: "ZEC".into(),
            pay_to: ctx.cfg.name.clone(),
            max_timeout_seconds: ctx.cfg.invoice_ttl,
            extra: ZchannelExtra { payment_flow: "upfront".into(), invoice: inv, solver: ctx.cfg.solver_url.clone() },
        }],
    };
    let mut h = HeaderMap::new();
    h.insert(x402::PAYMENT_REQUIRED, header(&x402::encode(&body)));
    if let Some(f) = failure {
        h.insert(x402::PAYMENT_RESPONSE, header(&x402::encode(&f)));
    }
    (StatusCode::PAYMENT_REQUIRED, h, Json(json!({}))).into_response()
}

/// Checks a payment payload for `path`; on success marks its invoice served.
fn accept_payment(ctx: &Ctx, path: &str, pp: &PaymentPayload) -> std::result::Result<Signed<Invoice>, String> {
    if pp.x402_version != x402::VERSION {
        return Err("unsupported_version".into());
    }
    if pp.accepted.scheme != x402::SCHEME || pp.accepted.network != ctx.network {
        return Err("unsupported_scheme_or_network".into());
    }
    let ack = &pp.payload.ack;
    if ack.verify_by(ACK, &keys::pubkey(&ctx.identity())).is_err() || ack.body.merchant_id != ctx.cfg.name {
        return Err("invalid_acknowledgment".into());
    }
    if pp.accepted.extra.invoice.body.id != ack.body.invoice_id {
        return Err("invoice_mismatch".into());
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

/// `GET /paid/{resource}`: x402 v2, scheme `zchannel`.
async fn paid(State(ctx): State<Arc<Ctx>>, Path(resource): Path<String>, headers: HeaderMap) -> Response {
    let path = format!("/paid/{resource}");
    let Some(h) = headers.get(x402::PAYMENT_SIGNATURE) else {
        return required(&ctx, &path, Some(format!("{} header is required", x402::PAYMENT_SIGNATURE)), None);
    };
    let pp: PaymentPayload = match h.to_str().map_err(|e| e.to_string()).and_then(|s| x402::decode(s).map_err(|e| e.to_string())) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("invalid payment payload: {e}")}))).into_response(),
    };
    match accept_payment(&ctx, &path, &pp) {
        Ok(inv) => {
            let settled = SettlementResponse {
                success: true,
                error_reason: None,
                transaction: String::new(),
                network: ctx.network.clone(),
                amount: Some(inv.body.amount.to_string()),
            };
            let mut h = HeaderMap::new();
            h.insert(x402::PAYMENT_RESPONSE, header(&x402::encode(&settled)));
            let body = json!({
                "resource": path, "merchant": ctx.cfg.name, "invoice": inv.body.id,
                "data": format!("{} · paid content from {} at {}", resource, ctx.cfg.name, now()),
            });
            (StatusCode::OK, h, Json(body)).into_response()
        }
        Err(reason) => {
            let failure = SettlementResponse {
                success: false,
                error_reason: Some(reason.clone()),
                transaction: String::new(),
                network: ctx.network.clone(),
                amount: None,
            };
            required(&ctx, &path, Some(reason), Some(failure))
        }
    }
}

#[derive(serde::Deserialize)]
struct NewInvoice {
    amount: u64,
    resource: String,
}

async fn new_invoice(State(ctx): State<Arc<Ctx>>, Json(req): Json<NewInvoice>) -> ApiResult<Json<Signed<Invoice>>> {
    if req.amount == 0 {
        return Err(err(StatusCode::BAD_REQUEST, "amount must be positive"));
    }
    Ok(Json(issue(&ctx, req.amount, &req.resource)?))
}

/// The invoice as JSON; to a browser (a phone that scanned the checkout QR), a page that
/// hands the invoice link to the payer's own agent.
async fn invoice(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<Response> {
    let row = ctx
        .store
        .read(|c| store::invoice_get(c, &id))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown invoice"))?;
    let html = headers
        .get("accept")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    if !html {
        return Ok(Json(row).into_response());
    }
    let b = format!(
        r#"<h1>{name}</h1><p class=mute>{item}</p><div class=card><div class=big>{z} ZEC</div><p id=st class=mute>{status}</p>
<p>Pay with your zchan agent:</p><input id=agent placeholder="http://your-agent:7300/t/…">
<p><button onclick="go()">Open in my agent</button></p></div>
<script>
const a=document.getElementById('agent');try{{a.value=localStorage.getItem('zchan-agent')||''}}catch(e){{}}
function go(){{const u=a.value.trim().replace(/\/$/,'');if(!u)return;try{{localStorage.setItem('zchan-agent',u)}}catch(e){{}}
location=u+'/pay?ref='+encodeURIComponent(location.origin+location.pathname)}}
</script>"#,
        name = esc(&ctx.cfg.name),
        item = esc(&row.invoice.body.resource),
        z = zec(row.invoice.body.amount),
        status = if row.status == "open" { "Unpaid" } else { "Paid ✓" },
    );
    Ok(Html(page(&format!("{} invoice", ctx.cfg.name), &b)).into_response())
}

/// The point-of-sale page: a fresh invoice for `amount` and a QR code of its URL. The QR
/// carries only the invoice reference; the payer fetches and verifies the signed invoice.
async fn checkout(State(ctx): State<Arc<Ctx>>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Html<String>> {
    let amount: u64 = q.get("amount").and_then(|a| a.parse().ok()).unwrap_or(ctx.cfg.price);
    let item = q.get("item").cloned().unwrap_or_else(|| "order".into());
    let inv = issue(&ctx, amount, &format!("checkout:{item}"))?;
    let url = format!("{}/invoices/{}", ctx.cfg.base_url, inv.body.id);
    let svg = qrcode::QrCode::new(url.as_bytes())
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .build();
    let b = format!(
        r#"<h1>{name}</h1><p class=mute>Checkout · {item}</p>
<div class=card><div class=big>{z} ZEC</div><div class=mute>{amount} zatoshis · expires in {ttl} s</div>
<div class=qr style="margin:14px 0">{svg}</div>
<div class=mono>{url}</div><p id=st class=mute>Waiting for payment…</p></div>
<script>
const st=document.getElementById('st');
async function poll(){{try{{const r=await fetch('/invoices/{id}');const j=await r.json();
if(j.status!=='open'){{st.className='ok big';st.textContent='Paid ✓';return}}}}catch(e){{}}setTimeout(poll,1500)}}
poll();
</script>"#,
        name = esc(&ctx.cfg.name),
        item = esc(&item),
        z = zec(amount),
        ttl = ctx.cfg.invoice_ttl,
        url = esc(&url),
        id = esc(&inv.body.id),
    );
    Ok(Html(page(&format!("{} checkout", ctx.cfg.name), &b)))
}

async fn admin_close(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let txid = payee::close_now(&ctx, &id).await?;
    Ok(Json(json!({"channel": id, "txid": txid})))
}

async fn admin_coop_close(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let txid = coop_close(&ctx, &id).await?;
    Ok(Json(json!({"channel": id, "txid": txid})))
}

/// Closes cooperatively, paying the merchant's share to its Ironwood address: the
/// merchant builds and proves the transaction, the solver checks its own change and
/// co-signs, the merchant signs and broadcasts.
pub async fn coop_close(ctx: &Ctx, id: &str) -> Result<String> {
    let lock = ctx.lock(id);
    let _g = lock.lock().await;
    let row = ctx
        .store
        .read(|c| store::channel_get(c, id))?
        .filter(|r| r.role == "payee" && r.status == "open" && r.cumulative > 0)
        .ok_or_else(|| Error::State(format!("no open channel {id} with a payment")))?;
    let terms = Terms::try_from(&row.terms)?;
    let tip = ctx.tip().await?;
    let change = terms.capacity - terms.state_fee(&ctx.p)? - row.cumulative;
    let to = ctx.wallet.ironwood_address(0);
    let mut spec = Spec {
        target_height: tip + 1,
        expiry_height: tip + 40,
        lock_height: None,
        inputs: vec![terms.funding_coin()?],
        outputs: vec![Pay::Ironwood(to, 1), Pay::Transparent(terms.payer_address, change)],
        ironwood_anchor: Some(ctx.node.ironwood_anchor(tip).await?),
    };
    let fee = tx::fee(&ctx.p, &spec)?;
    let mine = terms
        .capacity
        .checked_sub(change + fee)
        .filter(|m| *m > 0)
        .ok_or_else(|| {
            Error::State(format!(
                "an Ironwood close costs {fee} zat; the merchant's share {} does not cover it (close transparently)",
                terms.capacity - change
            ))
        })?;
    spec.outputs[0] = Pay::Ironwood(to, mine);
    let pczt = tx::build(&ctx.p, &spec)?;
    let bytes = pczt.clone().serialize().map_err(|e| Error::Other(format!("pczt: {e:?}")))?;
    let req = CoopCloseReq { pczt: hex::encode(bytes), cumulative: row.cumulative };
    let resp: CoopCloseResp = web::post(&ctx.http, &format!("{}/channels/{id}/coop-close", row.peer_url), &req).await?;
    let pczt = tx::append(pczt, 0, sig_from_hex(&resp.sig)?)?;
    let f = tx::extract(tx::sign(pczt, 0, &ctx.wallet.channel_key(row.idx))?)?;
    let txid = txid_hex(&f.txid);
    ctx.store.tx(|c| {
        store::channel_update(c, id, |r| {
            r.status = "closing".into();
            r.close_txid = Some(txid.clone());
            r.settled_by = Some("coop-ironwood".into());
        })?;
        store::event(c, "coop_close_broadcast", json!({"channel": id, "txid": txid, "ironwood_value": mine, "fee": fee}))
    })?;
    ctx.broadcast(&f).await?;
    Ok(txid)
}

async fn status_json(State(ctx): State<Arc<Ctx>>) -> ApiResult<Json<Value>> {
    let (chans, invs, events) = ctx.store.read(|c| {
        Ok((store::channels(c, "payee")?, store::invoices(c, None)?, store::events(c, 40)?))
    })?;
    let paid: u64 = invs.iter().filter(|i| i.status != "open").map(|i| i.invoice.body.amount).sum();
    Ok(Json(json!({
        "info": {"id": ctx.cfg.name, "pubkey": ctx.identity_hex(), "network": ctx.network, "price": ctx.cfg.price},
        "channels": chans, "paid_total": paid,
        "invoices": invs.iter().rev().take(40).collect::<Vec<_>>(), "events": events,
    })))
}

async fn status_html(State(ctx): State<Arc<Ctx>>) -> ApiResult<Html<String>> {
    let Json(s) = status_json(State(ctx.clone())).await?;
    let mut b = format!(
        "<h1>{}</h1><p class=mute>merchant · {} · received {} ZEC · <a href='/checkout'>new checkout</a></p><h2>Channels from the solver</h2><div class='card scroll'><table><tr><th>channel</th><th>status</th><th>received / capacity</th><th>lock</th><th>close tx</th></tr>",
        esc(&ctx.cfg.name),
        esc(&ctx.network),
        zec(s["paid_total"].as_u64().unwrap_or(0))
    );
    for r in s["channels"].as_array().into_iter().flatten() {
        let id = r["id"].as_str().unwrap_or("");
        b += &format!(
            "<tr><td class=mono>{}</td><td>{}{}</td><td>{} / {}</td><td>{}</td><td class=mono>{}</td></tr>",
            esc(&id[..12.min(id.len())]),
            esc(r["status"].as_str().unwrap_or("")),
            r["settled_by"].as_str().map(|s| format!(" ({})", esc(s))).unwrap_or_default(),
            r["cumulative"],
            r["terms"]["capacity"],
            r["terms"]["lock_height"],
            esc(r["close_txid"].as_str().unwrap_or("")),
        );
    }
    b += "</table></div><h2>Invoices</h2><div class='card scroll'><table><tr><th>invoice</th><th>resource</th><th>amount</th><th>status</th></tr>";
    for i in s["invoices"].as_array().into_iter().flatten() {
        let inv = &i["invoice"]["body"];
        b += &format!(
            "<tr><td class=mono>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&inv["id"].as_str().unwrap_or("")[..8]),
            esc(inv["resource"].as_str().unwrap_or("")),
            inv["amount"],
            esc(i["status"].as_str().unwrap_or("")),
        );
    }
    b += "</table></div><script>setTimeout(()=>location.reload(),4000)</script>";
    Ok(Html(page(&ctx.cfg.name, &b)))
}
