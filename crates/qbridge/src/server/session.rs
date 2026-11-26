//! 正向与反向 WebSocket 共用的会话循环。
//!
//! 每个请求单独开任务执行（每连接最多 64 个并发），结果经 mpsc 交给
//! 同一个写入循环；同时把广播事件推送给连接。

use super::{Hub, ws::Ws};
use crate::error::ActionError;
use qbridge_ob11::{
    Call, Event, EventKind, ParseError, Response,
    event::{LifecycleType, MetaEvent},
};
use serde_json::Value;
use std::sync::Arc;
use tokio::{
    sync::{Semaphore, broadcast, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

const CONCURRENT_REQUESTS: usize = 64;
const OUTBOX_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Universal,
    Api,
    Event,
}

impl Role {
    pub fn header(self) -> &'static str {
        match self {
            Self::Universal => "Universal",
            Self::Api => "API",
            Self::Event => "Event",
        }
    }

    fn accepts_events(self) -> bool {
        matches!(self, Self::Universal | Self::Event)
    }

    fn accepts_actions(self) -> bool {
        matches!(self, Self::Universal | Self::Api)
    }
}

pub async fn run(hub: Arc<Hub>, role: Role, ws: Ws, cancel: CancellationToken) {
    run_with_observer(hub, role, ws, cancel, |_| {}).await;
}

async fn run_with_observer(
    hub: Arc<Hub>,
    role: Role,
    mut ws: Ws,
    cancel: CancellationToken,
    mut on_tasks_len: impl FnMut(usize),
) {
    let connect = Event {
        time: crate::bridge::unix_now(),
        self_id: hub.self_id(),
        kind: EventKind::MetaEvent(MetaEvent::Lifecycle {
            sub_type: LifecycleType::Connect,
        }),
    };
    let text = serde_json::to_string(&connect.to_value(hub.message_format()))
        .expect("meta events serialize to JSON");
    if !ws.send_text(&text).await {
        return;
    }
    let mut events = role.accepts_events().then(|| hub.subscribe());
    let (outbox_tx, mut outbox_rx) = mpsc::channel::<String>(OUTBOX_CAPACITY);
    let semaphore = Arc::new(Semaphore::new(CONCURRENT_REQUESTS));
    let mut tasks = JoinSet::new();
    let shutdown = hub.cancellation();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = shutdown.cancelled() => break,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                log_task_result(result);
                drain_finished_tasks(&mut tasks);
                on_tasks_len(tasks.len());
            }
            incoming = ws.next_text() => {
                let Some(text) = incoming else { break };
                if !role.accepts_actions() {
                    let response = failure(ActionError::UnknownAction);
                    let response = serde_json::to_string(&response).unwrap_or_default();
                    if outbox_tx.send(response).await.is_err() {
                        break;
                    }
                    continue;
                }
                drain_finished_tasks(&mut tasks);
                handle_request(
                    &hub,
                    &text,
                    outbox_tx.clone(),
                    semaphore.clone(),
                    &mut tasks,
                );
                on_tasks_len(tasks.len());
            }
            Some(text) = outbox_rx.recv() => {
                if !ws.send_text(&text).await {
                    break;
                }
            }
            event = next_event(&mut events), if role.accepts_events() => {
                match event {
                    EventResult::Text(text) => {
                        if !ws.send_text(&text).await {
                            break;
                        }
                    }
                    EventResult::Lagged => {
                        tracing::warn!("OB11 WebSocket client fell behind the event broadcast; closing the connection");
                        break;
                    }
                    EventResult::Closed => break,
                    EventResult::Pending => {}
                }
            }
        }
    }
    tasks.abort_all();
    ws.close().await;
}

fn handle_request(
    hub: &Arc<Hub>,
    text: &str,
    outbox: mpsc::Sender<String>,
    semaphore: Arc<Semaphore>,
    tasks: &mut JoinSet<()>,
) {
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => {
            tracing::warn!("OB11 WebSocket request is not valid JSON");
            return;
        }
    };
    let Some(object) = payload.as_object() else {
        tracing::warn!("OB11 WebSocket request must be an object");
        return;
    };
    let Some(action) = object.get("action").and_then(Value::as_str) else {
        tracing::warn!("OB11 WebSocket request omitted its action");
        return;
    };
    let params = match object.get("params") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => {
            tracing::warn!("OB11 WebSocket request params must be an object");
            return;
        }
    };
    let echo = object.get("echo").cloned();
    let call = match Call::parse(action, params) {
        Ok(call) => call,
        Err(error) => {
            let response = with_echo(parse_failure(error), echo);
            let text = serde_json::to_string(&response).unwrap_or_default();
            tasks.spawn(async move {
                let _ = outbox.send(text).await;
            });
            return;
        }
    };
    let hub = hub.clone();
    let semaphore = semaphore.clone();
    tasks.spawn(async move {
        let Ok(_permit) = semaphore.acquire_owned().await else {
            return;
        };
        let response = with_echo(hub.dispatch(call).await, echo);
        let text = serde_json::to_string(&response).unwrap_or_default();
        let _ = outbox.send(text).await;
    });
}

fn drain_finished_tasks(tasks: &mut JoinSet<()>) {
    while let Some(result) = tasks.try_join_next() {
        log_task_result(result);
    }
}

fn log_task_result(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result
        && error.is_panic()
    {
        tracing::warn!("OB11 WebSocket request task panicked");
    }
}

fn parse_failure(error: ParseError) -> Response {
    let error = ActionError::from(error);
    failure(error)
}

fn failure(error: ActionError) -> Response {
    Response::failed(error.retcode(), error.reason(), error.message())
}

fn with_echo(response: Response, echo: Option<Value>) -> Response {
    match echo {
        Some(echo) => response.with_echo(Some(echo)),
        None => response,
    }
}

enum EventResult {
    Text(Arc<str>),
    Lagged,
    Closed,
    Pending,
}

async fn next_event(events: &mut Option<broadcast::Receiver<Arc<str>>>) -> EventResult {
    let Some(receiver) = events else {
        std::future::pending::<()>().await;
        return EventResult::Pending;
    };
    match receiver.recv().await {
        Ok(text) => EventResult::Text(text),
        Err(broadcast::error::RecvError::Lagged(_)) => EventResult::Lagged,
        Err(broadcast::error::RecvError::Closed) => EventResult::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{ActionOutcome, CallContext, Handler, HubOptions};
    use async_trait::async_trait;
    use axum::{Router, extract::ws::WebSocketUpgrade, routing::get};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    struct OkHandler;

    #[async_trait]
    impl Handler for OkHandler {
        fn self_id(&self) -> i64 {
            10_000
        }

        async fn call(
            &self,
            _call: Call,
            _context: CallContext,
        ) -> Result<ActionOutcome, ActionError> {
            Ok(ActionOutcome::Ok(json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn reaps_completed_request_tasks_while_session_stays_open() {
        let hub = Hub::new(
            Arc::new(OkHandler),
            HubOptions {
                heartbeat_ms: 0,
                ..HubOptions::default()
            },
        );
        let active_tasks = Arc::new(AtomicUsize::new(usize::MAX));
        let server_hub = hub.clone();
        let server_active = active_tasks.clone();
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let app = Router::new().route(
            "/ws",
            get(move |ws: WebSocketUpgrade| {
                let hub = server_hub.clone();
                let active = server_active.clone();
                let cancel = server_cancel.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        run_with_observer(
                            hub,
                            Role::Universal,
                            Ws::Forward(Box::new(socket)),
                            cancel,
                            move |len| active.store(len, Ordering::Release),
                        )
                        .await;
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let _connect = socket.next().await.unwrap().unwrap();

        for index in 0..8 {
            let payload = if index % 2 == 0 {
                json!({"action": "get_login_info", "params": {}, "echo": index})
            } else {
                json!({"action": "send_msg", "params": {}, "echo": index})
            };
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    payload.to_string().into(),
                ))
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(2), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(
                reply,
                tokio_tungstenite::tungstenite::Message::Text(_)
            ));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while active_tasks.load(Ordering::Acquire) != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "completed tasks were not reaped; remaining={}",
                active_tasks.load(Ordering::Acquire)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        cancel.cancel();
        hub.shutdown();
        server.abort();
    }
}
