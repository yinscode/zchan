//! MPP (the HTTP `Payment` authentication scheme) for the `zchannel` method, intent
//! `charge`.
//!
//! Wire format from draft-httpauth-payment-01 and draft-payment-intent-charge-00
//! (paymentauth.org, read 2026-10-21) and the mpp.dev protocol pages:
//!
//! - Challenge: `WWW-Authenticate: Payment id=".." realm=".." method=".." intent=".."
//!   request=".." expires=".."`. `request` is JCS JSON, base64url without padding.
//!   `id` is bound to the other parameters with HMAC-SHA256 (`realm|method|intent|
//!   request|expires|digest|opaque`, absent values empty).
//! - Credential: `Authorization: Payment <base64url JSON>` with the echoed `challenge`,
//!   an optional `source`, and the method's `payload`.
//! - Receipt: `Payment-Receipt: <base64url JSON>` on success.
//! - Failure: 402, a fresh challenge, `Cache-Control: no-store`, and an RFC 9457 problem
//!   (`https://paymentauth.org/problems/{code}`).
//!
//! `zchannel` is not a registered MPP method. Its `request.methodDetails` carries the
//! signed invoice, the solver and the network; its `payload` carries the merchant's
//! signed acknowledgment, as in the x402 scheme (docs/x402-zchannel.md).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    wire::{Ack, Invoice, Signed},
    Error, Result,
};

pub const METHOD: &str = "zchannel";
pub const INTENT: &str = "charge";
pub const RECEIPT: &str = "Payment-Receipt";
pub const PROBLEM: &str = "https://paymentauth.org/problems/";

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s.trim().trim_end_matches('='))
        .map_err(|e| Error::Wire(format!("base64url: {e}")))
}

/// RFC 8785 (JCS) for the values zchan produces: objects with keys sorted by code
/// point, no whitespace, strings and integers as serde_json writes them.
pub fn jcs(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", serde_json::to_string(k).unwrap(), jcs(&m[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(jcs).collect::<Vec<_>>().join(",")),
        other => serde_json::to_string(other).unwrap(),
    }
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(msg).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

/// The `charge` request object (draft-payment-intent-charge §5.1).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChargeRequest {
    /// Zatoshis.
    pub amount: String,
    pub currency: String,
    /// The merchant id.
    pub recipient: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The invoice id.
    pub external_id: String,
    pub method_details: MethodDetails,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MethodDetails {
    pub invoice: Signed<Invoice>,
    pub solver: String,
    pub network: String,
}

/// A challenge, as sent in `WWW-Authenticate` and echoed in a credential.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Challenge {
    pub id: String,
    pub realm: String,
    pub method: String,
    pub intent: String,
    pub request: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Challenge {
    /// A challenge whose id is HMAC-bound under `secret`.
    pub fn new(secret: &[u8], realm: &str, req: &ChargeRequest, expires: &str) -> Self {
        let request = b64(jcs(&serde_json::to_value(req).expect("serializable")).as_bytes());
        let mut c = Self {
            id: String::new(),
            realm: realm.into(),
            method: METHOD.into(),
            intent: INTENT.into(),
            request,
            expires: Some(expires.into()),
            description: req.description.clone(),
        };
        c.id = c.binding(secret);
        c
    }

    /// `realm|method|intent|request|expires|digest|opaque` (no digest, opaque or header).
    pub fn binding(&self, secret: &[u8]) -> String {
        let input = format!(
            "{}|{}|{}|{}|{}||",
            self.realm,
            self.method,
            self.intent,
            self.request,
            self.expires.as_deref().unwrap_or("")
        );
        b64(&hmac_sha256(secret, input.as_bytes()))
    }

    pub fn charge(&self) -> Result<ChargeRequest> {
        serde_json::from_slice(&unb64(&self.request)?).map_err(|e| Error::Wire(format!("charge request: {e}")))
    }

    /// The `WWW-Authenticate` value.
    pub fn header(&self) -> String {
        let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut h = format!(
            "Payment id=\"{}\", realm=\"{}\", method=\"{}\", intent=\"{}\", request=\"{}\"",
            q(&self.id),
            q(&self.realm),
            q(&self.method),
            q(&self.intent),
            q(&self.request)
        );
        if let Some(e) = &self.expires {
            h += &format!(", expires=\"{}\"", q(e));
        }
        if let Some(d) = &self.description {
            h += &format!(", description=\"{}\"", q(d));
        }
        h
    }

    /// Parses one `WWW-Authenticate: Payment ...` value (quoted or token auth-params).
    pub fn parse(header: &str) -> Result<Self> {
        let rest = header
            .trim()
            .strip_prefix("Payment")
            .filter(|r| r.starts_with(' '))
            .ok_or_else(|| Error::Wire("not a Payment challenge".into()))?;
        let mut params = std::collections::HashMap::new();
        let mut chars = rest.chars().peekable();
        loop {
            while chars.peek().is_some_and(|c| *c == ' ' || *c == ',') {
                chars.next();
            }
            let name: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ' ' && *c != ',')).collect();
            if name.is_empty() {
                break;
            }
            if chars.next() != Some('=') {
                return Err(Error::Wire(format!("auth-param {name} without a value")));
            }
            let value = if chars.peek() == Some(&'"') {
                chars.next();
                let mut v = String::new();
                loop {
                    match chars.next() {
                        Some('\\') => v.extend(chars.next()),
                        Some('"') => break,
                        Some(c) => v.push(c),
                        None => return Err(Error::Wire("unterminated quoted string".into())),
                    }
                }
                v
            } else {
                std::iter::from_fn(|| chars.next_if(|c| *c != ',' && *c != ' ')).collect()
            };
            params.insert(name.to_ascii_lowercase(), value);
        }
        let get = |k: &str| params.get(k).cloned().ok_or_else(|| Error::Wire(format!("challenge without {k}")));
        Ok(Self {
            id: get("id")?,
            realm: get("realm")?,
            method: get("method")?,
            intent: get("intent")?,
            request: get("request")?,
            expires: params.get("expires").cloned(),
            description: params.get("description").cloned(),
        })
        .and_then(|c: Self| if c.id.is_empty() { Err(Error::Wire("empty challenge id".into())) } else { Ok(c) })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Payload {
    pub ack: Signed<Ack>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Credential {
    pub challenge: Challenge,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub payload: Payload,
}

impl Credential {
    /// The `Authorization` value.
    pub fn header(&self) -> String {
        format!("Payment {}", b64(&serde_json::to_vec(self).expect("serializable")))
    }

    pub fn parse(header: &str) -> Result<Self> {
        let token = header
            .trim()
            .strip_prefix("Payment ")
            .ok_or_else(|| Error::Wire("not a Payment credential".into()))?;
        serde_json::from_slice(&unb64(token)?).map_err(|e| Error::Wire(format!("credential json: {e}")))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub status: String,
    pub method: String,
    pub timestamp: String,
    /// The invoice id.
    pub reference: String,
    /// zchannel additions: the channel and the level the merchant holds.
    pub channel_id: String,
    pub cumulative: u64,
}

impl Receipt {
    pub fn header(&self) -> String {
        b64(&serde_json::to_vec(self).expect("serializable"))
    }

    pub fn parse(header: &str) -> Result<Self> {
        serde_json::from_slice(&unb64(header)?).map_err(|e| Error::Wire(format!("receipt json: {e}")))
    }
}

/// An RFC 9457 problem body.
pub fn problem(code: &str, title: &str, detail: &str) -> Value {
    serde_json::json!({"type": format!("{PROBLEM}{code}"), "title": title, "status": 402, "detail": detail})
}

/// Unix seconds as RFC 3339 UTC (`2026-10-21T12:00:00Z`).
pub fn rfc3339(unix: u64) -> String {
    let (days, secs) = ((unix / 86_400) as i64, unix % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Parses `YYYY-MM-DDTHH:MM:SSZ` back to Unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::INVOICE;

    #[test]
    fn dates_roundtrip() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_792_526_762), "2026-10-20T20:06:02Z");
        for t in [0u64, 951_782_400, 1_709_164_800, 1_792_526_762, 4_102_444_799] {
            assert_eq!(parse_rfc3339(&rfc3339(t)), Some(t));
        }
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(hex::encode(mac), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn jcs_sorts_keys_without_whitespace() {
        let v = serde_json::json!({"b": "x", "a": {"d": 1, "c": [2, "y"]}});
        assert_eq!(jcs(&v), r#"{"a":{"c":[2,"y"],"d":1},"b":"x"}"#);
    }

    #[test]
    fn challenge_header_roundtrip_and_binding() {
        let sk = zchan_tx::keys::dev_key("m");
        let inv = Signed::sign(INVOICE, Invoice { id: "i".into(), merchant_id: "m".into(), amount: 7, resource: "/r".into(), expires_at: 9 }, &sk);
        let req = ChargeRequest {
            amount: "7".into(),
            currency: "zec".into(),
            recipient: "m".into(),
            description: Some("a \"quoted\" thing".into()),
            external_id: "i".into(),
            method_details: MethodDetails { invoice: inv, solver: "http://s".into(), network: "zcash:regtest".into() },
        };
        let c = Challenge::new(b"secret", "m.example", &req, &rfc3339(9));
        let back = Challenge::parse(&c.header()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.binding(b"secret"), back.id);
        assert_eq!(back.charge().unwrap(), req);
        let mut t = back.clone();
        t.request = b64(jcs(&serde_json::json!({"amount": "1"})).as_bytes());
        assert_ne!(t.binding(b"secret"), t.id, "a modified request breaks the binding");
        assert!(!c.request.contains('='), "no padding");
    }
}
