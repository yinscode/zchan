//! Refund-safety gate on Zakura regtest. Needs a fresh node:
//!
//! ```sh
//! infra/zakura/scripts/down.sh; infra/zakura/scripts/up.sh
//! cargo test -p zchan --release --test regtest -- --ignored --nocapture
//! ```
//!
//! Gate:
//! 1. agent→solver: the refund is fully signed before funding is broadcast, is rejected
//!    at `lock_height − 1`, and is broadcast by the watcher and confirmed at `lock_height`.
//! 2. solver→merchant: two payer-signed states are accepted, a decreasing one is refused,
//!    and the merchant's close of the latest state confirms with the agreed outputs. The
//!    refund then cannot spend the closed output.

use serde_json::json;
use zchan::channel::{Payee, Payer};
use zchan_tx::{
    keys,
    params::{BRANCH_NU7, NU7_AT},
    regtest::Regtest,
    tx::Final,
    txid_hex,
    wallet::Wallet,
    watch::{Kit, Status},
};

const CAPACITY: u64 = 5000_0000;

struct Log(Vec<serde_json::Value>);

impl Log {
    fn note(&mut self, step: &str, v: serde_json::Value) {
        println!("[{step}] {v}");
        self.0.push(json!({"step": step, "result": v}));
    }
}

async fn send(rt: &Regtest, f: &Final) -> Result<String, String> {
    rt.node.send(&f.raw).await.map_err(|e| e.to_string())
}

#[tokio::test]
#[ignore = "needs infra/zakura (fresh chain)"]
async fn gate() {
    let url = std::env::var("ZCHAN_NODE_URL").unwrap_or("http://127.0.0.1:18272/".into());
    let rt = Regtest::connect(&url).await.expect("node up: infra/zakura/scripts/up.sh");
    assert!(rt.tip().await.unwrap() < 100, "needs a fresh chain: down.sh && up.sh");
    rt.node.mine_to(NU7_AT + 10).await.unwrap();
    let p = rt.p;
    let mut log = Log(vec![]);
    let (agent, solver, merchant) = (Wallet::dev("agent"), Wallet::dev("solver"), Wallet::dev("merchant-a"));

    // ---- 1. agent→solver: refund before funding, valid only from its lock height -------
    let coin = rt.faucet(&agent.coin_key(0), 1_0000_0000).await.unwrap();
    let target = rt.tip().await.unwrap() + 1;
    let lock = target + 8;
    let pr = Payer::propose(
        &p,
        &agent.channel_key(0),
        keys::pubkey(&solver.channel_key(0)),
        solver.payout_address(0),
        agent.payout_address(0),
        CAPACITY,
        target,
        lock,
        vec![(coin, agent.coin_key(0))],
        agent.coin_address(0),
    )
    .unwrap();
    // The solver checks the terms and signs the refund; the agent holds a fully signed
    // refund before its funding transaction is even signed.
    pr.terms.check(&p).unwrap();
    let rsig = pr.terms.sign_refund(&p, &solver.channel_key(0)).unwrap();
    let (payer, refund, funding) = Payer::fund(&p, pr, &agent.channel_key(0), &rsig).unwrap();
    let terms = payer.terms.clone();
    assert_eq!(refund.branch_id, BRANCH_NU7);
    log.note("agent→solver refund signed before funding", json!({
        "funding_txid": txid_hex(&terms.funding_txid), "refund_txid": txid_hex(&refund.txid),
        "lock_height": lock, "expiry_height": refund.expiry_height, "branch": format!("{:08x}", refund.branch_id),
    }));
    let mut kit = Kit::new("agent→solver", &terms.funding_txid, 0, &refund, lock, rt.tip().await.unwrap());
    let h_fund = rt.send_confirm(&funding).await.expect("funding accepted");
    assert_eq!(funding.txid, terms.funding_txid);
    let seen = terms.check_funding(&rt.node, 1).await.expect("solver sees the funding");
    assert_eq!(seen, h_fund);
    log.note("agent→solver funding confirmed", json!({"height": h_fund, "capacity": CAPACITY}));

    rt.node.mine_to(lock - 1).await.unwrap();
    assert_eq!(kit.step(&rt.node).await.unwrap(), Status::Watching);
    let early = send(&rt, &refund).await;
    log.note("refund at tip = lock − 1", json!({"tip": lock - 1, "accepted": early.is_ok(), "detail": format!("{early:?}")}));
    assert!(early.is_err(), "refund must be rejected before its lock height");

    rt.mine(1).await.unwrap();
    let st = kit.step(&rt.node).await.unwrap();
    log.note("watcher at tip = lock", json!({"tip": lock, "status": format!("{st:?}")}));
    assert!(matches!(st, Status::RefundSent { .. }), "watcher broadcasts the refund at its lock height");
    rt.mine(1).await.unwrap();
    let st = kit.step(&rt.node).await.unwrap();
    let Status::Settled { height, by_refund, .. } = st.clone() else { panic!("{st:?}") };
    assert!(by_refund);
    assert_eq!(height, lock + 1);
    let fee = CAPACITY - terms.refund_spec(&p).unwrap().outputs[0].value();
    let outs = rt.outputs(&refund).await.unwrap();
    assert_eq!(outs, vec![CAPACITY - fee]);
    log.note("refund confirmed", json!({"mined_height": height, "txid": txid_hex(&refund.txid), "outputs_zat": outs, "fee": fee}));

    // ---- 2. solver→merchant: states, a refused decrease, and the merchant's close ------
    let coin = rt.faucet(&solver.coin_key(1), 1_0000_0000).await.unwrap();
    let target = rt.tip().await.unwrap() + 1;
    let lock = target + 30;
    let pr = Payer::propose(
        &p,
        &solver.channel_key(1),
        keys::pubkey(&merchant.channel_key(0)),
        merchant.payout_address(0),
        solver.payout_address(1),
        CAPACITY,
        target,
        lock,
        vec![(coin, solver.coin_key(1))],
        solver.coin_address(1),
    )
    .unwrap();
    let rsig = pr.terms.sign_refund(&p, &merchant.channel_key(0)).unwrap();
    let (mut payer, refund, funding) = Payer::fund(&p, pr, &solver.channel_key(1), &rsig).unwrap();
    let h_fund = rt.send_confirm(&funding).await.expect("funding accepted");
    let mut payee = Payee::new(payer.terms.clone());
    payee.terms.check_funding(&rt.node, 1).await.expect("merchant sees the funding");
    log.note("solver→merchant funding confirmed", json!({"height": h_fund, "funding_txid": txid_hex(&funding.txid), "lock_height": lock}));

    let sk = solver.channel_key(1);
    let (c1, s1) = payer.pay(&p, &sk, 30_000).unwrap();
    assert_eq!(payee.accept(&p, c1, &s1).unwrap(), 30_000);
    let (c2, s2) = payer.pay(&p, &sk, 40_000).unwrap();
    assert_eq!(payee.accept(&p, c2, &s2).unwrap(), 40_000);
    let dec = payee.accept(&p, c1, &s1);
    assert!(dec.is_err());
    log.note("states", json!({"accepted": [c1, c2], "decreasing_refused": format!("{:?}", dec.err())}));

    let close = payee.close(&p, &merchant.channel_key(0)).unwrap();
    let h_close = rt.send_confirm(&close).await.expect("close accepted");
    assert!(h_close <= lock);
    let fee = payer.terms.state_fee(&p).unwrap();
    let outs = rt.outputs(&close).await.unwrap();
    assert_eq!(outs, vec![c2, CAPACITY - fee - c2]);
    log.note("merchant close confirmed", json!({"mined_height": h_close, "txid": txid_hex(&close.txid), "outputs_zat": outs, "fee": fee}));

    rt.node.mine_to(lock).await.unwrap();
    let late = send(&rt, &refund).await;
    log.note("refund after close", json!({"tip": lock, "accepted": late.is_ok(), "detail": format!("{late:?}")}));
    assert!(late.is_err(), "a closed channel's refund has nothing to spend");

    if let Ok(path) = std::env::var("ZCHAN_RESULTS") {
        std::fs::write(path, serde_json::to_string_pretty(&log.0).unwrap()).unwrap();
    }
}
