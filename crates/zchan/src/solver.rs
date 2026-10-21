//! The solver: payee of every agent's channel, payer of one channel per merchant.
//!
//! A payment (docs/plan.md §4): the agent asks for a quote, then sends its next state
//! with a payment id. Under the agent-channel lock and then the merchant-channel lock the
//! solver verifies the state, signs the next merchant state, and commits both with the
//! payment (`agent_signed`) before pushing the merchant state. The merchant's signed
//! acknowledgment makes it `done`. A merchant refusal makes it `failed`: the merchant
//! channel's level is restored and the agent is credited. A push that gets no answer
//! leaves it `agent_signed`; retries resend the same stored bytes.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};

use crate::{
    channel::{ShieldedChange, Terms},
    ctx::Ctx,
    payee, payer,
    store::{self, MerchantRow, Payment},
    web::{self, err, esc, page, zec, ApiResult},
    wire::{
        now, pk_from_hex, pk_hex, random_id, sig_hex, Ack, CoopCloseReq, CoopCloseResp, Info, PayReq, PayResp, Quote,
        QuoteReq, Signed, State as WireState, UpdateReq, ACK, INVOICE, QUOTE,
    },
    Error, Result,
};

pub const QUOTE_TTL: u64 = 60;

pub fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .merge(payee::routes())
        .route("/info", get(info))
        .route("/quote", post(quote))
        .route("/pay", post(pay))
        .route("/payments/{id}", get(payment))
        .route("/channels/{id}/coop-close", post(coop_close))
        .route("/admin/merchants", post(add_merchant))
        .route("/admin/channels/{id}/close", post(admin_close))
        .route("/admin/shield", post(admin_shield))
        .route("/status", get(status_html))
        .route("/status.json", get(status_json))
        .with_state(ctx)
}

/// Background work: payee closes, payer refund watching, re-pushing pending payments.
pub async fn run(ctx: Arc<Ctx>) {
    loop {
        if let Err(e) = resume(&ctx).await {
            eprintln!("resume: {e}");
        }
        if let Err(e) = payee::tick(&ctx).await {
            eprintln!("payee tick: {e}");
        }
        if let Err(e) = payer::watch(&ctx).await {
            eprintln!("payer watch: {e}");
        }
        tokio::time::sleep(ctx.cfg.poll).await;
    }
}

async fn info(State(ctx): State<Arc<Ctx>>) -> Json<Info> {
    Json(Info {
        role: "solver".into(),
        id: ctx.cfg.name.clone(),
        pubkey: ctx.identity_hex(),
        network: ctx.network.clone(),
        deposit: ctx.deposit_address(),
        fee: ctx.cfg.fee,
        max_payment: ctx.cfg.max_payment,
    })
}

/// Moves the solver's transparent coins into its shielded account, so later channels can
/// be funded with `shielded: true`.
async fn admin_shield(State(ctx): State<Arc<Ctx>>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    crate::gate::admin(&ctx, &headers)?;
    let txid = ctx.shield().await?;
    Ok(Json(json!({"txid": txid})))
}

/// Closes an agent channel now with its best state (end-of-day settlement).
async fn admin_close(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    crate::gate::admin(&ctx, &headers)?;
    let txid = payee::close_now(&ctx, &id).await?;
    Ok(Json(json!({"channel": id, "txid": txid})))
}

#[derive(serde::Deserialize)]
struct AddMerchantReq {
    url: String,
    capacity: u64,
    lock_blocks: u32,
    /// Fund the channel from the solver's Ironwood notes.
    #[serde(default)]
    shielded: bool,
}

/// Registers a merchant and opens the solver's channel to it.
async fn add_merchant(State(ctx): State<Arc<Ctx>>, headers: HeaderMap, Json(req): Json<AddMerchantReq>) -> ApiResult<Json<MerchantRow>> {
    crate::gate::admin(&ctx, &headers)?;
    let url = req.url.trim_end_matches('/').to_string();
    let i: Info = web::get(&ctx.http, &format!("{url}/info")).await?;
    if i.role != "merchant" || i.network != ctx.network {
        return Err(err(StatusCode::BAD_REQUEST, format!("{url} is a {} on {}", i.role, i.network)));
    }
    // A merchant whose channel has closed or is closing may be registered again: the new
    // channel replaces it.
    if let Some(m) = ctx.store.read(|c| store::merchant_get(c, &i.id))? {
        let open = ctx.store.read(|c| store::channel_get(c, &m.channel_id))?.is_some_and(|ch| ch.status == "open");
        if open {
            return Err(err(StatusCode::CONFLICT, format!("merchant {} has an open channel", i.id)));
        }
    }
    let ch = payer::open(&ctx, &url, req.capacity, req.lock_blocks, req.shielded).await?;
    let m = MerchantRow { id: i.id.clone(), url, pubkey: i.pubkey, channel_id: ch.id.clone() };
    ctx.store.tx(|c| {
        store::merchant_put(c, &m)?;
        store::event(c, "merchant_added", json!({"merchant": m.id, "channel": ch.id, "capacity": req.capacity}))
    })?;
    Ok(Json(m))
}

async fn quote(State(ctx): State<Arc<Ctx>>, Json(req): Json<QuoteReq>) -> ApiResult<Json<Signed<Quote>>> {
    let inv = &req.invoice.body;
    let signer = req.invoice.verify(INVOICE)?;
    let m = ctx
        .store
        .read(|c| store::merchant_get(c, &inv.merchant_id))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("unknown merchant {}", inv.merchant_id)))?;
    if pk_hex(&signer) != m.pubkey {
        return Err(err(StatusCode::BAD_REQUEST, format!("invoice is not signed by merchant {}", m.id)));
    }
    if now() + 5 > inv.expires_at {
        return Err(err(StatusCode::GONE, "invoice expired"));
    }
    if inv.amount == 0 || inv.amount > ctx.cfg.max_payment {
        return Err(err(StatusCode::BAD_REQUEST, format!("amount must be 1..={}", ctx.cfg.max_payment)));
    }
    if ctx.store.read(|c| store::invoice_in_payment(c, &inv.id))? {
        return Err(err(StatusCode::CONFLICT, "invoice already paid"));
    }
    let (arow, mrow, credit) = ctx.store.read(|c| {
        Ok((
            store::channel_get(c, &req.agent_channel)?,
            store::channel_get(c, &m.channel_id)?,
            store::credit_get(c, &req.agent_channel)?,
        ))
    })?;
    let arow = arow
        .filter(|r| r.role == "payee" && r.status == "open")
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no open agent channel {}", req.agent_channel)))?;
    let mrow = mrow
        .filter(|r| r.status == "open")
        .ok_or_else(|| err(StatusCode::CONFLICT, format!("merchant {} has no open channel", m.id)))?;
    if payer::remaining(&ctx, &mrow)? < inv.amount {
        return Err(err(StatusCode::CONFLICT, "merchant channel lacks liquidity"));
    }
    let fee = ctx.cfg.fee;
    let credit_applied = credit.min(inv.amount + fee);
    let increment = inv.amount + fee - credit_applied;
    let agent_left = Terms::try_from(&arow.terms)?.max_payable(&ctx.p)?.saturating_sub(arow.cumulative);
    if agent_left < increment {
        return Err(err(StatusCode::CONFLICT, format!("agent channel has {agent_left} left, needs {increment}")));
    }
    let q = Signed::sign(
        QUOTE,
        Quote {
            id: random_id(),
            invoice: req.invoice.clone(),
            agent_channel: req.agent_channel,
            merchant_channel: m.channel_id,
            amount: inv.amount,
            fee,
            credit_applied,
            increment,
            expires_at: (now() + QUOTE_TTL).min(inv.expires_at),
        },
        &ctx.identity(),
    );
    ctx.store.tx(|c| store::quote_put(c, &q))?;
    Ok(Json(q))
}

async fn payment(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> ApiResult<Json<Payment>> {
    ctx.store
        .read(|c| store::payment_get(c, &id))?
        .map(Json)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such payment"))
}

fn conflict(m: impl Into<String>) -> web::ApiError {
    err(StatusCode::CONFLICT, m)
}

async fn pay(State(ctx): State<Arc<Ctx>>, Json(req): Json<PayReq>) -> ApiResult<Json<PayResp>> {
    // A retry: answer from the record (re-pushing if still pending).
    if let Some(p) = ctx.store.read(|c| store::payment_get(c, &req.payment_id))? {
        if p.quote.body.id != req.quote_id {
            return Err(conflict("payment id was used for another quote"));
        }
        return Ok(Json(push_locked(&ctx, &p).await?));
    }
    let (quote, used) = ctx
        .store
        .read(|c| store::quote_get(c, &req.quote_id))?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown quote"))?;
    if used.is_some() {
        return Err(conflict("quote already used"));
    }
    let q = quote.body.clone();
    if now() > q.expires_at {
        return Err(err(StatusCode::GONE, "quote expired"));
    }
    let (la, lm) = (ctx.lock(&q.agent_channel), ctx.lock(&q.merchant_channel));
    let _ga = la.lock().await;
    let _gm = lm.lock().await;
    let (existing, used, arow, mrow, inflight, credit, invoice_paid) = ctx.store.read(|c| {
        Ok((
            store::payment_get(c, &req.payment_id)?,
            store::quote_get(c, &q.id)?.and_then(|(_, u)| u),
            store::channel_get(c, &q.agent_channel)?,
            store::channel_get(c, &q.merchant_channel)?,
            store::payments(c, Some("agent_signed"))?.iter().any(|p| p.quote.body.agent_channel == q.agent_channel),
            store::credit_get(c, &q.agent_channel)?,
            store::invoice_in_payment(c, &q.invoice.body.id)?,
        ))
    })?;
    if existing.is_some() || used.is_some() {
        return Err(conflict("concurrent request for this payment or quote; retry"));
    }
    let arow = arow.filter(|r| r.role == "payee" && r.status == "open").ok_or_else(|| conflict("agent channel not open"))?;
    let mrow = mrow.filter(|r| r.role == "payer" && r.status == "open").ok_or_else(|| conflict("merchant channel not open"))?;
    if inflight {
        return Err(conflict("a payment on this agent channel is in flight"));
    }
    if credit < q.credit_applied {
        return Err(conflict("credit changed; quote again"));
    }
    if invoice_paid {
        return Err(conflict("invoice already paid"));
    }
    if payer::remaining(&ctx, &mrow)? < q.amount {
        return Err(conflict("merchant channel lacks liquidity"));
    }
    let agent_state = match (&req.state, q.increment) {
        (None, 0) => None,
        (Some(_), 0) => return Err(err(StatusCode::BAD_REQUEST, "credit covers this quote; send no state")),
        (None, _) => return Err(err(StatusCode::BAD_REQUEST, "state required")),
        (Some(s), inc) => {
            if s.channel_id != q.agent_channel || s.cumulative != arow.cumulative + inc {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    format!("state must be {} on {}", arow.cumulative + inc, q.agent_channel),
                ));
            }
            payee::verify(&ctx, &arow, s.cumulative, &s.sig)?;
            Some(s.clone())
        }
    };
    let (m_cum, m_sig) = payer::next_state(&ctx, &mrow, q.amount)?;
    let p = Payment {
        id: req.payment_id.clone(),
        quote: quote.clone(),
        status: "agent_signed".into(),
        agent_cumulative: agent_state.as_ref().map_or(arow.cumulative, |s| s.cumulative),
        agent_sig: agent_state.as_ref().map(|s| s.sig.clone()),
        merchant_cumulative: m_cum,
        merchant_sig: Some(m_sig.clone()),
        prev_merchant_sig: mrow.sig.clone(),
        ack: None,
        error: None,
        updated: now(),
    };
    ctx.store.tx(|c| {
        store::payment_put(c, &p)?;
        if !store::quote_use(c, &q.id, &p.id)? {
            return Err(Error::Store("quote already used".into()));
        }
        if let Some(s) = &agent_state {
            store::channel_update(c, &q.agent_channel, |r| {
                r.cumulative = s.cumulative;
                r.sig = Some(s.sig.clone());
            })?;
            store::state_log(c, &q.agent_channel, s.cumulative, &s.sig, &p.id)?;
        }
        store::credit_set(c, &q.agent_channel, credit - q.credit_applied)?;
        store::channel_update(c, &q.merchant_channel, |r| {
            r.cumulative = m_cum;
            r.sig = Some(m_sig.clone());
        })?;
        store::state_log(c, &q.merchant_channel, m_cum, &m_sig, &p.id)?;
        store::event(c, "payment_agent_signed", json!({
            "payment": p.id, "invoice": q.invoice.body.id, "merchant": q.invoice.body.merchant_id,
            "amount": q.amount, "fee": q.fee, "credit_applied": q.credit_applied,
            "agent_cumulative": p.agent_cumulative, "merchant_cumulative": m_cum,
        }))
    })?;
    ctx.fault("after_merchant_sign");
    Ok(Json(push(&ctx, &p.id).await?))
}

fn response(ctx: &Ctx, p: &Payment) -> Result<PayResp> {
    let credit = ctx.store.read(|c| store::credit_get(c, &p.quote.body.agent_channel))?;
    Ok(PayResp {
        payment_id: p.id.clone(),
        status: match p.status.as_str() {
            "agent_signed" => "pending".into(),
            s => s.into(),
        },
        ack: p.ack.clone(),
        error: p.error.clone(),
        credit,
    })
}

/// Takes the payment's locks, then `push`.
async fn push_locked(ctx: &Ctx, p: &Payment) -> Result<PayResp> {
    if p.status != "agent_signed" {
        return response(ctx, p);
    }
    let q = &p.quote.body;
    let (la, lm) = (ctx.lock(&q.agent_channel), ctx.lock(&q.merchant_channel));
    let _ga = la.lock().await;
    let _gm = lm.lock().await;
    push(ctx, &p.id).await
}

/// Sends a pending payment's stored merchant state (call with its locks held).
async fn push(ctx: &Ctx, id: &str) -> Result<PayResp> {
    let p = ctx
        .store
        .read(|c| store::payment_get(c, id))?
        .ok_or_else(|| Error::Store(format!("no payment {id}")))?;
    if p.status != "agent_signed" {
        return response(ctx, &p);
    }
    let q = p.quote.body.clone();
    let m = ctx
        .store
        .read(|c| store::merchant_get(c, &q.invoice.body.merchant_id))?
        .ok_or_else(|| Error::Store(format!("no merchant {}", q.invoice.body.merchant_id)))?;
    let req = UpdateReq {
        invoice_id: q.invoice.body.id.clone(),
        state: WireState {
            channel_id: q.merchant_channel.clone(),
            cumulative: p.merchant_cumulative,
            sig: p.merchant_sig.clone().unwrap_or_default(),
        },
    };
    let url = format!("{}/channels/{}/update", m.url, q.merchant_channel);
    let expected = Ack {
        invoice_id: q.invoice.body.id.clone(),
        merchant_id: m.id.clone(),
        channel_id: q.merchant_channel.clone(),
        cumulative: p.merchant_cumulative,
        amount: q.amount,
    };
    match web::post::<_, Signed<Ack>>(&ctx.http, &url, &req).await {
        Ok(ack) => {
            if ack.verify_by(ACK, &pk_from_hex(&m.pubkey)?).is_err() || ack.body != expected {
                let mut p = p.clone();
                p.error = Some("merchant acknowledgment does not match; will retry".into());
                return response(ctx, &p);
            }
            let p = ctx.store.tx(|c| {
                let mut p = p.clone();
                p.status = "done".into();
                p.ack = Some(ack.clone());
                p.error = None;
                p.updated = now();
                store::payment_put(c, &p)?;
                store::event(c, "payment_done", json!({
                    "payment": p.id, "invoice": q.invoice.body.id, "merchant": m.id, "amount": q.amount,
                    "merchant_cumulative": p.merchant_cumulative,
                }))?;
                Ok(p)
            })?;
            response(ctx, &p)
        }
        Err(e) if web::definitive(&e) => {
            let p = ctx.store.tx(|c| {
                let mut p = p.clone();
                p.status = "failed".into();
                p.error = Some(format!("merchant refused: {e}"));
                p.updated = now();
                store::payment_put(c, &p)?;
                store::channel_update(c, &q.merchant_channel, |r| {
                    if r.cumulative == p.merchant_cumulative {
                        r.cumulative -= q.amount;
                        r.sig = p.prev_merchant_sig.clone();
                    }
                })?;
                let credit = store::credit_get(c, &q.agent_channel)?;
                store::credit_set(c, &q.agent_channel, credit + q.amount + q.fee)?;
                // The refused state was sent: if the merchant kept it, it can close with it.
                store::event(c, "payment_failed", json!({
                    "payment": p.id, "invoice": q.invoice.body.id, "merchant": m.id, "reason": e.to_string(),
                    "credited": q.amount + q.fee, "sent_state": p.merchant_cumulative, "exposure": q.amount,
                }))?;
                Ok(p)
            })?;
            response(ctx, &p)
        }
        Err(e) => {
            let mut p = p.clone();
            p.error = Some(format!("merchant unreachable: {e}; will retry"));
            response(ctx, &p)
        }
    }
}

/// Re-pushes every pending payment (on start, and each background pass).
pub async fn resume(ctx: &Ctx) -> Result<()> {
    for p in ctx.store.read(|c| store::payments(c, Some("agent_signed")))? {
        push_locked(ctx, &p).await?;
    }
    Ok(())
}

async fn coop_close(
    State(ctx): State<Arc<Ctx>>,
    Path(id): Path<String>,
    Json(req): Json<CoopCloseReq>,
) -> ApiResult<Json<CoopCloseResp>> {
    let lock = ctx.lock(&id);
    let _g = lock.lock().await;
    let (row, inflight) = ctx.store.read(|c| {
        Ok((
            store::channel_get(c, &id)?,
            store::payments(c, Some("agent_signed"))?.iter().any(|p| p.quote.body.merchant_channel == id),
        ))
    })?;
    let row = row
        .filter(|r| r.role == "payer" && r.status == "open")
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no open merchant channel {id}")))?;
    if inflight {
        return Err(conflict("a payment on this channel is in flight"));
    }
    if req.cumulative != row.cumulative {
        return Err(conflict(format!("a close must pay the latest state, {}", row.cumulative)));
    }
    let bytes = hex::decode(&req.pczt).map_err(|e| err(StatusCode::BAD_REQUEST, format!("pczt hex: {e}")))?;
    let pczt = pczt::Pczt::parse(&bytes).map_err(|e| err(StatusCode::BAD_REQUEST, format!("pczt: {e:?}")))?;
    let terms = Terms::try_from(&row.terms)?;
    // A close with no transparent outputs pays the solver's change to its Ironwood
    // address for this channel; the solver checks that by decrypting it.
    let shielded_close = pczt
        .clone()
        .into_effects()
        .ok()
        .and_then(|fx| fx.transparent_bundle().map(|t| t.vout.is_empty()))
        .unwrap_or(false);
    let change = shielded_close.then(|| ShieldedChange {
        address: ctx.wallet.shielded_address(row.idx),
        ivk: ctx.wallet.shielded_ivk(),
    });
    let sig = terms.sign_coop_close(&ctx.p, &ctx.wallet.channel_key(row.idx), &pczt, row.cumulative, change.as_ref())?;
    ctx.store.tx(|c| {
        store::channel_update(c, &id, |r| {
            r.status = "closing".into();
            r.settled_by = Some("coop-ironwood".into());
        })?;
        store::event(c, "coop_close_signed", json!({"channel": id, "cumulative": row.cumulative, "both_shielded": shielded_close}))
    })?;
    Ok(Json(CoopCloseResp { sig: sig_hex(&sig) }))
}

/// Per channel: does its level equal what the payments account for?
pub fn reconcile(ctx: &Ctx) -> Result<Vec<Value>> {
    ctx.store.read(|c| {
        let pays = store::payments(c, None)?;
        let mut out = vec![];
        for m in store::channels(c, "payer")? {
            let of = |s: &str| -> u64 {
                pays.iter()
                    .filter(|p| p.quote.body.merchant_channel == m.id && p.status == s)
                    .map(|p| p.quote.body.amount)
                    .sum()
            };
            let (acked, pending) = (of("done"), of("agent_signed"));
            out.push(json!({
                "channel": m.id, "side": "merchant", "peer": m.peer_id, "cumulative": m.cumulative,
                "acked": acked, "pending": pending, "ok": m.cumulative == acked + pending,
            }));
        }
        for a in store::channels(c, "payee")? {
            let mine: Vec<&Payment> = pays.iter().filter(|p| p.quote.body.agent_channel == a.id).collect();
            let increments: u64 = mine.iter().map(|p| p.quote.body.increment).sum();
            let credited: u64 = mine.iter().filter(|p| p.status == "failed").map(|p| p.quote.body.amount + p.quote.body.fee).sum();
            let applied: u64 = mine.iter().map(|p| p.quote.body.credit_applied).sum();
            let credit = store::credit_get(c, &a.id)?;
            out.push(json!({
                "channel": a.id, "side": "agent", "peer": a.peer_id, "cumulative": a.cumulative,
                "increments": increments, "credit": credit,
                "ok": a.cumulative == increments && credit + applied == credited,
            }));
        }
        Ok(out)
    })
}

async fn status_json(State(ctx): State<Arc<Ctx>>) -> ApiResult<Json<Value>> {
    let balance = ctx.balance().await.unwrap_or(0);
    let (payers, payees, merchants, pays, events) = ctx.store.read(|c| {
        Ok((
            store::channels(c, "payer")?,
            store::channels(c, "payee")?,
            store::merchants(c)?,
            store::payments(c, None)?,
            store::events(c, 40)?,
        ))
    })?;
    Ok(Json(json!({
        "info": {"id": ctx.cfg.name, "pubkey": ctx.identity_hex(), "network": ctx.network,
                 "deposit": ctx.deposit_address(), "balance": balance, "fee": ctx.cfg.fee},
        "merchants": merchants, "merchant_channels": payers, "agent_channels": payees,
        "payments": pays, "reconcile": reconcile(&ctx)?, "events": events,
    })))
}

async fn status_html(State(ctx): State<Arc<Ctx>>) -> ApiResult<Html<String>> {
    let Json(s) = status_json(State(ctx.clone())).await?;
    let mut b = format!(
        "<h1>zchan solver · {}</h1><p class=mute>{} · fee {} zat · deposit <code>{}</code> ({} ZEC)</p>",
        esc(&ctx.cfg.name),
        esc(&ctx.network),
        ctx.cfg.fee,
        esc(&ctx.deposit_address()),
        zec(s["info"]["balance"].as_u64().unwrap_or(0))
    );
    let table = |title: &str, rows: &Value, side: &str| -> String {
        let mut t = format!("<h2>{title}</h2><div class='card scroll'><table><tr><th>channel</th><th>peer</th><th>status</th><th>paid / capacity</th><th>lock</th><th>reconciled</th></tr>");
        for r in rows.as_array().into_iter().flatten() {
            let id = r["id"].as_str().unwrap_or("");
            let ok = s["reconcile"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["channel"] == id && x["side"] == side))
                .map(|x| x["ok"].as_bool() == Some(true));
            t += &format!(
                "<tr><td class=mono>{}</td><td>{}</td><td>{}{}</td><td>{} / {}</td><td>{}</td><td>{}</td></tr>",
                esc(&id[..12.min(id.len())]),
                esc(r["peer_id"].as_str().unwrap_or("")),
                esc(r["status"].as_str().unwrap_or("")),
                r["settled_by"].as_str().map(|s| format!(" ({})", esc(s))).unwrap_or_default(),
                r["cumulative"],
                r["terms"]["capacity"],
                r["terms"]["lock_height"],
                match ok { Some(true) => "<span class=ok>✓</span>", Some(false) => "<span class=bad>✗</span>", None => "" },
            );
        }
        t + "</table></div>"
    };
    b += &table("Agent channels (solver is payee)", &s["agent_channels"], "agent");
    b += &table("Merchant channels (solver is payer)", &s["merchant_channels"], "merchant");
    b += "<h2>Payments</h2><div class='card scroll'><table><tr><th>payment</th><th>merchant</th><th>amount</th><th>fee</th><th>status</th><th>agent state</th><th>merchant state</th></tr>";
    for p in s["payments"].as_array().into_iter().flatten().rev().take(30) {
        let q = &p["quote"]["body"];
        b += &format!(
            "<tr><td class=mono>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&p["id"].as_str().unwrap_or("")[..8]),
            esc(q["invoice"]["body"]["merchant_id"].as_str().unwrap_or("")),
            q["amount"],
            q["fee"],
            esc(p["status"].as_str().unwrap_or("")),
            p["agent_cumulative"],
            p["merchant_cumulative"],
        );
    }
    b += "</table></div><h2>Events</h2><div class='card scroll'><table>";
    for e in s["events"].as_array().into_iter().flatten() {
        b += &format!("<tr><td>{}</td><td class=mono>{}</td></tr>", esc(e["kind"].as_str().unwrap_or("")), esc(&e["data"].to_string()));
    }
    b += "</table></div><script>if(!location.search.includes('still'))setTimeout(()=>location.reload(),4000)</script>";
    Ok(Html(page("zchan solver", &b)))
}
