//! QQ Webhook 事件源。
//!
//! Ed25519 验签、回调地址验证（`op=13`）、按 `event_id` 去重，
//! 并把事件作为 `Stream` 交给上层。

use crate::{dispatch::Envelope, error::Error, redact};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use futures_util::Stream;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const BODY_LIMIT: usize = 1024 * 1024;
const DEDUP_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookConfig {
    pub bind: SocketAddr,
    pub path: String,
}

/// 验签与回调验证所需的密钥。
#[derive(Clone)]
pub struct Webhook {
    signing: Arc<SigningKey>,
    verifying: VerifyingKey,
}

impl Webhook {
    /// 从 QQ 应用的 client secret 派生 Ed25519 密钥。
    pub fn new(secret: &str) -> Result<Self, Error> {
        if secret.is_empty() {
            return Err(Error::auth("webhook secret is empty"));
        }
        let seed = webhook_seed(secret);
        let signing = SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        Ok(Self {
            signing: Arc::new(signing),
            verifying,
        })
    }

    /// 绑定端口并启动 Webhook 服务。端口绑定失败时返回错误。
    pub async fn serve(
        &self,
        config: WebhookConfig,
        event_capacity: usize,
    ) -> Result<WebhookStream, Error> {
        if !config.path.starts_with('/')
            || config.path.len() > 128
            || config.path.contains(['?', '#'])
        {
            return Err(Error::auth(
                "webhook path must start with '/' and be at most 128 bytes",
            ));
        }
        let (events, receiver) = mpsc::channel(event_capacity.max(1));
        let state = WebhookState {
            signing: self.signing.clone(),
            verifying: self.verifying,
            events,
            seen: Arc::new(Mutex::new(HashMap::new())),
        };
        let app = Router::new()
            .route(&config.path, post(handle))
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|_| Error::network(format!("cannot bind webhook on {}", config.bind)))?;
        let address = listener
            .local_addr()
            .map_err(|_| Error::network("cannot inspect webhook listener address"))?;
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let online = Arc::new(AtomicBool::new(true));
        let server_online = online.clone();
        tracing::info!(
            bind = %address,
            path = %config.path,
            "QQ webhook server listening"
        );
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move { server_cancel.cancelled().await })
                .await;
            server_online.store(false, Ordering::Release);
            if let Err(error) = result {
                tracing::warn!(
                    reason = %redact::text(&error.to_string()),
                    "QQ webhook server stopped with an error"
                );
            }
        });
        Ok(WebhookStream {
            receiver,
            cancel,
            task,
            online,
            address,
        })
    }
}

/// Webhook 事件流。丢弃时自动停止 HTTP 服务。
pub struct WebhookStream {
    receiver: mpsc::Receiver<Envelope>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    online: Arc<AtomicBool>,
    address: SocketAddr,
}

impl WebhookStream {
    pub fn online(&self) -> Arc<AtomicBool> {
        self.online.clone()
    }

    /// 实际绑定的地址。配置使用端口 0 时可从这里取得系统分配的端口。
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Stream for WebhookStream {
    type Item = Envelope;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(context)
    }
}

impl Drop for WebhookStream {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

#[derive(Clone)]
struct WebhookState {
    signing: Arc<SigningKey>,
    verifying: VerifyingKey,
    events: mpsc::Sender<Envelope>,
    seen: Arc<Mutex<HashMap<String, Instant>>>,
}

async fn handle(State(state): State<WebhookState>, headers: HeaderMap, body: Bytes) -> Response {
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => {
            tracing::warn!("QQ webhook rejected invalid JSON");
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid json"})),
            )
                .into_response();
        }
    };
    if payload.get("op").and_then(Value::as_u64) == Some(13) {
        return validation_response(&state.signing, &payload);
    }
    if let Err(reason) = verify_signature(&state.verifying, &headers, &body) {
        tracing::warn!(reason, "QQ webhook signature verification failed");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid signature"})),
        )
            .into_response();
    }
    if payload.get("op").and_then(Value::as_u64) == Some(0) {
        let event_id = payload
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        if let Some(event_id) = &event_id {
            let mut seen = lock(&state.seen);
            let now = Instant::now();
            seen.retain(|_, at| now.duration_since(*at) < DEDUP_TTL);
            if seen.contains_key(event_id) {
                tracing::debug!("QQ webhook duplicate event acknowledged");
                return Json(json!({"op": 12})).into_response();
            }
        }
        let Some(envelope) = Envelope::from_json(&payload) else {
            tracing::warn!("QQ webhook event omitted its type; acknowledging without dispatch");
            return Json(json!({"op": 12})).into_response();
        };
        match state.events.try_send(envelope) {
            Ok(()) => {
                if let Some(event_id) = event_id {
                    lock(&state.seen).insert(event_id, Instant::now());
                }
            }
            Err(_) => {
                tracing::warn!("QQ webhook event queue is full; asking the platform to retry");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "event queue full"})),
                )
                    .into_response();
            }
        }
    }
    Json(json!({"op": 12})).into_response()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn validation_response(signing: &SigningKey, payload: &Value) -> Response {
    let Some(plain_token) = payload["d"]["plain_token"].as_str() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing plain_token"})),
        )
            .into_response();
    };
    let Some(event_ts) = payload["d"]["event_ts"].as_str() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "missing event_ts"})),
        )
            .into_response();
    };
    let message = format!("{event_ts}{plain_token}");
    let signature = signing.sign(message.as_bytes());
    Json(json!({
        "plain_token": plain_token,
        "signature": hex_encode(&signature.to_bytes()),
    }))
    .into_response()
}

fn verify_signature(
    verifying: &VerifyingKey,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), &'static str> {
    let signature = headers
        .get("X-Signature-Ed25519")
        .ok_or("missing signature")?
        .to_str()
        .map_err(|_| "invalid signature header")?;
    let timestamp = headers
        .get("X-Signature-Timestamp")
        .ok_or("missing timestamp")?
        .to_str()
        .map_err(|_| "invalid timestamp header")?;
    let signature = hex_decode(signature).ok_or("invalid signature encoding")?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| "invalid signature length")?;
    let signature = Signature::from_bytes(&signature);
    let mut message = Vec::with_capacity(timestamp.len() + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(body);
    verifying
        .verify_strict(&message, &signature)
        .map_err(|_| "signature mismatch")
}

fn webhook_seed(secret: &str) -> [u8; 32] {
    let mut seed = secret.as_bytes().to_vec();
    while seed.len() < 32 {
        seed.extend_from_slice(secret.as_bytes());
    }
    seed.truncate(32);
    seed.try_into().expect("seed length is exactly 32 bytes")
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push((high * 16 + low) as u8);
    }
    Some(bytes)
}
