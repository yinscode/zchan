//! HTTP plumbing shared by the services: JSON errors, a JSON client, HTML escaping.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::Error;

/// An error response: `{"error": "..."}` with a status.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let code = match &e {
            Error::Terms(_) | Error::State(_) | Error::Signature(_) | Error::Wire(_) | Error::Funding(_) => {
                StatusCode::BAD_REQUEST
            }
            Error::Refused(c, _) => StatusCode::from_u16(*c).unwrap_or(StatusCode::BAD_GATEWAY),
            Error::Http(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(code, e.to_string())
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

pub fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

async fn decode<R: DeserializeOwned>(resp: reqwest::Response) -> crate::Result<R> {
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_owned))
            .unwrap_or(text);
        return Err(Error::Refused(status.as_u16(), msg));
    }
    serde_json::from_str(&text).map_err(|e| Error::Wire(format!("{e}: {text}")))
}

/// POSTs JSON. A non-2xx answer is `Error::Refused(status, reason)`; a transport failure
/// is `Error::Http` (the request may or may not have been processed).
pub async fn post<B: Serialize, R: DeserializeOwned>(http: &reqwest::Client, url: &str, body: &B) -> crate::Result<R> {
    decode(http.post(url).json(body).send().await?).await
}

pub async fn get<R: DeserializeOwned>(http: &reqwest::Client, url: &str) -> crate::Result<R> {
    decode(http.get(url).send().await?).await
}

/// Whether a failure means the peer definitely did not act: a 4xx answer.
pub fn definitive(e: &Error) -> bool {
    matches!(e, Error::Refused(c, _) if (400..500).contains(c))
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Zatoshis as ZEC, e.g. `0.0002`.
pub fn zec(z: u64) -> String {
    let s = format!("{}.{:08}", z / 100_000_000, z % 100_000_000);
    let s = s.trim_end_matches('0');
    s.strip_suffix('.').map(|x| format!("{x}.0")).unwrap_or(s.into())
}

/// The shared page shell: light and dark themes, phone-width safe.
pub fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{t}</title><style>
:root{{--bg:#fbfaf7;--fg:#1d1d1b;--mute:#6b6a65;--line:#e4e1d8;--card:#fff;--acc:#c2410c;--ok:#15803d;--bad:#b91c1c}}
@media (prefers-color-scheme:dark){{:root{{--bg:#151513;--fg:#ecebe6;--mute:#a19f97;--line:#2e2d29;--card:#1d1c1a;--acc:#fb923c;--ok:#4ade80;--bad:#f87171}}}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 ui-sans-serif,system-ui,sans-serif}}
main{{max-width:980px;margin:0 auto;padding:20px 16px 48px}}h1{{font-size:22px;margin:0 0 4px}}h2{{font-size:16px;margin:28px 0 8px}}
.mute{{color:var(--mute)}}.card{{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:14px 16px;margin:10px 0}}
table{{width:100%;border-collapse:collapse;font-size:13px}}th,td{{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}}
th{{color:var(--mute);font-weight:600}}.scroll{{overflow-x:auto}}code,.mono{{font:12px ui-monospace,Menlo,monospace;word-break:break-all}}
.ok{{color:var(--ok)}}.bad{{color:var(--bad)}}.big{{font-size:28px;font-weight:700}}
button{{font:inherit;padding:10px 16px;border-radius:8px;border:1px solid var(--line);background:var(--acc);color:#fff;cursor:pointer}}
button.alt{{background:transparent;color:var(--fg)}}input{{font:inherit;padding:9px 10px;border:1px solid var(--line);border-radius:8px;background:var(--card);color:var(--fg);width:100%}}
.qr svg{{width:260px;height:260px;max-width:100%;background:#fff;padding:10px;border-radius:8px}}
</style></head><body><main>{b}</main></body></html>"#,
        t = esc(title),
        b = body
    )
}
