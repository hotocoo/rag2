//! Outbound webhooks. Events go into a bounded queue and are delivered by a background task, so a
//! slow or failing receiver never slows a request; a full queue drops events (counted in
//! `rag2_webhook_dropped_total`). Deliveries retry with exponential backoff and are signed with
//! HMAC-SHA256 when the webhook has a secret.

use hmac::{KeyInit, Mac};
use rag2_config::ConfigManager;
use rag2_config::schema::{Webhook, WebhookEvent};
use rag2_config::store::now_ms;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Events waiting for delivery before new ones are dropped.
const QUEUE_CAPACITY: usize = 10_000;
/// First retry delay; doubles per attempt.
const RETRY_BASE: Duration = Duration::from_millis(500);

pub const SIGNATURE_HEADER: &str = "x-rag2-signature";
pub const TIMESTAMP_HEADER: &str = "x-rag2-timestamp";
pub const EVENT_HEADER: &str = "x-rag2-event";
pub const DELIVERY_HEADER: &str = "x-rag2-delivery";

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: String,
    pub event: WebhookEvent,
    pub at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub data: serde_json::Value,
}

#[derive(Clone)]
pub struct Dispatcher {
    tx: mpsc::Sender<Event>,
}

/// `sha256=<hex>` over `"{timestamp}.{body}"`.
pub fn sign(secret: &[u8], timestamp: u64, body: &[u8]) -> String {
    // HMAC accepts keys of any length, so this never fails in practice.
    let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    format!(
        "sha256={}",
        tag.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn wants(w: &Webhook, e: &Event) -> bool {
    w.events.contains(&e.event)
        && (w.tenants.is_empty()
            || e.tenant
                .as_ref()
                .is_some_and(|t| w.tenants.iter().any(|x| x == t)))
}

async fn deliver(http: reqwest::Client, w: Webhook, e: Event) {
    let body = match serde_json::to_vec(&e) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(error = %err, "webhook event did not serialize");
            return;
        }
    };
    let secret = match w.secret.as_ref().map(|s| s.resolve()).transpose() {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(webhook = %w.name, error = %err, "webhook secret unavailable; not delivering");
            return;
        }
    };
    for attempt in 0..=w.max_retries {
        let ts = now_ms() / 1000;
        let mut req = http
            .post(&w.url)
            .timeout(Duration::from_millis(w.timeout_ms))
            .header("content-type", "application/json")
            .header(
                EVENT_HEADER,
                serde_json::to_value(e.event)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
            )
            .header(DELIVERY_HEADER, &e.id)
            .header(TIMESTAMP_HEADER, ts.to_string())
            .body(body.clone());
        if let Some(s) = &secret {
            req = req.header(SIGNATURE_HEADER, sign(s.as_bytes(), ts, &body));
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => {
                metrics::counter!("rag2_webhook_delivered_total", "webhook" => w.name.clone())
                    .increment(1);
                return;
            }
            Ok(r) => {
                tracing::warn!(webhook = %w.name, status = %r.status(), attempt, "webhook refused")
            }
            Err(err) => {
                tracing::warn!(webhook = %w.name, error = %err, attempt, "webhook delivery failed")
            }
        }
        if attempt < w.max_retries {
            tokio::time::sleep(RETRY_BASE * 2u32.saturating_pow(attempt)).await;
        }
    }
    metrics::counter!("rag2_webhook_failed_total", "webhook" => w.name.clone()).increment(1);
}

impl Dispatcher {
    /// Start the delivery task. Webhook definitions are read from the active profile per event,
    /// so they follow configuration changes without restart.
    pub fn start(config: Arc<ConfigManager>) -> Dispatcher {
        let (tx, mut rx) = mpsc::channel::<Event>(QUEUE_CAPACITY);
        // Without a runtime (some unit tests) events are queued and never sent.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let http = reqwest::Client::new();
                while let Some(e) = rx.recv().await {
                    let hooks: Vec<Webhook> = config
                        .current()
                        .profile
                        .webhooks
                        .iter()
                        .filter(|w| wants(w, &e))
                        .cloned()
                        .collect();
                    for w in hooks {
                        tokio::spawn(deliver(http.clone(), w, e.clone()));
                    }
                }
            });
        }
        Dispatcher { tx }
    }

    pub fn emit(&self, event: WebhookEvent, tenant: Option<&str>, data: serde_json::Value) {
        let e = Event {
            id: uuid::Uuid::now_v7().to_string(),
            event,
            at_ms: now_ms(),
            tenant: tenant.map(String::from),
            data,
        };
        if self.tx.try_send(e).is_err() {
            metrics::counter!("rag2_webhook_dropped_total").increment(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_hmac_sha256_over_timestamp_and_body() {
        // RFC 4231 test case 2 key/data, with the timestamp prefix applied by hand.
        let sig = sign(b"Jefe", 0, b"what do ya want for nothing?");
        assert!(sig.starts_with("sha256=") && sig.len() == "sha256=".len() + 64);
        assert_ne!(
            sig,
            sign(b"Jefe", 1, b"what do ya want for nothing?"),
            "timestamp is covered"
        );
        assert_ne!(sig, sign(b"other", 0, b"what do ya want for nothing?"));
    }

    #[test]
    fn subscriptions_filter_by_event_and_tenant() {
        let w = Webhook {
            name: "w".into(),
            url: "https://example.com/hook".into(),
            events: vec![WebhookEvent::DocumentsIngested],
            secret: None,
            tenants: vec!["acme".into()],
            timeout_ms: 1000,
            max_retries: 0,
        };
        let ev = |event, tenant: Option<&str>| Event {
            id: "1".into(),
            event,
            at_ms: 0,
            tenant: tenant.map(String::from),
            data: serde_json::Value::Null,
        };
        assert!(wants(
            &w,
            &ev(WebhookEvent::DocumentsIngested, Some("acme"))
        ));
        assert!(!wants(
            &w,
            &ev(WebhookEvent::DocumentsIngested, Some("globex"))
        ));
        assert!(!wants(&w, &ev(WebhookEvent::ConfigActivated, None)));
    }
}
