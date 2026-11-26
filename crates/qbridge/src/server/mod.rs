//! OB11 服务端：HTTP API、正向/反向 WebSocket、HTTP POST 与动作队列。

pub mod http;
pub mod post;
pub mod reverse;
pub mod session;
pub mod ws;

use crate::error::ActionError;
use async_trait::async_trait;
use qbridge_ob11::{
    Action, Call, Event, EventKind, MessageFormat, Mode, Response, Status,
    event::{BotStatus, LifecycleType, MetaEvent},
    quick_operation,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use url::Url;

const ASYNC_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum ActionOutcome {
    Ok(Value),
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct HttpPostDeliveryStats {
    pub queued: usize,
    pub queue_rejected: u64,
    pub retry_exhausted: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct EventDeliveryStats {
    pub http_post: HttpPostDeliveryStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallContext {
    event_delivery: EventDeliveryStats,
}

impl CallContext {
    pub fn new(event_delivery: EventDeliveryStats) -> Self {
        Self { event_delivery }
    }

    pub fn event_delivery(&self) -> EventDeliveryStats {
        self.event_delivery
    }
}

#[async_trait]
pub trait Handler: Send + Sync + 'static {
    fn self_id(&self) -> i64;
    async fn call(&self, call: Call, context: CallContext) -> Result<ActionOutcome, ActionError>;
}

#[derive(Debug, Clone)]
pub struct PostTargetConfig {
    pub url: Url,
    pub secret: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct HubOptions {
    pub message_format: MessageFormat,
    pub access_token: Option<String>,
    pub max_body_bytes: usize,
    pub max_ws_bytes: usize,
    pub queue_capacity: usize,
    pub rate_limit: Duration,
    pub heartbeat_ms: u64,
    pub online: Arc<AtomicBool>,
    pub http_posts: Vec<PostTargetConfig>,
}

impl Default for HubOptions {
    fn default() -> Self {
        Self {
            message_format: MessageFormat::Array,
            access_token: None,
            max_body_bytes: 1024 * 1024,
            max_ws_bytes: 1024 * 1024,
            queue_capacity: 256,
            rate_limit: Duration::from_millis(500),
            heartbeat_ms: 15_000,
            online: Arc::new(AtomicBool::new(true)),
            http_posts: Vec::new(),
        }
    }
}

pub struct Hub {
    handler: Arc<dyn Handler>,
    format: MessageFormat,
    access_token: Option<String>,
    max_body_bytes: usize,
    max_ws_bytes: usize,
    events: broadcast::Sender<Arc<str>>,
    post_targets: Vec<mpsc::Sender<post::Delivery>>,
    async_tx: mpsc::Sender<Queued>,
    rate_tx: mpsc::Sender<Queued>,
    cancel: CancellationToken,
    shutting_down: AtomicBool,
    next_request: AtomicU64,
    post_queue_rejected: AtomicU64,
    post_retry_exhausted: AtomicU64,
    heartbeat_ms: u64,
    online: Arc<AtomicBool>,
    pub(crate) post_client: reqwest::Client,
}

struct Queued {
    action: Action,
    span: tracing::Span,
    started: Instant,
}

impl Hub {
    pub fn new(handler: Arc<dyn Handler>, options: HubOptions) -> Arc<Self> {
        let capacity = options.queue_capacity.max(1);
        let (events, _) = broadcast::channel(capacity.max(16));
        let (async_tx, async_rx) = mpsc::channel(capacity);
        let (rate_tx, rate_rx) = mpsc::channel(capacity);
        let mut post_targets = Vec::new();
        let mut post_receivers = Vec::new();
        for _ in 0..options.http_posts.len() {
            let (sender, receiver) = mpsc::channel(capacity);
            post_targets.push(sender);
            post_receivers.push(receiver);
        }
        let post_client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| {
                tracing::warn!(
                    "cannot build the HTTP POST client; redirects stay enabled by default"
                );
                reqwest::Client::new()
            });
        let hub = Arc::new(Self {
            handler,
            format: options.message_format,
            access_token: options.access_token,
            max_body_bytes: options.max_body_bytes,
            max_ws_bytes: options.max_ws_bytes,
            events,
            post_targets,
            async_tx,
            rate_tx,
            cancel: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
            next_request: AtomicU64::new(1),
            post_queue_rejected: AtomicU64::new(0),
            post_retry_exhausted: AtomicU64::new(0),
            heartbeat_ms: options.heartbeat_ms,
            online: options.online,
            post_client,
        });
        spawn_action_workers(&hub, async_rx, rate_rx, options.rate_limit);
        for (index, (config, receiver)) in options
            .http_posts
            .into_iter()
            .zip(post_receivers)
            .enumerate()
        {
            post::spawn(&hub, config, receiver, index);
        }
        if hub.heartbeat_ms > 0 {
            spawn_heartbeat(&hub);
        }
        hub
    }

    pub fn self_id(&self) -> i64 {
        self.handler.self_id()
    }

    pub fn online(&self) -> Arc<AtomicBool> {
        self.online.clone()
    }

    pub fn message_format(&self) -> MessageFormat {
        self.format
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.events.subscribe()
    }

    pub(crate) fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }

    pub(crate) fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    pub(crate) fn max_ws_bytes(&self) -> usize {
        self.max_ws_bytes
    }

    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    /// 序列化一次，字符串广播给 WS，JSON 同时进入每个 POST 队列。
    pub fn publish(&self, event: Event) {
        let value = event.to_value(self.format);
        let text = match serde_json::to_string(&value) {
            Ok(text) => Arc::<str>::from(text),
            Err(_) => {
                tracing::warn!("cannot serialize an OB11 event");
                return;
            }
        };
        let value = Arc::new(value);
        let _ = self.events.send(text.clone());
        for (index, target) in self.post_targets.iter().enumerate() {
            let delivery = post::Delivery {
                text: text.clone(),
                value: value.clone(),
            };
            if let Err(error) = target.try_send(delivery) {
                self.post_queue_rejected.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    target = index,
                    reason = if matches!(error, mpsc::error::TrySendError::Full(_)) {
                        "queue full"
                    } else {
                        "closed"
                    },
                    "HTTP POST delivery queue rejected an event"
                );
            }
        }
    }

    pub fn delivery_stats(&self) -> EventDeliveryStats {
        let queued: usize = self
            .post_targets
            .iter()
            .map(|queue| queue.max_capacity().saturating_sub(queue.capacity()))
            .sum();
        EventDeliveryStats {
            http_post: HttpPostDeliveryStats {
                queued,
                queue_rejected: self.post_queue_rejected.load(Ordering::Relaxed),
                retry_exhausted: self.post_retry_exhausted.load(Ordering::Relaxed),
            },
        }
    }

    pub fn publish_lifecycle(&self, sub_type: LifecycleType) {
        self.publish(Event {
            time: crate::bridge::unix_now(),
            self_id: self.self_id(),
            kind: EventKind::MetaEvent(MetaEvent::Lifecycle { sub_type }),
        });
    }

    fn publish_heartbeat(&self) {
        let online = self.online.load(Ordering::Acquire);
        self.publish(Event {
            time: crate::bridge::unix_now(),
            self_id: self.self_id(),
            kind: EventKind::MetaEvent(MetaEvent::Heartbeat {
                status: BotStatus {
                    online: Some(online),
                    good: online,
                    extra: Default::default(),
                },
                interval: self.heartbeat_ms,
            }),
        });
    }

    /// 执行一个动作。异步与限速模式只负责入队。
    pub async fn dispatch(self: &Arc<Self>, call: Call) -> Response {
        if self.is_shutting_down() {
            return failure(ActionError::ShuttingDown);
        }
        let name = action_name(&call.action);
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let span = tracing::info_span!(
            "ob11_action",
            request_id,
            action = name,
            status = tracing::field::Empty,
            retcode = tracing::field::Empty,
            elapsed_ms = tracing::field::Empty
        );
        match call.mode {
            Mode::Sync => {
                self.clone()
                    .execute(call.action, span, Instant::now())
                    .await
            }
            Mode::Async | Mode::RateLimited => {
                let queue = if call.mode == Mode::Async {
                    &self.async_tx
                } else {
                    &self.rate_tx
                };
                let queued = Queued {
                    action: call.action,
                    span,
                    started: Instant::now(),
                };
                match queue.try_send(queued) {
                    Ok(()) => Response::accepted(),
                    Err(mpsc::error::TrySendError::Full(_)) => failure(ActionError::QueueFull),
                    Err(mpsc::error::TrySendError::Closed(_)) => failure(ActionError::ShuttingDown),
                }
            }
        }
    }

    async fn execute(
        self: Arc<Self>,
        action: Action,
        span: tracing::Span,
        started: Instant,
    ) -> Response {
        let span_for_log = span.clone();
        async move {
            let context = CallContext::new(self.delivery_stats());
            let result = self
                .handler
                .call(
                    Call {
                        action,
                        mode: Mode::Sync,
                    },
                    context,
                )
                .await;
            let response = match &result {
                Ok(ActionOutcome::Ok(data)) => Response::ok(data.clone()),
                Ok(ActionOutcome::Accepted) => Response::accepted(),
                Err(error) => failure(error.clone()),
            };
            span.record("status", status_name(&response));
            span.record("retcode", response.retcode);
            span.record("elapsed_ms", started.elapsed().as_millis() as u64);
            match result {
                Ok(_) => tracing::info!(retcode = response.retcode, "OB11 action completed"),
                Err(error) => tracing::warn!(
                    retcode = error.retcode(),
                    reason = error.reason(),
                    "OB11 action failed"
                ),
            }
            response
        }
        .instrument(span_for_log)
        .await
    }

    /// 用 HTTP POST 的事件上下文展开响应里的快速操作。
    pub async fn quick_operation(self: &Arc<Self>, context: &Value, operation: &Value) {
        let (Some(context), Some(operation)) = (context.as_object(), operation.as_object()) else {
            return;
        };
        let expanded = match quick_operation::expand(self.self_id(), context, operation) {
            Ok(expanded) => expanded,
            Err(error) => {
                tracing::debug!(
                    reason = %error,
                    "HTTP POST response did not contain a valid quick operation"
                );
                return;
            }
        };
        let mut calls = Vec::with_capacity(expanded.len());
        for (name, params) in expanded {
            match Call::parse(name, params) {
                Ok(call) => calls.push(call),
                Err(error) => {
                    tracing::warn!(
                        action = name,
                        reason = %error,
                        "quick operation contains an action that cannot be executed"
                    );
                    return;
                }
            }
        }
        for call in calls {
            let response = self.dispatch(call).await;
            if response.retcode != 0 && response.retcode != 1 {
                tracing::warn!(
                    retcode = response.retcode,
                    reason = response.reason.as_deref().unwrap_or_default(),
                    "quick operation failed"
                );
            }
        }
    }

    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.cancel.cancel();
    }
}

fn failure(error: ActionError) -> Response {
    Response::failed(error.retcode(), error.reason(), error.message())
}

fn status_name(response: &Response) -> &'static str {
    match response.status {
        Status::Ok => "ok",
        Status::Async => "async",
        Status::Failed => "failed",
    }
}

fn spawn_action_workers(
    hub: &Arc<Hub>,
    mut async_rx: mpsc::Receiver<Queued>,
    mut rate_rx: mpsc::Receiver<Queued>,
    rate_limit: Duration,
) {
    let weak = Arc::downgrade(hub);
    let cancel = hub.cancel.clone();
    let semaphore = Arc::new(Semaphore::new(ASYNC_CONCURRENCY));
    spawn_task(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                item = async_rx.recv() => {
                    let Some(item) = item else { break };
                    let Some(hub) = weak.upgrade() else { break };
                    let Ok(permit) = semaphore.clone().acquire_owned().await else { break };
                    tokio::spawn(async move {
                        let _permit = permit;
                        hub.execute(item.action, item.span, item.started).await;
                    });
                }
            }
        }
    });
    let weak = Arc::downgrade(hub);
    let cancel = hub.cancel.clone();
    spawn_task(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                item = rate_rx.recv() => {
                    let Some(item) = item else { break };
                    tokio::time::sleep(rate_limit).await;
                    let Some(hub) = weak.upgrade() else { break };
                    hub.execute(item.action, item.span, item.started).await;
                }
            }
        }
    });
}

fn spawn_heartbeat(hub: &Arc<Hub>) {
    let weak = Arc::downgrade(hub);
    let cancel = hub.cancel.clone();
    let interval = Duration::from_millis(hub.heartbeat_ms.max(1));
    spawn_task(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    let Some(hub) = weak.upgrade() else { break };
                    hub.publish_heartbeat();
                }
            }
        }
    });
}

fn spawn_task(future: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    }
}

pub(crate) fn action_name(action: &Action) -> &str {
    match action {
        Action::SendMsg(_) => "send_msg",
        Action::DeleteMsg(_) => "delete_msg",
        Action::GetMsg(_) => "get_msg",
        Action::GetImage(_) => "get_image",
        Action::GetGroupInfo(_) => "get_group_info",
        Action::GetGroupMemberInfo(_) => "get_group_member_info",
        Action::GetGroupMemberList(_) => "get_group_member_list",
        Action::SetGroupBan(_) => "set_group_ban",
        Action::SetGroupKick(_) => "set_group_kick",
        Action::SetGroupAddRequest(_) => "set_group_add_request",
        Action::GetLoginInfo => "get_login_info",
        Action::GetGroupList => "get_group_list",
        Action::GetFriendList => "get_friend_list",
        Action::GetOnlineClients => "get_online_clients",
        Action::GetStatus => "get_status",
        Action::GetVersionInfo => "get_version_info",
        Action::CanSendImage => "can_send_image",
        Action::CanSendRecord => "can_send_record",
        Action::CleanCache => "clean_cache",
        Action::SetRestart(_) => "set_restart",
        Action::HandleQuickOperation(_) => ".handle_quick_operation",
        Action::Extension { name, .. } => name,
    }
}
