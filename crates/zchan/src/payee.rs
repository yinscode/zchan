//! The payee's side of a running channel: handing out a key, signing the refund, checking
//! the funding, verifying states, and closing before the refund height.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use secp256k1::{ecdsa::Signature, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zchan_tx::{
    keys,
    tx::{self, Pay, Spec},
    txid_hex,
};

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
        .route("/channels/{id}/close-offer", post(close_offer))
        .route("/channels/{id}/close-sign", post(close_sign))
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
    if let Some(a) = &req.payer_ironwood {
        if ironwood_address(a).is_none() {
            return Err(err(StatusCode::BAD_REQUEST, "payer_ironwood is not an Ironwood address"));
        }
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
        peer_ironwood: req.payer_ironwood.clone(),
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

/// Parses a raw Ironwood address (43 bytes, hex).
pub fn ironwood_address(hex_raw: &str) -> Option<orchard::Address> {
    let b: [u8; 43] = hex::decode(hex_raw).ok()?.try_into().ok()?;
    Option::from(orchard::Address::from_raw_address_bytes(&b))
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

// ---- Cooperative close ------------------------------------------------------------------

/// A cooperative close the payee built and proved, waiting for the payer's signature.
pub struct CoopCloseDraft {
    pub pczt: pczt::Pczt,
    pub cumulative: u64,
    /// What the payee receives (its share less the close's fee above a state's).
    pub mine: u64,
    pub fee: u64,
    pub both_shielded: bool,
}

/// Builds and proves the cooperative close of payee channel `row` at its latest state:
/// the payee's share to its Ironwood address for the channel, the payer's change to the
/// payer's Ironwood address when it gave one (else to its transparent address). The
/// payee pays whatever the close costs above a state's fee.
pub async fn build_coop_close(ctx: &Ctx, row: &ChannelRow) -> Result<CoopCloseDraft> {
    if row.role != "payee" || row.status != "open" || row.cumulative == 0 {
        return Err(Error::State(format!("channel {} is not an open payee channel with a payment", row.id)));
    }
    let terms = Terms::try_from(&row.terms)?;
    let tip = ctx.tip().await?;
    let change = terms.capacity - terms.state_fee(&ctx.p)? - row.cumulative;
    let to = ctx.wallet.shielded_address(row.idx);
    let payer_ironwood = row.peer_ironwood.as_deref().and_then(ironwood_address);
    let mut spec = Spec {
        target_height: tip + 1,
        expiry_height: tip + 40,
        lock_height: None,
        inputs: vec![terms.funding_coin()?],
        outputs: vec![
            Pay::Ironwood(to, 1),
            match payer_ironwood {
                Some(a) => Pay::Ironwood(a, change),
                None => Pay::Transparent(terms.payer_address, change),
            },
        ],
        ironwood_anchor: Some(ctx.node.ironwood_anchor(tip).await?),
        ironwood_spends: vec![],
    };
    let fee = tx::fee(&ctx.p, &spec)?;
    let mine = terms.capacity.checked_sub(change + fee).filter(|m| *m > 0).ok_or_else(|| {
        Error::State(format!(
            "an Ironwood close costs {fee} zat; the payee's share {} does not cover it (close transparently)",
            terms.capacity - change
        ))
    })?;
    spec.outputs[0] = Pay::Ironwood(to, mine);
    Ok(CoopCloseDraft {
        pczt: tx::build(&ctx.p, &spec)?,
        cumulative: row.cumulative,
        mine,
        fee,
        both_shielded: payer_ironwood.is_some(),
    })
}

/// Checks the payer's signature on the draft, adds the payee's, records the close and
/// broadcasts it. Returns the display txid.
pub async fn finish_coop_close(ctx: &Ctx, row: &ChannelRow, draft: CoopCloseDraft, payer_sig: &Signature) -> Result<String> {
    let terms = Terms::try_from(&row.terms)?;
    let sighash = tx::sighash(&draft.pczt, 0)?;
    payer_sig
        .verify(Message::from_digest(sighash), &terms.payer)
        .map_err(|_| Error::Signature("payer's close signature invalid".into()))?;
    let pczt = tx::append(draft.pczt, 0, *payer_sig)?;
    let f = tx::extract(tx::sign(pczt, 0, &ctx.wallet.channel_key(row.idx))?)?;
    let txid = txid_hex(&f.txid);
    ctx.store.tx(|c| {
        store::channel_update(c, &row.id, |r| {
            r.status = "closing".into();
            r.close_txid = Some(txid.clone());
            r.settled_by = Some("coop-ironwood".into());
        })?;
        store::event(c, "coop_close_broadcast", json!({
            "channel": row.id, "txid": txid, "cumulative": draft.cumulative, "payee_shielded_value": draft.mine,
            "fee": draft.fee, "both_shielded": draft.both_shielded,
        }))
    })?;
    ctx.broadcast(&f).await?;
    Ok(txid)
}

/// A close the payee built for the payer to check and sign (payer-initiated close).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseOffer {
    pub channel_id: String,
    /// The proved PCZT, hex.
    pub pczt: String,
    pub cumulative: u64,
    /// The close's fee, and what the payee receives (both paid from the payee's share).
    pub fee: u64,
    pub payee_value: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseSign {
    pub sig: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloseDone {
    pub txid: String,
}

fn open_payee(ctx: &Ctx, id: &str) -> ApiResult<ChannelRow> {
    Ok(ctx
        .store
        .read(|c| store::channel_get(c, id))?
        .filter(|r| r.role == "payee" && r.status == "open")
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no open channel {id}")))?)
}

/// The payer asks for a cooperative close: the payee builds it at the latest state and
/// keeps it until the payer's signature arrives. Refused while a payment on the channel
/// is in flight.
async fn close_offer(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> ApiResult<Json<CloseOffer>> {
    let lock = ctx.lock(&id);
    let _g = lock.lock().await;
    let row = open_payee(&ctx, &id)?;
    let inflight = ctx
        .store
        .read(|c| store::payments(c, Some("agent_signed")))?
        .iter()
        .any(|p| p.quote.body.agent_channel == id);
    if inflight {
        return Err(err(StatusCode::CONFLICT, "a payment on this channel is in flight"));
    }
    let draft = build_coop_close(&ctx, &row).await?;
    let bytes = draft.pczt.clone().serialize().map_err(|e| Error::Other(format!("pczt: {e:?}")))?;
    let offer = CloseOffer {
        channel_id: id.clone(),
        pczt: hex::encode(bytes),
        cumulative: draft.cumulative,
        fee: draft.fee,
        payee_value: draft.mine,
    };
    ctx.store.tx(|c| store::meta_set(c, &format!("close_offer:{id}"), &serde_json::to_string(&offer).unwrap()))?;
    Ok(Json(offer))
}

/// The payer's signature on the offered close: checked, completed and broadcast.
async fn close_sign(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, Json(req): Json<CloseSign>) -> ApiResult<Json<CloseDone>> {
    let lock = ctx.lock(&id);
    let _g = lock.lock().await;
    let row = open_payee(&ctx, &id)?;
    let offer: CloseOffer = ctx
        .store
        .read(|c| store::meta_get(c, &format!("close_offer:{id}")))?
        .and_then(|o| serde_json::from_str(&o).ok())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no close offer for this channel; ask for one"))?;
    if offer.cumulative != row.cumulative {
        return Err(err(StatusCode::CONFLICT, "the channel moved since the offer; ask for a new one"));
    }
    let pczt = pczt::Pczt::parse(&hex::decode(&offer.pczt).map_err(|e| Error::Wire(e.to_string()))?)
        .map_err(|e| Error::Wire(format!("pczt: {e:?}")))?;
    let draft = CoopCloseDraft {
        pczt,
        cumulative: offer.cumulative,
        mine: offer.payee_value,
        fee: offer.fee,
        both_shielded: row.peer_ironwood.is_some(),
    };
    let txid = finish_coop_close(&ctx, &row, draft, &sig_from_hex(&req.sig)?).await?;
    ctx.store.tx(|c| store::meta_set(c, &format!("close_offer:{id}"), ""))?;
    Ok(Json(CloseDone { txid }))
}

