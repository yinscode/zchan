//! A running party: its seed wallet, store, node, network, and per-channel locks.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use secp256k1::SecretKey;
use zcash_protocol::{consensus::NetworkType, local_consensus::LocalNetwork};
use zchan_tx::{
    keys,
    node::Node,
    params,
    tx::{Coin, Final, Spend},
    txid_hex,
    wallet::Wallet,
};

use crate::{store::Store, Error, Result};

/// Zcash testnet's genesis block hash.
const TESTNET_GENESIS: &str = "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38";

/// Options for every role; each binary reads the ones it uses.
#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub node_url: String,
    /// This party's own base URL, as peers reach it.
    pub base_url: String,
    /// A label to derive the seed from instead of `<data_dir>/seed` (regtest and tests).
    pub dev_seed: Option<String>,
    /// Regtest: mine a block after each broadcast.
    pub automine: bool,
    pub poll: Duration,
    /// Payee: broadcast the best state this many blocks before the refund height.
    pub close_margin: u32,
    /// Payee: the shortest refund lock it accepts, in blocks from the funding target.
    pub min_lock: u32,
    /// Solver: flat fee per payment, zatoshis.
    pub fee: u64,
    /// Solver: largest single payment, zatoshis.
    pub max_payment: u64,
    /// Merchant: its id, its solver, and the price of each x402 resource.
    pub name: String,
    pub solver_url: String,
    pub price: u64,
    pub invoice_ttl: u64,
    /// Merchant: how long after expiry it still accepts a state for an invoice.
    pub invoice_grace: u64,
    /// Merchant proxy mode: the service to forward to, and the priced paths.
    pub upstream: Option<String>,
    pub routes: Vec<(String, u64)>,
    /// Required in `X-Admin-Token` on `/admin/*` when set.
    pub admin_token: Option<String>,
    /// Fault injection for tests: the process exits at this named point.
    pub fault: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("."),
            node_url: "http://127.0.0.1:18272/".into(),
            base_url: String::new(),
            dev_seed: None,
            automine: false,
            poll: Duration::from_secs(5),
            close_margin: 10,
            min_lock: 20,
            fee: 100,
            max_payment: 1_000_000,
            name: "merchant".into(),
            solver_url: String::new(),
            price: 2_000,
            invoice_ttl: 300,
            invoice_grace: 120,
            upstream: None,
            routes: vec![],
            admin_token: None,
            fault: None,
        }
    }
}

pub struct Ctx {
    pub cfg: Config,
    pub store: Store,
    pub wallet: Wallet,
    pub node: Node,
    pub p: LocalNetwork,
    pub net: NetworkType,
    /// CAIP-2-style label used in x402: `zcash:regtest`, `zcash:testnet`, `zcash:mainnet`.
    pub network: String,
    pub http: reqwest::Client,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Ctx {
    pub async fn new(cfg: Config) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&cfg.data_dir).map_err(|e| Error::Other(format!("data dir: {e}")))?;
        let wallet = wallet(&cfg)?;
        let store = Store::open(cfg.data_dir.join("zchan.db").to_str().unwrap())?;
        let node = Node::new(cfg.node_url.clone());
        let info = node.blockchain_info().await?;
        let p = params::from_node(&info["upgrades"])
            .ok_or_else(|| Error::Other("node reports no NU6.3 activation".into()))?;
        // Zakura reports `chain: "test"` on regtest too; the genesis block tells them apart.
        let genesis = node.call("getblockhash", serde_json::json!([0])).await?;
        let (net, network) = match info["chain"].as_str() {
            Some("main") => (NetworkType::Main, "zcash:mainnet"),
            Some("test") if genesis == TESTNET_GENESIS => (NetworkType::Test, "zcash:testnet"),
            _ => (NetworkType::Regtest, "zcash:regtest"),
        };
        if net == NetworkType::Main {
            return Err(Error::Other("mainnet is not enabled (docs/plan.md §9)".into()));
        }
        Ok(Arc::new(Self {
            cfg,
            store,
            wallet,
            node,
            p,
            net,
            network: network.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .expect("http client"),
            locks: Mutex::new(HashMap::new()),
        }))
    }

    /// The async lock serializing work on `key` (a channel id, or `coins`).
    pub fn lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks.lock().unwrap().entry(key.into()).or_default().clone()
    }

    /// Signs invoices, quotes and acknowledgments.
    pub fn identity(&self) -> SecretKey {
        self.wallet.identity_key()
    }

    pub fn identity_hex(&self) -> String {
        crate::wire::pk_hex(&keys::pubkey(&self.identity()))
    }

    /// The address that funds this party's channels (wallet index 0).
    pub fn deposit_address(&self) -> String {
        keys::encode(&self.wallet.coin_address(0), self.net)
    }

    /// Confirmed coins at the deposit address, with their key.
    pub async fn coins(&self) -> Result<Vec<(Coin, SecretKey)>> {
        let sk = self.wallet.coin_key(0);
        let pk = keys::pubkey(&sk);
        Ok(self
            .node
            .utxos(&self.deposit_address())
            .await?
            .into_iter()
            .filter(|(_, _, _, h)| *h > 0)
            .map(|(txid, index, value, _)| (Coin { txid, index, value, spend: Spend::P2pkh(pk) }, sk))
            .collect())
    }

    pub async fn balance(&self) -> Result<u64> {
        Ok(self.coins().await?.iter().map(|(c, _)| c.value).sum())
    }

    pub async fn tip(&self) -> Result<u32> {
        Ok(self.node.height().await?)
    }

    /// Broadcasts; on regtest with `automine`, mines a block. Returns the display txid.
    pub async fn broadcast(&self, f: &Final) -> Result<String> {
        let txid = self.node.send(&f.raw).await?;
        if self.cfg.automine {
            self.node.generate(1).await?;
        }
        Ok(txid)
    }

    pub async fn wait_mined(&self, f: &Final) -> Result<u32> {
        let txid = txid_hex(&f.txid);
        loop {
            if let Ok(Some(h)) = self.node.mined_height(&txid).await {
                return Ok(h);
            }
            tokio::time::sleep(self.cfg.poll).await;
        }
    }

    /// Fault injection: exits the process at `point` when configured to.
    pub fn fault(&self, point: &str) {
        if self.cfg.fault.as_deref() == Some(point) {
            eprintln!("fault injected at {point}: exiting");
            std::process::exit(86);
        }
    }
}

/// The party's wallet: from `dev_seed`, or from `<data_dir>/seed` (created if absent).
/// Needs no node, so an account can be made offline.
pub fn wallet(cfg: &Config) -> Result<Wallet> {
    std::fs::create_dir_all(&cfg.data_dir).map_err(|e| Error::Other(format!("data dir: {e}")))?;
    Ok(match &cfg.dev_seed {
        Some(label) => Wallet::dev(label),
        None => Wallet::from_seed(load_or_create_seed(&cfg.data_dir)?),
    })
}

fn load_or_create_seed(dir: &std::path::Path) -> Result<[u8; 32]> {
    let path = dir.join("seed");
    if let Ok(s) = std::fs::read_to_string(&path) {
        return hex::decode(s.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| Error::Other(format!("bad seed in {}", path.display())));
    }
    let seed: [u8; 32] = rand10::random();
    std::fs::write(&path, hex::encode(seed)).map_err(|e| Error::Other(format!("write seed: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(seed)
}
