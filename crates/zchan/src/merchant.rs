//! A merchant: payee of the solver's channel, issuer of invoices, and a resource server
//! paid over x402 v2 or MPP (`gate`). It serves a paid resource once per invoice, against
//! its own signed acknowledgment of the channel state that paid it. In proxy mode it
//! gates and forwards requests to an existing HTTP service.

use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    ctx::Ctx,
    gate, payee,
    store::{self, InvoiceRow},
    web::{self, err, esc, page, zec, ApiResult},
    wire::{now, sig_from_hex, Ack, CoopCloseReq, CoopCloseResp, Info, Invoice, Signed, UpdateReq, ACK},
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
        .fallback(gate::proxy)
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

/// `GET /paid/{resource}`: a demo resource at the merchant's `--price`, payable over
/// x402 v2 or MPP.
async fn paid(State(ctx): State<Arc<Ctx>>, Path(resource): Path<String>, headers: HeaderMap) -> Response {
    let path = format!("/paid/{resource}");
    match gate::check(&ctx, &path, ctx.cfg.price, &headers) {
        Ok(p) => (StatusCode::OK, p.headers, Json(gate::demo_body(&ctx, &path, &p.invoice))).into_response(),
        Err(resp) => resp,
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
    Ok(Json(gate::issue(&ctx, req.amount, &req.resource)?))
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
    // `?invoice=<id>` shows an existing invoice (a POS screen that reloads); otherwise a
    // new one is issued for `amount` and `item`.
    let inv = match q.get("invoice") {
        Some(id) => ctx
            .store
            .read(|c| store::invoice_get(c, id))?
            .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown invoice"))?
            .invoice,
        None => {
            let amount: u64 = q.get("amount").and_then(|a| a.parse().ok()).unwrap_or(ctx.cfg.price);
            let item = q.get("item").cloned().unwrap_or_else(|| "order".into());
            gate::issue(&ctx, amount, &format!("checkout:{item}"))?
        }
    };
    let amount = inv.body.amount;
    let item = inv.body.resource.trim_start_matches("checkout:").to_string();
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

async fn admin_close(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    gate::admin(&ctx, &headers)?;
    let txid = payee::close_now(&ctx, &id).await?;
    Ok(Json(json!({"channel": id, "txid": txid})))
}

async fn admin_coop_close(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    gate::admin(&ctx, &headers)?;
    let txid = coop_close(&ctx, &id).await?;
    Ok(Json(json!({"channel": id, "txid": txid})))
}

/// Closes cooperatively, paying the merchant's share to its Ironwood address, and the
/// solver's change to the solver's Ironwood address when it gave one (else to its
/// transparent address). The merchant builds and proves the transaction, the solver
/// checks its own change (by decrypting it, when shielded) and co-signs, the merchant
/// signs and broadcasts.
pub async fn coop_close(ctx: &Ctx, id: &str) -> Result<String> {
    let lock = ctx.lock(id);
    let _g = lock.lock().await;
    let row = ctx
        .store
        .read(|c| store::channel_get(c, id))?
        .ok_or_else(|| Error::State(format!("no channel {id}")))?;
    let draft = payee::build_coop_close(ctx, &row).await?;
    let bytes = draft.pczt.clone().serialize().map_err(|e| Error::Other(format!("pczt: {e:?}")))?;
    let req = CoopCloseReq { pczt: hex::encode(bytes), cumulative: row.cumulative };
    let resp: CoopCloseResp = web::post(&ctx.http, &format!("{}/channels/{id}/coop-close", row.peer_url), &req).await?;
    payee::finish_coop_close(ctx, &row, draft, &sig_from_hex(&resp.sig)?).await
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
    b += "</table></div><script>if(!location.search.includes('still'))setTimeout(()=>location.reload(),4000)</script>";
    Ok(Html(page(&ctx.cfg.name, &b)))
}
