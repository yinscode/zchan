//! The judge playground: a web page where anyone can try zchan end to end on a live
//! network, with an agent of their own held by this server.
//!
//! Each browser session gets a fresh agent (its own seed and data directory). The page
//! walks it through the private path: shield in (the house wallet sends it Ironwood
//! funds), open a channel funded from those notes, pay merchants over x402 or MPP (each
//! payment shown step by step), and shield out (a cooperative close paying both sides to
//! Ironwood). Every transaction is shown with its transparent inputs and outputs.
//!
//! Limits: one grant per session, `max_sessions` sessions, small capacities. The house
//! wallet's sends are serialized and each waits for a block.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    agent::{Agent, Protocol},
    ctx::{Config, Ctx},
    payer,
    web::{err, ApiResult},
    wire::{now, random_id},
    Error, Result,
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub solver: String,
    /// (name, base URL) of each merchant the page offers.
    pub merchants: Vec<(String, String)>,
    /// What the house sends each session, and the channel capacity it opens.
    pub grant: u64,
    pub capacity: u64,
    pub lock_blocks: u32,
    pub max_sessions: usize,
    /// A block explorer's transaction URL prefix, if any (txid appended).
    pub explorer: Option<String>,
}

struct Session {
    agent: Arc<Agent>,
    granted: bool,
    /// Shielded balance, refreshed after each job (scanning on every poll would be slow).
    shielded: u64,
    busy: Option<String>,
    log: Vec<Value>,
}

pub struct Playground {
    pub house: Arc<Ctx>,
    pub settings: Settings,
    base: Config,
    sessions: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Session>>>>,
}

impl Playground {
    pub fn new(house: Arc<Ctx>, base: Config, settings: Settings) -> Arc<Self> {
        Arc::new(Self { house, settings, base, sessions: Mutex::new(HashMap::new()) })
    }
}

pub fn router(pg: Arc<Playground>) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/api/info", get(info))
        .route("/api/session", post(new_session))
        .route("/api/session/{id}", get(session))
        .route("/api/session/{id}/shield-in", post(shield_in))
        .route("/api/session/{id}/open", post(open))
        .route("/api/session/{id}/pay", post(pay))
        .route("/api/session/{id}/close", post(close))
        .with_state(pg)
}

async fn page() -> Html<&'static str> {
    Html(include_str!("playground.html"))
}

async fn info(State(pg): State<Arc<Playground>>) -> ApiResult<Json<Value>> {
    let shielded: u64 = pg.house.shielded_notes().await?.iter().map(|n| n.value()).sum();
    Ok(Json(json!({
        "network": pg.house.network, "solver": pg.settings.solver,
        "merchants": pg.settings.merchants.iter().map(|(n, u)| json!({"name": n, "url": u})).collect::<Vec<_>>(),
        "grant": pg.settings.grant, "capacity": pg.settings.capacity, "house_shielded": shielded,
        "explorer": pg.settings.explorer, "sessions": pg.sessions.lock().unwrap().len(), "max_sessions": pg.settings.max_sessions,
    })))
}

/// The session's state; after a restart, a session is rebuilt from its saved agent
/// (its wallet and channel survive; its on-page log starts again).
async fn get_session(pg: &Playground, id: &str) -> ApiResult<Arc<tokio::sync::Mutex<Session>>> {
    if let Some(s) = pg.sessions.lock().unwrap().get(id).cloned() {
        return Ok(s);
    }
    let dir = pg.base.data_dir.join("sessions").join(id);
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) || !dir.join("seed").exists() {
        return Err(err(StatusCode::NOT_FOUND, "no such session; start a new one"));
    }
    let ctx = Ctx::new(Config { data_dir: dir, dev_seed: None, name: format!("judge-{}", &id[..6]), ..pg.base.clone() }).await?;
    let shielded = ctx.shielded_notes().await.map(|n| n.iter().map(|n| n.value()).sum()).unwrap_or(0);
    let agent = Arc::new(Agent::new(ctx, &pg.settings.solver));
    // A step cut off by the restart: settle a funding that was waiting for its block.
    let resume = agent.clone();
    tokio::spawn(async move {
        if let Err(e) = payer::resume_funding(&resume.ctx).await {
            eprintln!("resume funding: {e}");
        }
    });
    let log: Vec<Value> = agent
        .ctx
        .store
        .read(|c| crate::store::meta_get(c, "playground_log"))?
        .and_then(|l| serde_json::from_str(&l).ok())
        .unwrap_or_default();
    let s = Arc::new(tokio::sync::Mutex::new(Session { agent, granted: true, shielded, busy: None, log }));
    pg.sessions.lock().unwrap().entry(id.to_string()).or_insert(s.clone());
    Ok(s)
}

async fn new_session(State(pg): State<Arc<Playground>>) -> ApiResult<Json<Value>> {
    if pg.sessions.lock().unwrap().len() >= pg.settings.max_sessions {
        return Err(err(StatusCode::SERVICE_UNAVAILABLE, "the playground is full; try again later"));
    }
    let id = random_id();
    let cfg = Config {
        data_dir: pg.base.data_dir.join("sessions").join(&id),
        dev_seed: None,
        name: format!("judge-{}", &id[..6]),
        ..pg.base.clone()
    };
    let ctx = Ctx::new(cfg).await?;
    let agent = Arc::new(Agent::new(ctx, &pg.settings.solver));
    let s = Session { agent, granted: false, shielded: 0, busy: None, log: vec![] };
    pg.sessions.lock().unwrap().insert(id.clone(), Arc::new(tokio::sync::Mutex::new(s)));
    Ok(Json(json!({"id": id})))
}

async fn session(State(pg): State<Arc<Playground>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = get_session(&pg, &id).await?;
    let s = s.lock().await;
    let a = &s.agent;
    let shielded = s.shielded;
    let ch = a.ctx.store.read(|c| crate::store::channels(c, "payer"))?.into_iter().last();
    let channel = ch.as_ref().map(|r| {
        json!({
            "id": r.id, "capacity": r.terms.capacity, "level": r.cumulative, "status": r.status,
            "lock_height": r.terms.lock_height, "settled_by": r.settled_by,
            "remaining": payer::remaining(&a.ctx, r).unwrap_or(0),
        })
    });
    Ok(Json(json!({
        "id": id, "name": a.ctx.cfg.name, "shielded": shielded, "granted": s.granted,
        "channel": channel, "busy": s.busy, "log": s.log,
    })))
}

/// What the chain shows of a transaction, once mined.
async fn tx_facts(ctx: &Ctx, txid: &str) -> Value {
    for _ in 0..240 {
        if let Ok(v) = ctx.node.tx(txid).await {
            if let Some(h) = v["height"].as_u64() {
                return json!({
                    "txid": txid, "height": h,
                    "transparent_inputs": v["vin"].as_array().map_or(0, |a| a.len()),
                    "transparent_outputs": v["vout"].as_array().map(|a| a.iter().map(|o| o["valueZat"].clone()).collect::<Vec<_>>()).unwrap_or_default(),
                    "into_ironwood": v["ironwood"]["valueBalanceZat"].as_i64().map(|b| -b),
                });
            }
        }
        tokio::time::sleep(ctx.cfg.poll).await;
    }
    json!({"txid": txid, "height": null})
}

/// Saves a session's on-page log with its agent, so a restart keeps it.
fn save_log(agent: &Agent, log: &[Value]) {
    let _ = agent.ctx.store.tx(|c| crate::store::meta_set(c, "playground_log", &serde_json::to_string(log).unwrap()));
}

/// Runs `job` in the background with the session marked busy, logging its outcome.
fn spawn_job<F, Fut>(s: Arc<tokio::sync::Mutex<Session>>, label: &'static str, job: F)
where
    F: FnOnce(Arc<Agent>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<Value>> + Send + 'static,
{
    tokio::spawn(async move {
        let agent = s.lock().await.agent.clone();
        let out = job(agent.clone()).await;
        let shielded: u64 = agent.ctx.shielded_notes().await.map(|n| n.iter().map(|n| n.value()).sum()).unwrap_or(0);
        let mut g = s.lock().await;
        g.busy = None;
        g.shielded = shielded;
        g.log.push(match out {
            Ok(v) => json!({"kind": label, "at": now(), "ok": true, "result": v}),
            Err(e) => json!({"kind": label, "at": now(), "ok": false, "error": e.to_string()}),
        });
        save_log(&agent, &g.log);
    });
}

async fn start(s: &Arc<tokio::sync::Mutex<Session>>, label: &str) -> ApiResult<()> {
    let mut g = s.lock().await;
    if let Some(b) = &g.busy {
        return Err(err(StatusCode::CONFLICT, format!("still {b}")));
    }
    g.busy = Some(label.into());
    Ok(())
}

/// Shield in: the house wallet sends this session's agent a grant, shielded.
async fn shield_in(State(pg): State<Arc<Playground>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = get_session(&pg, &id).await?;
    if s.lock().await.granted {
        return Err(err(StatusCode::CONFLICT, "this session already received its grant"));
    }
    start(&s, "shielding funds in").await?;
    s.lock().await.granted = true;
    let (house, grant) = (pg.house.clone(), pg.settings.grant);
    spawn_job(s, "shield-in", move |agent| async move {
        let to = agent.ctx.wallet.shielded_address(0);
        let txid = payer::send_shielded(&house, to, grant).await?;
        Ok(json!({"amount": grant, "tx": tx_facts(&house, &txid).await}))
    });
    Ok(Json(json!({"started": "shield-in"})))
}

async fn open(State(pg): State<Arc<Playground>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = get_session(&pg, &id).await?;
    start(&s, "opening a channel").await?;
    let (max_cap, lock) = (pg.settings.capacity, pg.settings.lock_blocks);
    spawn_job(s, "open", move |agent| async move {
        // The default capacity, or as much as the balance covers after the fee and change.
        let have: u64 = agent.ctx.shielded_notes().await?.iter().map(|n| n.value()).sum();
        let cap = max_cap.min(have.saturating_sub(zchan_tx::notes::zip317_fee(0, 1, 2, 1) + crate::channel::MIN_CHANGE));
        if cap < 20_000 {
            return Err(Error::Funding(format!("shielded balance {have} is too small to open a channel")));
        }
        let row = agent.open_shielded(cap, lock).await?;
        let txid = row.id.split(':').next().unwrap_or_default().to_string();
        Ok(json!({"channel": row.id, "capacity": cap, "lock_height": row.terms.lock_height, "tx": tx_facts(&agent.ctx, &txid).await}))
    });
    Ok(Json(json!({"started": "open"})))
}

#[derive(Deserialize)]
struct PayReq {
    merchant: String,
    path: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    body: Option<String>,
    /// `x402` or `mpp`.
    protocol: String,
}

/// One paid request, done now; the answer carries its step-by-step trace.
async fn pay(State(pg): State<Arc<Playground>>, Path(id): Path<String>, Json(req): Json<PayReq>) -> ApiResult<Json<Value>> {
    let s = get_session(&pg, &id).await?;
    let base = pg
        .settings
        .merchants
        .iter()
        .find(|(n, _)| *n == req.merchant)
        .map(|(_, u)| u.clone())
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "unknown merchant"))?;
    if !req.path.starts_with('/') || req.path.len() > 200 {
        return Err(err(StatusCode::BAD_REQUEST, "bad path"));
    }
    let agent = {
        let g = s.lock().await;
        if let Some(b) = &g.busy {
            return Err(err(StatusCode::CONFLICT, format!("still {b}")));
        }
        g.agent.clone()
    };
    let method = reqwest::Method::from_bytes(req.method.as_deref().unwrap_or("GET").as_bytes())
        .map_err(|_| err(StatusCode::BAD_REQUEST, "bad method"))?;
    let proto = if req.protocol == "mpp" { Protocol::Mpp } else { Protocol::X402 };
    let body = req.body.map(|b| b.chars().take(500).collect::<String>().into_bytes());
    let entry = match agent.fetch(method, &format!("{base}{}", req.path), body, proto).await {
        Ok(f) => json!({
            "kind": "pay", "at": now(), "ok": f.status == 200, "merchant": req.merchant, "path": req.path,
            "protocol": req.protocol, "status": f.status, "elapsed_ms": f.elapsed_ms, "trace": f.trace, "body": f.body,
        }),
        Err(e) => json!({"kind": "pay", "at": now(), "ok": false, "merchant": req.merchant, "path": req.path, "protocol": req.protocol, "error": e.to_string()}),
    };
    let mut g = s.lock().await;
    g.log.push(entry.clone());
    save_log(&agent, &g.log);
    Ok(Json(entry))
}

/// Shield out: a cooperative close the agent starts, both sides paid to Ironwood.
async fn close(State(pg): State<Arc<Playground>>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let s = get_session(&pg, &id).await?;
    start(&s, "closing the channel").await?;
    spawn_job(s, "close", move |agent| async move {
        let ch = agent.channel()?;
        let before: u64 = agent.ctx.shielded_notes().await?.iter().map(|n| n.value()).sum();
        let txid = agent.close(&ch.id).await?;
        let tx = tx_facts(&agent.ctx, &txid).await;
        // Record the settlement (session agents run no background watcher).
        payer::watch(&agent.ctx).await?;
        let notes = agent.ctx.shielded_notes().await?;
        let got: u64 = notes.iter().filter(|n| zchan_tx::txid_hex(&n.txid) == txid).map(|n| n.value()).sum();
        let after: u64 = notes.iter().map(|n| n.value()).sum();
        if got == 0 {
            return Err(Error::Other(format!("close {txid} mined but no note found for this agent")));
        }
        Ok(json!({"channel": ch.id, "level": ch.cumulative, "tx": tx, "change_note": got, "shielded_before": before, "shielded_after": after}))
    });
    Ok(Json(json!({"started": "close"})))
}
