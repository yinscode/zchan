//! The judge playground. `zchan-playground --data DIR --node URL --solver URL
//! --merchants "merchant-a=URL,merchant-b=URL,merchant-d=URL" [--listen HOST:PORT]
//! [--grant ZAT] [--capacity ZAT] [--lock-blocks N] [--max-sessions N] [--explorer URL]
//! [--automine] [--poll-ms MS] [--house-dev-seed LABEL]`.
//!
//! The house wallet lives in `DIR/house` (fund it with `zchan-agent shield` or
//! `send-shielded`); each browser session's agent lives in `DIR/sessions/<id>`.

use zchan::{
    args::Args,
    ctx::{Config, Ctx},
    playground::{self, Playground, Settings},
};

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let (base, listen) = a.config_open("127.0.0.1:8080");
    let merchants: Vec<(String, String)> = a
        .str("merchants", "")
        .split(',')
        .filter_map(|m| m.split_once('=').map(|(n, u)| (n.trim().to_string(), u.trim().trim_end_matches('/').to_string())))
        .collect();
    if merchants.is_empty() || a.get("solver").is_none() {
        die("--solver URL and --merchants name=url,... are required");
    }
    let house = Ctx::new(Config {
        data_dir: base.data_dir.join("house"),
        dev_seed: a.get("house-dev-seed").map(str::to_owned),
        name: "house".into(),
        ..base.clone()
    })
    .await
    .unwrap_or_else(|e| die(e));
    let settings = Settings {
        solver: a.str("solver", "").trim_end_matches('/').to_string(),
        merchants,
        grant: a.num("grant", 160_000),
        capacity: a.num("capacity", 100_000),
        lock_blocks: a.num("lock-blocks", 2_000),
        max_sessions: a.num("max-sessions", 100),
        explorer: a.get("explorer").map(str::to_owned),
    };
    eprintln!(
        "zchan-playground on http://{listen} · {} · house shielded address {}",
        house.network,
        hex::encode(house.wallet.shielded_address(0).to_raw_address_bytes())
    );
    let pg = Playground::new(house, base, settings);
    let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(e));
    axum::serve(l, playground::router(pg)).await.unwrap_or_else(|e| die(e));
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("zchan-playground: {e}");
    std::process::exit(1)
}
