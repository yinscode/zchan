//! The payer's side of a running channel: opening it (the refund is stored before the
//! funding transaction is broadcast), signing states, and watching for the refund height.

use serde::{Deserialize, Serialize};
use serde_json::json;
use zchan_tx::{txid_hex, watch::{Kit, Status}};

use crate::{
    channel::{Payer, Terms},
    ctx::Ctx,
    store::{self, ChannelRow},
    web,
    wire::{addr_from_str, pk_from_hex, sig_from_hex, sig_hex, TermsWire},
    Error, Result,
};

/// A payee's fresh channel key, handed out by `GET /channels/key`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeySlot {
    pub slot: u32,
    pub pubkey: String,
    pub address: String,
    pub payee_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposeReq {
    pub slot: u32,
    pub terms: TermsWire,
    pub payer_url: String,
    pub payer_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposeResp {
    pub refund_sig: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundedResp {
    pub channel_id: String,
    pub height: u32,
}

/// Opens a channel to `payee_url` funded from this party's deposit coins, and waits until
/// the payee has seen the funding confirm.
pub async fn open(ctx: &Ctx, payee_url: &str, capacity: u64, lock_blocks: u32) -> Result<ChannelRow> {
    let coins_lock = ctx.lock("coins");
    let _g = coins_lock.lock().await;
    let key: KeySlot = web::get(&ctx.http, &format!("{payee_url}/channels/key")).await?;
    let payee = pk_from_hex(&key.pubkey)?;
    let payee_address = addr_from_str(&key.address)?;
    let idx = ctx.store.tx(store::next_index)?;
    let sk = ctx.wallet.channel_key(idx);
    let coins = ctx.coins().await?;
    if coins.is_empty() {
        return Err(Error::Funding(format!("no confirmed coins at {}", ctx.deposit_address())));
    }
    let tip = ctx.tip().await?;
    let target = tip + 1;
    let pr = Payer::propose(
        &ctx.p,
        &sk,
        payee,
        payee_address,
        ctx.wallet.payout_address(idx),
        capacity,
        target,
        target + lock_blocks,
        coins,
        ctx.wallet.coin_address(0),
    )?;
    let terms = pr.terms.clone();
    let req = ProposeReq {
        slot: key.slot,
        terms: (&terms).into(),
        payer_url: ctx.cfg.base_url.clone(),
        payer_id: ctx.cfg.name.clone(),
    };
    let resp: ProposeResp = web::post(&ctx.http, &format!("{payee_url}/channels/propose"), &req).await?;
    let (_, refund, funding) = Payer::fund(&ctx.p, pr, &sk, &sig_from_hex(&resp.refund_sig)?)?;
    let label = format!("{}→{}", ctx.cfg.name, key.payee_id);
    let kit = Kit::new(&label, &terms.funding_txid, terms.funding_index, &refund, terms.lock_height, tip);
    let id = terms.id();
    let row = ChannelRow {
        id: id.clone(),
        role: "payer".into(),
        idx,
        peer_url: payee_url.into(),
        peer_id: key.payee_id.clone(),
        terms: (&terms).into(),
        status: "funding".into(),
        cumulative: 0,
        sig: None,
        refund: Some(hex::encode(&refund.raw)),
        kit: Some(kit),
        close_txid: None,
        funding_height: None,
        settled_by: None,
    };
    // The refund is durable before the funding transaction leaves this process.
    ctx.store.tx(|c| {
        store::channel_put(c, &row)?;
        store::event(c, "channel_funding", json!({
            "channel": id, "role": "payer", "peer": key.payee_id, "capacity": capacity,
            "lock_height": terms.lock_height, "refund_txid": txid_hex(&refund.txid),
        }))
    })?;
    ctx.broadcast(&funding).await?;
    let h = ctx.wait_mined(&funding).await?;
    let url = format!("{payee_url}/channels/{id}/funded");
    let mut tries = 0;
    loop {
        match web::post::<_, FundedResp>(&ctx.http, &url, &json!({})).await {
            Ok(_) => break,
            Err(e) if tries < 20 => {
                tries += 1;
                eprintln!("payee has not seen the funding yet ({e}); retrying");
                tokio::time::sleep(ctx.cfg.poll).await;
            }
            Err(e) => return Err(e),
        }
    }
    ctx.store.tx(|c| {
        let r = store::channel_update(c, &id, |r| {
            r.status = "open".into();
            r.funding_height = Some(h);
        })?;
        store::event(c, "channel_open", json!({"channel": id, "role": "payer", "height": h}))?;
        Ok(r)
    })
}

/// How much more this payer channel can pay.
pub fn remaining(ctx: &Ctx, row: &ChannelRow) -> Result<u64> {
    let terms = Terms::try_from(&row.terms)?;
    Ok(terms.max_payable(&ctx.p)?.saturating_sub(row.cumulative))
}

/// The next state, `increment` above the row's: (cumulative, signature hex). Nothing is
/// stored; the caller persists it with whatever else the payment changes.
pub fn next_state(ctx: &Ctx, row: &ChannelRow, increment: u64) -> Result<(u64, String)> {
    if row.role != "payer" || row.status != "open" {
        return Err(Error::State(format!("channel {} is {} {}", row.id, row.role, row.status)));
    }
    let terms = Terms::try_from(&row.terms)?;
    let cumulative = row.cumulative + increment;
    let sig = terms.sign_state(&ctx.p, &ctx.wallet.channel_key(row.idx), cumulative)?;
    Ok((cumulative, sig_hex(&sig)))
}

/// One pass of the refund watcher over every unsettled payer channel.
pub async fn watch(ctx: &Ctx) -> Result<()> {
    for row in ctx.store.read(|c| store::channels(c, "payer"))? {
        if row.status == "closed" {
            continue;
        }
        let Some(mut kit) = row.kit.clone() else { continue };
        let before = (kit.status.clone(), kit.scanned_to);
        let st = kit.step(&ctx.node).await?;
        if (st.clone(), kit.scanned_to) == before {
            continue;
        }
        ctx.store.tx(|c| {
            store::channel_update(c, &row.id, |r| {
                r.kit = Some(kit.clone());
                if let Status::Settled { txid, by_refund, .. } = &st {
                    r.status = "closed".into();
                    r.close_txid = Some(txid.clone());
                    if r.settled_by.is_none() {
                        r.settled_by = Some(if *by_refund { "refund" } else { "close" }.into());
                    }
                }
            })?;
            match &st {
                Status::RefundSent { txid } => store::event(c, "refund_sent", json!({"channel": row.id, "txid": txid})),
                Status::Settled { txid, height, by_refund } => store::event(
                    c,
                    "channel_settled",
                    json!({"channel": row.id, "role": "payer", "txid": txid, "height": height, "by_refund": by_refund}),
                ),
                Status::Watching => Ok(()),
            }
        })?;
    }
    Ok(())
}
