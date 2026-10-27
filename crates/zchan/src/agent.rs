//! The agent: payer of one channel to the solver. It pays an invoice by asking the
//! solver for a quote, signing its next state, and sending it with a payment id that makes
//! every retry idempotent. It speaks x402 v2 (`get`) and serves a small approval page for
//! QR checkout (`router`).

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    channel::{ShieldedChange, Terms},
    ctx::Ctx,
    mpp, payer,
    store::{self, ChannelRow, InvoiceRow, Payment},
    web::{self, err, esc, page, zec, ApiResult},
    wire::{
        now, pk_from_hex, random_id, Ack, Info, Invoice, PayReq, PayResp, Quote, QuoteReq, Signed, State as WireState,
        ACK, INVOICE, QUOTE,
    },
    x402::{self, PaymentPayload, PaymentRequired, SettlementResponse, ZchannelPayload},
    Error, Result,
};

pub struct Agent {
    pub ctx: Arc<Ctx>,
    pub solver: String,
    /// The approval page's secret path segment.
    pub token: String,
}

/// Refuses a price above the caller's cap, before anything is quoted or signed.
fn check_price(amount: u64, max_price: Option<u64>) -> Result<()> {
    match max_price {
        Some(max) if amount > max => Err(Error::Other(format!("price {amount} zat is above the cap of {max} zat; nothing was paid"))),
        _ => Ok(()),
    }
}

/// The result of an x402 request.
#[derive(Debug, Serialize)]
pub struct Fetched {
    pub status: u16,
    pub body: Value,
    pub settlement: Option<SettlementResponse>,
    /// MPP `Payment-Receipt`, when paid over MPP.
    pub receipt: Option<mpp::Receipt>,
    pub ack: Option<Signed<Ack>>,
    /// What happened, step by step: the request, the 402 offers, the quote, the signed
    /// state, the merchant's acknowledgment, the paid request and its answer.
    pub trace: Vec<Value>,
    pub elapsed_ms: u64,
}

/// Which protocol to pay a 402 with, when the merchant offers both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocol {
    X402,
    Mpp,
}

impl Agent {
    pub fn new(ctx: Arc<Ctx>, solver: &str) -> Self {
        let token = ctx
            .store
            .tx(|c| {
                Ok(match store::meta_get(c, "ui_token")? {
                    Some(t) => t,
                    None => {
                        let t = random_id()[..16].to_string();
                        store::meta_set(c, "ui_token", &t)?;
                        t
                    }
                })
            })
            .unwrap_or_else(|_| random_id());
        Self { ctx, solver: solver.trim_end_matches('/').to_string(), token }
    }

    pub async fn solver_info(&self) -> Result<Info> {
        let i: Info = web::get(&self.ctx.http, &format!("{}/info", self.solver)).await?;
        if i.role != "solver" || i.network != self.ctx.network {
            return Err(Error::Other(format!("{} is a {} on {}", self.solver, i.role, i.network)));
        }
        Ok(i)
    }

    pub async fn open(&self, capacity: u64, lock_blocks: u32) -> Result<ChannelRow> {
        payer::open(&self.ctx, &self.solver, capacity, lock_blocks, false).await
    }

    /// Opens a channel funded from the agent's shielded notes.
    pub async fn open_shielded(&self, capacity: u64, lock_blocks: u32) -> Result<ChannelRow> {
        payer::open(&self.ctx, &self.solver, capacity, lock_blocks, true).await
    }

    /// Closes the agent's channel cooperatively: the solver builds the close, the agent
    /// checks that it pays the agent exactly its change (decrypting it, when shielded, to
    /// its own Ironwood address for the channel) at the agent's own latest level, and
    /// signs. Returns the close's display txid.
    pub async fn close(&self, channel_id: &str) -> Result<String> {
        let lock = self.ctx.lock(channel_id);
        let _g = lock.lock().await;
        let row = self
            .ctx
            .store
            .read(|c| store::channel_get(c, channel_id))?
            .filter(|r| r.role == "payer" && r.status == "open")
            .ok_or_else(|| Error::State(format!("no open channel {channel_id}")))?;
        let offer: crate::payee::CloseOffer =
            web::post(&self.ctx.http, &format!("{}/channels/{channel_id}/close-offer", self.solver), &json!({})).await?;
        if offer.cumulative != row.cumulative {
            return Err(Error::State(format!("solver offers a close at {}, the agent signed {}", offer.cumulative, row.cumulative)));
        }
        let pczt = pczt::Pczt::parse(&hex::decode(&offer.pczt).map_err(|e| Error::Wire(e.to_string()))?)
            .map_err(|e| Error::Wire(format!("pczt: {e:?}")))?;
        let shielded = pczt
            .clone()
            .into_effects()
            .ok()
            .and_then(|fx| fx.transparent_bundle().map(|t| t.vout.is_empty()))
            .unwrap_or(false)
            .then(|| ShieldedChange { address: self.ctx.wallet.shielded_address(row.idx), ivk: self.ctx.wallet.shielded_ivk() });
        let terms = Terms::try_from(&row.terms)?;
        let sig = terms.sign_coop_close(&self.ctx.p, &self.ctx.wallet.channel_key(row.idx), &pczt, row.cumulative, shielded.as_ref())?;
        let done: crate::payee::CloseDone = web::post(
            &self.ctx.http,
            &format!("{}/channels/{channel_id}/close-sign", self.solver),
            &crate::payee::CloseSign { sig: crate::wire::sig_hex(&sig) },
        )
        .await?;
        self.ctx.store.tx(|c| {
            store::channel_update(c, channel_id, |r| {
                r.status = "closing".into();
                r.close_txid = Some(done.txid.clone());
                r.settled_by = Some(if shielded.is_some() { "coop-ironwood" } else { "coop" }.into());
            })?;
            store::event(c, "coop_close_signed", json!({
                "channel": channel_id, "txid": done.txid, "cumulative": row.cumulative, "both_shielded": shielded.is_some(),
            }))
        })?;
        Ok(done.txid)
    }

    /// The newest open channel to this agent's solver.
    pub fn channel(&self) -> Result<ChannelRow> {
        self.ctx
            .store
            .read(|c| store::channels(c, "payer"))?
            .into_iter()
            .rev()
            .find(|r| r.status == "open" && r.peer_url == self.solver)
            .ok_or_else(|| Error::State(format!("no open channel to {}", self.solver)))
    }

    pub async fn quote(&self, invoice: &Signed<Invoice>) -> Result<Signed<Quote>> {
        invoice.verify(INVOICE)?;
        let info = self.solver_info().await?;
        let ch = self.channel()?;
        let q: Signed<Quote> = web::post(
            &self.ctx.http,
            &format!("{}/quote", self.solver),
            &QuoteReq { invoice: invoice.clone(), agent_channel: ch.id.clone() },
        )
        .await?;
        self.check_quote(&q, &info)?;
        if q.body.invoice != *invoice || q.body.agent_channel != ch.id {
            return Err(Error::Other("quote is for another invoice or channel".into()));
        }
        Ok(q)
    }

    fn check_quote(&self, q: &Signed<Quote>, info: &Info) -> Result<()> {
        q.verify_by(QUOTE, &pk_from_hex(&info.pubkey)?)?;
        let b = &q.body;
        if b.amount != b.invoice.body.amount || b.increment != b.amount + b.fee - b.credit_applied || b.fee > info.fee {
            return Err(Error::Other("quote arithmetic does not hold".into()));
        }
        Ok(())
    }

    /// Signs the next state for a quote, stores it, and sends it until the solver answers.
    pub async fn pay_quote(&self, q: Signed<Quote>) -> Result<Signed<Ack>> {
        let lock = self.ctx.lock(&q.body.agent_channel);
        let _g = lock.lock().await;
        let row = self
            .ctx
            .store
            .read(|c| store::channel_get(c, &q.body.agent_channel))?
            .ok_or_else(|| Error::State("no such channel".into()))?;
        let (cum, sig) = if q.body.increment > 0 {
            let (c, s) = payer::next_state(&self.ctx, &row, q.body.increment)?;
            (c, Some(s))
        } else {
            (row.cumulative, None)
        };
        let p = Payment {
            id: random_id(),
            quote: q.clone(),
            status: "signed".into(),
            agent_cumulative: cum,
            agent_sig: sig.clone(),
            merchant_cumulative: 0,
            merchant_sig: None,
            prev_merchant_sig: None,
            ack: None,
            error: None,
            updated: now(),
        };
        // The state is durable before it leaves: a retry resends these exact bytes.
        self.ctx.store.tx(|c| {
            store::payment_put(c, &p)?;
            if let Some(s) = &sig {
                store::channel_update(c, &row.id, |r| {
                    r.cumulative = cum;
                    r.sig = Some(s.clone());
                })?;
                store::state_log(c, &row.id, cum, s, &p.id)?;
            }
            store::event(c, "payment_signed", json!({
                "payment": p.id, "merchant": q.body.invoice.body.merchant_id, "amount": q.body.amount,
                "fee": q.body.fee, "credit_applied": q.body.credit_applied, "cumulative": cum,
            }))
        })?;
        self.send(&p.id).await
    }

    async fn send(&self, id: &str) -> Result<Signed<Ack>> {
        let p = self
            .ctx
            .store
            .read(|c| store::payment_get(c, id))?
            .ok_or_else(|| Error::Store(format!("no payment {id}")))?;
        let q = &p.quote.body;
        let req = PayReq {
            payment_id: p.id.clone(),
            quote_id: q.id.clone(),
            state: p.agent_sig.as_ref().map(|s| WireState {
                channel_id: q.agent_channel.clone(),
                cumulative: p.agent_cumulative,
                sig: s.clone(),
            }),
        };
        let url = format!("{}/pay", self.solver);
        let mut last = String::new();
        for _ in 0..40 {
            match web::post::<_, PayResp>(&self.ctx.http, &url, &req).await {
                Ok(r) if r.status == "done" => {
                    let ack = r.ack.ok_or_else(|| Error::Other("done without acknowledgment".into()))?;
                    let merchant = pk_from_hex(&q.invoice.signer)?;
                    ack.verify_by(ACK, &merchant)?;
                    if ack.body.invoice_id != q.invoice.body.id || ack.body.amount != q.amount {
                        return Err(Error::Other("acknowledgment is for another invoice".into()));
                    }
                    self.finish(&p, "done", Some(ack.clone()), None)?;
                    return Ok(ack);
                }
                Ok(r) if r.status == "failed" => {
                    let e = r.error.unwrap_or_default();
                    self.finish(&p, "failed", None, Some(format!("{e}; credit now {}", r.credit)))?;
                    return Err(Error::Refused(402, format!("payment failed, credited: {e}")));
                }
                Ok(r) => last = r.error.unwrap_or_else(|| "pending".into()),
                Err(e) if web::definitive(&e) => {
                    // The solver did not take the state: restore the previous level.
                    self.ctx.store.tx(|c| {
                        if p.agent_sig.is_some() {
                            let prev = store::state_before(c, &q.agent_channel, p.agent_cumulative)?;
                            store::channel_update(c, &q.agent_channel, |r| {
                                if r.cumulative == p.agent_cumulative {
                                    r.cumulative = prev.as_ref().map_or(0, |x| x.0);
                                    r.sig = prev.as_ref().map(|x| x.1.clone());
                                }
                            })?;
                        }
                        let mut p = p.clone();
                        p.status = "refused".into();
                        p.error = Some(e.to_string());
                        store::payment_put(c, &p)?;
                        store::event(c, "payment_refused", json!({"payment": p.id, "reason": e.to_string()}))
                    })?;
                    return Err(e);
                }
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(Error::Other(format!("payment {id} still pending ({last}); run resume later")))
    }

    fn finish(&self, p: &Payment, status: &str, ack: Option<Signed<Ack>>, error: Option<String>) -> Result<()> {
        self.ctx.store.tx(|c| {
            let mut p = p.clone();
            p.status = status.into();
            p.ack = ack;
            p.error = error;
            p.updated = now();
            store::payment_put(c, &p)?;
            store::event(c, &format!("payment_{status}"), json!({"payment": p.id, "error": p.error}))
        })
    }

    /// Retries payments left `signed` (e.g. after a restart).
    pub async fn resume(&self) -> Result<Vec<String>> {
        let mut out = vec![];
        for p in self.ctx.store.read(|c| store::payments(c, Some("signed")))? {
            let lock = self.ctx.lock(&p.quote.body.agent_channel);
            let _g = lock.lock().await;
            out.push(match self.send(&p.id).await {
                Ok(_) => format!("{} done", p.id),
                Err(e) => format!("{} {e}", p.id),
            });
        }
        Ok(out)
    }

    pub async fn pay_invoice(&self, invoice: &Signed<Invoice>) -> Result<Signed<Ack>> {
        let q = self.quote(invoice).await?;
        self.pay_quote(q).await
    }

    /// GET, paying a 402 over x402 v2 (or MPP if that is all the merchant offers).
    pub async fn get(&self, url: &str) -> Result<Fetched> {
        self.fetch(reqwest::Method::GET, url, None, Protocol::X402).await
    }

    /// Sends a request; on a 402 offering `zchannel` through this agent's solver, pays the
    /// invoice and repeats the request with the merchant's acknowledgment, over `prefer`
    /// when offered, else the other protocol.
    pub async fn fetch(&self, method: reqwest::Method, url: &str, body: Option<Vec<u8>>, prefer: Protocol) -> Result<Fetched> {
        self.fetch_capped(method, url, body, prefer, None).await
    }

    /// `fetch`, refusing before any quote or signature when the merchant's price is above
    /// `max_price` zat. The solver's fee is bounded separately, by its published fee.
    pub async fn fetch_capped(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<Vec<u8>>,
        prefer: Protocol,
        max_price: Option<u64>,
    ) -> Result<Fetched> {
        let t0 = std::time::Instant::now();
        let ms = |t: std::time::Instant| t.elapsed().as_millis() as u64;
        let mut trace = vec![json!({"step": "request", "method": method.as_str(), "url": url, "ms": 0})];
        let send = |extra: Option<(&'static str, String)>| {
            let mut rq = self.ctx.http.request(method.clone(), url);
            if let Some(b) = &body {
                rq = rq.body(b.clone());
            }
            if let Some((k, v)) = extra {
                rq = rq.header(k, v);
            }
            rq.send()
        };
        let r = send(None).await?;
        if r.status().as_u16() != 402 {
            return fetched(r, None, trace, t0).await;
        }
        let x402_offer = r
            .headers()
            .get(x402::PAYMENT_REQUIRED)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| x402::decode::<PaymentRequired>(h).ok())
            .and_then(|pr| {
                let a = pr.accepts.iter().find(|a| a.scheme == x402::SCHEME && a.network == self.ctx.network)?.clone();
                Some((pr, a))
            });
        let mpp_offer = r
            .headers()
            .get_all(reqwest::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|h| h.to_str().ok())
            .filter_map(|h| mpp::Challenge::parse(h).ok())
            .find(|c| c.method == mpp::METHOD && c.intent == mpp::INTENT);
        trace.push(json!({
            "step": "402", "ms": ms(t0),
            "x402": x402_offer.as_ref().map(|(_, a)| json!({"header": x402::PAYMENT_REQUIRED, "scheme": a.scheme, "network": a.network, "amount": a.amount, "payTo": a.pay_to, "paymentFlow": a.extra.payment_flow})),
            "mpp": mpp_offer.as_ref().map(|c| json!({"header": "WWW-Authenticate", "scheme": "Payment", "method": c.method, "intent": c.intent, "id": c.id, "expires": c.expires})),
        }));
        let use_mpp = match (prefer, &x402_offer, &mpp_offer) {
            (_, None, None) => return Err(Error::Other(format!("no zchannel offer on {}", self.ctx.network))),
            (Protocol::Mpp, _, Some(_)) | (Protocol::X402, None, Some(_)) => true,
            _ => false,
        };
        if use_mpp {
            let c = mpp_offer.expect("checked");
            let charge = c.charge()?;
            let inv = charge.method_details.invoice.clone();
            inv.verify(INVOICE)?;
            if charge.amount != inv.body.amount.to_string()
                || charge.recipient != inv.body.merchant_id
                || charge.external_id != inv.body.id
                || charge.method_details.network != self.ctx.network
            {
                return Err(Error::Other("challenge does not match its invoice".into()));
            }
            if charge.method_details.solver.trim_end_matches('/') != self.solver {
                return Err(Error::Other(format!("merchant pays through {}, not {}", charge.method_details.solver, self.solver)));
            }
            if c.expires.as_deref().and_then(mpp::parse_rfc3339).is_some_and(|e| now() > e) {
                return Err(Error::Other("challenge expired".into()));
            }
            let ack = self.pay_traced(&inv, &mut trace, t0, max_price).await?;
            let cred = mpp::Credential { challenge: c, source: None, payload: mpp::Payload { ack: ack.clone() } };
            trace.push(json!({"step": "retry", "ms": ms(t0), "header": "Authorization", "value": format!("Payment {}…", &cred.header()[8..40])}));
            let r = send(Some(("authorization", cred.header()))).await?;
            return fetched(r, Some(ack), trace, t0).await;
        }
        let (pr, req) = x402_offer.expect("checked");
        let inv = req.extra.invoice.clone();
        inv.verify(INVOICE)?;
        if inv.body.amount.to_string() != req.amount || inv.body.merchant_id != req.pay_to {
            return Err(Error::Other("offer does not match its invoice".into()));
        }
        if req.extra.solver.trim_end_matches('/') != self.solver {
            return Err(Error::Other(format!("merchant pays through {}, not {}", req.extra.solver, self.solver)));
        }
        let ack = self.pay_traced(&inv, &mut trace, t0, max_price).await?;
        let pp = PaymentPayload {
            x402_version: x402::VERSION,
            resource: Some(pr.resource),
            accepted: req,
            payload: ZchannelPayload { ack: ack.clone() },
        };
        let header = x402::encode(&pp);
        trace.push(json!({"step": "retry", "ms": ms(t0), "header": x402::PAYMENT_SIGNATURE, "value": format!("{}…", &header[..32])}));
        let r = send(Some((x402::PAYMENT_SIGNATURE, header))).await?;
        fetched(r, Some(ack), trace, t0).await
    }

    /// Quote and pay, recording each step.
    async fn pay_traced(&self, inv: &Signed<Invoice>, trace: &mut Vec<Value>, t0: std::time::Instant, max_price: Option<u64>) -> Result<Signed<Ack>> {
        check_price(inv.body.amount, max_price)?;
        let q = self.quote(inv).await?;
        trace.push(json!({
            "step": "quote", "ms": t0.elapsed().as_millis() as u64, "merchant": inv.body.merchant_id, "amount": q.body.amount,
            "fee": q.body.fee, "credit_applied": q.body.credit_applied, "channel_debit": q.body.increment,
        }));
        let channel = q.body.agent_channel.clone();
        let ack = self.pay_quote(q).await?;
        let level = self.ctx.store.read(|c| store::channel_get(c, &channel))?.map(|r| r.cumulative).unwrap_or(0);
        trace.push(json!({
            "step": "paid", "ms": t0.elapsed().as_millis() as u64, "agent_channel_level": level,
            "merchant_ack": {"merchant": ack.body.merchant_id, "invoice": ack.body.invoice_id, "merchant_channel_level": ack.body.cumulative},
        }));
        Ok(ack)
    }

    /// Pays a checkout invoice, refusing above `max_price` zat as `fetch_capped` does.
    pub async fn pay_invoice_capped(&self, invoice: &Signed<Invoice>, max_price: Option<u64>) -> Result<Signed<Ack>> {
        check_price(invoice.body.amount, max_price)?;
        self.pay_invoice(invoice).await
    }

    /// Repeats a request with an x402 payment payload (for tests of replays and mismatches).
    pub async fn get_with(&self, url: &str, pp: &PaymentPayload) -> Result<Fetched> {
        let r = self.ctx.http.get(url).header(x402::PAYMENT_SIGNATURE, x402::encode(pp)).send().await?;
        fetched(r, Some(pp.payload.ack.clone()), vec![], std::time::Instant::now()).await
    }

    /// Repeats a request with an MPP credential (for tests of replays and tampering).
    pub async fn get_with_credential(&self, url: &str, cred: &mpp::Credential) -> Result<Fetched> {
        let r = self.ctx.http.get(url).header("authorization", cred.header()).send().await?;
        fetched(r, Some(cred.payload.ack.clone()), vec![], std::time::Instant::now()).await
    }
}

async fn fetched(r: reqwest::Response, ack: Option<Signed<Ack>>, mut trace: Vec<Value>, t0: std::time::Instant) -> Result<Fetched> {
    let status = r.status().as_u16();
    let settlement = r
        .headers()
        .get(x402::PAYMENT_RESPONSE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| x402::decode(h).ok());
    let receipt = r.headers().get(mpp::RECEIPT).and_then(|h| h.to_str().ok()).and_then(|h| mpp::Receipt::parse(h).ok());
    let text = r.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    let elapsed_ms = t0.elapsed().as_millis() as u64;
    if !trace.is_empty() {
        trace.push(json!({"step": "response", "ms": elapsed_ms, "status": status, "x402_settlement": settlement, "mpp_receipt": receipt}));
    }
    Ok(Fetched { status, body, settlement, receipt, ack, trace, elapsed_ms })
}

// ---- Approval page (QR checkout) ------------------------------------------------------

pub fn router(agent: Arc<Agent>) -> Router {
    Router::new()
        .route("/t/{token}/", get(home))
        .route("/t/{token}/pay", get(pay_page))
        .route("/t/{token}/api/resolve", post(resolve))
        .route("/t/{token}/api/approve", post(approve))
        .route("/t/{token}/api/status", get(status))
        .with_state(agent)
}

fn authorize(a: &Agent, token: &str) -> ApiResult<()> {
    if token != a.token {
        return Err(err(StatusCode::NOT_FOUND, "not found"));
    }
    Ok(())
}

pub async fn run(agent: Arc<Agent>) {
    loop {
        if let Err(e) = payer::watch(&agent.ctx).await {
            eprintln!("payer watch: {e}");
        }
        tokio::time::sleep(agent.ctx.cfg.poll).await;
    }
}

#[derive(Deserialize)]
struct ResolveReq {
    r#ref: String,
}

/// Fetches a checkout invoice by reference, checks it, and asks the solver for a quote.
async fn resolve(State(a): State<Arc<Agent>>, Path(token): Path<String>, Json(req): Json<ResolveReq>) -> ApiResult<Json<Value>> {
    authorize(&a, &token)?;
    if !req.r#ref.starts_with("http://") && !req.r#ref.starts_with("https://") {
        return Err(err(StatusCode::BAD_REQUEST, "reference must be an http(s) invoice URL"));
    }
    let row: InvoiceRow = web::get(&a.ctx.http, &req.r#ref).await?;
    let inv = row.invoice;
    inv.verify(INVOICE)?;
    if !req.r#ref.ends_with(&format!("/invoices/{}", inv.body.id)) {
        return Err(err(StatusCode::BAD_REQUEST, "the reference does not name this invoice"));
    }
    if row.status != "open" {
        return Err(err(StatusCode::CONFLICT, "invoice already paid"));
    }
    if now() > inv.body.expires_at {
        return Err(err(StatusCode::GONE, "invoice expired"));
    }
    let q = a.quote(&inv).await?;
    Ok(Json(json!({
        "merchant": inv.body.merchant_id, "item": inv.body.resource, "amount": inv.body.amount,
        "fee": q.body.fee, "credit_applied": q.body.credit_applied, "total": q.body.increment,
        "expires_in": inv.body.expires_at.saturating_sub(now()), "quote": q,
    })))
}

#[derive(Deserialize)]
struct ApproveReq {
    quote: Signed<Quote>,
}

async fn approve(State(a): State<Arc<Agent>>, Path(token): Path<String>, Json(req): Json<ApproveReq>) -> ApiResult<Json<Value>> {
    authorize(&a, &token)?;
    let info = a.solver_info().await?;
    a.check_quote(&req.quote, &info)?;
    let ack = a.pay_quote(req.quote).await?;
    Ok(Json(json!({"ack": ack})))
}

async fn status(State(a): State<Arc<Agent>>, Path(token): Path<String>) -> ApiResult<Json<Value>> {
    authorize(&a, &token)?;
    let (chans, pays) = a.ctx.store.read(|c| Ok((store::channels(c, "payer")?, store::payments(c, None)?)))?;
    let left = a.channel().ok().and_then(|r| payer::remaining(&a.ctx, &r).ok());
    Ok(Json(json!({"channels": chans, "payments": pays, "remaining": left, "network": a.ctx.network})))
}

async fn home(State(a): State<Arc<Agent>>, Path(token): Path<String>) -> ApiResult<Html<String>> {
    authorize(&a, &token)?;
    let ch = a.channel().ok();
    let left = ch.as_ref().and_then(|r| payer::remaining(&a.ctx, r).ok()).unwrap_or(0);
    let b = format!(
        r#"<h1>zchan agent</h1><p class=mute>{net} · solver {solver}</p>
<div class=card><div class=mute>Channel {id}</div><div class=big>{left} ZEC left</div><div class=mute>paid so far {paid} zat</div></div>
<div class=card><p>Scan a merchant's checkout QR with your phone camera, or paste its invoice link:</p>
<input id=ref placeholder="http://merchant/invoices/…"><p><button onclick="location='pay?ref='+encodeURIComponent(document.getElementById('ref').value)">Continue</button></p>
<p class=mute>Save this page's address as your agent link on the merchant's page: <span class=mono id=me></span></p></div>
<script>document.getElementById('me').textContent=location.href.replace(/\/$/,'')</script>"#,
        net = esc(&a.ctx.network),
        solver = esc(&a.solver),
        id = esc(&ch.as_ref().map(|r| r.id[..12].to_string()).unwrap_or_else(|| "none".into())),
        left = zec(left),
        paid = ch.as_ref().map_or(0, |r| r.cumulative),
    );
    Ok(Html(page("zchan agent", &b)))
}

async fn pay_page(State(a): State<Arc<Agent>>, Path(token): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Html<String>> {
    authorize(&a, &token)?;
    let r = q.get("ref").cloned().unwrap_or_default();
    let b = format!(
        r#"<h1>Approve payment</h1><div class=card id=box><p class=mute>Checking invoice…</p></div>
<script>
const ref={r};const box=document.getElementById('box');let quote=null;
function z(n){{return (n/1e8).toFixed(8).replace(/0+$/,'').replace(/\.$/,'.0')}}
function esc(s){{return String(s).replace(/[&<>"]/g,c=>({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}}[c]))}}
async function post(u,b){{const r=await fetch(u,{{method:'POST',headers:{{'content-type':'application/json'}},body:JSON.stringify(b)}});const j=await r.json();if(!r.ok)throw new Error(j.error||r.status);return j}}
(async()=>{{try{{const j=await post('api/resolve',{{ref}});quote=j.quote;
box.innerHTML=`<div class=mute>Merchant</div><div class=big>${{esc(j.merchant)}}</div><p>${{esc(j.item)}}</p>
<table><tr><td>Amount</td><td>${{z(j.amount)}} ZEC</td></tr><tr><td>Solver fee</td><td>${{z(j.fee)}} ZEC</td></tr>
<tr><td>Credit applied</td><td>${{z(j.credit_applied)}} ZEC</td></tr><tr><th>Channel debit</th><th>${{z(j.total)}} ZEC</th></tr>
<tr><td>Expires in</td><td>${{j.expires_in}} s</td></tr></table>
<p><button id=ok>Approve</button> <button class=alt onclick="location='./'">Cancel</button></p>`;
document.getElementById('ok').onclick=approve}}catch(e){{box.innerHTML='<p class=bad>'+esc(e.message)+'</p>'}}}})();
async function approve(){{box.innerHTML='<p class=mute>Paying…</p>';try{{const j=await post('api/approve',{{quote}});
box.innerHTML='<div class="big ok">Paid ✓</div><p class=mute>Receipt: merchant acknowledged invoice '+esc(j.ack.body.invoice_id.slice(0,8))+' at channel level '+j.ack.body.cumulative+'</p><p><a href="./">Back</a></p>'}}
catch(e){{box.innerHTML='<p class=bad>'+esc(e.message)+'</p>'}}}}
</script>"#,
        r = serde_json::to_string(&r).unwrap(),
    );
    Ok(Html(page("Approve payment", &b)))
}
