//! QQ Gateway WebSocket 事件源。
//!
//! 负责 `/gateway/bot` 连接发现、多 Shard 调度、Identify/Resume、心跳、按 `s` 去重、断线指数退避重连，
//! 并把会话通过 [`SessionStore`] 交给上层持久化。对外只暴露 `Stream`。

use crate::{
    client::{Client, Intents},
    dispatch::Envelope,
    error::Error,
    redact,
};
use async_trait::async_trait;
use futures_util::{SinkExt, Stream, StreamExt, future::BoxFuture};
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_IDENTIFY_DELAY: Duration = Duration::from_secs(5);

mod checkpoint;
use checkpoint::CheckpointWriter;
pub use checkpoint::GatewayAcknowledgement;

type DispatchItem = (Envelope, Option<GatewayAcknowledgement>);

/// 单个 Gateway 分片标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shard {
    pub id: u32,
    pub count: u32,
}

/// Gateway 分片策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShardConfig {
    /// 启动时通过 `/gateway/bot` 自动采用平台推荐分片数（接口不可用时退回单分片）。
    #[default]
    Auto,
    /// 指定总分片数，当前进程运行全部 `[0..count)` 分片。
    Count(u32),
    /// 仅运行指定分片 `[id, count]`，适用于多进程外部编排。
    Single { id: u32, count: u32 },
}

/// 可持久化的 Gateway 会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: String,
    pub seq: u64,
    /// 建立该会话时使用的 intents 位掩码；变化后必须重新 Identify。
    pub intents: u64,
    /// 建立该会话时的分片总数；变化后必须重新 Identify。
    pub shard_count: u32,
}

/// 上层提供会话持久化；SDK 不依赖具体数据库。
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn load(&self, shard_id: u32) -> Result<Option<Session>, Error>;
    async fn save(&self, shard_id: u32, session: &Session) -> Result<(), Error>;
    async fn clear(&self, shard_id: u32) -> Result<(), Error>;
}

/// 不持久化会话的实现，用于不需要 Resume 的调用方。
pub struct NoopSessionStore;

#[async_trait]
impl SessionStore for NoopSessionStore {
    async fn load(&self, _shard_id: u32) -> Result<Option<Session>, Error> {
        Ok(None)
    }

    async fn save(&self, _shard_id: u32, _session: &Session) -> Result<(), Error> {
        Ok(())
    }

    async fn clear(&self, _shard_id: u32) -> Result<(), Error> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub intents: Intents,
    pub shard: ShardConfig,
    pub reconnect_initial: Duration,
    pub reconnect_max: Duration,
    pub ready_timeout: Duration,
    pub identify_delay: Duration,
    pub event_capacity: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            intents: Intents::default(),
            shard: ShardConfig::Auto,
            reconnect_initial: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
            ready_timeout: Duration::from_secs(10),
            identify_delay: DEFAULT_IDENTIFY_DELAY,
            event_capacity: 256,
        }
    }
}

/// Gateway 事件流。丢弃时自动停止后台任务。
pub struct GatewayStream {
    receiver: mpsc::Receiver<DispatchItem>,
    pending: Option<BoxFuture<'static, Envelope>>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
    online: Arc<AtomicBool>,
}

impl GatewayStream {
    /// 连接是否处于在线状态，供 `get_status` 使用。
    pub fn online(&self) -> Arc<AtomicBool> {
        self.online.clone()
    }

    /// 按顺序处理事件，并在处理完成后调用确认句柄。
    /// 消费者应始终使用此方法或始终使用 `Stream` 接口。
    pub async fn next_dispatch(&mut self) -> Option<(Envelope, Option<GatewayAcknowledgement>)> {
        self.receiver.recv().await
    }
}

impl Stream for GatewayStream {
    type Item = Envelope;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.pending.is_none() {
            let Some((envelope, acknowledgement)) =
                std::task::ready!(self.receiver.poll_recv(context))
            else {
                return Poll::Ready(None);
            };
            let Some(acknowledgement) = acknowledgement else {
                return Poll::Ready(Some(envelope));
            };
            self.pending = Some(Box::pin(async move {
                acknowledgement.acknowledge().await;
                envelope
            }));
        }
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Pending;
        };
        let envelope = std::task::ready!(pending.as_mut().poll(context));
        self.pending = None;
        Poll::Ready(Some(envelope))
    }
}

impl Drop for GatewayStream {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

/// 启动 Gateway 事件源。
pub fn connect(
    client: Arc<Client>,
    config: GatewayConfig,
    store: Arc<dyn SessionStore>,
) -> GatewayStream {
    let (sender, receiver) = mpsc::channel(config.event_capacity.max(1));
    let cancel = CancellationToken::new();
    let online = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run(
        client,
        config,
        store,
        cancel.clone(),
        sender,
        online.clone(),
    ));
    GatewayStream {
        receiver,
        pending: None,
        cancel,
        task,
        online,
    }
}

enum Failure {
    Fatal(Error),
    Reconnect { reason: &'static str, ready: bool },
}

fn reconnect(reason: &'static str, ready: bool) -> Failure {
    Failure::Reconnect { reason, ready }
}

struct DiscoveredGateway {
    url: Url,
    shards: u32,
    max_concurrency: u32,
}

async fn discover_gateway(client: &Client) -> Result<DiscoveredGateway, Failure> {
    match client.gateway_bot().await {
        Ok(bot) => {
            let url = validate_gateway_url(&bot.url).map_err(Failure::Fatal)?;
            let max_concurrency = bot
                .session_start_limit
                .as_ref()
                .map(|limit| limit.max_concurrency)
                .unwrap_or(1)
                .max(1);
            Ok(DiscoveredGateway {
                url,
                shards: bot.shards.max(1),
                max_concurrency,
            })
        }
        Err(Error::Api {
            http_status: 404, ..
        }) => {
            let raw_url = client
                .gateway_url()
                .await
                .map_err(|error| client_failure(error, "gateway discovery failed"))?;
            let url = validate_gateway_url(&raw_url).map_err(Failure::Fatal)?;
            Ok(DiscoveredGateway {
                url,
                shards: 1,
                max_concurrency: 1,
            })
        }
        Err(error) => Err(client_failure(error, "gateway discovery failed")),
    }
}

fn resolve_shards(setting: ShardConfig, recommended: u32) -> Result<Vec<Shard>, Error> {
    match setting {
        ShardConfig::Auto => {
            let count = recommended.max(1);
            Ok((0..count).map(|id| Shard { id, count }).collect())
        }
        ShardConfig::Count(0) => Err(Error::decode("gateway shard count must be positive")),
        ShardConfig::Count(count) => Ok((0..count).map(|id| Shard { id, count }).collect()),
        ShardConfig::Single { id, count } => {
            if count == 0 || id >= count {
                return Err(Error::decode(
                    "invalid gateway shard specification: shard id must be smaller than shard count",
                ));
            }
            Ok(vec![Shard { id, count }])
        }
    }
}

struct IdentifyWindow {
    used: usize,
    until: Instant,
}

struct IdentifyLimiter {
    max_concurrency: usize,
    delay: Duration,
    window: tokio::sync::Mutex<IdentifyWindow>,
}

impl IdentifyLimiter {
    fn new(max_concurrency: u32, delay: Duration) -> Self {
        Self {
            max_concurrency: usize::try_from(max_concurrency.max(1)).unwrap_or(1),
            delay,
            window: tokio::sync::Mutex::new(IdentifyWindow {
                used: 0,
                until: Instant::now(),
            }),
        }
    }

    async fn acquire(&self, cancel: &CancellationToken) -> bool {
        if self.delay.is_zero() {
            return !cancel.is_cancelled();
        }
        let mut window = self.window.lock().await;
        let now = Instant::now();
        if window.used == 0 || now >= window.until {
            window.used = 1;
            window.until = now + self.delay;
            return true;
        }
        if window.used < self.max_concurrency {
            window.used += 1;
            return true;
        }
        let wait = window.until.saturating_duration_since(now);
        tokio::select! {
            _ = cancel.cancelled() => false,
            _ = tokio::time::sleep(wait) => {
                window.used = 1;
                window.until = Instant::now() + self.delay;
                true
            }
        }
    }
}

struct OnlineTracker {
    online: Arc<AtomicBool>,
    connected: std::sync::Mutex<usize>,
}

impl OnlineTracker {
    fn new(online: Arc<AtomicBool>) -> Self {
        Self {
            online,
            connected: std::sync::Mutex::new(0),
        }
    }

    fn enter(self: &Arc<Self>) -> OnlineGuard {
        if let Ok(mut connected) = self.connected.lock() {
            *connected += 1;
            self.online.store(true, Ordering::Release);
        }
        OnlineGuard(self.clone())
    }

    fn leave(&self) {
        if let Ok(mut connected) = self.connected.lock() {
            *connected = connected.saturating_sub(1);
            self.online.store(*connected > 0, Ordering::Release);
        }
    }

    fn clear(&self) {
        if let Ok(mut connected) = self.connected.lock() {
            *connected = 0;
        }
        self.online.store(false, Ordering::Release);
    }
}

struct OnlineGuard(Arc<OnlineTracker>);

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        self.0.leave();
    }
}

async fn run(
    client: Arc<Client>,
    config: GatewayConfig,
    store: Arc<dyn SessionStore>,
    cancel: CancellationToken,
    out: mpsc::Sender<DispatchItem>,
    online: Arc<AtomicBool>,
) {
    if let Err(error) = resolve_shards(config.shard, 1) {
        tracing::warn!(
            reason = %redact::text(&error.to_string()),
            "QQ Gateway terminated by invalid shard configuration"
        );
        online.store(false, Ordering::Release);
        return;
    }
    let mut backoff = config.reconnect_initial;
    let discovered = loop {
        if cancel.is_cancelled() {
            online.store(false, Ordering::Release);
            return;
        }
        match discover_gateway(&client).await {
            Ok(discovered) => break discovered,
            Err(Failure::Fatal(error)) => {
                tracing::warn!(
                    reason = %redact::text(&error.to_string()),
                    "QQ Gateway terminated by a non-recoverable error"
                );
                online.store(false, Ordering::Release);
                return;
            }
            Err(Failure::Reconnect { reason, .. }) => {
                tracing::warn!(reason, "QQ Gateway discovery retrying");
                sleep_or_cancel(backoff, &cancel).await;
                backoff = (backoff * 2).min(config.reconnect_max);
            }
        }
    };
    let shards = match resolve_shards(config.shard, discovered.shards) {
        Ok(shards) => shards,
        Err(error) => {
            tracing::warn!(
                reason = %redact::text(&error.to_string()),
                "QQ Gateway terminated by invalid shard configuration"
            );
            online.store(false, Ordering::Release);
            return;
        }
    };
    let tracker = Arc::new(OnlineTracker::new(online));
    let limiter = Arc::new(IdentifyLimiter::new(
        discovered.max_concurrency,
        config.identify_delay,
    ));
    let mut tasks = JoinSet::new();
    for shard in shards {
        let client = client.clone();
        let config = config.clone();
        let checkpoint = Arc::new(CheckpointWriter::new(store.clone(), shard));
        let cancel = cancel.clone();
        let out = out.clone();
        let tracker = tracker.clone();
        let limiter = limiter.clone();
        let initial_url = Some(discovered.url.clone());
        tasks.spawn(async move {
            run_shard(
                client,
                config,
                shard,
                initial_url,
                checkpoint,
                cancel,
                out,
                tracker,
                limiter,
            )
            .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        if matches!(result, Ok(true) | Err(_)) {
            cancel.cancel();
        }
    }
    tracker.clear();
}

#[allow(clippy::too_many_arguments)]
async fn run_shard(
    client: Arc<Client>,
    config: GatewayConfig,
    shard: Shard,
    mut initial_url: Option<Url>,
    checkpoint: Arc<CheckpointWriter>,
    cancel: CancellationToken,
    out: mpsc::Sender<DispatchItem>,
    tracker: Arc<OnlineTracker>,
    limiter: Arc<IdentifyLimiter>,
) -> bool {
    let mut session = match checkpoint.store.load(shard.id).await {
        Ok(Some(session))
            if session.intents == config.intents.bits() && session.shard_count == shard.count =>
        {
            checkpoint
                .activate(&session.session_id, Some(session.seq))
                .await;
            Some(session)
        }
        Ok(Some(_)) => {
            tracing::info!(
                shard_id = shard.id,
                shard_count = shard.count,
                "persisted Gateway session intents or shard count changed; starting a new session"
            );
            checkpoint.clear().await;
            None
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                shard_id = shard.id,
                shard_count = shard.count,
                reason = %redact::text(&error.to_string()),
                "cannot load persisted Gateway session; starting without Resume"
            );
            None
        }
    };
    let mut backoff = config.reconnect_initial;
    let mut fatal = false;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match run_once(
            &client,
            &config,
            shard,
            initial_url.take(),
            &checkpoint,
            &cancel,
            &out,
            &tracker,
            &limiter,
            &mut session,
        )
        .await
        {
            Ok(ready) => {
                if cancel.is_cancelled() {
                    break;
                }
                if ready {
                    backoff = config.reconnect_initial;
                }
                sleep_or_cancel(backoff, &cancel).await;
                backoff = (backoff * 2).min(config.reconnect_max);
            }
            Err(Failure::Fatal(error)) => {
                tracing::warn!(
                    shard_id = shard.id,
                    shard_count = shard.count,
                    reason = %redact::text(&error.to_string()),
                    "QQ Gateway terminated by a non-recoverable error"
                );
                fatal = true;
                break;
            }
            Err(Failure::Reconnect { reason, ready }) => {
                tracing::warn!(
                    shard_id = shard.id,
                    shard_count = shard.count,
                    reason,
                    ready,
                    "QQ Gateway reconnecting"
                );
                if ready {
                    backoff = config.reconnect_initial;
                }
                sleep_or_cancel(backoff, &cancel).await;
                backoff = (backoff * 2).min(config.reconnect_max);
            }
        }
    }
    fatal
}

async fn sleep_or_cancel(duration: Duration, cancel: &CancellationToken) {
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(duration) => {}
    }
}

enum Control {
    Text(String),
    Pong(Vec<u8>),
}

#[allow(clippy::too_many_arguments)]
async fn run_once(
    client: &Client,
    config: &GatewayConfig,
    shard: Shard,
    initial_url: Option<Url>,
    checkpoint: &Arc<CheckpointWriter>,
    cancel: &CancellationToken,
    out: &mpsc::Sender<DispatchItem>,
    tracker: &Arc<OnlineTracker>,
    limiter: &IdentifyLimiter,
    session: &mut Option<Session>,
) -> Result<bool, Failure> {
    let url = match initial_url {
        Some(url) => url,
        None => discover_gateway(client).await?.url,
    };
    tracing::info!(
        shard_id = shard.id,
        shard_count = shard.count,
        endpoint = %url.origin().ascii_serialization(),
        "QQ Gateway connecting"
    );
    let mut socket_config = WebSocketConfig::default();
    socket_config.max_message_size = Some(MAX_MESSAGE_BYTES);
    socket_config.max_frame_size = Some(MAX_MESSAGE_BYTES);
    let (socket, _) = timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(url.as_str(), Some(socket_config), false),
    )
    .await
    .map_err(|_| reconnect("gateway connect timed out", false))?
    .map_err(|_| reconnect("gateway handshake failed", false))?;
    tracing::info!(
        shard_id = shard.id,
        shard_count = shard.count,
        "QQ Gateway WebSocket connected"
    );
    let _guard = tracker.enter();

    let (mut sink, mut stream) = socket.split();
    let (control_tx, mut control_rx) = mpsc::channel::<Control>(8);
    let mut writer = tokio::spawn(async move {
        while let Some(frame) = control_rx.recv().await {
            let message = match frame {
                Control::Text(text) => Message::Text(text.into()),
                Control::Pong(data) => Message::Pong(data.into()),
            };
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    let (fail_tx, mut fail_rx) = mpsc::channel::<&'static str>(1);
    let sequence = Arc::new(AtomicU64::new(session.as_ref().map(|s| s.seq).unwrap_or(0)));
    let ack_pending = Arc::new(AtomicBool::new(false));
    let backpressured = Arc::new(AtomicBool::new(false));
    let child = cancel.child_token();
    let mut heartbeat: Option<JoinHandle<()>> = None;
    let mut hello = false;
    let mut ready = false;
    let mut last_seq = sequence.load(Ordering::Acquire);
    let deadline = tokio::time::sleep(config.ready_timeout);
    tokio::pin!(deadline);

    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(ready),
            _ = child.cancelled() => break Ok(ready),
            writer_result = &mut writer => {
                match writer_result {
                    Ok(()) => break Err(reconnect("gateway writer stopped", ready)),
                    Err(_) => break Err(reconnect("gateway writer failed", ready)),
                }
            }
            reason = fail_rx.recv() => {
                break Err(reconnect(reason.unwrap_or("heartbeat stopped"), ready));
            }
            _ = &mut deadline, if !ready => {
                break Err(reconnect("gateway ready timeout", false));
            }
            incoming = stream.next() => {
                let Some(incoming) = incoming else {
                    break Err(reconnect("gateway peer disconnected", ready));
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(_) => break Err(reconnect("gateway receive failed", ready)),
                };
                match message {
                    Message::Text(text) => {
                        let payload: Value = match serde_json::from_str(&text) {
                            Ok(payload) => payload,
                            Err(_) => break Err(reconnect("gateway sent invalid JSON", ready)),
                        };
                        match payload.get("op").and_then(Value::as_u64) {
                            Some(10) => {
                                if hello {
                                    break Err(reconnect("gateway sent duplicate hello", ready));
                                }
                                let interval = payload["d"]["heartbeat_interval"]
                                    .as_u64()
                                    .filter(|value| (1..=300_000).contains(value));
                                let Some(interval) = interval else {
                                    break Err(Failure::Fatal(Error::decode(
                                        "gateway hello omitted a valid heartbeat_interval",
                                    )));
                                };
                                hello = true;
                                let token = match client.access_token().await {
                                    Ok(token) => token,
                                    Err(error) => {
                                        break Err(client_failure(error, "access token unavailable"));
                                    }
                                };
                                let auth = match session.as_ref() {
                                    Some(session) => json!({
                                        "op": 6,
                                        "d": {
                                            "token": format!("QQBot {token}"),
                                            "session_id": session.session_id,
                                            "seq": session.seq,
                                        }
                                    }),
                                    None => {
                                        if !limiter.acquire(cancel).await {
                                            break Ok(ready);
                                        }
                                        json!({
                                            "op": 2,
                                            "d": {
                                                "token": format!("QQBot {token}"),
                                                "intents": config.intents.bits(),
                                                "shard": [shard.id, shard.count],
                                            }
                                        })
                                    }
                                };
                                if control_tx
                                    .send(Control::Text(auth.to_string()))
                                    .await
                                    .is_err()
                                {
                                    break Err(reconnect("gateway writer unavailable", ready));
                                }
                                heartbeat = Some(spawn_heartbeat(
                                    interval,
                                    control_tx.clone(),
                                    sequence.clone(),
                                    ack_pending.clone(),
                                    backpressured.clone(),
                                    fail_tx.clone(),
                                    child.clone(),
                                ));
                            }
                            Some(11) => {
                                ack_pending.store(false, Ordering::Release);
                            }
                            Some(1) => {
                                let payload = heartbeat_payload(sequence.load(Ordering::Acquire));
                                if control_tx
                                    .send(Control::Text(payload.to_string()))
                                    .await
                                    .is_err()
                                {
                                    break Err(reconnect("gateway writer unavailable", ready));
                                }
                            }
                            Some(7) => break Err(reconnect("gateway requested reconnect", ready)),
                            Some(9) => {
                                tracing::warn!("QQ Gateway invalidated the session; clearing it");
                                *session = None;
                                sequence.store(0, Ordering::Release);
                                checkpoint.clear().await;
                                break Err(reconnect("gateway invalidated the session", false));
                            }
                            Some(0) => {
                                let seq = payload.get("s").and_then(Value::as_u64);
                                if let Some(seq) = seq
                                    && seq <= last_seq
                                {
                                    continue;
                                }
                                let event_type = payload
                                    .get("t")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                let data = payload.get("d").cloned().unwrap_or(Value::Null);
                                if event_type == "READY" {
                                    let session_id = data
                                        .get("session_id")
                                        .and_then(Value::as_str)
                                        .filter(|id| !id.is_empty());
                                    let Some(session_id) = session_id else {
                                        break Err(Failure::Fatal(Error::decode(
                                            "gateway READY omitted session_id",
                                        )));
                                    };
                                    let new_session = Session {
                                        session_id: session_id.to_owned(),
                                        seq: seq.unwrap_or(0),
                                        intents: config.intents.bits(),
                                        shard_count: shard.count,
                                    };
                                    checkpoint.activate(session_id, None).await;
                                    *session = Some(new_session);
                                    ready = true;
                                    tracing::info!(
                                        shard_id = shard.id,
                                        shard_count = shard.count,
                                        "QQ Gateway ready"
                                    );
                                } else if event_type == "RESUMED" {
                                    ready = true;
                                    tracing::info!(
                                        shard_id = shard.id,
                                        shard_count = shard.count,
                                        "QQ Gateway session resumed"
                                    );
                                }
                                if let Some(seq) = seq {
                                    last_seq = seq;
                                    sequence.store(seq, Ordering::Release);
                                    if let Some(current) = session.as_mut() {
                                        current.seq = seq;
                                    }
                                }
                                let envelope = Envelope::parse(
                                    payload.get("id").and_then(Value::as_str).map(str::to_owned),
                                    if event_type.is_empty() { "UNKNOWN" } else { event_type },
                                    data,
                                );
                                backpressured.store(true, Ordering::Release);
                                let confirmed_sequence = seq.or((event_type == "READY").then_some(0));
                                let acknowledgement = checkpoint
                                    .acknowledgement(confirmed_sequence, config.intents.bits())
                                    .await;
                                let sent = tokio::select! {
                                    _ = cancel.cancelled() => break Ok(ready),
                                    sent = out.send((envelope, acknowledgement)) => sent,
                                };
                                backpressured.store(false, Ordering::Release);
                                if sent.is_err() {
                                    break Ok(ready);
                                }
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(payload) => {
                        if control_tx.send(Control::Pong(payload.to_vec())).await.is_err() {
                            break Err(reconnect("gateway writer unavailable", ready));
                        }
                    }
                    Message::Close(frame) => {
                        let code = frame.as_ref().map(|frame| u16::from(frame.code));
                        if let Some(code) = code {
                            tracing::warn!(
                                shard_id = shard.id,
                                shard_count = shard.count,
                                close_code = code,
                                "QQ Gateway closed by peer"
                            );
                            if fatal_close(code) {
                                break Err(Failure::Fatal(Error::auth(format!(
                                    "gateway rejected authentication, intents or sharding (close code {code})"
                                ))));
                            }
                            if reset_session_on_close(code) {
                                *session = None;
                                sequence.store(0, Ordering::Release);
                                checkpoint.clear().await;
                            }
                        }
                        break Err(reconnect("gateway closed by peer", ready));
                    }
                    _ => {}
                }
            }
        }
    };

    child.cancel();
    if let Some(heartbeat) = heartbeat {
        heartbeat.abort();
    }
    writer.abort();
    result
}

fn spawn_heartbeat(
    interval_ms: u64,
    control: mpsc::Sender<Control>,
    sequence: Arc<AtomicU64>,
    ack_pending: Arc<AtomicBool>,
    backpressured: Arc<AtomicBool>,
    fail: mpsc::Sender<&'static str>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let interval = Duration::from_millis(interval_ms);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if backpressured.load(Ordering::Acquire) {
                        continue;
                    }
                    if ack_pending.swap(true, Ordering::AcqRel) {
                        let _ = fail.send("gateway heartbeat acknowledgement timed out").await;
                        break;
                    }
                    let payload = heartbeat_payload(sequence.load(Ordering::Acquire));
                    if control.send(Control::Text(payload.to_string())).await.is_err() {
                        let _ = fail.send("gateway heartbeat send failed").await;
                        break;
                    }
                }
            }
        }
    })
}

fn heartbeat_payload(sequence: u64) -> Value {
    json!({
        "op": 1,
        "d": if sequence == 0 { Value::Null } else { Value::from(sequence) },
    })
}

fn client_failure(error: Error, reason: &'static str) -> Failure {
    if error.is_auth() {
        Failure::Fatal(error)
    } else {
        reconnect(reason, false)
    }
}

fn validate_gateway_url(raw: &str) -> Result<Url, Error> {
    let url = Url::parse(raw).map_err(|_| Error::decode("invalid gateway URL"))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(host)) => host.is_loopback(),
        Some(url::Host::Ipv6(host)) => host.is_loopback(),
        None => false,
    };
    if !(url.scheme() == "wss" || url.scheme() == "ws" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::decode(
            "gateway URL must use WSS (or WS on loopback) without credentials or fragment",
        ));
    }
    Ok(url)
}

fn fatal_close(code: u16) -> bool {
    matches!(code, 4001 | 4002 | 4004 | 4010..=4014 | 4914 | 4915)
}

fn reset_session_on_close(code: u16) -> bool {
    matches!(code, 4006 | 4007 | 4900..=4913)
}
