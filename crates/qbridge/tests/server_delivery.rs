mod common;

use axum::{
    Router,
    body::Body,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::{any, get},
};
use common::*;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use qbridge::server::{Hub, PostTargetConfig, reverse::ReverseConfig, session::Role};
use qbridge_ob11::{
    Event, EventKind, Message as Ob11Message,
    event::{MessageEvent, MessageScene, PrivateSubType, Sender},
};
use serde_json::{Value, json};
use sha1::Sha1;
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;

fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha1={hex}")
}

#[derive(Clone)]
struct Receiver(Arc<Mutex<ReceiverState>>);

struct ReceiverState {
    responses: VecDeque<(u16, Option<Value>)>,
    requests: Vec<(HeaderMap, Vec<u8>)>,
}

async fn receiver_handler(
    State(state): State<Receiver>,
    request: axum::extract::Request<Body>,
) -> Response {
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default()
        .to_vec();
    let response = {
        let mut state = state.0.lock().unwrap();
        state.requests.push((headers, body));
        state.responses.pop_front().unwrap_or((204, None))
    };
    let status = StatusCode::from_u16(response.0).unwrap();
    match response.1 {
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

async fn start_receiver() -> (Receiver, SocketAddr, tokio::task::JoinHandle<()>) {
    let state = Receiver(Arc::new(Mutex::new(ReceiverState {
        responses: VecDeque::new(),
        requests: Vec::new(),
    })));
    let app = Router::new()
        .fallback(any(receiver_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, addr, task)
}

async fn wait_for(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    condition()
}

#[tokio::test]
async fn status_reports_full_post_queues_and_rejected_events() {
    let mut options = hub_options();
    options.queue_capacity = 1;
    options.http_posts = vec![PostTargetConfig {
        url: url::Url::parse("http://127.0.0.1:1/post").unwrap(),
        secret: None,
        timeout: Duration::from_secs(1),
    }];
    let hub = Hub::new(Arc::new(StubHandler::new(10_000)), options);
    hub.publish(message_context());
    hub.publish(message_context());
    let status = hub
        .dispatch(qbridge_ob11::Call::parse("get_status", Default::default()).unwrap())
        .await;
    assert_eq!(status.data["event_delivery"]["http_post"]["queued"], 1);
    assert_eq!(
        status.data["event_delivery"]["http_post"]["queue_rejected"],
        1
    );
    assert_eq!(
        status.data["event_delivery"]["http_post"]["retry_exhausted"],
        0
    );
    hub.shutdown();
}

#[tokio::test]
async fn http_post_signs_retries_and_executes_quick_operations() {
    let (receiver, addr, task) = start_receiver().await;
    {
        let mut state = receiver.0.lock().unwrap();
        state
            .responses
            .push_back((500, Some(json!({"reply": "ignored"}))));
        state
            .responses
            .push_back((200, Some(json!({"reply": "pong"}))));
    }
    let handler = Arc::new(StubHandler::new(10_000));
    let mut options = hub_options();
    options.http_posts = vec![PostTargetConfig {
        url: url::Url::parse(&format!("http://{addr}/post")).unwrap(),
        secret: Some("SECRET".into()),
        timeout: Duration::from_secs(1),
    }];
    let hub = Hub::new(handler.clone(), options);
    hub.publish(message_context());
    assert!(
        wait_for(
            || receiver.0.lock().unwrap().requests.len() >= 2,
            Duration::from_secs(4)
        )
        .await
    );
    let requests = receiver.0.lock().unwrap().requests.clone();
    assert_eq!(requests.len(), 2);
    for (headers, body) in &requests {
        assert_eq!(headers.get("x-self-id").unwrap(), "10000");
        assert_eq!(
            headers.get("x-signature").unwrap().to_str().unwrap(),
            signature("SECRET", body)
        );
        let value: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(value["message_type"], "private");
    }
    assert!(
        wait_for(|| handler.call_count() == 1, Duration::from_secs(1)).await,
        "the successful quick operation should run exactly once"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(handler.call_count(), 1);
    task.abort();
}

#[derive(Clone)]
struct ReverseState {
    frames: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Option<HeaderMap>>>,
    commands: broadcast::Sender<Value>,
}

async fn reverse_handler(
    headers: HeaderMap,
    State(state): State<ReverseState>,
    ws: WebSocketUpgrade,
) -> Response {
    *state.headers.lock().unwrap() = Some(headers);
    ws.on_upgrade(move |socket| reverse_session(socket, state))
}

async fn reverse_session(mut socket: WebSocket, state: ReverseState) {
    let mut commands = state.commands.subscribe();
    loop {
        tokio::select! {
            incoming = socket.next() => {
                let Some(Ok(Message::Text(text))) = incoming else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    state.frames.lock().unwrap().push(value);
                }
            }
            command = commands.recv() => {
                let Ok(value) = command else { break };
                if socket
                    .send(Message::Text(value.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

#[tokio::test]
async fn reverse_websocket_connects_with_headers_and_handles_both_directions() {
    let state = ReverseState {
        frames: Arc::new(Mutex::new(Vec::new())),
        headers: Arc::new(Mutex::new(None)),
        commands: broadcast::channel(8).0,
    };
    let app = Router::new()
        .route("/ws", get(reverse_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let reverse = qbridge::server::reverse::spawn(
        hub.clone(),
        ReverseConfig {
            url: url::Url::parse(&format!("ws://{addr}/ws")).unwrap(),
            role: Role::Universal,
            reconnect: Duration::from_millis(50),
        },
    );
    assert!(
        wait_for(
            || {
                state
                    .frames
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|frame| frame["meta_event_type"] == "lifecycle")
            },
            Duration::from_secs(2)
        )
        .await
    );
    let headers = state.headers.lock().unwrap().clone().unwrap();
    assert_eq!(headers.get("x-self-id").unwrap(), "10000");
    assert_eq!(headers.get("x-client-role").unwrap(), "Universal");

    state
        .commands
        .send(json!({"action": "get_login_info", "params": {}, "echo": "r"}))
        .unwrap();
    assert!(
        wait_for(
            || {
                state
                    .frames
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|frame| frame["echo"] == "r" && frame["retcode"] == 0)
            },
            Duration::from_secs(2)
        )
        .await
    );

    hub.publish(sample_event());
    assert!(
        wait_for(
            || {
                state
                    .frames
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|frame| frame["notice_type"] == "test_notice")
            },
            Duration::from_secs(2)
        )
        .await
    );
    hub.shutdown();
    reverse.abort();
    task.abort();
}

fn message_context() -> Event {
    Event {
        time: 1,
        self_id: 10_000,
        kind: EventKind::Message(MessageEvent::new(
            MessageScene::Private {
                sub_type: PrivateSubType::Friend,
            },
            1,
            42,
            Ob11Message::text("hello"),
            Sender {
                user_id: 42,
                nickname: Some("Alice".into()),
                card: None,
                role: None,
            },
        )),
    }
}
