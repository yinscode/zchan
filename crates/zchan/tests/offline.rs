//! Channel rules that need no node: terms, refund-before-funding, and the payee's
//! verifier. The funding outputs here are made up; nothing is broadcast.

use zchan::channel::{Payee, Payer, Terms, MIN_LOCK};
use zchan_tx::{
    keys,
    params::{self, NU7_AT},
    tx::{self, Coin, Spend},
    wallet::Wallet,
};
use zcash_protocol::local_consensus::LocalNetwork;

const CAPACITY: u64 = 5000_0000;
const TARGET: u32 = NU7_AT + 10;

fn p() -> LocalNetwork {
    params::regtest(Some(NU7_AT))
}

fn terms(payer: &Wallet, payee: &Wallet, funding_txid: [u8; 32]) -> Terms {
    Terms {
        payer: keys::pubkey(&payer.channel_key(0)),
        payee: keys::pubkey(&payee.channel_key(0)),
        capacity: CAPACITY,
        funding_txid,
        funding_index: 0,
        target_height: TARGET,
        lock_height: TARGET + 100,
        payer_address: payer.payout_address(0),
        payee_address: payee.payout_address(0),
    }
}

#[test]
fn state_rebuilds_identically() {
    let (a, s) = (Wallet::dev("agent"), Wallet::dev("solver"));
    let t = terms(&a, &s, [7; 32]);
    let spec = t.state_spec(&p(), 12_345).unwrap();
    let (x, y) = (tx::build(&p(), &spec).unwrap(), tx::build(&p(), &spec).unwrap());
    assert_eq!(tx::txid(&x).unwrap(), tx::txid(&y).unwrap());
    assert_eq!(tx::sighash(&x, 0).unwrap(), tx::sighash(&y, 0).unwrap());
}

#[test]
fn payee_accepts_only_valid_states() {
    let p = p();
    let (a, s) = (Wallet::dev("agent"), Wallet::dev("solver"));
    let t = terms(&a, &s, [7; 32]);
    t.check(&p).unwrap();
    let mut payer = Payer { terms: t.clone(), paid: 0 };
    let mut payee = Payee::new(t.clone());
    let sk = a.channel_key(0);

    let (c1, s1) = payer.pay(&p, &sk, 10_000).unwrap();
    assert_eq!(payee.accept(&p, c1, &s1).unwrap(), 10_000);
    let (c2, s2) = payer.pay(&p, &sk, 20_000).unwrap();
    assert_eq!(payee.accept(&p, c2, &s2).unwrap(), 20_000);
    assert_eq!(payee.received(), 30_000);

    // Decreasing and repeated states.
    assert!(payee.accept(&p, c1, &s1).is_err(), "decreasing");
    assert!(payee.accept(&p, c2, &s2).is_err(), "repeated");
    // A signature for one amount presented for another.
    assert!(payee.accept(&p, 40_000, &s2).is_err(), "amount mismatch");
    // Above capacity: the payer cannot sign it, and the payee rejects it.
    let max = t.max_payable(&p).unwrap();
    assert!(t.sign_state(&p, &sk, max + 1).is_err());
    assert!(payee.accept(&p, max + 1, &s2).is_err(), "above capacity");
    // Signed by a key that is not the payer's.
    let rogue = Wallet::dev("rogue");
    let rogue_terms = Terms { payer: keys::pubkey(&rogue.channel_key(0)), ..t.clone() };
    let bad = rogue_terms.sign_state(&p, &rogue.channel_key(0), 40_000).unwrap();
    assert!(payee.accept(&p, 40_000, &bad).is_err(), "wrong signer");
    // Signed by the payer, but over another funding outpoint.
    let other = Terms { funding_txid: [8; 32], ..t.clone() };
    let bad = other.sign_state(&p, &sk, 40_000).unwrap();
    assert!(payee.accept(&p, 40_000, &bad).is_err(), "wrong outpoint");
    let bad = Terms { funding_index: 1, ..t.clone() }.sign_state(&p, &sk, 40_000).unwrap();
    assert!(payee.accept(&p, 40_000, &bad).is_err(), "wrong output index");
    assert_eq!(payee.received(), 30_000);

    // The maximum is payable, and the close builds with both signatures.
    let sig = t.sign_state(&p, &sk, max).unwrap();
    payee.accept(&p, max, &sig).unwrap();
    let close = payee.close(&p, &s.channel_key(0)).unwrap();
    assert_eq!((close.lock_time, close.expiry_height), (0, 0));
    assert_eq!(close.branch_id, params::BRANCH_NU7);
}

#[test]
fn terms_rules() {
    let p = p();
    let (a, s) = (Wallet::dev("agent"), Wallet::dev("solver"));
    let t = terms(&a, &s, [7; 32]);
    let short = Terms { lock_height: TARGET + MIN_LOCK - 1, ..t.clone() };
    assert!(short.check(&p).is_err(), "lock too short");
    let across = Terms { target_height: NU7_AT - 20, lock_height: NU7_AT + 5, ..t.clone() };
    assert!(across.check(&p).is_err(), "lock crosses NU7");
    let tiny = Terms { capacity: 10_000, ..t.clone() };
    assert!(tiny.check(&p).is_err(), "capacity below fee + change");
    let same = Terms { payee: t.payer, ..t.clone() };
    assert!(same.check(&p).is_err(), "one key on both sides");
    assert!(t.sign_refund(&p, &a.channel_key(0)).is_err(), "payer cannot sign as payee");
}

fn proposal(a: &Wallet, s: &Wallet) -> zchan::channel::Proposal {
    let coin = Coin {
        txid: [9; 32],
        index: 0,
        value: 1_0000_0000,
        spend: Spend::P2pkh(keys::pubkey(&a.coin_key(0))),
    };
    Payer::propose(
        &p(),
        &a.channel_key(0),
        keys::pubkey(&s.channel_key(0)),
        s.payout_address(0),
        a.payout_address(0),
        CAPACITY,
        TARGET,
        TARGET + 20,
        vec![(coin, a.coin_key(0))],
        a.coin_address(0),
    )
    .unwrap()
}

#[test]
fn funding_is_signed_only_after_a_valid_refund() {
    let p = p();
    let (a, s) = (Wallet::dev("agent"), Wallet::dev("solver"));

    // A refund signature from the wrong key is refused, and nothing is signed.
    let pr = proposal(&a, &s);
    let wrong = Terms { payee: pr.terms.payer, payer: pr.terms.payee, ..pr.terms.clone() };
    let bad = wrong.sign_refund(&p, &a.channel_key(0)).unwrap();
    assert!(Payer::fund(&p, pr, &a.channel_key(0), &bad).is_err());

    // A valid one yields the refund and a funding transaction whose txid the terms named.
    let pr = proposal(&a, &s);
    let terms = pr.terms.clone();
    let sig = terms.sign_refund(&p, &s.channel_key(0)).unwrap();
    let (payer, refund, funding) = Payer::fund(&p, pr, &a.channel_key(0), &sig).unwrap();
    assert_eq!(funding.txid, terms.funding_txid);
    assert_eq!(payer.terms, terms);
    assert_eq!((refund.lock_time, refund.expiry_height), (terms.lock_height, 0));
    assert_eq!(refund.branch_id, params::BRANCH_NU7);
}
