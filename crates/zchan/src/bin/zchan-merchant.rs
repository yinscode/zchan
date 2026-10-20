//! A merchant service. `zchan-merchant --name ID --solver URL --data DIR --node URL
//! --listen HOST:PORT [--url URL] [--price ZAT] [--automine] [--poll-ms MS] [--dev-seed LABEL]`.

use zchan::{args::Args, ctx::Ctx, merchant};

#[tokio::main]
async fn main() {
    let a = Args::parse();
    let (cfg, listen) = a.config("127.0.0.1:7200");
    if cfg.solver_url.is_empty() {
        die("--solver URL is required");
    }
    let ctx = Ctx::new(cfg).await.unwrap_or_else(|e| die(e));
    eprintln!("zchan-merchant {} on {} · {} · price {} zat", ctx.cfg.name, listen, ctx.network, ctx.cfg.price);
    tokio::spawn(merchant::run(ctx.clone()));
    let l = tokio::net::TcpListener::bind(&listen).await.unwrap_or_else(|e| die(e));
    axum::serve(l, merchant::router(ctx)).await.unwrap_or_else(|e| die(e));
}

fn die(e: impl std::fmt::Display) -> ! {
    eprintln!("zchan-merchant: {e}");
    std::process::exit(1)
}
