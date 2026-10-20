//! The payee's side of a running channel: handing out a key, signing the refund, checking
//! the funding, verifying states, and closing before the refund height.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use zchan_tx::{keys, txid_hex};

use crate::{
    channel::Terms,
    ctx::Ctx,
    payer::{FundedResp, KeySlot, ProposeReq, ProposeResp},
    store::{self, ChannelRow},
    web::{err, ApiResult},
    wire::{addr_str, pk_hex, sig_from_hex, sig_hex},
    Error, Result,
};

pub fn routes() -> Router<Arc<Ctx>> {
    Router::new()
        .route("/channels/key", get(key))
        .route("/channels/propose", post(propose))
        .route("/channels/{id}/funded", post(funded))
}

async fn key(State(ctx): State<Arc<Ctx>>) -> ApiResult<Json<KeySlot>> {
    let idx = ctx.store.tx(|c| {
        let i = store::next_index(c)?;
        store::slot_add(c, i)?;
        Ok(i)
    })?;
    Ok(Json(KeySlot {
        slot: idx,
        pubkey: pk_hex(&keys::pubkey(&ctx.wallet.channel_key(idx))),
        address: addr_str(&ctx.wallet.payout_address(idx)),
        payee_id: ctx.cfg.name.clone(),
    }))
}

async fn propose(State(ctx): State<Arc<Ctx>>, Json(req): Json<ProposeReq>) -> ApiResult<Json<ProposeResp>> {
    let terms = Terms::try_from(&req.terms)?;
    if !ctx.store.tx(|c| store::slot_take(c, req.slot))? {
        return Err(err(StatusCode::BAD_REQUEST, format!("unknown or used key slot {}", req.slot)));
    }
    let sk = ctx.wallet.channel_key(req.slot);
    if terms.payee != keys::pubkey(&sk) || terms.payee_address != ctx.wallet.payout_address(req.slot) {
        return Err(err(StatusCode::BAD_REQUEST, "terms do not use the key slot's key and address"));
    }
    terms.check(&ctx.p)?;
    let tip = ctx.tip().await?;
    if terms.target_height.abs_diff(tip + 1) > 3 {
        return Err(err(StatusCode::BAD_REQUEST, format!("target height {} is not near {}", terms.target_height, tip + 1)));
    }
    if terms.lock_height < terms.target_height + ctx.cfg.min_lock {
        return Err(err(StatusCode::BAD_REQUEST, format!("lock under {} blocks", ctx.cfg.min_lock)));
    }
    let id = terms.id();
    if ctx.store.read(|c| store::channel_get(c, &id))?.is_some() {
        return Err(err(StatusCode::CONFLICT, format!("channel {id} exists")));
    }
    let sig = terms.sign_refund(&ctx.p, &sk)?;
    let row = ChannelRow {
        id: id.clone(),
        role: "payee".into(),
        idx: req.slot,
        peer_url: req.payer_url,
        peer_id: req.payer_id.clone(),
        terms: req.terms,
        status: "funding".into(),
        cumulative: 0,
        sig: None,
        refund: None,
        kit: None,
        close_txid: None,
        funding_height: None,
        settled_by: None,
    };
    ctx.store.tx(|c| {
        store::channel_put(c, &row)?;
        store::event(c, "refund_signed", json!({
            "channel": id, "payer": req.payer_id, "capacity": terms.capacity, "lock_height": terms.lock_height,
        }))
    })?;
    Ok(Json(ProposeResp { refund_sig: sig_hex(&sig) }))
}

async fn funded(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> ApiResult<Json<FundedResp>> {
    let row = ctx
        .store
        .read(|c| store::channel_get(c, &id))?
        .filter(|r| r.role == "payee")
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no channel {id}")))?;
    if let (true, Some(h)) = (row.status == "open", row.funding_height) {
        return Ok(Json(FundedResp { channel_id: id, height: h }));
    }
    if row.status != "funding" {
        return Err(err(StatusCode::CONFLICT, format!("channel {id} is {}", row.status)));
    }
    let terms = Terms::try_from(&row.terms)?;
    let h = terms.check_funding(&ctx.node, 1).await?;
    ctx.store.tx(|c| {
        store::channel_update(c, &id, |r| {
            r.status = "open".into();
            r.funding_height = Some(h);
        })?;
        store::event(c, "channel_open", json!({"channel": id, "role": "payee", "height": h}))
    })?;
    Ok(Json(FundedResp { channel_id: id, height: h }))
}

/// Verifies a state for an open payee channel: above the one held, within capacity,
/// signed by the payer over this channel. Stores nothing.
pub fn verify(ctx: &Ctx, row: &ChannelRow, cumulative: u64, sig: &str) -> Result<()> {
    if row.role != "payee" || row.status != "open" {
        return Err(Error::State(format!("channel {} is {} {}", row.id, row.role, row.status)));
    }
    if cumulative <= row.cumulative {
        return Err(Error::State(format!("{cumulative} is not above {}", row.cumulative)));
    }
    let terms = Terms::try_from(&row.terms)?;
    terms.verify_state(&ctx.p, cumulative, &sig_from_hex(sig)?)
}

/// Broadcasts the best state of payee channel `id` now (the payee's unilateral close).
pub async fn close_now(ctx: &Ctx, id: &str) -> Result<String> {
    let lock = ctx.lock(id);
    let _g = lock.lock().await;
    let row = ctx
        .store
        .read(|c| store::channel_get(c, id))?
        .ok_or_else(|| Error::State(format!("no channel {id}")))?;
    if row.role != "payee" || row.status != "open" {
        return Err(Error::State(format!("channel {id} is {} {}", row.role, row.status)));
    }
    let sig = row.sig.as_deref().ok_or_else(|| Error::State("no state to close".into()))?;
    let terms = Terms::try_from(&row.terms)?;
    let f = terms.close(&ctx.p, &ctx.wallet.channel_key(row.idx), row.cumulative, &sig_from_hex(sig)?)?;
    let txid = txid_hex(&f.txid);
    ctx.store.tx(|c| {
        store::channel_update(c, id, |r| {
            r.status = "closing".into();
            r.close_txid = Some(txid.clone());
            r.settled_by = Some("close".into());
        })?;
        store::event(c, "close_broadcast", json!({"channel": id, "cumulative": row.cumulative, "txid": txid}))
    })?;
    ctx.broadcast(&f).await?;
    Ok(txid)
}

/// One pass: close channels whose refund height is near, and settle broadcast closes.
pub async fn tick(ctx: &Ctx) -> Result<()> {
    let tip = ctx.tip().await?;
    for row in ctx.store.read(|c| store::channels(c, "payee"))? {
        match row.status.as_str() {
            "open" if tip + ctx.cfg.close_margin >= row.terms.lock_height => {
                if row.cumulative == 0 {
                    ctx.store.tx(|c| {
                        store::channel_update(c, &row.id, |r| r.status = "expired".into())?;
                        store::event(c, "channel_expired", json!({"channel": row.id, "note": "nothing received; the payer refunds"}))
                    })?;
                } else if let Err(e) = close_now(ctx, &row.id).await {
                    eprintln!("close {} failed: {e}", row.id);
                    ctx.store.tx(|c| store::event(c, "close_failed", json!({"channel": row.id, "error": e.to_string()})))?;
                }
            }
            "closing" => {
                let Some(txid) = &row.close_txid else { continue };
                if let Ok(Some(h)) = ctx.node.mined_height(txid).await {
                    ctx.store.tx(|c| {
                        store::channel_update(c, &row.id, |r| r.status = "closed".into())?;
                        store::event(c, "channel_settled", json!({"channel": row.id, "role": "payee", "txid": txid, "height": h}))
                    })?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
