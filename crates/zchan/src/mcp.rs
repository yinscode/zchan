//! `zchan-agent mcp`: the agent as an MCP server over stdio, for assistants that call
//! tools rather than run commands. The person who starts it sets the price cap and the
//! session budget; tool arguments can only lower them.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{
    agent::{Agent, Protocol},
    payer, Error, Result,
};

/// What the person allows: per request, per session, and the refund lock of new channels.
pub struct Limits {
    pub max_price: u64,
    pub budget: u64,
    pub lock_blocks: u32,
}

/// Answers JSON-RPC messages, one per line on stdin, until stdin closes.
pub async fn serve(agent: Arc<Agent>, limits: Limits) -> Result<()> {
    let io = |e: std::io::Error| Error::Other(format!("stdio: {e}"));
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    let mut s = Session { agent, limits, spent: 0 };
    while let Some(line) = lines.next_line().await.map_err(io)? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => s.handle(msg).await,
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await.map_err(io)?;
            out.flush().await.map_err(io)?;
        }
    }
    Ok(())
}

struct Session {
    agent: Arc<Agent>,
    limits: Limits,
    /// Debited from the agent's channel by this server since it started.
    spent: u64,
}

impl Session {
    /// One message in; a response for requests, nothing for notifications.
    async fn handle(&mut self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned()?;
        let method = msg["method"].as_str().unwrap_or("");
        if let Some(r) = protocol(method, &msg["params"]) {
            return Some(match r {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, m)) => error(id, code, &m),
            });
        }
        let (name, args) = (msg["params"]["name"].as_str().unwrap_or(""), &msg["params"]["arguments"]);
        let result = match self.call(name, args).await {
            Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
            Err(e) => json!({"content": [{"type": "text", "text": explain(&e)}], "isError": true}),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    async fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        let a = &self.agent;
        let out = match name {
            "zchan_fetch" => {
                let url = args["url"].as_str().ok_or_else(|| Error::Other("url is required".into()))?;
                let method = reqwest::Method::from_bytes(args["method"].as_str().unwrap_or("GET").as_bytes())
                    .map_err(|e| Error::Other(format!("method: {e}")))?;
                let body = args["body"].as_str().map(|b| b.as_bytes().to_vec());
                let proto = if args["mpp"].as_bool() == Some(true) { Protocol::Mpp } else { Protocol::X402 };
                // The cap leaves room for the solver's fee within what is left of the budget.
                let fee = a.solver_info().await?.fee;
                let budget_cap = self.budget_left().saturating_sub(fee);
                let cap = [Some(self.limits.max_price), Some(budget_cap), args["max_price"].as_u64()].into_iter().flatten().min().unwrap_or(0);
                let by_budget = cap == budget_cap && budget_cap < self.limits.max_price;
                let before = a.channel().ok().map(|r| (r.id, r.cumulative));
                let left = self.budget_left();
                let f = a.fetch_capped(method, url, body, proto, Some(cap)).await.map_err(|e| match e {
                    Error::Other(m) if by_budget && m.contains("above the cap") => {
                        Error::Other(format!("{m}; this session's budget has {left} zat left, solver fee included"))
                    }
                    e => e,
                })?;
                let paid = match (before, a.channel().ok()) {
                    (Some((id, was)), Some(r)) if r.id == id => r.cumulative.saturating_sub(was),
                    _ => 0,
                };
                self.spent += paid;
                json!({"status": f.status, "paid_zat": paid, "budget_left_zat": self.budget_left(), "body": f.body})
            }
            "zchan_status" => {
                let ch = a.channel().ok();
                let shielded: u64 = a.ctx.shielded_notes().await?.iter().map(|n| n.value()).sum();
                json!({
                    "network": a.ctx.network,
                    "channel": match &ch {
                        Some(r) => json!({"id": r.id, "capacity_zat": r.terms.capacity, "paid_zat": r.cumulative,
                                          "remaining_zat": payer::remaining(&a.ctx, r)?}),
                        None => Value::Null,
                    },
                    "shielded_balance_zat": shielded,
                    "session": {"spent_zat": self.spent, "budget_left_zat": self.budget_left(), "max_price_zat": self.limits.max_price},
                })
            }
            "zchan_request_funds" => {
                let zat = args["amount_zat"].as_u64().filter(|z| *z > 0).ok_or_else(|| Error::Other("amount_zat is required".into()))?;
                let ua = zchan_tx::keys::unified_address(&a.ctx.wallet.shielded_address(0), a.ctx.net);
                json!({
                    "payment_link": zchan_tx::keys::payment_uri(&ua, zat, args["message"].as_str()),
                    "note": "Give this link to the person. A Zcash wallet that pays shielded addresses opens it with the amount filled in.",
                })
            }
            "zchan_open" => {
                let cap = args["capacity_zat"].as_u64().filter(|z| *z > 0).ok_or_else(|| Error::Other("capacity_zat is required".into()))?;
                let r = a.open_shielded(cap, self.limits.lock_blocks).await?;
                json!({"channel": r.id, "capacity_zat": r.terms.capacity, "status": r.status})
            }
            "zchan_close" => {
                let id = a.channel()?.id;
                json!({"channel": id, "close_txid": a.close(&id).await?, "note": "Both sides are paid to shielded addresses once the close is mined."})
            }
            _ => return Err(Error::Other(format!("unknown tool {name}"))),
        };
        Ok(serde_json::to_string_pretty(&out).unwrap_or_default())
    }

    fn budget_left(&self) -> u64 {
        self.limits.budget.saturating_sub(self.spent)
    }
}

/// The protocol's own methods; `None` for `tools/call`.
fn protocol(method: &str, params: &Value) -> Option<std::result::Result<Value, (i64, String)>> {
    Some(match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "zchan", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Pay for HTTP resources from a Zcash payment channel. Use zchan_fetch for any URL that may answer 402. Check zchan_status for the budget; when there is no channel or too little left, ask the person for funds with zchan_request_funds, then zchan_open. Close with zchan_close when the work is done.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => return None,
        _ => Err((-32601, format!("method not found: {method}"))),
    })
}

fn tools() -> Value {
    let obj = |props: Value, required: &[&str]| json!({"type": "object", "properties": props, "required": required});
    json!([
        {"name": "zchan_fetch",
         "description": "Fetch an HTTP URL. If it answers 402 with the zchannel scheme (x402 or MPP), pay it from the Zcash channel within the price cap and session budget, and return the paid response.",
         "inputSchema": obj(json!({
             "url": {"type": "string"},
             "method": {"type": "string", "description": "HTTP method, GET by default"},
             "body": {"type": "string"},
             "mpp": {"type": "boolean", "description": "Prefer MPP over x402 when both are offered"},
             "max_price": {"type": "integer", "description": "The most to pay for this request, in zatoshis. Can only lower the server's cap."},
         }), &["url"])},
        {"name": "zchan_status",
         "description": "The open channel (capacity, paid, remaining), the shielded balance, and this session's spending and budget.",
         "inputSchema": obj(json!({}), &[])},
        {"name": "zchan_request_funds",
         "description": "A ZIP 321 payment link for the person to fund this agent from a Zcash wallet. Ask for the channel capacity plus 20,000 zatoshis for fees.",
         "inputSchema": obj(json!({
             "amount_zat": {"type": "integer"},
             "message": {"type": "string", "description": "Shown to the person in their wallet"},
         }), &["amount_zat"])},
        {"name": "zchan_open",
         "description": "Open a payment channel to the solver from the agent's shielded funds. Waits for one block.",
         "inputSchema": obj(json!({"capacity_zat": {"type": "integer"}}), &["capacity_zat"])},
        {"name": "zchan_close",
         "description": "Settle the open channel cooperatively, both sides paid to shielded addresses. The unspent balance returns to the agent.",
         "inputSchema": obj(json!({}), &[])},
    ])
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// An error as the model reads it, with what it can do next.
fn explain(e: &Error) -> String {
    let m = e.to_string();
    if m.contains("above the cap") {
        format!("{m}. The person running this server sets the cap and the budget; ask them before paying more.")
    } else if m.contains("with a payment") {
        format!("{m}. A channel with no payments has nothing to settle; the refund watcher returns it at its lock height.")
    } else if m.contains("no open channel") {
        format!("{m}. Check zchan_status; if there are shielded funds, open a channel with zchan_open, otherwise ask for funds with zchan_request_funds.")
    } else {
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_methods() {
        let init = protocol("initialize", &json!({"protocolVersion": "2025-03-26"})).unwrap().unwrap();
        assert_eq!(init["protocolVersion"], "2025-03-26");
        assert_eq!(init["capabilities"], json!({"tools": {}}));
        let names: Vec<_> = protocol("tools/list", &Value::Null).unwrap().unwrap()["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["zchan_fetch", "zchan_status", "zchan_request_funds", "zchan_open", "zchan_close"]);
        assert!(protocol("tools/call", &Value::Null).is_none());
        assert_eq!(protocol("resources/list", &Value::Null).unwrap().unwrap_err().0, -32601);
    }

    #[test]
    fn errors_say_what_to_do() {
        assert!(explain(&Error::Other("price 3000 zat is above the cap of 1000 zat; nothing was paid".into())).contains("ask them"));
        assert!(explain(&Error::State("no open channel to http://s".into())).contains("zchan_request_funds"));
    }
}
