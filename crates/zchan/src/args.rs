//! A small command-line parser: `bin [command] --key value --flag ...`.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use crate::ctx::Config;

pub struct Args {
    pub command: Option<String>,
    pub positional: Vec<String>,
    opts: HashMap<String, String>,
}

impl Args {
    pub fn parse() -> Self {
        Self::from(std::env::args().skip(1).collect())
    }

    pub fn from(v: Vec<String>) -> Self {
        let mut it = v.into_iter().peekable();
        let mut positional = vec![];
        let mut opts = HashMap::new();
        while let Some(a) = it.next() {
            if let Some(k) = a.strip_prefix("--") {
                let flag = it.peek().is_none_or(|n| n.starts_with("--"));
                let v = if flag { "true".into() } else { it.next().unwrap() };
                opts.insert(k.to_string(), v);
            } else {
                positional.push(a);
            }
        }
        let command = if positional.is_empty() { None } else { Some(positional.remove(0)) };
        Self { command, positional, opts }
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.opts.get(k).map(String::as_str).or(None)
    }

    pub fn str(&self, k: &str, default: &str) -> String {
        self.get(k).unwrap_or(default).to_string()
    }

    pub fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> T {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    pub fn flag(&self, k: &str) -> bool {
        self.get(k) == Some("true")
    }

    /// The options every service shares. Environment `ZCHAN_NODE_URL` and `ZCHAN_FAULT`
    /// apply when the flags are absent.
    pub fn config(&self, default_listen: &str) -> (Config, String) {
        let listen = self.str("listen", default_listen);
        let d = Config::default();
        let cfg = Config {
            data_dir: PathBuf::from(self.str("data", "./data")),
            node_url: self
                .get("node")
                .map(str::to_owned)
                .or_else(|| std::env::var("ZCHAN_NODE_URL").ok())
                .unwrap_or(d.node_url),
            base_url: self.str("url", &format!("http://{listen}")),
            dev_seed: self.get("dev-seed").map(str::to_owned),
            automine: self.flag("automine"),
            poll: Duration::from_millis(self.num("poll-ms", 5_000)),
            close_margin: self.num("close-margin", d.close_margin),
            min_lock: self.num("min-lock", d.min_lock),
            fee: self.num("fee", d.fee),
            max_payment: self.num("max-payment", d.max_payment),
            name: self.str("name", &d.name),
            solver_url: self.str("solver", ""),
            price: self.num("price", d.price),
            invoice_ttl: self.num("invoice-ttl", d.invoice_ttl),
            invoice_grace: self.num("invoice-grace", d.invoice_grace),
            upstream: self.get("upstream").map(str::to_owned),
            routes: self.get("routes").map(|r| crate::gate::parse_routes(r).unwrap_or_else(|e| {
                eprintln!("--routes: {e}");
                std::process::exit(2)
            })).unwrap_or_default(),
            admin_token: self.get("admin-token").map(str::to_owned).or_else(|| std::env::var("ZCHAN_ADMIN_TOKEN").ok()),
            fault: self.get("fault").map(str::to_owned).or_else(|| std::env::var("ZCHAN_FAULT").ok()),
        };
        let loopback = listen.starts_with("127.") || listen.starts_with("localhost:") || listen.starts_with("[::1]");
        if !loopback && cfg.admin_token.is_none() && self.command.as_deref() != Some("serve") {
            eprintln!("listening on {listen}: set --admin-token (or ZCHAN_ADMIN_TOKEN) to protect /admin/*");
            std::process::exit(2);
        }
        (cfg, listen)
    }
}
