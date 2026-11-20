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

pub async fn run(hub: Arc<Hub>, role: Role, mut ws: Ws, cancel: CancellationToken) {
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
                handle_request(
                    &hub,
                    &text,
                    outbox_tx.clone(),
                    semaphore.clone(),
                    &mut tasks,
                );
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
            let outbox = outbox.clone();
            tokio::spawn(async move {
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
