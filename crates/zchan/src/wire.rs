//! Messages between agent, solver and merchant, and their encodings.
//!
//! Every signed message is `Signed<T>`: the body, the signer's public key, and a
//! secp256k1 ECDSA signature over `SHA-256(domain ‖ JSON(body))`. Bodies are structs, so
//! their JSON field order is fixed and the digest is reproducible. Amounts are integer
//! zatoshis; times are Unix seconds.

use secp256k1::{ecdsa::Signature, Message, PublicKey, SecretKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zcash_transparent::address::TransparentAddress;
use zchan_tx::{keys, txid_from_hex, txid_hex};

use crate::{channel::Terms, Error, Result};

pub const INVOICE: &str = "zchan/invoice/v1";
pub const QUOTE: &str = "zchan/quote/v1";
pub const ACK: &str = "zchan/ack/v1";

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn random_id() -> String {
    hex::encode(rand10::random::<[u8; 16]>())
}

pub fn pk_hex(pk: &PublicKey) -> String {
    hex::encode(pk.serialize())
}

pub fn pk_from_hex(s: &str) -> Result<PublicKey> {
    let b = hex::decode(s).map_err(|e| Error::Wire(format!("pubkey hex: {e}")))?;
    PublicKey::from_slice(&b).map_err(|e| Error::Wire(format!("pubkey: {e}")))
}

pub fn sig_hex(sig: &Signature) -> String {
    hex::encode(sig.serialize_compact())
}

pub fn sig_from_hex(s: &str) -> Result<Signature> {
    let b = hex::decode(s).map_err(|e| Error::Wire(format!("signature hex: {e}")))?;
    Signature::from_compact(&b).map_err(|e| Error::Wire(format!("signature: {e}")))
}

/// `pkh:<hash160>` or `sh:<hash160>`: a transparent address without a network prefix.
pub fn addr_str(a: &TransparentAddress) -> String {
    match a {
        TransparentAddress::PublicKeyHash(h) => format!("pkh:{}", hex::encode(h)),
        TransparentAddress::ScriptHash(h) => format!("sh:{}", hex::encode(h)),
    }
}

pub fn addr_from_str(s: &str) -> Result<TransparentAddress> {
    let bad = || Error::Wire(format!("address {s}"));
    let (kind, h) = s.split_once(':').ok_or_else(bad)?;
    let h: [u8; 20] = hex::decode(h).map_err(|_| bad())?.try_into().map_err(|_| bad())?;
    match kind {
        "pkh" => Ok(TransparentAddress::PublicKeyHash(h)),
        "sh" => Ok(TransparentAddress::ScriptHash(h)),
        _ => Err(bad()),
    }
}

fn digest<T: Serialize>(domain: &str, body: &T) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    h.update(serde_json::to_vec(body).expect("serializable"));
    h.finalize().into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Signed<T> {
    pub body: T,
    /// Compressed public key, hex.
    pub signer: String,
    /// Compact signature, hex.
    pub sig: String,
}

impl<T: Serialize + DeserializeOwned + Clone> Signed<T> {
    pub fn sign(domain: &str, body: T, sk: &SecretKey) -> Self {
        let sig = secp256k1::ecdsa::sign(Message::from_digest(digest(domain, &body)), sk);
        Self { body, signer: pk_hex(&keys::pubkey(sk)), sig: sig_hex(&sig) }
    }

    /// Checks the signature against the embedded signer; returns that signer.
    pub fn verify(&self, domain: &str) -> Result<PublicKey> {
        let pk = pk_from_hex(&self.signer)?;
        sig_from_hex(&self.sig)?
            .verify(Message::from_digest(digest(domain, &self.body)), &pk)
            .map_err(|_| Error::Signature(format!("{domain} signature invalid")))?;
        Ok(pk)
    }

    /// Checks the signature and that `expected` made it.
    pub fn verify_by(&self, domain: &str, expected: &PublicKey) -> Result<()> {
        if self.verify(domain)? != *expected {
            return Err(Error::Signature(format!("{domain} signed by another key")));
        }
        Ok(())
    }
}

/// A merchant's request for payment.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Invoice {
    /// 16 random bytes, hex: also the invoice's single-use nonce.
    pub id: String,
    pub merchant_id: String,
    pub amount: u64,
    /// What is paid for: an x402 resource path or a checkout order.
    pub resource: String,
    pub expires_at: u64,
}

/// A solver's offer to pay an invoice for an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Quote {
    pub id: String,
    pub invoice: Signed<Invoice>,
    /// The agent's channel to the solver that this quote is for.
    pub agent_channel: String,
    pub merchant_channel: String,
    pub amount: u64,
    pub fee: u64,
    /// Credit the agent holds from a failed payment, set against this one.
    pub credit_applied: u64,
    /// How much the agent's cumulative state rises: `amount + fee − credit_applied`.
    pub increment: u64,
    pub expires_at: u64,
}

/// A merchant's signed receipt that a channel state paid an invoice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ack {
    pub invoice_id: String,
    pub merchant_id: String,
    pub channel_id: String,
    pub cumulative: u64,
    pub amount: u64,
}

/// `Terms` as JSON.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TermsWire {
    pub payer: String,
    pub payee: String,
    pub capacity: u64,
    /// Display-order hex.
    pub funding_txid: String,
    pub funding_index: u32,
    pub target_height: u32,
    pub lock_height: u32,
    pub payer_address: String,
    pub payee_address: String,
}

impl From<&Terms> for TermsWire {
    fn from(t: &Terms) -> Self {
        Self {
            payer: pk_hex(&t.payer),
            payee: pk_hex(&t.payee),
            capacity: t.capacity,
            funding_txid: txid_hex(&t.funding_txid),
            funding_index: t.funding_index,
            target_height: t.target_height,
            lock_height: t.lock_height,
            payer_address: addr_str(&t.payer_address),
            payee_address: addr_str(&t.payee_address),
        }
    }
}

impl TryFrom<&TermsWire> for Terms {
    type Error = Error;

    fn try_from(w: &TermsWire) -> Result<Self> {
        Ok(Terms {
            payer: pk_from_hex(&w.payer)?,
            payee: pk_from_hex(&w.payee)?,
            capacity: w.capacity,
            funding_txid: txid_from_hex(&w.funding_txid)?,
            funding_index: w.funding_index,
            target_height: w.target_height,
            lock_height: w.lock_height,
            payer_address: addr_from_str(&w.payer_address)?,
            payee_address: addr_from_str(&w.payee_address)?,
        })
    }
}

/// A channel state on the wire: the cumulative amount and the payer's signature.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct State {
    pub channel_id: String,
    pub cumulative: u64,
    pub sig: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_roundtrip_and_tamper() {
        let sk = keys::dev_key("m");
        let inv = Invoice { id: random_id(), merchant_id: "m".into(), amount: 5, resource: "/x".into(), expires_at: 9 };
        let s = Signed::sign(INVOICE, inv, &sk);
        s.verify_by(INVOICE, &keys::pubkey(&sk)).unwrap();
        assert!(s.verify(QUOTE).is_err(), "domain separation");
        let mut t = s.clone();
        t.body.amount = 6;
        assert!(t.verify(INVOICE).is_err(), "altered amount");
        let json = serde_json::to_string(&s).unwrap();
        let back: Signed<Invoice> = serde_json::from_str(&json).unwrap();
        back.verify(INVOICE).unwrap();
    }
}

/// What a service says about itself (`GET /info`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Info {
    /// `solver` or `merchant`.
    pub role: String,
    /// The party's id: the merchant id, or the solver's name.
    pub id: String,
    /// Identity key (signs invoices, quotes, acknowledgments), hex.
    pub pubkey: String,
    pub network: String,
    /// Funds this party's channels.
    pub deposit: String,
    #[serde(default)]
    pub fee: u64,
    #[serde(default)]
    pub max_payment: u64,
}

/// Agent → solver: quote this invoice against my channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuoteReq {
    pub invoice: Signed<Invoice>,
    pub agent_channel: String,
}

/// Agent → solver: pay a quote. `payment_id` makes retries idempotent; `state` is the
/// agent's next state (absent when credit covers the whole quote).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayReq {
    pub payment_id: String,
    pub quote_id: String,
    pub state: Option<State>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayResp {
    pub payment_id: String,
    /// `done`, `pending` (retry with the same payment id) or `failed` (credited).
    pub status: String,
    pub ack: Option<Signed<Ack>>,
    pub error: Option<String>,
    /// The agent's credit balance after this payment.
    pub credit: u64,
}

/// Solver → merchant: this state pays this invoice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateReq {
    pub invoice_id: String,
    pub state: State,
}

/// Merchant → solver: co-sign this cooperative close (a PCZT paying the merchant's share
/// to Ironwood and the payer's change as in the latest state).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoopCloseReq {
    pub pczt: String,
    pub cumulative: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoopCloseResp {
    pub sig: String,
}
