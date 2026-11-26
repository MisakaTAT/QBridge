#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use qbridge::{bridge::Bridge, store::Store};
use qbridge_qq::Client;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, atomic::AtomicBool},
};

#[derive(Clone)]
pub struct FakeQq(Arc<Mutex<Inner>>);

struct Inner {
    responses: VecDeque<(u16, Option<Value>)>,
    by_path: HashMap<String, (u16, Value)>,
    requests: Vec<Recorded>,
}

#[derive(Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: Value,
    pub headers: HeaderMap,
}

impl FakeQq {
    pub fn respond_to(&self, path: &str, status: u16, body: Value) {
        self.0
            .lock()
            .unwrap()
            .by_path
            .insert(path.into(), (status, body));
    }

    pub fn respond(&self, status: u16, body: Value) {
        self.0
            .lock()
            .unwrap()
            .responses
            .push_back((status, Some(body)));
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.0.lock().unwrap().requests.clone()
    }

    pub fn take_requests(&self) -> Vec<Recorded> {
        std::mem::take(&mut self.0.lock().unwrap().requests)
    }
}

async fn handle(State(fake): State<FakeQq>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let method = request.method().as_str().to_owned();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    if path == "/token" {
        return json_response(
            200,
            Some(json!({"access_token": "TOKEN", "expires_in": 7200})),
        );
    }
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let response = {
        let mut state = fake.0.lock().unwrap();
        let specific = state.by_path.get(&path).cloned();
        state.requests.push(Recorded {
            method,
            path,
            query,
            body,
            headers,
        });
        specific
            .map(|(status, body)| (status, Some(body)))
            .or_else(|| state.responses.pop_front())
            .unwrap_or((
                404,
                Some(json!({"code": 404, "message": "missing fake response"})),
            ))
    };
    json_response(response.0, response.1)
}

fn json_response(status: u16, body: Option<Value>) -> Response {
    let status = StatusCode::from_u16(status).unwrap();
    match body {
        Some(body) => Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => Response::builder()
            .status(status)
            .body(Body::empty())
            .unwrap(),
    }
}

pub struct Service {
    pub fake: FakeQq,
    pub base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn service() -> Service {
    let fake = FakeQq(Arc::new(Mutex::new(Inner {
        responses: VecDeque::new(),
        by_path: HashMap::new(),
        requests: Vec::new(),
    })));
    let app = Router::new().fallback(any(handle)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Service {
        fake,
        base: format!("http://{address}"),
        task,
    }
}

impl Service {
    pub fn client(&self) -> Client {
        Client::with_endpoints(
            "1234",
            "SECRET",
            &self.base,
            &format!("{}/token", self.base),
        )
        .unwrap()
    }

    pub fn bridge(&self, store: Store) -> Bridge {
        Bridge::new(
            Arc::new(self.client()),
            store,
            10_000,
            Arc::new(AtomicBool::new(true)),
        )
    }
}

pub fn memory_store() -> Store {
    Store::open(std::path::Path::new(":memory:"), "app").unwrap()
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub fn message_event(scope: &str, target: i64, text: &str, time: i64) -> Value {
    match scope {
        "group" => json!({
            "time": time,
            "self_id": 10_000,
            "post_type": "message",
            "message_type": "group",
            "group_id": target,
            "sub_type": "normal",
            "anonymous": null,
            "message_id": 0,
            "user_id": 42,
            "message": [{"type": "text", "data": {"text": text}}],
            "raw_message": text,
            "font": 0,
            "sender": {"user_id": 42, "nickname": "Alice", "role": "member"}
        }),
        _ => json!({
            "time": time,
            "self_id": 10_000,
            "post_type": "message",
            "message_type": "private",
            "sub_type": "friend",
            "message_id": 0,
            "user_id": target,
            "message": [{"type": "text", "data": {"text": text}}],
            "raw_message": text,
            "font": 0,
            "sender": {"user_id": target, "nickname": "Alice"}
        }),
    }
}

use async_trait::async_trait;
use qbridge::{
    error::ActionError,
    server::{ActionOutcome, CallContext, Handler, Hub, HubOptions},
};
use qbridge_ob11::{
    Action, Call, Event, EventKind,
    event::{LifecycleType, MetaEvent, NoticeEvent},
};
use std::sync::atomic::Ordering;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub struct StubHandler {
    pub self_id: i64,
    calls: Mutex<Vec<String>>,
    block: AtomicBool,
    started: Notify,
    release: Notify,
}

impl StubHandler {
    pub fn new(self_id: i64) -> Self {
        Self {
            self_id,
            calls: Mutex::new(Vec::new()),
            block: AtomicBool::new(false),
            started: Notify::new(),
            release: Notify::new(),
        }
    }

    pub fn block(&self, blocked: bool) {
        self.block.store(blocked, Ordering::Release);
    }

    pub fn wait_started(&self) -> impl std::future::Future<Output = ()> + '_ {
        self.started.notified()
    }

    pub fn release(&self) {
        self.release.notify_one();
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl Handler for StubHandler {
    fn self_id(&self) -> i64 {
        self.self_id
    }

    async fn call(&self, call: Call, context: CallContext) -> Result<ActionOutcome, ActionError> {
        let name = match &call.action {
            Action::SendMsg(_) => "send_msg",
            Action::GetLoginInfo => "get_login_info",
            Action::GetStatus => "get_status",
            Action::Extension { name, .. } => name,
            _ => "other",
        };
        if self.block.load(Ordering::Acquire) {
            self.started.notify_one();
            self.release.notified().await;
        }
        self.calls.lock().unwrap().push(name.into());
        match call.action {
            Action::Extension { .. } => Err(ActionError::UnknownAction),
            Action::SetRestart(_) => Ok(ActionOutcome::Accepted),
            Action::GetStatus => Ok(ActionOutcome::Ok(json!({
                "ok": true,
                "event_delivery": context.event_delivery(),
            }))),
            _ => Ok(ActionOutcome::Ok(json!({"ok": true}))),
        }
    }
}

pub fn hub_options() -> HubOptions {
    HubOptions {
        heartbeat_ms: 0,
        ..HubOptions::default()
    }
}

pub struct HttpServer {
    pub addr: std::net::SocketAddr,
    pub cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

pub async fn start_http(hub: std::sync::Arc<Hub>) -> HttpServer {
    start_http_routes(hub, qbridge::server::http::Routes::BOTH).await
}

pub async fn start_http_routes(
    hub: std::sync::Arc<Hub>,
    routes: qbridge::server::http::Routes,
) -> HttpServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cancel = CancellationToken::new();
    let server_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        let _ = qbridge::server::http::serve_listener(hub, listener, server_cancel, routes).await;
    });
    HttpServer { addr, cancel, task }
}

pub fn sample_event() -> Event {
    Event {
        time: 1,
        self_id: 10_000,
        kind: EventKind::Notice(NoticeEvent::custom("test_notice", 7)),
    }
}

pub fn lifecycle_event(sub_type: LifecycleType) -> Event {
    Event {
        time: 1,
        self_id: 10_000,
        kind: EventKind::MetaEvent(MetaEvent::Lifecycle { sub_type }),
    }
}
