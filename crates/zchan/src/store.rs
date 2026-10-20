//! SQLite, one file per process (WAL). Every change a party acknowledges to a peer is
//! committed here first. Rows that carry structure are JSON columns; `states` and
//! `events` are append-only.

use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use zchan_tx::watch::Kit;

use crate::{
    wire::{now, Ack, Invoice, Quote, Signed, TermsWire},
    Error, Result,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS channels (id TEXT PRIMARY KEY, role TEXT NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS states (seq INTEGER PRIMARY KEY AUTOINCREMENT, channel_id TEXT NOT NULL,
  cumulative INTEGER NOT NULL, sig TEXT NOT NULL, ref TEXT, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS slots (idx INTEGER PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS invoices (id TEXT PRIMARY KEY, status TEXT NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS merchants (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS quotes (id TEXT PRIMARY KEY, used_by TEXT, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS payments (id TEXT PRIMARY KEY, quote_id TEXT NOT NULL UNIQUE,
  agent_channel TEXT NOT NULL, status TEXT NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS credits (agent_channel TEXT PRIMARY KEY, amount INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL,
  kind TEXT NOT NULL, data TEXT NOT NULL);
";

/// A channel as one party holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelRow {
    pub id: String,
    /// `payer` or `payee`.
    pub role: String,
    /// The wallet index of this party's channel key.
    pub idx: u32,
    pub peer_url: String,
    pub peer_id: String,
    pub terms: TermsWire,
    /// `funding` → `open` → `closing` → `closed`.
    pub status: String,
    /// Highest state: signed (payer) or accepted (payee).
    pub cumulative: u64,
    pub sig: Option<String>,
    /// Payer: the fully signed refund, raw hex.
    pub refund: Option<String>,
    /// Payer: the refund watcher's state.
    pub kit: Option<Kit>,
    pub close_txid: Option<String>,
    pub funding_height: Option<u32>,
    /// How the channel ended: `close`, `refund`, or `coop-ironwood`.
    pub settled_by: Option<String>,
}

/// A payment as the solver (or the agent) tracks it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    pub quote: Signed<Quote>,
    /// Solver: `agent_signed` → `done` | `failed`. Agent: `signed` → `done` | `failed` |
    /// `refused` (the solver did not take the state).
    pub status: String,
    pub agent_cumulative: u64,
    pub agent_sig: Option<String>,
    pub merchant_cumulative: u64,
    pub merchant_sig: Option<String>,
    /// Solver: the merchant channel's signature before this payment, restored if the
    /// merchant refuses.
    pub prev_merchant_sig: Option<String>,
    pub ack: Option<Signed<Ack>>,
    pub error: Option<String>,
    pub updated: u64,
}

/// A merchant the solver pays, and its channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MerchantRow {
    pub id: String,
    pub url: String,
    pub pubkey: String,
    pub channel_id: String,
}

/// An invoice the merchant issued.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvoiceRow {
    pub invoice: Signed<Invoice>,
    /// `open` → `paid` → `served`.
    pub status: String,
    pub ack: Option<Signed<Ack>>,
}

pub struct Store(Mutex<Connection>);

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
}

fn parse<T: for<'de> Deserialize<'de>>(s: String) -> Result<T> {
    serde_json::from_str(&s).map_err(|e| Error::Store(format!("row: {e}")))
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        let c = Connection::open(path)?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.pragma_update(None, "synchronous", "FULL")?;
        c.execute_batch(SCHEMA)?;
        Ok(Self(Mutex::new(c)))
    }

    /// Runs `f` in one transaction: all of it commits, or none.
    pub fn tx<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let mut c = self.0.lock().unwrap();
        let t = c.transaction()?;
        let r = f(&t)?;
        t.commit()?;
        Ok(r)
    }

    pub fn read<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let c = self.0.lock().unwrap();
        f(&c)
    }
}

pub fn meta_get(c: &Connection, k: &str) -> Result<Option<String>> {
    Ok(c.query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0)).optional()?)
}

pub fn meta_set(c: &Connection, k: &str, v: &str) -> Result<()> {
    c.execute("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)", [k, v])?;
    Ok(())
}

/// The next unused wallet index for channel keys (starts at 1; 0 is the deposit key).
pub fn next_index(c: &Connection) -> Result<u32> {
    let n = meta_get(c, "next_index")?.and_then(|v| v.parse().ok()).unwrap_or(1u32);
    meta_set(c, "next_index", &(n + 1).to_string())?;
    Ok(n)
}

pub fn event(c: &Connection, kind: &str, data: serde_json::Value) -> Result<()> {
    c.execute(
        "INSERT INTO events (at, kind, data) VALUES (?1, ?2, ?3)",
        params![now() as i64, kind, data.to_string()],
    )?;
    Ok(())
}

pub fn events(c: &Connection, limit: u32) -> Result<Vec<serde_json::Value>> {
    let mut s = c.prepare("SELECT seq, at, kind, data FROM events ORDER BY seq DESC LIMIT ?1")?;
    let rows = s.query_map([limit], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut out = vec![];
    for r in rows {
        let (seq, at, kind, data) = r?;
        let data: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
        out.push(serde_json::json!({"seq": seq, "at": at, "kind": kind, "data": data}));
    }
    Ok(out)
}

pub fn channel_put(c: &Connection, ch: &ChannelRow) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO channels (id, role, data) VALUES (?1, ?2, ?3)",
        params![ch.id, ch.role, json(ch)],
    )?;
    Ok(())
}

pub fn channel_get(c: &Connection, id: &str) -> Result<Option<ChannelRow>> {
    let s: Option<String> = c
        .query_row("SELECT data FROM channels WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    s.map(parse).transpose()
}

/// Reads, changes and writes one channel row (inside the caller's transaction).
pub fn channel_update(c: &Connection, id: &str, f: impl FnOnce(&mut ChannelRow)) -> Result<ChannelRow> {
    let mut r = channel_get(c, id)?.ok_or_else(|| Error::Store(format!("no channel {id}")))?;
    f(&mut r);
    channel_put(c, &r)?;
    Ok(r)
}

pub fn channels(c: &Connection, role: &str) -> Result<Vec<ChannelRow>> {
    let mut s = c.prepare("SELECT data FROM channels WHERE role = ?1 ORDER BY rowid")?;
    let rows: Vec<String> = s.query_map([role], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    rows.into_iter().map(parse).collect()
}

/// Appends a state to the log (`reference` ties it to a payment or invoice).
pub fn state_log(c: &Connection, channel_id: &str, cumulative: u64, sig: &str, reference: &str) -> Result<()> {
    c.execute(
        "INSERT INTO states (channel_id, cumulative, sig, ref, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![channel_id, cumulative as i64, sig, reference, now() as i64],
    )?;
    Ok(())
}

/// The signature of the highest logged state below `cumulative` (to restore a level).
pub fn state_before(c: &Connection, channel_id: &str, cumulative: u64) -> Result<Option<(u64, String)>> {
    Ok(c.query_row(
        "SELECT cumulative, sig FROM states WHERE channel_id = ?1 AND cumulative < ?2 ORDER BY cumulative DESC LIMIT 1",
        params![channel_id, cumulative as i64],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
    )
    .optional()?)
}

pub fn slot_add(c: &Connection, idx: u32) -> Result<()> {
    c.execute("INSERT INTO slots (idx, at) VALUES (?1, ?2)", params![idx, now() as i64])?;
    Ok(())
}

/// Consumes a key slot handed out by `GET /channels/key`; false if unknown or used.
pub fn slot_take(c: &Connection, idx: u32) -> Result<bool> {
    Ok(c.execute("DELETE FROM slots WHERE idx = ?1", [idx])? == 1)
}

pub fn invoice_put(c: &Connection, row: &InvoiceRow) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO invoices (id, status, data) VALUES (?1, ?2, ?3)",
        params![row.invoice.body.id, row.status, json(row)],
    )?;
    Ok(())
}

pub fn invoice_get(c: &Connection, id: &str) -> Result<Option<InvoiceRow>> {
    let s: Option<String> = c
        .query_row("SELECT data FROM invoices WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    s.map(parse).transpose()
}

pub fn invoices(c: &Connection, status: Option<&str>) -> Result<Vec<InvoiceRow>> {
    let mut s = c.prepare("SELECT data FROM invoices WHERE ?1 IS NULL OR status = ?1 ORDER BY rowid")?;
    let rows: Vec<String> = s.query_map([status], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    rows.into_iter().map(parse).collect()
}

pub fn merchant_put(c: &Connection, m: &MerchantRow) -> Result<()> {
    c.execute("INSERT OR REPLACE INTO merchants (id, data) VALUES (?1, ?2)", params![m.id, json(m)])?;
    Ok(())
}

pub fn merchant_get(c: &Connection, id: &str) -> Result<Option<MerchantRow>> {
    let s: Option<String> = c
        .query_row("SELECT data FROM merchants WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    s.map(parse).transpose()
}

pub fn merchants(c: &Connection) -> Result<Vec<MerchantRow>> {
    let mut s = c.prepare("SELECT data FROM merchants ORDER BY rowid")?;
    let rows: Vec<String> = s.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    rows.into_iter().map(parse).collect()
}

pub fn quote_put(c: &Connection, q: &Signed<Quote>) -> Result<()> {
    c.execute("INSERT INTO quotes (id, used_by, data) VALUES (?1, NULL, ?2)", params![q.body.id, json(q)])?;
    Ok(())
}

/// The quote and the payment that used it, if any.
pub fn quote_get(c: &Connection, id: &str) -> Result<Option<(Signed<Quote>, Option<String>)>> {
    let r: Option<(String, Option<String>)> = c
        .query_row("SELECT data, used_by FROM quotes WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    r.map(|(d, u)| Ok((parse(d)?, u))).transpose()
}

pub fn quote_use(c: &Connection, id: &str, payment_id: &str) -> Result<bool> {
    Ok(c.execute("UPDATE quotes SET used_by = ?2 WHERE id = ?1 AND used_by IS NULL", [id, payment_id])? == 1)
}

/// Whether an invoice has a payment that is not `failed` (the solver pays an invoice once).
pub fn invoice_in_payment(c: &Connection, invoice_id: &str) -> Result<bool> {
    let mut s = c.prepare("SELECT data FROM payments WHERE status != 'failed'")?;
    let rows: Vec<String> = s.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    for d in rows {
        let p: Payment = parse(d)?;
        if p.quote.body.invoice.body.id == invoice_id {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn payment_put(c: &Connection, p: &Payment) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO payments (id, quote_id, agent_channel, status, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![p.id, p.quote.body.id, p.quote.body.agent_channel, p.status, json(p)],
    )?;
    Ok(())
}

pub fn payment_get(c: &Connection, id: &str) -> Result<Option<Payment>> {
    let s: Option<String> = c
        .query_row("SELECT data FROM payments WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    s.map(parse).transpose()
}

pub fn payments(c: &Connection, status: Option<&str>) -> Result<Vec<Payment>> {
    let mut s = c.prepare("SELECT data FROM payments WHERE ?1 IS NULL OR status = ?1 ORDER BY rowid")?;
    let rows: Vec<String> = s.query_map([status], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    rows.into_iter().map(parse).collect()
}

pub fn credit_get(c: &Connection, agent_channel: &str) -> Result<u64> {
    let v: Option<i64> = c
        .query_row("SELECT amount FROM credits WHERE agent_channel = ?1", [agent_channel], |r| r.get(0))
        .optional()?;
    Ok(v.unwrap_or(0) as u64)
}

pub fn credit_set(c: &Connection, agent_channel: &str, amount: u64) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO credits (agent_channel, amount) VALUES (?1, ?2)",
        params![agent_channel, amount as i64],
    )?;
    Ok(())
}
