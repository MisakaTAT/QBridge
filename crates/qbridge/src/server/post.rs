//! HTTP POST 上报：每个目标一个有界队列，失败最多重试 3 次。

use super::{Hub, PostTargetConfig};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha1::Sha1;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

const MAX_ATTEMPTS: usize = 4;

pub struct Delivery {
    pub text: Arc<str>,
    pub value: Arc<Value>,
}

pub fn spawn(
    hub: &Arc<Hub>,
    config: PostTargetConfig,
    mut receiver: mpsc::Receiver<Delivery>,
    index: usize,
) {
    let weak = Arc::downgrade(hub);
    let cancel = hub.cancellation();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            loop {
                let delivery = tokio::select! {
                    _ = cancel.cancelled() => break,
                    delivery = receiver.recv() => {
                        let Some(delivery) = delivery else { break };
                        delivery
                    }
                };
                let Some(hub) = weak.upgrade() else { break };
                let mut delivered = false;
                for attempt in 0..MAX_ATTEMPTS {
                    if attempt > 0 {
                        let delay = Duration::from_secs(1 << (attempt - 1));
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                    match deliver(&hub, &config, &delivery).await {
                        Ok(value) => {
                            delivered = true;
                            if let Some(operation) = value {
                                hub.quick_operation(&delivery.value, &operation).await;
                            }
                            break;
                        }
                        Err(reason) => {
                            tracing::debug!(
                                target = index,
                                attempt = attempt + 1,
                                reason,
                                "HTTP POST delivery attempt failed"
                            );
                        }
                    }
                }
                if !delivered {
                    tracing::warn!(
                        target = index,
                        attempts = MAX_ATTEMPTS,
                        "HTTP POST delivery failed and was dropped"
                    );
                }
            }
        });
    }
}

async fn deliver(
    hub: &Arc<Hub>,
    config: &PostTargetConfig,
    delivery: &Delivery,
) -> Result<Option<Value>, &'static str> {
    let mut request = hub
        .post_client
        .post(config.url.clone())
        .header("Content-Type", "application/json")
        .header("X-Self-ID", hub.self_id())
        .timeout(config.timeout)
        .body(delivery.text.as_bytes().to_vec());
    if let Some(secret) = &config.secret {
        let mut mac =
            Hmac::<Sha1>::new_from_slice(secret.as_bytes()).map_err(|_| "invalid signature key")?;
        mac.update(delivery.text.as_bytes());
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        request = request.header("X-Signature", format!("sha1={signature}"));
    }
    let response = request
        .send()
        .await
        .map_err(|_| "HTTP POST request failed")?;
    if !response.status().is_success() {
        return Err("HTTP POST returned a non-2xx status");
    }
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "HTTP POST response failed")?
    {
        if body.len() + chunk.len() > hub.max_body_bytes() {
            tracing::warn!(
                "HTTP POST response exceeded the size limit; ignoring its quick operation"
            );
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(value) => Ok(Some(value)),
        Err(_) => {
            tracing::debug!("HTTP POST response was not JSON; no quick operation");
            Ok(None)
        }
    }
}
