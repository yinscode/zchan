//! x402 v2 over HTTP for the `zchannel` scheme.
//!
//! Wire format from the x402 v2 specification and its HTTP transport
//! (x402-foundation/x402 `specs/x402-specification-v2.md`, `specs/transports-v2/http.md`,
//! read at commit faa1c694, 2026-10-15): `PAYMENT-REQUIRED` (402 response),
//! `PAYMENT-SIGNATURE` (request) and `PAYMENT-RESPONSE` (response) headers, each
//! base64-encoded JSON.
//!
//! `zchannel` is a local extension, not a registered scheme: no public facilitator or
//! client supports it. Its `PaymentRequirements.extra` carries the signed invoice and the
//! solver to pay through, and `paymentFlow` is `upfront` (the channel state is committed
//! before the resource is served). Its `PaymentPayload.payload` carries the merchant's
//! signed acknowledgment. See `docs/x402-zchannel.md`.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::{
    wire::{Ack, Invoice, Signed},
    Error, Result,
};

pub const VERSION: u32 = 2;
pub const SCHEME: &str = "zchannel";
pub const PAYMENT_REQUIRED: &str = "PAYMENT-REQUIRED";
pub const PAYMENT_SIGNATURE: &str = "PAYMENT-SIGNATURE";
pub const PAYMENT_RESPONSE: &str = "PAYMENT-RESPONSE";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ZchannelExtra {
    /// Protocol-reserved key (spec §6.1): the state is committed before the resource runs.
    pub payment_flow: String,
    pub invoice: Signed<Invoice>,
    /// Base URL of the solver that pays this merchant.
    pub solver: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    pub scheme: String,
    pub network: String,
    /// Zatoshis, as a decimal string.
    pub amount: String,
    pub asset: String,
    /// The merchant id.
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    pub extra: ZchannelExtra,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequired {
    pub x402_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub resource: ResourceInfo,
    pub accepts: Vec<PaymentRequirements>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ZchannelPayload {
    pub ack: Signed<Ack>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceInfo>,
    pub accepted: PaymentRequirements,
    pub payload: ZchannelPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SettlementResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// Empty: a channel payment broadcasts no transaction.
    pub transaction: String,
    pub network: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

pub fn encode<T: Serialize>(v: &T) -> String {
    STANDARD.encode(serde_json::to_vec(v).expect("serializable"))
}

pub fn decode<T: DeserializeOwned>(header: &str) -> Result<T> {
    let b = STANDARD
        .decode(header.trim())
        .map_err(|e| Error::Wire(format!("base64: {e}")))?;
    serde_json::from_slice(&b).map_err(|e| Error::Wire(format!("x402 json: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_match_the_spec() {
        let sk = zchan_tx::keys::dev_key("m");
        let inv = Invoice { id: "i".into(), merchant_id: "m".into(), amount: 7, resource: "/r".into(), expires_at: 1 };
        let req = PaymentRequired {
            x402_version: 2,
            error: None,
            resource: ResourceInfo { url: "http://x/r".into(), description: None, mime_type: Some("application/json".into()) },
            accepts: vec![PaymentRequirements {
                scheme: SCHEME.into(),
                network: "zcash:regtest".into(),
                amount: "7".into(),
                asset: "ZEC".into(),
                pay_to: "m".into(),
                max_timeout_seconds: 60,
                extra: ZchannelExtra {
                    payment_flow: "upfront".into(),
                    invoice: Signed::sign(crate::wire::INVOICE, inv, &sk),
                    solver: "http://s".into(),
                },
            }],
        };
        let v: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(v["x402Version"], 2);
        assert_eq!(v["resource"]["mimeType"], "application/json");
        let a = &v["accepts"][0];
        for k in ["scheme", "network", "amount", "asset", "payTo", "maxTimeoutSeconds", "extra"] {
            assert!(!a[k].is_null(), "{k}");
        }
        assert_eq!(a["extra"]["paymentFlow"], "upfront");
        let back: PaymentRequired = decode(&encode(&req)).unwrap();
        assert_eq!(back, req);
    }
}
