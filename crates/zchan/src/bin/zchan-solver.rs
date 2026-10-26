//! The solver service. `zchan-solver --data DIR --node URL --listen HOST:PORT [--url URL]
//! [--fee ZAT] [--max-payment ZAT] [--automine] [--poll-ms MS] [--dev-seed LABEL]`.

use zchan::{args::Args, ctx::Ctx, solver};

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let (mut cfg, listen) = a.config("127.0.0.1:7100");
    if a.get("name").is_none() {
        cfg.name = "solver".into();
    }
    let ctx = Ctx::new(cfg).await.unwrap_or_else(|e| die(e));
    // Build the Ironwood proving key now, so the first shielded send or close does not wait for it.
    std::thread::spawn(|| {
        zchan_tx::tx::proving_key();
    });
    eprintln!("zchan-solver {} on {} · {} · deposit {}", ctx.cfg.name, listen, ctx.network, ctx.deposit_address());
    tokio::spawn(solver::run(ctx.clone()));
    let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(e));
    axum::serve(l, solver::router(ctx)).await.unwrap_or_else(|e| die(e));
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("zchan-solver: {e}");
    std::process::exit(1)
}
