//! The payer's side of a running channel: opening it (the refund is stored before the
//! funding transaction is broadcast), signing states, and watching for the refund height.

use serde::{Deserialize, Serialize};
use serde_json::json;
use zchan_tx::{txid_hex, watch::{Kit, Status}};

use crate::{
    channel::{Funding, Payer, Terms},
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
    /// The payer's Ironwood address for this channel (raw, hex): a cooperative close may
    /// pay the payer's change there instead of to `terms.payer_address`.
    #[serde(default)]
    pub payer_ironwood: Option<String>,
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

/// Opens a channel to `payee_url` and waits until the payee has seen the funding confirm.
/// With `shielded`, the funding spends this party's Ironwood notes (no transparent
/// inputs); otherwise its transparent coins, fewest first, with change to a fresh
/// address that no other channel uses.
pub async fn open(ctx: &Ctx, payee_url: &str, capacity: u64, lock_blocks: u32, shielded: bool) -> Result<ChannelRow> {
    let coins_lock = ctx.lock("coins");
    let _g = coins_lock.lock().await;
    let key: KeySlot = web::get(&ctx.http, &format!("{payee_url}/channels/key")).await?;
    let payee = pk_from_hex(&key.pubkey)?;
    let payee_address = addr_from_str(&key.address)?;
    let idx = ctx.store.tx(store::next_index)?;
    let sk = ctx.wallet.channel_key(idx);
    let tip = ctx.tip().await?;
    let target = tip + 1;
    let funding = if shielded {
        let notes = ctx.shielded_notes().await?;
        let have: u64 = notes.iter().map(|n| n.value()).sum();
        // The channel output and shielded change: ZIP 317 for n spends, one transparent
        // output and one Ironwood output.
        let fee = |n: usize| zchan_tx::notes::zip317_fee(0, 1, n, 1);
        let pick = zchan_tx::notes::select(notes, capacity + crate::channel::MIN_CHANGE, fee)
            .ok_or_else(|| Error::Funding(format!("shielded balance {have} cannot fund {capacity}")))?;
        let (spends, anchor) = ctx.shielded_spends(&pick).await?;
        Funding::Shielded { spends, anchor, sk: ctx.wallet.ironwood_sk(0), change: ctx.wallet.shielded_change() }
    } else {
        let mut coins = ctx.coins().await?;
        coins.sort_by_key(|(c, _)| std::cmp::Reverse(c.value));
        // The channel output and change: ZIP 317 for n P2PKH inputs and two outputs.
        let need = |n: usize| capacity + crate::channel::MIN_CHANGE + zchan_tx::notes::zip317_fee(n, 2, 0, 0);
        let mut pick = vec![];
        let mut got = 0;
        for c in coins {
            if !pick.is_empty() && got >= need(pick.len()) {
                break;
            }
            got += c.0.value;
            pick.push(c);
        }
        if pick.is_empty() || got < need(pick.len()) {
            return Err(Error::Funding(format!("transparent coins hold {got}; a {capacity} channel needs {}", need(pick.len().max(1)))));
        }
        // Change goes to this channel's own coin address, watched from now on.
        ctx.store.tx(|c| Ctx::watch_coin_index(c, idx))?;
        Funding::Transparent { coins: pick, change: ctx.wallet.coin_address(idx) }
    };
    let pr = Payer::propose_with(
        &ctx.p,
        &sk,
        payee,
        payee_address,
        ctx.wallet.payout_address(idx),
        capacity,
        target,
        target + lock_blocks,
        funding,
    )?;
    let terms = pr.terms.clone();
    let req = ProposeReq {
        slot: key.slot,
        terms: (&terms).into(),
        payer_url: ctx.cfg.base_url.clone(),
        payer_id: ctx.cfg.name.clone(),
        payer_ironwood: Some(hex::encode(ctx.wallet.shielded_address(idx).to_raw_address_bytes())),
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
        peer_ironwood: None,
    };
    // The refund is durable before the funding transaction leaves this process.
    ctx.store.tx(|c| {
        store::channel_put(c, &row)?;
        store::event(c, "channel_funding", json!({
            "channel": id, "role": "payer", "peer": key.payee_id, "capacity": capacity, "shielded_funding": shielded,
            "lock_height": terms.lock_height, "refund_txid": txid_hex(&refund.txid),
        }))
    })?;
    ctx.broadcast(&funding).await?;
    let h = match ctx.wait_mined(&funding).await {
        Ok(h) => h,
        Err(e) => {
            abandon(ctx, &id, &e.to_string())?;
            return Err(e);
        }
    };
    finish_open(ctx, &id, h).await
}

/// Marks a channel whose funding expired unmined: nothing was spent, so the coins or
/// notes are free again.
fn abandon(ctx: &Ctx, id: &str, why: &str) -> Result<()> {
    ctx.store.tx(|c| {
        store::channel_update(c, id, |r| r.status = "abandoned".into())?;
        store::event(c, "channel_abandoned", json!({"channel": id, "reason": why}))
    })
}

/// After the funding is mined: has the payee check it, then marks the channel open.
async fn finish_open(ctx: &Ctx, id: &str, h: u32) -> Result<ChannelRow> {
    let row = ctx
        .store
        .read(|c| store::channel_get(c, id))?
        .ok_or_else(|| Error::State(format!("no channel {id}")))?;
    let payee_url = row.peer_url.clone();
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

/// Settles channels left mid-funding (the process stopped while waiting for a block):
/// opens those whose funding was mined, abandons those whose funding expired.
pub async fn resume_funding(ctx: &Ctx) -> Result<()> {
    let tip = ctx.tip().await?;
    for row in ctx.store.read(|c| store::channels(c, "payer"))? {
        if row.status != "funding" {
            continue;
        }
        let txid = row.terms.funding_txid.clone();
        match ctx.node.mined_height(&txid).await {
            Ok(Some(h)) => {
                finish_open(ctx, &row.id, h).await?;
            }
            _ if tip >= row.terms.target_height + crate::channel::FUNDING_EXPIRY => {
                abandon(ctx, &row.id, "funding expired without being mined")?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Sends `amount` from this party's deposit coins to `to`, change back to the deposit
/// address (to split faucet funds between parties). Returns the display txid.
pub async fn send(ctx: &Ctx, to: zcash_transparent::address::TransparentAddress, amount: u64) -> Result<String> {
    use zchan_tx::tx::{self, Pay, Spec};
    let coins_lock = ctx.lock("coins");
    let _g = coins_lock.lock().await;
    let coins = ctx.coins().await?;
    let total: u64 = coins.iter().map(|(c, _)| c.value).sum();
    let ci = ctx.store.tx(|c| {
        let i = store::next_index(c)?;
        Ctx::watch_coin_index(c, i)?;
        Ok(i)
    })?;
    let change = ctx.wallet.coin_address(ci);
    let tip = ctx.tip().await?;
    let mut spec = Spec {
        target_height: tip + 1,
        expiry_height: tip + 40,
        lock_height: None,
        inputs: coins.iter().map(|(c, _)| c.clone()).collect(),
        outputs: vec![Pay::Transparent(to, amount), Pay::Transparent(change, 1)],
        ironwood_anchor: None,
        ironwood_spends: vec![],
    };
    let fee = tx::fee(&ctx.p, &spec)?;
    let left = total
        .checked_sub(amount + fee)
        .filter(|l| *l >= crate::channel::MIN_CHANGE)
        .ok_or_else(|| Error::Funding(format!("deposit holds {total}; cannot send {amount} + fee {fee} + change")))?;
    spec.outputs[1] = Pay::Transparent(change, left);
    let mut pczt = tx::build(&ctx.p, &spec)?;
    for (i, (_, sk)) in coins.iter().enumerate() {
        pczt = tx::sign(pczt, i, sk)?;
    }
    let f = tx::extract(pczt)?;
    let txid = ctx.broadcast(&f).await?;
    ctx.wait_mined(&f).await?;
    ctx.store.tx(|c| store::event(c, "sent", json!({"to": crate::wire::addr_str(&to), "amount": amount, "fee": fee, "txid": txid})))?;
    Ok(txid)
}

/// Sends `amount` from this party's shielded notes to an Ironwood address, change back
/// to the account (to fund another party privately). Returns the display txid.
pub async fn send_shielded(ctx: &Ctx, to: orchard::Address, amount: u64) -> Result<String> {
    use zchan_tx::tx::{self, Pay, Spec};
    let coins_lock = ctx.lock("coins");
    let _g = coins_lock.lock().await;
    let notes = ctx.shielded_notes().await?;
    let pick = zchan_tx::notes::select(notes, amount, |n| zchan_tx::notes::zip317_fee(0, 0, n, 2))
        .ok_or_else(|| Error::Funding(format!("shielded balance cannot send {amount}")))?;
    let have: u64 = pick.iter().map(|n| n.value()).sum();
    let tip = ctx.tip().await?;
    let (spends, anchor) = ctx.shielded_spends(&pick).await?;
    let change = ctx.wallet.shielded_change();
    let mut spec = Spec {
        target_height: tip + 1,
        expiry_height: tip + 40,
        lock_height: None,
        inputs: vec![],
        outputs: vec![Pay::Ironwood(to, amount), Pay::Ironwood(change, 1)],
        ironwood_anchor: Some(anchor),
        ironwood_spends: spends,
    };
    let fee = tx::fee(&ctx.p, &spec)?;
    let left = have.checked_sub(amount + fee).ok_or_else(|| Error::Funding(format!("{have} cannot cover {amount} + fee {fee}")))?;
    spec.outputs[1] = Pay::Ironwood(change, left);
    let (pczt, n) = tx::sign_ironwood(tx::build(&ctx.p, &spec)?, &orchard::keys::SpendAuthorizingKey::from(&ctx.wallet.ironwood_sk(0)))?;
    if n != pick.len() {
        return Err(Error::Funding(format!("signed {n} of {} spends", pick.len())));
    }
    let f = tx::extract(pczt)?;
    let txid = ctx.broadcast(&f).await?;
    ctx.wait_mined(&f).await?;
    ctx.store.tx(|c| store::event(c, "sent_shielded", json!({"amount": amount, "fee": fee, "txid": txid})))?;
    Ok(txid)
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
    resume_funding(ctx).await?;
    for row in ctx.store.read(|c| store::channels(c, "payer"))? {
        if row.status == "closed" || row.status == "abandoned" {
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
