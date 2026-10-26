//! zchan: unidirectional (Spilman) payment channels on Zcash v6. An agent pays merchants
//! through a solver: one agent→solver channel, and one solver→merchant channel per
//! merchant shared by every agent. See `docs/plan.md`.
//!
//! - `channel`: terms, refund, states, the payee's verifier (no I/O).
//! - `wire`: signed messages (invoice, quote, acknowledgment) and JSON encodings.
//! - `store`: SQLite, one file per process.
//! - `ctx`, `payer`, `payee`: a running party: node access, opening, watching, closing.
//! - `solver`, `merchant`, `agent`: the three services.
//! - `x402`, `mpp`: the x402 v2 and MPP wire formats for the `zchannel` method.
//! - `gate`: the merchant's payment gate (both protocols) and proxy mode.
//! - `playground`: a web page where anyone can try the whole flow with an agent of their own.

pub mod agent;
pub mod args;
pub mod channel;
pub mod ctx;
pub mod gate;
pub mod merchant;
pub mod mpp;
pub mod payee;
pub mod payer;
pub mod playground;
pub mod solver;
pub mod store;
pub mod web;
pub mod wire;
pub mod x402;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Tx(#[from] zchan_tx::Error),
    #[error("terms: {0}")]
    Terms(String),
    #[error("state: {0}")]
    State(String),
    #[error("signature: {0}")]
    Signature(String),
    #[error("funding: {0}")]
    Funding(String),
    #[error("wire: {0}")]
    Wire(String),
    #[error("store: {0}")]
    Store(String),
    #[error("http: {0}")]
    Http(String),
    /// The other side refused with an HTTP status and reason.
    #[error("refused ({0}): {1}")]
    Refused(u16, String),
    #[error("{0}")]
    Other(String),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Store(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Wire(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
