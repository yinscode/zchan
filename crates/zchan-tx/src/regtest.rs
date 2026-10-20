//! Regtest helpers shared by the gate tests and the demo: coins paid from the miner's
//! coinbase on `infra/zakura`.

use serde_json::json;
use zcash_protocol::local_consensus::LocalNetwork;
use zcash_transparent::address::TransparentAddress;

use crate::{
    keys,
    node::Node,
    params::{self, NU7_AT},
    tx::{self, Coin, Pay, Spec, Spend},
    txid_from_hex, txid_hex, Error, Result,
};

pub const COINBASE_MATURITY: u32 = 100;

pub struct Regtest {
    pub node: Node,
    pub p: LocalNetwork,
}

impl Regtest {
    /// Connects and checks the chain is the zchan regtest (NU7 at `NU7_AT`).
    pub async fn connect(url: &str) -> Result<Self> {
        let node = Node::new(url);
        let info = node.blockchain_info().await?;
        let nu7 = &info["upgrades"][format!("{:08x}", params::BRANCH_NU7)]["activationheight"];
        if nu7 != NU7_AT {
            return Err(Error::Rpc(format!(
                "not the zchan regtest (NU7 at {nu7}, want {NU7_AT}): infra/zakura/scripts/up.sh"
            )));
        }
        Ok(Self {
            node,
            p: params::regtest(Some(NU7_AT)),
        })
    }

    pub async fn tip(&self) -> Result<u32> {
        self.node.height().await
    }

    pub async fn mine(&self, n: u32) -> Result<()> {
        self.node.generate(n).await
    }

    /// Sends, mines one block, returns the height it was mined at.
    pub async fn send_confirm(&self, f: &tx::Final) -> Result<u32> {
        self.node.send(&f.raw).await?;
        self.mine(1).await?;
        self.node
            .mined_height(&txid_hex(&f.txid))
            .await?
            .ok_or(Error::Rpc("not mined".into()))
    }

    /// Output values of a mined or mempool transaction, in zatoshis.
    pub async fn outputs(&self, f: &tx::Final) -> Result<Vec<u64>> {
        let v = self.node.tx(&txid_hex(&f.txid)).await?;
        Ok(v["vout"]
            .as_array()
            .map(|a| a.iter().filter_map(|o| o["valueZat"].as_u64()).collect())
            .unwrap_or_default())
    }

    /// Pays the most recently matured coinbase to `outputs` (change to the miner), mines
    /// it, and returns the txid. Output `k` is coin `k` of that transaction.
    pub async fn pay(&self, outputs: &[(TransparentAddress, u64)]) -> Result<[u8; 32]> {
        let tip = self.tip().await?;
        if tip < COINBASE_MATURITY {
            self.node.mine_to(COINBASE_MATURITY).await?;
        }
        let tip = self.tip().await?;
        let h = tip + 1 - COINBASE_MATURITY;
        let b = self.node.call("getblock", json!([h.to_string(), 2])).await?;
        let cb = &b["tx"][0];
        let value = cb["vout"][0]["valueZat"]
            .as_u64()
            .ok_or(Error::Rpc("coinbase value".into()))?;
        let miner = keys::miner();
        let to_miner = keys::p2pkh(&keys::pubkey(&miner));
        let mut outs: Vec<Pay> = outputs.iter().map(|(a, v)| Pay::Transparent(*a, *v)).collect();
        outs.push(Pay::Transparent(to_miner, 1));
        let mut spec = Spec {
            target_height: tip + 1,
            expiry_height: tip + 20,
            lock_height: None,
            inputs: vec![Coin {
                txid: txid_from_hex(cb["txid"].as_str().unwrap_or_default())?,
                index: 0,
                value,
                spend: Spend::P2pkh(keys::pubkey(&miner)),
            }],
            outputs: outs,
            ironwood_anchor: None,
        };
        let fee = tx::fee(&self.p, &spec)?;
        let paid: u64 = outputs.iter().map(|(_, v)| v).sum();
        let last = spec.outputs.len() - 1;
        spec.outputs[last] = Pay::Transparent(
            to_miner,
            value
                .checked_sub(paid + fee)
                .ok_or(Error::Build("coinbase too small".into()))?,
        );
        let f = tx::extract(tx::sign(tx::build(&self.p, &spec)?, 0, &miner)?)?;
        self.send_confirm(&f).await?;
        Ok(f.txid)
    }

    /// A fresh P2PKH coin of `amount` for the holder of `sk`.
    pub async fn faucet(&self, sk: &secp256k1::SecretKey, amount: u64) -> Result<Coin> {
        let pk = keys::pubkey(sk);
        let txid = self.pay(&[(keys::p2pkh(&pk), amount)]).await?;
        Ok(Coin { txid, index: 0, value: amount, spend: Spend::P2pkh(pk) })
    }
}
