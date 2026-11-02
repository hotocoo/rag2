//! Thin REST client for the console: the same endpoints every other client uses.

use gloo_net::http::{Request, RequestBuilder, Response};
use gloo_storage::{SessionStorage, Storage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const KEY_STORAGE: &str = "rag2.console.key";
const URL_STORAGE: &str = "rag2.console.url";

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub base: String,
    pub key: String,
}

impl Session {
    /// The saved session for this browser tab, if any.
    pub fn load() -> Option<Session> {
        let key: String = SessionStorage::get(KEY_STORAGE).ok()?;
        let base: String = SessionStorage::get(URL_STORAGE).unwrap_or_default();
        Some(Session { base, key })
    }

    pub fn save(&self) {
        // Session storage only: the key is gone when the tab closes.
        let _ = SessionStorage::set(KEY_STORAGE, &self.key);
        let _ = SessionStorage::set(URL_STORAGE, &self.base);
    }

    pub fn clear() {
        SessionStorage::delete(KEY_STORAGE);
        SessionStorage::delete(URL_STORAGE);
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }

    fn auth(&self, r: RequestBuilder) -> RequestBuilder {
        r.header("authorization", &format!("Bearer {}", self.key))
    }

    async fn read<T: DeserializeOwned>(resp: Response) -> Result<T, String> {
        let status = resp.status();
        if status == 204 {
            return serde_json::from_value(Value::Null).map_err(|e| e.to_string());
        }
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let msg = v["error"]["message"].as_str().map(String::from).unwrap_or(text);
            return Err(format!("{status}: {msg}"));
        }
        serde_json::from_str(&text).map_err(|e| format!("unreadable response: {e}"))
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let resp = self.auth(Request::get(&self.url(path))).send().await.map_err(|e| e.to_string())?;
        Self::read(resp).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, String> {
        let req = self.auth(Request::post(&self.url(path))).json(body).map_err(|e| e.to_string())?;
        Self::read(req.send().await.map_err(|e| e.to_string())?).await
    }

    pub async fn delete(&self, path: &str) -> Result<Value, String> {
        let resp = self.auth(Request::delete(&self.url(path))).send().await.map_err(|e| e.to_string())?;
        Self::read(resp).await
    }
}

/// Percent-encode one path segment.
pub fn seg(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// JSON value to YAML text for display and editing.
pub fn to_yaml(v: &Value) -> String {
    serde_norway::to_string(v).unwrap_or_else(|e| format!("# could not render: {e}"))
}
