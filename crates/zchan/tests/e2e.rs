//! End-to-end on Zakura regtest: the solver and three merchants run as separate
//! processes (the real binaries), agents run in-process. Needs a fresh node:
//!
//! ```sh
//! infra/zakura/scripts/down.sh; infra/zakura/scripts/up.sh
//! cargo test -p zchan --release --test e2e -- --ignored --nocapture
//! ```
//!
//! Covers the gates of docs/plan.md §7, Oct 22–26: channels open, update, close and
//! refund; one agent channel pays two merchants; two agents share one merchant channel;
//! idempotent and concurrent retries; capacity exhaustion; merchant refusal and credit;
//! x402 (paid once; invalid, replayed, mismatched, expired stay unpaid); QR checkout
//! (altered, wrong-merchant, expired, reused invoices fail); a solver killed mid-payment;
//! a merchant down mid-payment; every process restarted; the payee's unilateral close at
//! its deadline; the payer's refund; a cooperative close to Ironwood; reconciliation.

use std::{
    future::IntoFuture,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use zchan::{
    agent::{Agent, Protocol},
    mpp,
    ctx::{Config, Ctx},
    payer, store,
    store::InvoiceRow,
    web,
    wire::{random_id, Invoice, PayReq, PayResp, Signed, State as WireState, INVOICE},
    x402::{self, PaymentPayload, PaymentRequired, ZchannelPayload},
    Error,
};
use zchan_tx::{keys, params::NU7_AT, regtest::Regtest, wallet::Wallet};

const SOLVER: &str = "http://127.0.0.1:17100";
const A: &str = "http://127.0.0.1:17201";
const B: &str = "http://127.0.0.1:17202";
const C: &str = "http://127.0.0.1:17203";
const D: &str = "http://127.0.0.1:17204";
const UI: &str = "http://127.0.0.1:17300";
const UPSTREAM: &str = "127.0.0.1:17400";
const ADMIN: &str = "t0ken";

fn node_url() -> String {
    std::env::var("ZCHAN_NODE_URL").unwrap_or("http://127.0.0.1:18272/".into())
}

struct Svc {
    bin: &'static str,
    args: Vec<String>,
    url: &'static str,
    log: PathBuf,
    child: Option<Child>,
}

impl Svc {
    fn new(bin: &'static str, url: &'static str, dir: &PathBuf, name: &str, extra: &[&str]) -> Self {
        let port = url.rsplit(':').next().unwrap();
        let mut args: Vec<String> = [
            "--data", dir.join(name).to_str().unwrap(), "--node", &node_url(), "--listen",
            &format!("127.0.0.1:{port}"), "--automine", "--poll-ms", "300", "--dev-seed", name, "--name", name,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        Self { bin, args, url, log: dir.join(format!("{name}.log")), child: None }
    }

    fn start(&mut self, env: &[(&str, &str)]) {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&self.log).unwrap();
        let mut cmd = Command::new(self.bin);
        cmd.args(&self.args).stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log));
        for (k, v) in env {
            cmd.env(k, v);
        }
        self.child = Some(cmd.spawn().unwrap());
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    async fn up(&self) {
        let http = reqwest::Client::new();
        let t = Instant::now();
        while http.get(format!("{}/info", self.url)).send().await.is_err() {
            assert!(t.elapsed() < Duration::from_secs(60), "{} did not start; see {}", self.url, self.log.display());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn exited(&mut self) -> Option<i32> {
        let t = Instant::now();
        loop {
            if let Some(s) = self.child.as_mut().unwrap().try_wait().unwrap() {
                self.child = None;
                return s.code();
            }
            assert!(t.elapsed() < Duration::from_secs(60), "{} did not exit", self.url);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Svc {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Log(Vec<Value>);

impl Log {
    fn note(&mut self, step: &str, v: Value) {
        println!("[{step}] {v}");
        self.0.push(json!({"step": step, "result": v}));
    }
}

async fn get(url: &str) -> Value {
    web::get::<Value>(&reqwest::Client::new(), url).await.unwrap_or_else(|e| panic!("GET {url}: {e}"))
}

async fn post(url: &str, body: Value) -> zchan::Result<Value> {
    web::post(&reqwest::Client::new(), url, &body).await
}

async fn agent(dir: &PathBuf, name: &str) -> Arc<Agent> {
    let cfg = Config {
        data_dir: dir.join(name),
        node_url: node_url(),
        dev_seed: Some(name.into()),
        automine: true,
        poll: Duration::from_millis(300),
        name: name.into(),
        ..Config::default()
    };
    Arc::new(Agent::new(Ctx::new(cfg).await.unwrap(), SOLVER))
}

async fn fund(rt: &Regtest, deposit: &str, amount: u64) {
    let (_, addr) = keys::decode(deposit).unwrap();
    rt.pay(&[(addr, amount)]).await.unwrap();
}

/// The solver's view of one channel.
async fn solver_channel(id: &str) -> Value {
    let s = get(&format!("{SOLVER}/status.json")).await;
    for k in ["agent_channels", "merchant_channels"] {
        if let Some(c) = s[k].as_array().unwrap().iter().find(|c| c["id"] == id) {
            return c.clone();
        }
    }
    panic!("solver has no channel {id}")
}

async fn cum(id: &str) -> u64 {
    solver_channel(id).await["cumulative"].as_u64().unwrap()
}

async fn reconciled(log: &mut Log, when: &str) {
    let s = get(&format!("{SOLVER}/status.json")).await;
    let r = s["reconcile"].clone();
    log.note(&format!("reconcile: {when}"), r.clone());
    for row in r.as_array().unwrap() {
        assert_eq!(row["ok"], true, "{when}: {row}");
    }
}

async fn wait_until<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let t = Instant::now();
    while !f().await {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// A 402 offer from `url`, decoded.
async fn offer(url: &str) -> PaymentRequired {
    let r = reqwest::get(url).await.unwrap();
    assert_eq!(r.status().as_u16(), 402);
    x402::decode(r.headers()[x402::PAYMENT_REQUIRED].to_str().unwrap()).unwrap()
}

async fn paid_payload(ag: &Agent, url: &str) -> PaymentPayload {
    let pr = offer(url).await;
    let accepted = pr.accepts[0].clone();
    let ack = ag.pay_invoice(&accepted.extra.invoice).await.unwrap();
    PaymentPayload { x402_version: 2, resource: Some(pr.resource), accepted, payload: ZchannelPayload { ack } }
}

async fn close_outputs(rt: &Regtest, txid: &str) -> Vec<u64> {
    let t = Instant::now();
    loop {
        if let Ok(v) = rt.node.tx(txid).await {
            if v["height"].as_u64().is_some() {
                return v["vout"].as_array().unwrap().iter().map(|o| o["valueZat"].as_u64().unwrap()).collect();
            }
        }
        assert!(t.elapsed() < Duration::from_secs(60), "{txid} not mined");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The existing API that merchant D fronts in proxy mode. It reports which headers it
/// received, so the test can check that payment headers are not forwarded.
async fn upstream() {
    use axum::{extract::Query, http::HeaderMap, routing::{get, post}, Router};
    let names = |h: &HeaderMap| h.keys().map(|k| k.as_str().to_string()).collect::<Vec<_>>();
    let app = Router::new()
        .route("/free", get(|| async { "free" }))
        .route(
            "/api/search",
            get(move |Query(q): Query<std::collections::HashMap<String, String>>, h: HeaderMap| async move {
                axum::Json(json!({"q": q.get("q"), "headers": names(&h)}))
            }),
        )
        .route("/api/echo", post(move |h: HeaderMap, body: String| async move { axum::Json(json!({"echo": body, "headers": names(&h)})) }))
        .route("/api/boom", get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom") }));
    let l = tokio::net::TcpListener::bind(UPSTREAM).await.unwrap();
    tokio::spawn(axum::serve(l, app).into_future());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs infra/zakura (fresh chain)"]
async fn e2e() {
    let rt = Regtest::connect(&node_url()).await.expect("node up: infra/zakura/scripts/up.sh");
    assert!(rt.tip().await.unwrap() < 100, "needs a fresh chain: down.sh && up.sh");
    rt.node.mine_to(NU7_AT + 10).await.unwrap();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = Log(vec![]);

    // ---- Services -------------------------------------------------------------------
    let mut solver = Svc::new(env!("CARGO_BIN_EXE_zchan-solver"), SOLVER, &dir, "solver", &["--fee", "100"]);
    let mut ma = Svc::new(env!("CARGO_BIN_EXE_zchan-merchant"), A, &dir, "merchant-a", &["--solver", SOLVER, "--price", "2000"]);
    let mut mb = Svc::new(env!("CARGO_BIN_EXE_zchan-merchant"), B, &dir, "merchant-b", &["--solver", SOLVER, "--price", "10000"]);
    let mut mc = Svc::new(
        env!("CARGO_BIN_EXE_zchan-merchant"),
        C,
        &dir,
        "merchant-c",
        &["--solver", SOLVER, "--price", "1000", "--fault", "refuse_update", "--invoice-ttl", "6", "--invoice-grace", "0"],
    );
    let routes = "/api/search=3000,/api/*=500";
    let mut md = Svc::new(
        env!("CARGO_BIN_EXE_zchan-merchant"),
        D,
        &dir,
        "merchant-d",
        &["--solver", SOLVER, "--upstream", &format!("http://{UPSTREAM}"), "--routes", routes, "--admin-token", ADMIN],
    );
    upstream().await;
    for s in [&mut solver, &mut ma, &mut mb, &mut mc, &mut md] {
        s.start(&[]);
    }
    for s in [&solver, &ma, &mb, &mc, &md] {
        s.up().await;
    }
    let sinfo = get(&format!("{SOLVER}/info")).await;
    fund(&rt, sinfo["deposit"].as_str().unwrap(), 1_0000_0000).await;

    // ---- Oct 22–23: channels -----------------------------------------------------------
    let mut mchan = std::collections::HashMap::new();
    for (url, cap) in [(A, 2_000_000u64), (B, 40_000), (C, 100_000)] {
        let m = post(&format!("{SOLVER}/admin/merchants"), json!({"url": url, "capacity": cap, "lock_blocks": 300})).await.unwrap();
        log.note("solver→merchant channel open", json!({"merchant": m["id"], "channel": m["channel_id"], "capacity": cap}));
        mchan.insert(url, m["channel_id"].as_str().unwrap().to_string());
    }
    // Privacy: the solver shields its remaining coins and funds D's channel from notes.
    let sh = post(&format!("{SOLVER}/admin/shield"), json!({})).await.unwrap();
    let m = post(&format!("{SOLVER}/admin/merchants"), json!({"url": D, "capacity": 200_000, "lock_blocks": 300, "shielded": true})).await.unwrap();
    mchan.insert(D, m["channel_id"].as_str().unwrap().to_string());
    let fund_d = rt.node.tx(m["channel_id"].as_str().unwrap().split(':').next().unwrap()).await.unwrap();
    assert_eq!(fund_d["vin"].as_array().map_or(0, |v| v.len()), 0, "shielded funding has no transparent inputs");
    assert_eq!(fund_d["vout"][0]["valueZat"], 200_000);
    log.note("solver→merchant-d channel, shielded funding", json!({"shield_txid": sh["txid"], "channel": m["channel_id"], "transparent_inputs": 0}));
    let (ag1, ag2, ag3) = (agent(&dir, "agent-1").await, agent(&dir, "agent-2").await, agent(&dir, "agent-3").await);
    fund(&rt, &ag1.ctx.deposit_address(), 5_000_000).await;
    fund(&rt, &ag2.ctx.deposit_address(), 1_000_000).await;
    fund(&rt, &ag3.ctx.deposit_address(), 500_000).await;
    let c1 = ag1.open(1_000_000, 300).await.unwrap();
    let c2 = ag2.open(200_000, 60).await.unwrap();
    let c3 = ag3.open(100_000, 30).await.unwrap();
    // Fresh transparent change: agent-1's change went to its channel's own coin address.
    let w1 = Wallet::dev("agent-1");
    let dep0 = rt.node.utxos(&ag1.ctx.deposit_address()).await.unwrap();
    let fresh = rt.node.utxos(&keys::encode(&w1.coin_address(c1.idx), ag1.ctx.net)).await.unwrap();
    assert!(dep0.is_empty() && fresh.len() == 1, "change at a fresh address: deposit {dep0:?}, fresh {fresh:?}");
    // Shielded funding: agent-4 shields its coins, then funds its channel from notes.
    let ag4 = agent(&dir, "agent-4").await;
    fund(&rt, &ag4.ctx.deposit_address(), 1_000_000).await;
    let shield4 = ag4.ctx.shield().await.unwrap().expect("coins to shield");
    let c4 = ag4.open_shielded(200_000, 300).await.unwrap();
    let f4 = rt.node.tx(c4.id.split(':').next().unwrap()).await.unwrap();
    assert_eq!(f4["vin"].as_array().map_or(0, |v| v.len()), 0, "no transparent inputs");
    assert_eq!(f4["vout"].as_array().unwrap().len(), 1, "only the channel output is transparent");
    log.note("privacy: fresh change, shielded funding", json!({
        "agent-1_change_address_index": c1.idx, "agent-4_shield_txid": shield4, "agent-4_channel": c4.id,
        "agent-4_funding_transparent_inputs": 0, "agent-4_funding_transparent_outputs": 1,
    }));
    for c in [&c1, &c2, &c3] {
        assert!(c.refund.is_some(), "refund stored before funding");
        log.note("agent→solver channel open", json!({"channel": c.id, "capacity": c.terms.capacity, "lock_height": c.terms.lock_height}));
    }

    // ---- Oct 23–24: one agent channel, two merchants; one merchant channel, two agents ---
    let w = ag1.get(&format!("{A}/paid/weather")).await.unwrap();
    assert_eq!(w.status, 200, "{w:?}");
    assert!(w.settlement.as_ref().unwrap().success);
    let n = ag1.get(&format!("{B}/paid/news")).await.unwrap();
    assert_eq!(n.status, 200);
    let w2 = ag2.get(&format!("{A}/paid/weather")).await.unwrap();
    assert_eq!(w2.status, 200);
    assert_eq!(cum(&mchan[A]).await, 4_000, "two agents advanced the same solver→A channel");
    assert_eq!(cum(&c1.id).await, 2_100 + 10_100, "one agent channel paid A and B");
    log.note("shared channel", json!({"solver→A": cum(&mchan[A]).await, "agent-1": cum(&c1.id).await, "agent-2": cum(&c2.id).await, "body": w.body}));
    let p4 = ag4.fetch(reqwest::Method::GET, &format!("{A}/paid/weather"), None, Protocol::Mpp).await.unwrap();
    assert_eq!(p4.status, 200, "the shielded-funded channel pays");
    reconciled(&mut log, "after first payments").await;

    // ---- Oct 24: x402 negatives -----------------------------------------------------------
    let pr = offer(&format!("{A}/paid/weather")).await;
    assert_eq!((pr.x402_version, pr.accepts[0].scheme.as_str(), pr.accepts[0].extra.payment_flow.as_str()), (2, "zchannel", "upfront"));
    let pp = paid_payload(&ag1, &format!("{A}/paid/weather")).await;
    let ok = ag1.get_with(&format!("{A}/paid/weather"), &pp).await.unwrap();
    assert_eq!(ok.status, 200);
    let replay = ag1.get_with(&format!("{A}/paid/weather"), &pp).await.unwrap();
    let reason = |f: &zchan::agent::Fetched| f.settlement.as_ref().and_then(|s| s.error_reason.clone());
    assert_eq!((replay.status, reason(&replay).as_deref()), (402, Some("invoice_already_used")));
    let pp2 = paid_payload(&ag1, &format!("{A}/paid/weather")).await;
    let mism = ag1.get_with(&format!("{A}/paid/news"), &pp2).await.unwrap();
    assert_eq!((mism.status, reason(&mism).as_deref()), (402, Some("resource_mismatch")));
    let other = ag1.get_with(&format!("{B}/paid/news"), &pp2).await.unwrap();
    assert_eq!((other.status, reason(&other).as_deref()), (402, Some("invalid_acknowledgment")));
    let mut forged = pp2.clone();
    forged.payload.ack.body.amount = 1;
    let bad = ag1.get_with(&format!("{A}/paid/weather"), &forged).await.unwrap();
    assert_eq!((bad.status, reason(&bad).as_deref()), (402, Some("invalid_acknowledgment")));
    let malformed = reqwest::Client::new().get(format!("{A}/paid/weather")).header(x402::PAYMENT_SIGNATURE, "!!").send().await.unwrap();
    assert_eq!(malformed.status().as_u16(), 400);
    let still = ag1.get_with(&format!("{A}/paid/weather"), &pp2).await.unwrap();
    assert_eq!(still.status, 200, "the mismatched attempts left the invoice unserved");
    log.note("x402", json!({"paid": 200, "replay": reason(&replay), "mismatch": reason(&mism), "other_merchant": reason(&other), "forged": reason(&bad), "malformed": 400}));

    // ---- Proxy mode (merchant D in front of an existing API) and MPP ---------------------
    let free = reqwest::get(format!("{D}/free")).await.unwrap();
    assert_eq!((free.status().as_u16(), free.text().await.unwrap()), (200, "free".to_string()), "unpriced paths pass through");
    let raw = reqwest::get(format!("{D}/api/search?q=x")).await.unwrap();
    assert_eq!(raw.status().as_u16(), 402);
    assert!(raw.headers().contains_key(x402::PAYMENT_REQUIRED), "x402 offer");
    let www = raw.headers()["www-authenticate"].to_str().unwrap().to_string();
    let ch = mpp::Challenge::parse(&www).unwrap();
    assert_eq!((ch.method.as_str(), ch.intent.as_str(), ch.charge().unwrap().amount.as_str()), ("zchannel", "charge", "3000"));
    let px = ag1.get(&format!("{D}/api/search?q=zec")).await.unwrap();
    assert_eq!((px.status, px.body["q"].as_str()), (200, Some("zec")), "{px:?}");
    assert_eq!(px.settlement.as_ref().unwrap().amount.as_deref(), Some("3000"));
    let fwd: Vec<String> = serde_json::from_value(px.body["headers"].clone()).unwrap();
    assert!(!fwd.iter().any(|h| h == "payment-signature"), "payment header not forwarded: {fwd:?}");
    let pm = ag2.fetch(reqwest::Method::GET, &format!("{D}/api/search?q=mpp"), None, Protocol::Mpp).await.unwrap();
    assert_eq!((pm.status, pm.body["q"].as_str()), (200, Some("mpp")), "{pm:?}");
    let receipt = pm.receipt.clone().expect("Payment-Receipt");
    assert_eq!((receipt.status.as_str(), receipt.method.as_str()), ("success", "zchannel"));
    let fwd: Vec<String> = serde_json::from_value(pm.body["headers"].clone()).unwrap();
    assert!(!fwd.iter().any(|h| h == "authorization"), "credential not forwarded: {fwd:?}");
    let echo = ag1.fetch(reqwest::Method::POST, &format!("{D}/api/echo"), Some(b"hello zchan".to_vec()), Protocol::Mpp).await.unwrap();
    assert_eq!((echo.status, echo.body["echo"].as_str()), (200, Some("hello zchan")));
    let demo = ag1.fetch(reqwest::Method::GET, &format!("{A}/paid/weather"), None, Protocol::Mpp).await.unwrap();
    assert_eq!(demo.status, 200);
    assert!(demo.receipt.is_some());
    // MPP replay, tampering and malformed credentials stay unpaid.
    let mpp_cred = |url: String, ag: Arc<Agent>| async move {
        let r = reqwest::get(&url).await.unwrap();
        let c = mpp::Challenge::parse(r.headers()["www-authenticate"].to_str().unwrap()).unwrap();
        let ack = ag.pay_invoice(&c.charge().unwrap().method_details.invoice).await.unwrap();
        mpp::Credential { challenge: c, source: None, payload: mpp::Payload { ack } }
    };
    let problem = |f: &zchan::agent::Fetched| f.body["type"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
    let cred = mpp_cred(format!("{D}/api/search?q=r"), ag1.clone()).await;
    let first = ag1.get_with_credential(&format!("{D}/api/search?q=r"), &cred).await.unwrap();
    assert_eq!(first.status, 200);
    let again = ag1.get_with_credential(&format!("{D}/api/search?q=r"), &cred).await.unwrap();
    assert_eq!((again.status, problem(&again).as_str()), (402, "invalid-challenge"));
    let cred2 = mpp_cred(format!("{D}/api/search?q=t"), ag1.clone()).await;
    let mut tampered = cred2.clone();
    tampered.challenge.request = mpp::b64(mpp::jcs(&json!({"amount": "1"})).as_bytes());
    let t = ag1.get_with_credential(&format!("{D}/api/search?q=t"), &tampered).await.unwrap();
    assert_eq!((t.status, problem(&t).as_str()), (402, "invalid-challenge"));
    let other_path = ag1.get_with_credential(&format!("{D}/api/echo"), &cred2).await.unwrap();
    assert_eq!((other_path.status, problem(&other_path).as_str()), (402, "verification-failed"));
    let ok2 = ag1.get_with_credential(&format!("{D}/api/search?q=t"), &cred2).await.unwrap();
    assert_eq!(ok2.status, 200, "the rejected attempts left the invoice unserved");
    let mal = reqwest::Client::new().get(format!("{D}/api/search")).header("authorization", "Payment !!").send().await.unwrap();
    let mal_status = mal.status().as_u16();
    let mal_body: Value = mal.json().await.unwrap();
    assert_eq!((mal_status, mal_body["type"].as_str().unwrap().ends_with("malformed-credential")), (402, true));
    // An upstream failure releases the invoice for another try.
    let boom = ag1.get(&format!("{D}/api/boom")).await.unwrap();
    assert_eq!(boom.status, 500);
    let boom_inv = boom.ack.as_ref().unwrap().body.invoice_id.clone();
    let st: InvoiceRow = web::get(&reqwest::Client::new(), &format!("{D}/invoices/{boom_inv}")).await.unwrap();
    assert_eq!(st.status, "paid", "released after the upstream 500");
    // Admin routes need the token on D.
    let no = reqwest::Client::new().post(format!("{D}/admin/channels/x/close")).send().await.unwrap();
    assert_eq!(no.status().as_u16(), 401);
    log.note("proxy + MPP", json!({
        "free": 200, "x402_proxied": px.body["q"], "mpp_proxied": pm.body["q"], "mpp_receipt": receipt,
        "mpp_post_echo": echo.body["echo"], "mpp_demo_route": demo.status, "replay": problem(&again),
        "tampered": problem(&t), "wrong_path": problem(&other_path), "malformed": "malformed-credential",
        "upstream_500": {"status": boom.status, "invoice_after": st.status}, "admin_without_token": 401,
    }));
    reconciled(&mut log, "after proxy and MPP").await;

    // ---- Oct 23: duplicate and concurrent retries pay once -------------------------------
    let before = cum(&mchan[A]).await;
    let inv: Signed<Invoice> = serde_json::from_value(post(&format!("{A}/invoices"), json!({"amount": 2000, "resource": "/dup"})).await.unwrap()).unwrap();
    let q = ag1.quote(&inv).await.unwrap();
    let row = ag1.channel().unwrap();
    let (c, sig) = payer::next_state(&ag1.ctx, &row, q.body.increment).unwrap();
    ag1.ctx
        .store
        .tx(|db| {
            store::channel_update(db, &row.id, |r| {
                r.cumulative = c;
                r.sig = Some(sig.clone());
            })?;
            store::state_log(db, &row.id, c, &sig, "dup")
        })
        .unwrap();
    let req = PayReq {
        payment_id: random_id(),
        quote_id: q.body.id.clone(),
        state: Some(WireState { channel_id: row.id.clone(), cumulative: c, sig }),
    };
    let http = reqwest::Client::new();
    let url = format!("{SOLVER}/pay");
    let (r1, r2) = tokio::join!(web::post::<_, PayResp>(&http, &url, &req), web::post::<_, PayResp>(&http, &url, &req));
    let r3: PayResp = web::post(&http, &url, &req).await.unwrap();
    assert_eq!(r3.status, "done");
    let done = [&r1, &r2].iter().filter(|r| matches!(r, Ok(p) if p.status == "done")).count();
    assert!(done >= 1, "{r1:?} {r2:?}");
    assert_eq!(cum(&mchan[A]).await, before + 2000, "paid exactly once");
    let s = get(&format!("{SOLVER}/status.json")).await;
    let n_dup = s["payments"].as_array().unwrap().iter().filter(|p| p["quote"]["body"]["invoice"]["body"]["id"] == inv.body.id.as_str()).count();
    assert_eq!(n_dup, 1);
    log.note("duplicate + concurrent pay", json!({"first": format!("{:?}", r1.map(|r| r.status)), "second": format!("{:?}", r2.map(|r| r.status)), "retry": r3.status, "payments_for_invoice": n_dup}));

    // ---- Oct 23: exhausted merchant capacity fails before fulfilment ---------------------
    let n2 = ag1.get(&format!("{B}/paid/news")).await.unwrap();
    assert_eq!(n2.status, 200);
    let n3 = ag1.get(&format!("{B}/paid/news")).await;
    assert!(matches!(&n3, Err(Error::Refused(409, m)) if m.contains("liquidity")), "{n3:?}");
    assert_eq!(cum(&mchan[B]).await, 20_000);
    log.note("capacity exhausted", json!({"solver→B": 20_000, "third_purchase": format!("{:?}", n3.err())}));

    // ---- Oct 23: merchant refusal → failed + credit, consumed by the next quote ----------
    let refused = ag1.get(&format!("{C}/paid/joke")).await;
    assert!(matches!(&refused, Err(Error::Refused(402, _))), "{refused:?}");
    let s = get(&format!("{SOLVER}/status.json")).await;
    assert_eq!(cum(&mchan[C]).await, 0, "refused level restored");
    let credit = ag1.quote(&serde_json::from_value(post(&format!("{A}/invoices"), json!({"amount": 2000, "resource": "/credit"})).await.unwrap()).unwrap()).await.unwrap();
    assert_eq!((credit.body.credit_applied, credit.body.increment), (1_100, 1_000));
    ag1.pay_quote(credit).await.unwrap();
    log.note("merchant refusal", json!({"error": format!("{:?}", refused.err()), "credit_applied_next": 1_100, "events": s["events"][0]}));
    reconciled(&mut log, "after refusal and credit").await;

    // ---- Oct 23–24: expiry; a refused /pay rolls the agent back -------------------------
    let level = ag1.channel().unwrap().cumulative;
    let late: Signed<Invoice> = serde_json::from_value(post(&format!("{C}/invoices"), json!({"amount": 1000, "resource": "/late"})).await.unwrap()).unwrap();
    let q = ag1.quote(&late).await.unwrap();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let r = ag1.pay_quote(q).await;
    assert!(matches!(&r, Err(Error::Refused(410, _))), "{r:?}");
    assert_eq!(ag1.channel().unwrap().cumulative, level, "agent rolled back the refused state");
    let pr = offer(&format!("{C}/paid/joke")).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let r2 = ag1.pay_invoice(&pr.accepts[0].extra.invoice).await;
    assert!(matches!(&r2, Err(Error::Refused(410, _))), "{r2:?}");
    log.note("expiry", json!({"pay_after_quote_expiry": format!("{:?}", r.err()), "x402_expired_invoice": format!("{:?}", r2.err())}));

    // ---- Oct 25: QR checkout ----------------------------------------------------------------
    let l = tokio::net::TcpListener::bind("127.0.0.1:17300").await.unwrap();
    tokio::spawn(axum::serve(l, zchan::agent::router(ag1.clone())).into_future());
    let ui = format!("{UI}/t/{}", ag1.token);
    let html = reqwest::get(format!("{A}/checkout?amount=3000&item=coffee")).await.unwrap().text().await.unwrap();
    assert!(html.contains("<svg"));
    let at = html.find(&format!("{A}/invoices/")).unwrap();
    let inv_url = html[at..at + A.len() + 10 + 32].to_string();
    let hand = reqwest::Client::new().get(&inv_url).header("accept", "text/html").send().await.unwrap().text().await.unwrap();
    assert!(hand.contains("Open in my agent"));
    let res = post(&format!("{ui}/api/resolve"), json!({"ref": inv_url})).await.unwrap();
    assert_eq!((res["merchant"].as_str(), res["total"].as_u64()), (Some("merchant-a"), Some(3_100)));
    let paid = post(&format!("{ui}/api/approve"), json!({"quote": res["quote"]})).await.unwrap();
    let st: InvoiceRow = web::get(&http, &inv_url).await.unwrap();
    assert_eq!(st.status, "paid");
    let reused = post(&format!("{ui}/api/resolve"), json!({"ref": inv_url})).await;
    assert!(matches!(&reused, Err(Error::Refused(409, _))), "{reused:?}");
    let wrong_token = post(&format!("{UI}/t/nope/api/resolve"), json!({"ref": inv_url})).await;
    assert!(matches!(&wrong_token, Err(Error::Refused(404, _))));
    let mut altered = st.invoice.clone();
    altered.body.amount = 1;
    altered.body.id = random_id();
    let alt = post(&format!("{SOLVER}/quote"), json!({"invoice": altered, "agent_channel": c1.id})).await;
    assert!(matches!(&alt, Err(Error::Refused(400, _))), "{alt:?}");
    assert!(ag1.quote(&altered).await.is_err());
    let forged = Signed::sign(
        INVOICE,
        Invoice { id: random_id(), merchant_id: "merchant-a".into(), amount: 10, resource: "checkout:x".into(), expires_at: zchan::wire::now() + 60 },
        &Wallet::dev("merchant-b").identity_key(),
    );
    let wm = post(&format!("{SOLVER}/quote"), json!({"invoice": forged, "agent_channel": c1.id})).await;
    assert!(matches!(&wm, Err(Error::Refused(400, m)) if m.contains("not signed by merchant")), "{wm:?}");
    let html = reqwest::get(format!("{C}/checkout?amount=1000&item=tea")).await.unwrap().text().await.unwrap();
    let at = html.find(&format!("{C}/invoices/")).unwrap();
    let c_url = html[at..at + C.len() + 10 + 32].to_string();
    tokio::time::sleep(Duration::from_secs(7)).await;
    let exp = post(&format!("{ui}/api/resolve"), json!({"ref": c_url})).await;
    assert!(matches!(&exp, Err(Error::Refused(410, _))), "{exp:?}");
    log.note("QR checkout", json!({
        "invoice_url": inv_url, "quote_total": res["total"], "ack": paid["ack"]["body"],
        "reused": format!("{:?}", reused.err()), "altered": format!("{:?}", alt.err()),
        "wrong_merchant": format!("{:?}", wm.err()), "expired": format!("{:?}", exp.err()),
    }));

    // ---- Oct 26: solver killed between agent_signed and the merchant's ack ---------------
    let before = cum(&mchan[A]).await;
    let inv: Signed<Invoice> = serde_json::from_value(post(&format!("{A}/invoices"), json!({"amount": 2000, "resource": "/crash"})).await.unwrap()).unwrap();
    solver.kill();
    solver.start(&[("ZCHAN_FAULT", "after_merchant_sign")]);
    solver.up().await;
    let ag = ag1.clone();
    let inv2 = inv.clone();
    let task = tokio::spawn(async move { ag.pay_invoice(&inv2).await });
    let code = solver.exited().await;
    assert_eq!(code, Some(86), "solver exited at the fault point");
    solver.start(&[]);
    solver.up().await;
    let ack = task.await.unwrap().expect("payment completes after the restart");
    assert_eq!(cum(&mchan[A]).await, before + 2000, "advanced exactly once");
    let ms = get(&format!("{A}/status.json")).await;
    let a_side = ms["channels"].as_array().unwrap().iter().find(|c| c["id"] == mchan[A].as_str()).unwrap()["cumulative"].as_u64().unwrap();
    assert_eq!(a_side, before + 2000, "merchant holds the same level");
    log.note("solver crash mid-payment", json!({"exit_code": code, "ack": ack.body, "merchant_level": a_side}));
    reconciled(&mut log, "after solver crash").await;

    // ---- Oct 26: merchant down mid-payment ------------------------------------------------
    let inv: Signed<Invoice> = serde_json::from_value(post(&format!("{A}/invoices"), json!({"amount": 2000, "resource": "/down"})).await.unwrap()).unwrap();
    ma.kill();
    let ag = ag2.clone();
    let task = tokio::spawn(async move { ag.pay_invoice(&inv).await });
    tokio::time::sleep(Duration::from_secs(3)).await;
    let pending = get(&format!("{SOLVER}/status.json")).await["payments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["status"] == "agent_signed")
        .count();
    ma.start(&[]);
    ma.up().await;
    let ack = task.await.unwrap().expect("payment completes once the merchant is back");
    log.note("merchant down mid-payment", json!({"pending_while_down": pending, "ack": ack.body}));
    assert_eq!(pending, 1);
    reconciled(&mut log, "after merchant outage").await;

    // ---- Oct 22: restart every process; state and refunds recovered ---------------------
    let snap = get(&format!("{SOLVER}/status.json")).await;
    for s in [&mut solver, &mut ma, &mut mb, &mut mc, &mut md] {
        s.kill();
    }
    for s in [&mut solver, &mut ma, &mut mb, &mut mc, &mut md] {
        s.start(&[]);
    }
    for s in [&solver, &ma, &mb, &mc, &md] {
        s.up().await;
    }
    let after = get(&format!("{SOLVER}/status.json")).await;
    for k in ["agent_channels", "merchant_channels"] {
        for (x, y) in snap[k].as_array().unwrap().iter().zip(after[k].as_array().unwrap()) {
            assert_eq!((&x["id"], &x["cumulative"], &x["sig"]), (&y["id"], &y["cumulative"], &y["sig"]));
        }
    }
    for c in after["merchant_channels"].as_array().unwrap() {
        assert!(c["refund"].is_string(), "solver kept every refund");
    }
    let again = Agent::new(
        Ctx::new(Config { data_dir: dir.join("agent-1"), node_url: node_url(), dev_seed: Some("agent-1".into()), ..Config::default() })
            .await
            .unwrap(),
        SOLVER,
    );
    let r = again.channel().unwrap();
    assert_eq!((r.cumulative, r.refund.is_some()), (ag1.channel().unwrap().cumulative, true));
    log.note("restart all", json!({"channels": after["agent_channels"].as_array().unwrap().len() + after["merchant_channels"].as_array().unwrap().len(), "agent_level": r.cumulative}));
    reconciled(&mut log, "after restart").await;

    // ---- Oct 22: payee closes at its deadline; payer refunds an unused channel -----------
    let lock2 = c2.terms.lock_height;
    let tip = rt.tip().await.unwrap();
    if tip + 10 < lock2 {
        rt.mine(lock2 - 10 - tip).await.unwrap();
    }
    wait_until("solver closes agent-2's channel", 60, || async { solver_channel(&c2.id).await["status"] == "closed" }).await;
    let ch2 = solver_channel(&c2.id).await;
    let outs = close_outputs(&rt, ch2["close_txid"].as_str().unwrap()).await;
    let level2 = ch2["cumulative"].as_u64().unwrap();
    assert_eq!(outs, vec![level2, 200_000 - 10_000 - level2]);
    log.note("payee unilateral close at deadline", json!({"channel": c2.id, "lock_height": lock2, "txid": ch2["close_txid"], "outputs_zat": outs}));
    let lock3 = c3.terms.lock_height;
    rt.node.mine_to(lock3.max(rt.tip().await.unwrap())).await.unwrap();
    payer::watch(&ag3.ctx).await.unwrap();
    rt.mine(1).await.unwrap();
    payer::watch(&ag3.ctx).await.unwrap();
    let r3 = ag3.ctx.store.read(|db| store::channel_get(db, &c3.id)).unwrap().unwrap();
    assert_eq!((r3.status.as_str(), r3.settled_by.as_deref()), ("closed", Some("refund")));
    let outs3 = close_outputs(&rt, r3.close_txid.as_ref().unwrap()).await;
    assert_eq!(outs3, vec![100_000 - 10_000]);
    wait_until("solver marks agent-3's channel expired", 30, || async { solver_channel(&c3.id).await["status"] == "expired" }).await;
    log.note("payer refund of an unused channel", json!({"channel": c3.id, "lock_height": lock3, "txid": r3.close_txid, "outputs_zat": outs3}));

    // ---- Oct 25 (P1): cooperative close to Ironwood; B and agent-1 close transparently ---
    let pool = rt.node.pool_zat("ironwood").await.unwrap();
    let level_a = cum(&mchan[A]).await;
    let coop = post(&format!("{A}/admin/channels/{}/coop-close", mchan[A]), json!({})).await.unwrap();
    let coop_txid = coop["txid"].as_str().unwrap().to_string();
    let outs_a = close_outputs(&rt, &coop_txid).await;
    let pool2 = rt.node.pool_zat("ironwood").await.unwrap();
    let shielded = (pool2 - pool) as u64;
    assert!(outs_a.is_empty(), "both sides paid to Ironwood: {outs_a:?}");
    let change_a = 2_000_000 - 10_000 - level_a;
    let tip = rt.tip().await.unwrap();
    let solver_notes = zchan_tx::notes::unspent(&rt.node, &Wallet::dev("solver").shielded_fvk(), tip - 3, tip).await.unwrap();
    assert!(solver_notes.iter().any(|n| n.value() == change_a), "solver received exactly its change, shielded");
    assert!(shielded > change_a && shielded < 2_000_000, "the whole channel, less the fee, went to Ironwood: {shielded}");
    wait_until("solver sees the coop close", 60, || async { solver_channel(&mchan[A]).await["status"] == "closed" }).await;
    log.note("cooperative close, both sides to Ironwood", json!({
        "txid": coop_txid, "level": level_a, "transparent_outputs": outs_a, "ironwood_pool_delta": shielded,
        "solver_change_shielded": change_a, "fee": 2_000_000 - shielded,
    }));
    let cb = post(&format!("{B}/admin/channels/{}/close", mchan[B]), json!({})).await.unwrap();
    let outs_b = close_outputs(&rt, cb["txid"].as_str().unwrap()).await;
    assert_eq!(outs_b, vec![20_000, 40_000 - 10_000 - 20_000]);
    let level1 = cum(&c1.id).await;
    let c1c = post(&format!("{SOLVER}/admin/channels/{}/close", c1.id), json!({})).await.unwrap();
    let outs_1 = close_outputs(&rt, c1c["txid"].as_str().unwrap()).await;
    assert_eq!(outs_1, vec![level1, 1_000_000 - 10_000 - level1]);
    let cd = reqwest::Client::new()
        .post(format!("{D}/admin/channels/{}/close", mchan[D]))
        .header("x-admin-token", ADMIN)
        .send()
        .await
        .unwrap();
    assert_eq!(cd.status().as_u16(), 200, "admin close with the token");
    let cd: Value = cd.json().await.unwrap();
    let outs_d = close_outputs(&rt, cd["txid"].as_str().unwrap()).await;
    let level_d = cum(&mchan[D]).await;
    assert_eq!(outs_d, vec![level_d, 200_000 - 10_000 - level_d]);
    log.note("transparent closes", json!({"merchant-b": {"txid": cb["txid"], "outputs_zat": outs_b}, "agent-1": {"txid": c1c["txid"], "outputs_zat": outs_1}}));
    reconciled(&mut log, "final").await;

    if let Ok(path) = std::env::var("ZCHAN_RESULTS") {
        std::fs::write(path, serde_json::to_string_pretty(&log.0).unwrap()).unwrap();
    }
}
