//! A unidirectional (Spilman) payment channel on one transparent 2-of-2 P2SH output.
//!
//! The payer funds the output. Before the funding transaction is signed, the payee signs
//! a refund that returns the whole output to the payer from `lock_height`; the payer
//! verifies it, and only then signs the funding transaction (`Payer::fund`). The funding
//! txid is known before anyone signs it because v6 txids do not commit to signatures.
//!
//! Each payment is a new *state*: a transaction spending the funding output that pays the
//! payee a cumulative amount and returns the rest to the payer. The payer signs a state
//! and sends only the signature; the payee rebuilds the same transaction from the terms,
//! so it never inspects a transaction the payer built. The payee keeps the highest state
//! and broadcasts it, with its own signature, before `lock_height`.
//!
//! Refunds and states are transparent-only with a fixed shape (one input; one output for
//! the refund, two for a state), so their fee is fixed by the terms. Funding may come
//! from Ironwood notes (`Funding::Shielded`), and a cooperative close may pay both sides
//! to Ironwood (`check_coop_close` with a `ShieldedChange`).

use secp256k1::{ecdsa::Signature, Message, PublicKey, SecretKey};
use zcash_protocol::local_consensus::LocalNetwork;
use zcash_script::script::Evaluable;
use zcash_transparent::address::TransparentAddress;
use zchan_tx::{
    keys,
    node::Node,
    params, script,
    tx::{self, Coin, Final, IronwoodSpend, Pay, Spec, Spend},
    txid_hex,
};

use crate::{Error, Result};

/// The payer's Ironwood address for a shielded cooperative close, and the key that
/// decrypts what is paid to it.
pub struct ShieldedChange {
    pub address: orchard::Address,
    pub ivk: orchard::keys::PreparedIncomingViewingKey,
}

/// The payer's change in a state never goes below this: one ZIP 317 marginal fee, well
/// above any relay dust rule. It keeps every state the same two-output shape.
pub const MIN_CHANGE: u64 = 5_000;

/// The shortest refund lock a channel may have, in blocks after `target_height`.
pub const MIN_LOCK: u32 = 6;

/// What both parties agree before the payer signs the funding transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub payer: PublicKey,
    pub payee: PublicKey,
    pub capacity: u64,
    /// Funding output. Internal byte order.
    pub funding_txid: [u8; 32],
    pub funding_index: u32,
    /// The height every refund and state is built for: it fixes their branch ID.
    pub target_height: u32,
    /// The refund is valid in blocks after this height; the payee must close before it.
    pub lock_height: u32,
    /// Receives the refund and the payer's change in every state.
    pub payer_address: TransparentAddress,
    /// Receives the payee's cumulative amount.
    pub payee_address: TransparentAddress,
}

impl Terms {
    pub fn redeem(&self) -> Result<zcash_script::script::FromChain> {
        Ok(script::multisig_2of2(&self.payer, &self.payee)?)
    }

    /// `<funding txid>:<index>`, display-order hex.
    pub fn id(&self) -> String {
        format!("{}:{}", txid_hex(&self.funding_txid), self.funding_index)
    }

    pub fn funding_coin(&self) -> Result<Coin> {
        Ok(Coin {
            txid: self.funding_txid,
            index: self.funding_index,
            value: self.capacity,
            spend: Spend::P2sh(self.redeem()?),
        })
    }

    /// Rules both parties check before signing anything.
    pub fn check(&self, p: &LocalNetwork) -> Result<()> {
        if self.payer == self.payee {
            return Err(Error::Terms("payer and payee share a key".into()));
        }
        if self.lock_height < self.target_height + MIN_LOCK {
            return Err(Error::Terms(format!(
                "lock {} is under {MIN_LOCK} blocks after {}",
                self.lock_height, self.target_height
            )));
        }
        if params::spans_upgrade(p, self.target_height, self.lock_height) {
            return Err(Error::Terms(format!(
                "[{}, {}] crosses the network upgrade at {:?}",
                self.target_height,
                self.lock_height,
                params::next_activation(p, self.target_height)
            )));
        }
        if self.max_payable(p)? == 0 {
            return Err(Error::Terms(format!("capacity {} too small", self.capacity)));
        }
        Ok(())
    }

    /// The fee every state pays.
    pub fn state_fee(&self, p: &LocalNetwork) -> Result<u64> {
        let spec = Spec {
            target_height: self.target_height,
            expiry_height: 0,
            lock_height: None,
            inputs: vec![self.funding_coin()?],
            outputs: vec![
                Pay::Transparent(self.payee_address, 1),
                Pay::Transparent(self.payer_address, 1),
            ],
            ironwood_anchor: None,
            ironwood_spends: vec![],
        };
        Ok(tx::fee(p, &spec)?)
    }

    /// The most the payee can be paid: capacity less the state fee and `MIN_CHANGE`.
    pub fn max_payable(&self, p: &LocalNetwork) -> Result<u64> {
        Ok(self
            .capacity
            .saturating_sub(self.state_fee(p)? + MIN_CHANGE))
    }

    /// The refund: the whole output, less its fee, to the payer from `lock_height`.
    pub fn refund_spec(&self, p: &LocalNetwork) -> Result<Spec> {
        let mut spec = Spec {
            target_height: self.target_height,
            expiry_height: 0,
            lock_height: Some(self.lock_height),
            inputs: vec![self.funding_coin()?],
            outputs: vec![Pay::Transparent(self.payer_address, 1)],
            ironwood_anchor: None,
            ironwood_spends: vec![],
        };
        let fee = tx::fee(p, &spec)?;
        spec.outputs[0] = Pay::Transparent(self.payer_address, self.capacity - fee);
        Ok(spec)
    }

    /// State `cumulative`: that much to the payee, the rest less the fee to the payer.
    pub fn state_spec(&self, p: &LocalNetwork, cumulative: u64) -> Result<Spec> {
        let max = self.max_payable(p)?;
        if cumulative == 0 || cumulative > max {
            return Err(Error::State(format!("{cumulative} outside 1..={max}")));
        }
        let fee = self.state_fee(p)?;
        Ok(Spec {
            target_height: self.target_height,
            expiry_height: 0,
            lock_height: None,
            inputs: vec![self.funding_coin()?],
            outputs: vec![
                Pay::Transparent(self.payee_address, cumulative),
                Pay::Transparent(self.payer_address, self.capacity - fee - cumulative),
            ],
            ironwood_anchor: None,
            ironwood_spends: vec![],
        })
    }

    fn refund_sighash(&self, p: &LocalNetwork) -> Result<[u8; 32]> {
        Ok(tx::sighash(&tx::build(p, &self.refund_spec(p)?)?, 0)?)
    }

    fn state_sighash(&self, p: &LocalNetwork, cumulative: u64) -> Result<[u8; 32]> {
        Ok(tx::sighash(&tx::build(p, &self.state_spec(p, cumulative)?)?, 0)?)
    }

    /// The payee's signature on the refund, sent to the payer before funding.
    pub fn sign_refund(&self, p: &LocalNetwork, payee_sk: &SecretKey) -> Result<Signature> {
        self.check(p)?;
        own(payee_sk, &self.payee, "payee")?;
        Ok(sign(self.refund_sighash(p)?, payee_sk))
    }

    pub fn verify_refund(&self, p: &LocalNetwork, sig: &Signature) -> Result<()> {
        verify(self.refund_sighash(p)?, sig, &self.payee, "refund")
    }

    /// The payer's signature on state `cumulative`.
    pub fn sign_state(&self, p: &LocalNetwork, payer_sk: &SecretKey, cumulative: u64) -> Result<Signature> {
        own(payer_sk, &self.payer, "payer")?;
        Ok(sign(self.state_sighash(p, cumulative)?, payer_sk))
    }

    pub fn verify_state(&self, p: &LocalNetwork, cumulative: u64, sig: &Signature) -> Result<()> {
        verify(self.state_sighash(p, cumulative)?, sig, &self.payer, "state")
    }

    /// The payee's close: state `cumulative` with both signatures.
    pub fn close(&self, p: &LocalNetwork, payee_sk: &SecretKey, cumulative: u64, payer_sig: &Signature) -> Result<Final> {
        own(payee_sk, &self.payee, "payee")?;
        let pczt = tx::append(tx::build(p, &self.state_spec(p, cumulative)?)?, 0, *payer_sig)?;
        Ok(tx::extract(tx::sign(pczt, 0, payee_sk)?)?)
    }

    /// The payer's check of a cooperative close that the payee built: it spends only this
    /// channel's output, has no other pools, and pays the payer exactly its change in
    /// state `cumulative`, either transparently to `payer_address` or, with `shielded`,
    /// as an Ironwood note to the payer's address that the payer's key decrypts (and then
    /// nothing transparently). The rest, and any extra fee, is the payee's share.
    pub fn check_coop_close(&self, p: &LocalNetwork, pczt: &pczt::Pczt, cumulative: u64, shielded: Option<&ShieldedChange>) -> Result<()> {
        let bad = |m: &str| Error::State(format!("coop close: {m}"));
        let fx = pczt
            .clone()
            .into_effects()
            .map_err(|e| bad(&format!("effects: {e:?}")))?;
        if fx.sapling_bundle().is_some() || fx.orchard_bundle().is_some() {
            return Err(bad("unexpected Sapling or Orchard bundle"));
        }
        let t = fx.transparent_bundle().ok_or_else(|| bad("no transparent bundle"))?;
        let funding = zcash_transparent::bundle::OutPoint::new(self.funding_txid, self.funding_index);
        if t.vin.len() != 1 || *t.vin[0].prevout() != funding {
            return Err(bad("must spend exactly the channel output"));
        }
        let max = self.max_payable(p)?;
        if cumulative == 0 || cumulative > max {
            return Err(bad(&format!("{cumulative} outside 1..={max}")));
        }
        let change = self.capacity - self.state_fee(p)? - cumulative;
        let Some(sh) = shielded else {
            let want: zcash_transparent::address::Script = self.payer_address.script().into();
            if t.vout.len() != 1 || u64::from(t.vout[0].value()) != change || *t.vout[0].script_pubkey() != want {
                return Err(bad(&format!("must pay the payer exactly {change} and nothing else transparently")));
            }
            return Ok(());
        };
        if !t.vout.is_empty() {
            return Err(bad("a shielded close pays nothing transparently"));
        }
        let mine = sh.address.to_raw_address_bytes();
        let mut paid = 0u64;
        let mut problem: Option<String> = None;
        pczt::roles::verifier::Verifier::new(pczt.clone())
            .with_ironwood::<std::convert::Infallible, _>(|b| {
                let version = *b.bundle_version();
                for a in b.actions() {
                    let out = a.output();
                    if let (Some(r), Some(v)) = (out.recipient(), out.value()) {
                        if r.to_raw_address_bytes() != mine || v.inner() == 0 {
                            continue;
                        }
                        if out.verify_note_commitment(a.spend()).is_err() {
                            problem = Some("the payer's output does not match its commitment".into());
                            return Ok(());
                        }
                        match a.decrypt_output_with_ivk(&sh.ivk, version) {
                            Ok(Some((note, _, _))) if note.value().inner() == v.inner() => paid += v.inner(),
                            _ => {
                                problem = Some("the payer's output does not decrypt with its key".into());
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e| bad(&format!("ironwood: {e:?}")))?;
        if let Some(m) = problem {
            return Err(bad(&m));
        }
        if paid != change {
            return Err(bad(&format!("pays the payer {paid} shielded, not {change}")));
        }
        Ok(())
    }

    /// The payer's signature on a cooperative close that passes `check_coop_close`.
    pub fn sign_coop_close(
        &self,
        p: &LocalNetwork,
        payer_sk: &SecretKey,
        pczt: &pczt::Pczt,
        cumulative: u64,
        shielded: Option<&ShieldedChange>,
    ) -> Result<Signature> {
        own(payer_sk, &self.payer, "payer")?;
        self.check_coop_close(p, pczt, cumulative, shielded)?;
        Ok(sign(tx::sighash(pczt, 0)?, payer_sk))
    }

    /// Checks the funding output is on chain with `min_conf` confirmations, pays the
    /// channel's P2SH address, and holds `capacity`. Returns its mined height.
    pub async fn check_funding(&self, node: &Node, min_conf: u32) -> Result<u32> {
        let txid = txid_hex(&self.funding_txid);
        let v = node.tx(&txid).await?;
        let out = &v["vout"][self.funding_index as usize];
        let want = hex::encode(script::p2sh(&self.redeem()?).script().to_bytes());
        if out["scriptPubKey"]["hex"].as_str() != Some(want.as_str()) {
            return Err(Error::Funding(format!("{txid}:{} does not pay the channel", self.funding_index)));
        }
        if out["valueZat"].as_u64() != Some(self.capacity) {
            return Err(Error::Funding(format!("{txid}:{} holds {}, not {}", self.funding_index, out["valueZat"], self.capacity)));
        }
        let mined = v["height"]
            .as_u64()
            .ok_or_else(|| Error::Funding(format!("{txid} not mined")))? as u32;
        let tip = node.height().await?;
        if tip + 1 < mined + min_conf {
            return Err(Error::Funding(format!("{txid} has {} of {min_conf} confirmations", tip + 1 - mined)));
        }
        Ok(mined)
    }
}

fn sign(sighash: [u8; 32], sk: &SecretKey) -> Signature {
    secp256k1::ecdsa::sign(Message::from_digest(sighash), sk)
}

fn verify(sighash: [u8; 32], sig: &Signature, pk: &PublicKey, what: &str) -> Result<()> {
    sig.verify(Message::from_digest(sighash), pk)
        .map_err(|_| Error::Signature(format!("{what} signature invalid")))
}

fn own(sk: &SecretKey, pk: &PublicKey, role: &str) -> Result<()> {
    if keys::pubkey(sk) != *pk {
        return Err(Error::Terms(format!("key is not the {role}'s")));
    }
    Ok(())
}

/// The payer's side of a channel.
pub struct Payer {
    pub terms: Terms,
    /// Cumulative amount signed so far.
    pub paid: u64,
}

/// What funds a channel.
pub enum Funding {
    /// P2PKH coins with their keys; change to a transparent address.
    Transparent { coins: Vec<(Coin, SecretKey)>, change: TransparentAddress },
    /// Ironwood notes (spends against `anchor`) with the account's spending key; change
    /// to an Ironwood address. The funding transaction has no transparent inputs, so
    /// nothing on chain links the channel to where its money came from.
    Shielded {
        spends: Vec<IronwoodSpend>,
        anchor: orchard::Anchor,
        sk: orchard::keys::SpendingKey,
        change: orchard::Address,
    },
}

/// What the payer proposes: the terms, and the unsigned funding transaction they name.
pub struct Proposal {
    pub terms: Terms,
    funding: pczt::Pczt,
    inputs: Vec<SecretKey>,
    shielded: Option<(orchard::keys::SpendingKey, usize)>,
}

impl Payer {
    /// Builds the unsigned funding transaction from P2PKH coins (channel output + change)
    /// and the terms naming its output. Nothing is signed yet.
    #[allow(clippy::too_many_arguments)]
    pub fn propose(
        p: &LocalNetwork,
        payer_sk: &SecretKey,
        payee: PublicKey,
        payee_address: TransparentAddress,
        payer_address: TransparentAddress,
        capacity: u64,
        target_height: u32,
        lock_height: u32,
        coins: Vec<(Coin, SecretKey)>,
        change: TransparentAddress,
    ) -> Result<Proposal> {
        Self::propose_with(
            p,
            payer_sk,
            payee,
            payee_address,
            payer_address,
            capacity,
            target_height,
            lock_height,
            Funding::Transparent { coins, change },
        )
    }

    /// Builds the unsigned funding transaction from `funding` and the terms naming its
    /// output (always transparent output 0). Nothing is signed yet.
    #[allow(clippy::too_many_arguments)]
    pub fn propose_with(
        p: &LocalNetwork,
        payer_sk: &SecretKey,
        payee: PublicKey,
        payee_address: TransparentAddress,
        payer_address: TransparentAddress,
        capacity: u64,
        target_height: u32,
        lock_height: u32,
        funding: Funding,
    ) -> Result<Proposal> {
        let payer = keys::pubkey(payer_sk);
        let address = script::p2sh(&script::multisig_2of2(&payer, &payee)?);
        let mut spec = Spec {
            target_height,
            expiry_height: target_height + 20,
            lock_height: None,
            inputs: vec![],
            outputs: vec![Pay::Transparent(address, capacity)],
            ironwood_anchor: None,
            ironwood_spends: vec![],
        };
        let (input_keys, shielded, total, change) = match funding {
            Funding::Transparent { coins, change } => {
                let (inputs, keys): (Vec<Coin>, Vec<SecretKey>) = coins.into_iter().unzip();
                let total = inputs.iter().map(|c| c.value).sum::<u64>();
                spec.inputs = inputs;
                (keys, None, total, Pay::Transparent(change, 1))
            }
            Funding::Shielded { spends, anchor, sk, change } => {
                let total = spends.iter().map(|s| s.note.value().inner()).sum::<u64>();
                let n = spends.len();
                spec.ironwood_spends = spends;
                spec.ironwood_anchor = Some(anchor);
                (vec![], Some((sk, n)), total, Pay::Ironwood(change, 1))
            }
        };
        spec.outputs.push(change.clone());
        let fee = tx::fee(p, &spec)?;
        let left = total
            .checked_sub(capacity + fee)
            .filter(|l| *l >= MIN_CHANGE)
            .ok_or_else(|| Error::Funding(format!("funds {total} cannot cover {capacity} + fee {fee} + change")))?;
        spec.outputs[1] = match change {
            Pay::Transparent(a, _) => Pay::Transparent(a, left),
            Pay::Ironwood(a, _) => Pay::Ironwood(a, left),
            other => other,
        };
        let funding = tx::build(p, &spec)?;
        let terms = Terms {
            payer,
            payee,
            capacity,
            funding_txid: tx::txid(&funding)?,
            funding_index: 0,
            target_height,
            lock_height,
            payer_address,
            payee_address,
        };
        terms.check(p)?;
        Ok(Proposal { terms, funding, inputs: input_keys, shielded })
    }

    /// Verifies the payee's refund signature, completes the refund, and only then signs
    /// the funding transaction. Returns the payer, the fully signed refund (keep it) and
    /// the funding transaction (broadcast it).
    pub fn fund(
        p: &LocalNetwork,
        proposal: Proposal,
        payer_sk: &SecretKey,
        refund_sig: &Signature,
    ) -> Result<(Payer, Final, Final)> {
        let Proposal { terms, funding, inputs, shielded } = proposal;
        own(payer_sk, &terms.payer, "payer")?;
        terms.verify_refund(p, refund_sig)?;
        let refund = tx::append(tx::build(p, &terms.refund_spec(p)?)?, 0, *refund_sig)?;
        let refund = tx::extract(tx::sign(refund, 0, payer_sk)?)?;
        let mut funding = funding;
        for (i, sk) in inputs.iter().enumerate() {
            funding = tx::sign(funding, i, sk)?;
        }
        if let Some((sk, n)) = shielded {
            let (signed, got) = tx::sign_ironwood(funding, &orchard::keys::SpendAuthorizingKey::from(&sk))?;
            if got != n {
                return Err(Error::Funding(format!("signed {got} of {n} Ironwood spends")));
            }
            funding = signed;
        }
        let funding = tx::extract(funding)?;
        if funding.txid != terms.funding_txid {
            return Err(Error::Funding("signed funding txid differs from the terms".into()));
        }
        Ok((Payer { terms, paid: 0 }, refund, funding))
    }

    /// Pays `amount` more: returns the new cumulative amount and its signature.
    pub fn pay(&mut self, p: &LocalNetwork, payer_sk: &SecretKey, amount: u64) -> Result<(u64, Signature)> {
        let cumulative = self
            .paid
            .checked_add(amount)
            .filter(|_| amount > 0)
            .ok_or_else(|| Error::State(format!("bad amount {amount}")))?;
        let sig = self.terms.sign_state(p, payer_sk, cumulative)?;
        self.paid = cumulative;
        Ok((cumulative, sig))
    }
}

/// The payee's side of a channel: the highest state it holds.
pub struct Payee {
    pub terms: Terms,
    pub latest: Option<(u64, Signature)>,
}

impl Payee {
    pub fn new(terms: Terms) -> Self {
        Self { terms, latest: None }
    }

    pub fn received(&self) -> u64 {
        self.latest.map_or(0, |(c, _)| c)
    }

    /// Accepts a state above the one held, within capacity, signed by the payer over this
    /// channel's funding output. Returns the increase.
    pub fn accept(&mut self, p: &LocalNetwork, cumulative: u64, sig: &Signature) -> Result<u64> {
        let held = self.received();
        if cumulative <= held {
            return Err(Error::State(format!("{cumulative} is not above {held}")));
        }
        self.terms.verify_state(p, cumulative, sig)?;
        self.latest = Some((cumulative, *sig));
        Ok(cumulative - held)
    }

    /// The close transaction for the highest state held.
    pub fn close(&self, p: &LocalNetwork, payee_sk: &SecretKey) -> Result<Final> {
        let (c, sig) = self.latest.ok_or_else(|| Error::State("no state to close".into()))?;
        self.terms.close(p, payee_sk, c, &sig)
    }
}
