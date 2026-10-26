//! The agent. Every command takes `--data DIR --node URL --solver URL [--dev-seed LABEL]`.
//!
//! ```text
//! zchan-agent deposit                         print the address that funds channels
//!                                             (`--network testnet|regtest`: offline, no node)
//! zchan-agent send ADDRESS ZAT                pay from the deposit (split faucet funds)
//! zchan-agent deposit --fresh                 a transparent address no channel has used
//! zchan-agent shield                          move transparent coins to the shielded account
//! zchan-agent balance                         transparent and shielded balances
//! zchan-agent notes                           unspent shielded notes
//! zchan-agent shielded-address [--ua]         this party's Ironwood address (raw hex, or a
//!                                             unified address any shielded wallet can pay)
//! zchan-agent send-shielded ADDRESS_HEX ZAT   pay another party's Ironwood address
//! zchan-agent open --capacity ZAT --lock-blocks N [--shielded]
//! zchan-agent get URL [--mpp] [--method M --body B]
//!                                             pay a 402 (x402 v2, or MPP with --mpp) and fetch
//! zchan-agent pay-ref INVOICE_URL             pay a checkout invoice by reference
//! zchan-agent close [--channel ID]            cooperative close (both sides to Ironwood)
//! zchan-agent resume                          retry payments left pending
//! zchan-agent status
//! zchan-agent watch                           refund watcher: broadcasts refunds when due
//! zchan-agent serve --listen HOST:PORT        approval page for QR checkout + refund watcher
//! ```

use std::sync::Arc;

use zchan::{agent::{Agent, Protocol}, args::Args, ctx::Ctx, store, store::InvoiceRow, web};

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let (mut cfg, listen) = a.config("127.0.0.1:7300");
    if a.get("name").is_none() {
        cfg.name = "agent".into();
    }
    let solver = a.str("solver", "http://127.0.0.1:7100");
    if let (Some("deposit"), Some(net)) = (a.command.as_deref(), a.get("network")) {
        let w = zchan::ctx::wallet(&cfg).unwrap_or_else(|e| die(e));
        let net = match net {
            "testnet" => zcash_protocol::consensus::NetworkType::Test,
            "regtest" => zcash_protocol::consensus::NetworkType::Regtest,
            _ => die("--network testnet|regtest"),
        };
        println!("{}", zchan_tx::keys::encode(&w.coin_address(0), net));
        return;
    }
    let ctx = Ctx::new(cfg).await.unwrap_or_else(|e| die(e));
    let agent = Arc::new(Agent::new(ctx.clone(), &solver));
    let out = |v: serde_json::Value| println!("{}", serde_json::to_string_pretty(&v).unwrap());
    match a.command.as_deref() {
        Some("deposit") if a.flag("fresh") => println!("{}", ctx.fresh_deposit().unwrap_or_else(|e| die(e))),
        Some("deposit") => println!("{}", ctx.deposit_address()),
        Some("shield") => match ctx.shield().await.unwrap_or_else(|e| die(e)) {
            Some(txid) => println!("{txid}"),
            None => die("no confirmed transparent coins to shield"),
        },
        Some("shielded-address") if a.flag("ua") => println!("{}", zchan_tx::keys::unified_address(&ctx.wallet.shielded_address(0), ctx.net)),
        Some("shielded-address") => println!("{}", hex::encode(ctx.wallet.shielded_address(0).to_raw_address_bytes())),
        Some("send-shielded") => {
            let (to, amount) = match a.positional.as_slice() {
                [to, amount] => (to, amount.parse::<u64>().unwrap_or_else(|e| die(e))),
                _ => die("send-shielded IRONWOOD_ADDRESS_HEX ZAT"),
            };
            let to = zchan::payee::ironwood_address(to).unwrap_or_else(|| die("not a raw Ironwood address"));
            println!("{}", zchan::payer::send_shielded(&ctx, to, amount).await.unwrap_or_else(|e| die(e)));
        }
        Some("notes") => {
            let notes = ctx.shielded_notes().await.unwrap_or_else(|e| die(e));
            out(serde_json::json!(notes.iter().map(|n| serde_json::json!({
                "value": n.value(), "height": n.height, "txid": zchan_tx::txid_hex(&n.txid), "action": n.action,
            })).collect::<Vec<_>>()));
        }
        Some("balance") => {
            let t: u64 = ctx.coins().await.unwrap_or_else(|e| die(e)).iter().map(|(c, _)| c.value).sum();
            let s: u64 = ctx.shielded_notes().await.unwrap_or_else(|e| die(e)).iter().map(|n| n.value()).sum();
            out(serde_json::json!({"transparent": t, "shielded": s}));
        }
        Some("open") => {
            let (cap, lock) = (a.num("capacity", 1_000_000), a.num("lock-blocks", 2_000));
            let row = if a.flag("shielded") { agent.open_shielded(cap, lock).await } else { agent.open(cap, lock).await }
                .unwrap_or_else(|e| die(e));
            out(serde_json::to_value(&row).unwrap());
        }
        Some("get") => {
            let url = a.positional.first().unwrap_or_else(|| die("get URL"));
            let proto = if a.flag("mpp") { Protocol::Mpp } else { Protocol::X402 };
            let method = reqwest::Method::from_bytes(a.str("method", "GET").as_bytes()).unwrap_or_else(|e| die(e));
            let body = a.get("body").map(|b| b.as_bytes().to_vec());
            let f = agent.fetch(method, url, body, proto).await.unwrap_or_else(|e| die(e));
            out(serde_json::to_value(&f).unwrap());
        }
        Some("pay-ref") => {
            let r = a.positional.first().unwrap_or_else(|| die("pay-ref INVOICE_URL"));
            let row: InvoiceRow = web::get(&ctx.http, r).await.unwrap_or_else(|e| die(e));
            let ack = agent.pay_invoice(&row.invoice).await.unwrap_or_else(|e| die(e));
            out(serde_json::to_value(&ack).unwrap());
        }
        Some("send") => {
            let (to, amount) = match a.positional.as_slice() {
                [to, amount] => (to, amount.parse::<u64>().unwrap_or_else(|e| die(e))),
                _ => die("send ADDRESS ZAT"),
            };
            let (_, addr) = zchan_tx::keys::decode(to).unwrap_or_else(|| die("bad address"));
            println!("{}", zchan::payer::send(&ctx, addr, amount).await.unwrap_or_else(|e| die(e)));
        }
        Some("close") => {
            let id = match a.get("channel") {
                Some(id) => id.to_string(),
                None => agent.channel().unwrap_or_else(|e| die(e)).id,
            };
            println!("{}", agent.close(&id).await.unwrap_or_else(|e| die(e)));
        }
        Some("resume") => out(serde_json::to_value(agent.resume().await.unwrap_or_else(|e| die(e))).unwrap()),
        Some("status") => {
            let (chans, pays) = ctx
                .store
                .read(|c| Ok((store::channels(c, "payer")?, store::payments(c, None)?)))
                .unwrap_or_else(|e| die(e));
            out(serde_json::json!({"deposit": ctx.deposit_address(), "channels": chans, "payments": pays}));
        }
        Some("watch") => zchan::agent::run(agent).await,
        Some("serve") => {
            let base = a.str("url", &format!("http://{listen}"));
            eprintln!("zchan-agent approval page: {base}/t/{}/", agent.token);
            tokio::spawn(zchan::agent::run(agent.clone()));
            let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(e));
            axum::serve(l, zchan::agent::router(agent)).await.unwrap_or_else(|e| die(e));
        }
        _ => die("command: deposit | shield | balance | notes | shielded-address | send-shielded | send | open | close | get | pay-ref | resume | status | watch | serve"),
    }
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("zchan-agent: {e}");
    std::process::exit(1)
}
