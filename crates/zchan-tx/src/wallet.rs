//! A seed wallet: every key a party uses derives from one 32-byte seed, so a party
//! restored from its seed alone re-derives the keys of every channel `i` it holds.

use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use secp256k1::SecretKey;
use sha2::{Digest, Sha256};
use zcash_transparent::address::TransparentAddress;

use crate::keys;

#[derive(Clone)]
pub struct Wallet {
    seed: [u8; 32],
}

impl Wallet {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    /// A wallet whose seed derives from a label. For regtest and tests only.
    pub fn dev(label: &str) -> Self {
        Self::from_seed(Sha256::digest(format!("zchan-dev/wallet/{label}").as_bytes()).into())
    }

    fn derive(&self, what: &str, i: u32, attempt: u8) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"zchan/wallet/v1");
        h.update(self.seed);
        h.update(what.as_bytes());
        h.update(i.to_be_bytes());
        h.update([attempt]);
        h.finalize().into()
    }

    fn secp_key(&self, what: &str, i: u32) -> SecretKey {
        (0u8..=255)
            .find_map(|n| SecretKey::from_secret_bytes(self.derive(what, i, n)).ok())
            .expect("a valid key")
    }

    /// The party's identity: it signs invoices, quotes and acknowledgments.
    pub fn identity_key(&self) -> SecretKey {
        self.secp_key("identity", 0)
    }

    /// The party's key in channel `i`'s 2-of-2.
    pub fn channel_key(&self, i: u32) -> SecretKey {
        self.secp_key("channel", i)
    }

    /// The key holding the coins that fund channel `i`.
    pub fn coin_key(&self, i: u32) -> SecretKey {
        self.secp_key("coin", i)
    }

    pub fn coin_address(&self, i: u32) -> TransparentAddress {
        keys::p2pkh(&keys::pubkey(&self.coin_key(i)))
    }

    /// The key whose P2PKH receives what channel `i` pays this party: the payee's side of
    /// a close, or the payer's change and refund.
    pub fn payout_key(&self, i: u32) -> SecretKey {
        self.secp_key("payout", i)
    }

    pub fn payout_address(&self, i: u32) -> TransparentAddress {
        keys::p2pkh(&keys::pubkey(&self.payout_key(i)))
    }

    /// Spends what a cooperative close paid this party's Ironwood address `i`.
    pub fn ironwood_sk(&self, i: u32) -> SpendingKey {
        (0u8..=255)
            .find_map(|n| Option::<SpendingKey>::from(SpendingKey::from_bytes(self.derive("ironwood", i, n))))
            .expect("a valid spending key")
    }

    pub fn ironwood_fvk(&self, i: u32) -> FullViewingKey {
        FullViewingKey::from(&self.ironwood_sk(i))
    }

    pub fn ironwood_address(&self, i: u32) -> orchard::Address {
        self.ironwood_fvk(i).address_at(0u32, Scope::External)
    }
}
