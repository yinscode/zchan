//! Regtest helpers. `zchan-dev [--node URL] fund ADDRESS ZAT | mine N | tip`.

use zchan::args::Args;
use zchan_tx::{keys, regtest::Regtest, txid_hex};

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let url = a
        .get("node")
        .map(str::to_owned)
        .or_else(|| std::env::var("ZCHAN_NODE_URL").ok())
        .unwrap_or("http://127.0.0.1:18272/".into());
    let rt = Regtest::connect(&url).await.unwrap_or_else(|e| die(e));
    match a.command.as_deref() {
        Some("fund") => {
            let (addr, amount) = match a.positional.as_slice() {
                [addr, amount] => (addr, amount.parse::<u64>().unwrap_or_else(|e| die(e))),
                _ => die("fund ADDRESS ZAT"),
            };
            let (_, to) = keys::decode(addr).unwrap_or_else(|| die("bad address"));
            let txid = rt.pay(&[(to, amount)]).await.unwrap_or_else(|e| die(e));
            println!("{}", txid_hex(&txid));
        }
        Some("mine") => {
            rt.mine(a.positional.first().and_then(|n| n.parse().ok()).unwrap_or(1)).await.unwrap_or_else(|e| die(e));
            println!("{}", rt.tip().await.unwrap_or_else(|e| die(e)));
        }
        Some("tip") => println!("{}", rt.tip().await.unwrap_or_else(|e| die(e))),
        _ => die("command: fund ADDRESS ZAT | mine N | tip"),
    }
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("zchan-dev: {e}");
    std::process::exit(1)
}
