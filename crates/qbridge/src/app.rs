//! 应用组装、事件泵与进程内重启。

use crate::{
    bridge::Bridge,
    config::ResolvedConfig,
    error::ActionError,
    server::{self, Handler, Hub, HubOptions, reverse::ReverseConfig},
    store::Store,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use qbridge_ob11::event::LifecycleType;
use qbridge_ob11::{Action, Call};
use qbridge_qq::{
    Client, Envelope, GatewayConfig, SessionStore, Webhook, WebhookConfig,
    gateway::{GatewayAcknowledgement, GatewayStream},
    webhook::WebhookStream,
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_util::sync::CancellationToken;

pub type AppError = Box<dyn std::error::Error + Send + Sync>;

/// 测试与私有部署可覆盖 QQ API 地址。
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    pub api_base: Option<String>,
    pub token_url: Option<String>,
}

/// 加载配置并运行，直到收到退出信号。`set_restart` 会在进程内重新组装。
pub async fn run(config_path: PathBuf) -> Result<(), AppError> {
    run_with_endpoints(config_path, Endpoints::default()).await
}

pub async fn run_with_endpoints(
    config_path: PathBuf,
    endpoints: Endpoints,
) -> Result<(), AppError> {
    loop {
        let config = crate::config::Config::load(&config_path)?.resolve()?;
        match run_once(&config, &endpoints, None).await? {
            Outcome::Stop => return Ok(()),
            Outcome::Restart(delay) => {
                tracing::info!(delay_ms = delay, "restarting QBridge in process");
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
    }
}

/// 可由调用方取消的运行入口，用于端到端测试或嵌入其它运行时。
pub async fn run_until(
    config_path: PathBuf,
    endpoints: Endpoints,
    stop: CancellationToken,
) -> Result<(), AppError> {
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let config = crate::config::Config::load(&config_path)?.resolve()?;
        match run_once(&config, &endpoints, Some(&stop)).await? {
            Outcome::Stop => return Ok(()),
            Outcome::Restart(delay) => {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
        }
    }
}

enum Outcome {
    Stop,
    Restart(u64),
}

struct AppHandler {
    bridge: Arc<Bridge>,
    restart: mpsc::Sender<u64>,
}

#[async_trait]
impl Handler for AppHandler {
    fn self_id(&self) -> i64 {
        self.bridge.self_id()
    }

    async fn call(&self, call: Call) -> Result<Value, ActionError> {
        match call.action {
            Action::SetRestart(request) => {
                self.restart
                    .try_send(request.delay)
                    .map_err(|_| ActionError::ShuttingDown)?;
                Ok(Value::Null)
            }
            action => self.bridge.execute(action).await,
        }
    }
}

enum Source {
    Gateway(GatewayStream),
    Webhook(WebhookStream),
}

impl Source {
    fn online(&self) -> Arc<AtomicBool> {
        match self {
            Self::Gateway(stream) => stream.online(),
            Self::Webhook(stream) => stream.online(),
        }
    }

    async fn next(&mut self) -> Option<(Envelope, Option<GatewayAcknowledgement>)> {
        match self {
            Self::Gateway(stream) => stream.next_dispatch().await,
            Self::Webhook(stream) => stream.next().await.map(|envelope| (envelope, None)),
        }
    }
}

async fn run_once(
    config: &ResolvedConfig,
    endpoints: &Endpoints,
    stop: Option<&CancellationToken>,
) -> Result<Outcome, AppError> {
    let store = Store::open_with_identity(
        &config.storage_path,
        &config.qq.app_id,
        config.identity.clone(),
    )?;
    let qq = Arc::new(match (&endpoints.api_base, &endpoints.token_url) {
        (Some(api_base), Some(token_url)) => {
            Client::with_endpoints(&config.qq.app_id, &config.qq.secret, api_base, token_url)?
        }
        (None, None) => Client::new(&config.qq.app_id, &config.qq.secret)?,
        _ => return Err(std::io::Error::other("both endpoint overrides are required").into()),
    });
    let source = create_source(&config.qq, &config.limits, qq.clone(), &store).await?;
    let online = source.online();
    let bridge = Arc::new(Bridge::new(
        qq.clone(),
        store.clone(),
        config.self_id,
        online.clone(),
    ));
    let (restart_tx, mut restart_rx) = mpsc::channel::<u64>(1);
    let handler = Arc::new(AppHandler {
        bridge: bridge.clone(),
        restart: restart_tx,
    });
    let hub = Hub::new(
        handler,
        HubOptions {
            message_format: config.message_format,
            access_token: config.access_token.clone(),
            max_body_bytes: config.limits.max_body_bytes,
            max_ws_bytes: config.limits.max_ws_bytes,
            queue_capacity: config.limits.queue_capacity,
            rate_limit: config.limits.rate_limit,
            heartbeat_ms: config.heartbeat_ms,
            online: online.clone(),
            http_posts: config
                .http_post
                .iter()
                .map(|target| target.target())
                .collect(),
        },
    );
    let cancel = CancellationToken::new();
    let mut listener_configs = Vec::new();
    match (config.http_bind, config.forward_ws_bind) {
        (Some(http), Some(forward)) if http == forward => {
            listener_configs.push((http, server::http::Routes::BOTH));
        }
        (Some(http), Some(forward)) => {
            listener_configs.push((http, server::http::Routes::HTTP));
            listener_configs.push((forward, server::http::Routes::FORWARD_WS));
        }
        (Some(http), None) => listener_configs.push((http, server::http::Routes::HTTP)),
        (None, Some(forward)) => {
            listener_configs.push((forward, server::http::Routes::FORWARD_WS));
        }
        (None, None) => {
            return Err(std::io::Error::other("no OB11 listener configured").into());
        }
    }
    let mut listeners = Vec::new();
    for (bind, routes) in listener_configs {
        listeners.push((TcpListener::bind(bind).await?, routes));
    }
    let (http_failure_tx, mut http_failure_rx) = mpsc::channel::<String>(1);
    let mut http_tasks = Vec::new();
    for (listener, routes) in listeners {
        let hub = hub.clone();
        let cancel = cancel.clone();
        let failure = http_failure_tx.clone();
        http_tasks.push(tokio::spawn(async move {
            if let Err(error) = server::http::serve_listener(hub, listener, cancel, routes).await {
                let _ = failure.send(format!("OB11 listener failed: {error}")).await;
            }
        }));
    }
    drop(http_failure_tx);
    let mut reverse_tasks = Vec::new();
    for target in &config.reverse_ws {
        reverse_tasks.push(server::reverse::spawn(
            hub.clone(),
            ReverseConfig {
                url: target.url.clone(),
                role: target.role,
                reconnect: target.reconnect,
            },
        ));
    }
    let (source_end_tx, mut source_end_rx) = mpsc::channel::<()>(1);
    let pump_hub = hub.clone();
    let pump_bridge = bridge.clone();
    let pump_cancel = cancel.clone();
    let source_task = tokio::spawn(async move {
        let hub_cancel = pump_hub.cancellation();
        let mut source = source;
        loop {
            tokio::select! {
                _ = pump_cancel.cancelled() => break,
                _ = hub_cancel.cancelled() => break,
                envelope = source.next() => {
                    let Some((envelope, acknowledgement)) = envelope else {
                        let _ = source_end_tx.try_send(());
                        break;
                    };
                    if let Some(event) = pump_bridge.translate(envelope).await {
                        pump_hub.publish(event);
                    }
                    if let Some(acknowledgement) = acknowledgement {
                        acknowledgement.acknowledge().await;
                    }
                }
            }
        }
    });
    let cleanup_store = store.clone();
    let cleanup_cancel = cancel.clone();
    let cleanup_hub = hub.clone();
    let cleanup_task = tokio::spawn(async move {
        let hub_cancel = cleanup_hub.cancellation();
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cleanup_cancel.cancelled() => break,
                _ = hub_cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if let Err(error) = cleanup_store.clean_expired(crate::bridge::unix_now()).await {
                        tracing::warn!(reason = %error, "expired message cleanup failed");
                    }
                }
            }
        }
    });
    hub.publish_lifecycle(LifecycleType::Enable);

    let outcome = tokio::select! {
        _ = shutdown_signal() => Ok(Outcome::Stop),
        _ = wait_for_stop(stop) => Ok(Outcome::Stop),
        delay = restart_rx.recv() => Ok(Outcome::Restart(delay.unwrap_or(0))),
        _ = source_end_rx.recv() => Err("QQ event source terminated unexpectedly".to_string()),
        failure = http_failure_rx.recv() => Err(failure
            .unwrap_or_else(|| "OB11 HTTP server stopped unexpectedly".to_string())),
    };

    hub.publish_lifecycle(LifecycleType::Disable);
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    hub.shutdown();
    let shutdown = async {
        for task in http_tasks {
            let _ = task.await;
        }
        for task in reverse_tasks {
            task.abort();
            let _ = task.await;
        }
        source_task.abort();
        let _ = source_task.await;
        cleanup_task.abort();
        let _ = cleanup_task.await;
    };
    if tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .is_err()
    {
        tracing::warn!("timed out waiting for tasks to stop");
    }
    drop(hub);
    drop(bridge);
    drop(store);
    match outcome {
        Ok(outcome) => Ok(outcome),
        Err(error) => Err(std::io::Error::other(error).into()),
    }
}

async fn create_source(
    qq_config: &crate::config::ResolvedQq,
    limits: &crate::config::ResolvedLimits,
    qq: Arc<Client>,
    store: &Store,
) -> Result<Source, AppError> {
    match qq_config.source {
        crate::config::QqSource::Gateway => {
            let session_store: Arc<dyn SessionStore> = Arc::new(store.clone());
            let stream = qbridge_qq::gateway::connect(
                qq,
                GatewayConfig {
                    intents: qq_config.intents,
                    event_capacity: limits.queue_capacity,
                    ..GatewayConfig::default()
                },
                session_store,
            );
            Ok(Source::Gateway(stream))
        }
        crate::config::QqSource::Webhook => {
            let webhook = qq_config
                .webhook
                .as_ref()
                .ok_or_else(|| std::io::Error::other("webhook source requires qq.webhook"))?;
            let stream = Webhook::new(&qq_config.secret)?
                .serve(
                    WebhookConfig {
                        bind: webhook.bind,
                        path: webhook.path.clone(),
                    },
                    limits.queue_capacity,
                )
                .await?;
            Ok(Source::Webhook(stream))
        }
    }
}

async fn wait_for_stop(stop: Option<&CancellationToken>) {
    match stop {
        Some(stop) => stop.cancelled().await,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
