use async_trait::async_trait;
use axum::{
    Router,
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
    routing::{get, post},
};
use futures_util::StreamExt;
use qbridge_qq::{
    Client, Dispatch, Envelope, Error, GatewayConfig, GatewayStream, Intents, Session,
    SessionStore, gateway::connect,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct Script {
    heartbeat_ms: u64,
    before_auth: Vec<Value>,
    after_auth: Vec<Value>,
    close_after_auth: Option<u16>,
}

#[derive(Clone)]
struct FakeGateway(Arc<Mutex<Inner>>);

struct Inner {
    scripts: VecDeque<Script>,
    received: Vec<Value>,
    connections: usize,
    ws_url: String,
}

impl FakeGateway {
    fn push(&self, script: Script) {
        self.0.lock().unwrap().scripts.push_back(script);
    }

    fn received(&self) -> Vec<Value> {
        self.0.lock().unwrap().received.clone()
    }

    fn connections(&self) -> usize {
        self.0.lock().unwrap().connections
    }
}

async fn start() -> FakeGateway {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fake = FakeGateway(Arc::new(Mutex::new(Inner {
        scripts: VecDeque::new(),
        received: Vec::new(),
        connections: 0,
        ws_url: format!("ws://{address}/ws"),
    })));
    let app = Router::new()
        .route(
            "/gateway",
            get(|State(fake): State<FakeGateway>| async move {
                let url = fake.0.lock().unwrap().ws_url.clone();
                axum::Json(json!({"url": url}))
            }),
        )
        .route(
            "/token",
            post(|| async { axum::Json(json!({"access_token": "TOKEN", "expires_in": 7200})) }),
        )
        .route("/ws", get(ws_handler))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    fake
}

async fn ws_handler(ws: WebSocketUpgrade, State(fake): State<FakeGateway>) -> Response {
    ws.on_upgrade(move |socket| run_socket(socket, fake))
}

async fn run_socket(mut socket: WebSocket, fake: FakeGateway) {
    let script = {
        let mut state = fake.0.lock().unwrap();
        state.connections += 1;
        state.scripts.pop_front().unwrap_or_default()
    };
    let heartbeat = if script.heartbeat_ms == 0 {
        60_000
    } else {
        script.heartbeat_ms
    };
    if socket
        .send(Message::Text(
            json!({"op": 10, "d": {"heartbeat_interval": heartbeat}})
                .to_string()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    for frame in &script.before_auth {
        if socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    let mut authenticated = false;
    loop {
        let incoming = match socket.next().await {
            Some(Ok(message)) => message,
            _ => break,
        };
        match incoming {
            Message::Text(text) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let op = value.get("op").and_then(Value::as_u64);
                fake.0.lock().unwrap().received.push(value.clone());
                match op {
                    Some(1) => {
                        if socket
                            .send(Message::Text(json!({"op": 11}).to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(2 | 6) if !authenticated => {
                        authenticated = true;
                        for frame in &script.after_auth {
                            if socket
                                .send(Message::Text(frame.to_string().into()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        if let Some(code) = script.close_after_auth {
                            let _ = socket
                                .send(Message::Close(Some(CloseFrame {
                                    code,
                                    reason: "".into(),
                                })))
                                .await;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            Message::Ping(payload) => {
                let _ = socket.send(Message::Pong(payload)).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

#[derive(Default)]
struct Store {
    session: Mutex<Option<Session>>,
    saves: AtomicUsize,
    clears: AtomicUsize,
}

#[async_trait]
impl SessionStore for Store {
    async fn load(&self) -> Result<Option<Session>, Error> {
        Ok(self.session.lock().unwrap().clone())
    }

    async fn save(&self, session: &Session) -> Result<(), Error> {
        self.saves.fetch_add(1, Ordering::Relaxed);
        *self.session.lock().unwrap() = Some(session.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), Error> {
        self.clears.fetch_add(1, Ordering::Relaxed);
        *self.session.lock().unwrap() = None;
        Ok(())
    }
}

impl Store {
    fn with_session(session: Session) -> Self {
        Self {
            session: Mutex::new(Some(session)),
            ..Self::default()
        }
    }

    fn session(&self) -> Option<Session> {
        self.session.lock().unwrap().clone()
    }

    fn clears(&self) -> usize {
        self.clears.load(Ordering::Relaxed)
    }
}

struct Harness {
    stream: GatewayStream,
}

impl Harness {
    fn new(fake: FakeGateway, store: Arc<Store>, config: GatewayConfig) -> Self {
        let base = fake
            .0
            .lock()
            .unwrap()
            .ws_url
            .strip_suffix("/ws")
            .unwrap()
            .replace("ws://", "http://");
        let client =
            Client::with_endpoints("1234", "SECRET", &base, &format!("{base}/token")).unwrap();
        let stream = connect(Arc::new(client), config, store);
        Self { stream }
    }

    fn default_config() -> GatewayConfig {
        GatewayConfig {
            intents: Intents::GROUP_AND_C2C,
            reconnect_initial: Duration::from_millis(10),
            reconnect_max: Duration::from_millis(50),
            ready_timeout: Duration::from_secs(2),
            event_capacity: 16,
        }
    }
}

async fn next(stream: &mut GatewayStream) -> Option<Envelope> {
    tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for a gateway event")
}

fn ready(session_id: &str, seq: u64) -> Value {
    json!({
        "op": 0,
        "s": seq,
        "t": "READY",
        "d": {"session_id": session_id, "user": {"username": "bot"}}
    })
}

fn resumed(seq: u64) -> Value {
    json!({"op": 0, "s": seq, "t": "RESUMED", "d": {}})
}

fn group_message(seq: u64, content: &str) -> Value {
    json!({
        "op": 0,
        "s": seq,
        "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": format!("M{seq}"),
            "content": content,
            "group_openid": "G1",
            "timestamp": "2026-01-01T00:00:00Z",
            "author": {"member_openid": "U1"}
        }
    })
}

fn received_ops(fake: &FakeGateway) -> Vec<u64> {
    fake.received()
        .iter()
        .filter_map(|value| value.get("op").and_then(Value::as_u64))
        .collect()
}

#[tokio::test]
async fn identifies_and_forwards_dispatches() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![ready("S1", 1), group_message(2, "hello")],
        ..Script::default()
    });
    let store = Arc::new(Store::default());
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());

    let first = next(&mut harness.stream).await.unwrap();
    assert!(matches!(first.dispatch, Dispatch::Unknown { ref t, .. } if t == "READY"));
    let second = next(&mut harness.stream).await.unwrap();
    let Dispatch::GroupAtMessage(message) = second.dispatch else {
        panic!("unexpected dispatch: {:?}", second.dispatch)
    };
    assert_eq!(message.content, "hello");

    let messages = fake.received();
    assert_eq!(messages[0]["op"], 2);
    assert_eq!(messages[0]["d"]["token"], "QQBot TOKEN");
    assert_eq!(
        messages[0]["d"]["intents"],
        json!(Intents::GROUP_AND_C2C.bits())
    );
    assert_eq!(messages[0]["d"]["shard"], json!([0, 1]));

    let session = store.session().unwrap();
    assert_eq!(session.session_id, "S1");
    assert_eq!(session.seq, 2);
    assert!(harness.stream.online().load(Ordering::Acquire));
}

#[tokio::test]
async fn starts_a_new_session_when_intents_change() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![ready("S2", 1)],
        ..Script::default()
    });
    let store = Arc::new(Store::with_session(Session {
        session_id: "OLD".into(),
        seq: 5,
        intents: 0,
    }));
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());

    let first = next(&mut harness.stream).await.unwrap();
    assert!(matches!(first.dispatch, Dispatch::Unknown { ref t, .. } if t == "READY"));
    let messages = fake.received();
    assert_eq!(messages[0]["op"], 2);
    assert_eq!(
        messages[0]["d"]["intents"],
        json!(Intents::GROUP_AND_C2C.bits())
    );
    let session = store.session().unwrap();
    assert_eq!(session.session_id, "S2");
    assert_eq!(session.intents, Intents::GROUP_AND_C2C.bits());
}

#[tokio::test]
async fn resumes_persisted_session() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![resumed(6), group_message(7, "again")],
        ..Script::default()
    });
    let store = Arc::new(Store::with_session(Session {
        session_id: "SAVED".into(),
        seq: 5,
        intents: Intents::GROUP_AND_C2C.bits(),
    }));
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());

    let first = next(&mut harness.stream).await.unwrap();
    assert!(matches!(first.dispatch, Dispatch::Unknown { ref t, .. } if t == "RESUMED"));
    let second = next(&mut harness.stream).await.unwrap();
    assert!(matches!(second.dispatch, Dispatch::GroupAtMessage(_)));

    let messages = fake.received();
    assert_eq!(messages[0]["op"], 6);
    assert_eq!(messages[0]["d"]["session_id"], "SAVED");
    assert_eq!(messages[0]["d"]["seq"], 5);
    assert_eq!(store.session().unwrap().seq, 7);
}

#[tokio::test]
async fn keeps_buffered_dispatches_out_of_the_persisted_resume_checkpoint() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![group_message(6, "queued")],
        close_after_auth: Some(1000),
        ..Script::default()
    });
    let store = Arc::new(Store::with_session(Session {
        session_id: "SAVED".into(),
        seq: 5,
        intents: Intents::GROUP_AND_C2C.bits(),
    }));
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());
    tokio::time::timeout(Duration::from_secs(2), async {
        while received_ops(&fake).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fake.received()[1]["d"]["seq"], 6);
    assert_eq!(store.session().unwrap().seq, 5);
    let (dispatch, acknowledgement) =
        tokio::time::timeout(Duration::from_secs(2), harness.stream.next_dispatch())
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(dispatch.dispatch, Dispatch::GroupAtMessage(_)));
    assert_eq!(store.session().unwrap().seq, 5);
    acknowledgement.unwrap().acknowledge().await;
    assert_eq!(store.session().unwrap().seq, 6);
}

#[tokio::test]
async fn drops_duplicate_sequence_numbers() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![
            ready("S1", 1),
            group_message(2, "first"),
            group_message(2, "duplicate"),
            group_message(3, "second"),
        ],
        ..Script::default()
    });
    let mut harness = Harness::new(
        fake.clone(),
        Arc::new(Store::default()),
        Harness::default_config(),
    );
    let _ = next(&mut harness.stream).await.unwrap();
    let second = next(&mut harness.stream).await.unwrap();
    assert!(
        matches!(second.dispatch, Dispatch::GroupAtMessage(ref message) if message.content == "first")
    );
    let third = next(&mut harness.stream).await.unwrap();
    assert!(
        matches!(third.dispatch, Dispatch::GroupAtMessage(ref message) if message.content == "second")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), harness.stream.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn reconnects_and_resumes_after_normal_close() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![ready("S1", 1)],
        close_after_auth: Some(1000),
        ..Script::default()
    });
    fake.push(Script {
        after_auth: vec![resumed(1), group_message(2, "after reconnect")],
        ..Script::default()
    });
    let store = Arc::new(Store::default());
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());
    let _ = next(&mut harness.stream).await.unwrap();
    let event = next(&mut harness.stream).await.unwrap();
    assert!(
        matches!(event.dispatch, Dispatch::GroupAtMessage(ref message) if message.content == "after reconnect")
    );
    let ops = received_ops(&fake);
    assert_eq!(ops, vec![2, 6]);
    assert_eq!(fake.connections(), 2);
    assert_eq!(store.session().unwrap().session_id, "S1");
}

#[tokio::test]
async fn clears_session_when_gateway_requests_a_new_identify() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![ready("S1", 1), json!({"op": 9})],
        ..Script::default()
    });
    fake.push(Script {
        after_auth: vec![ready("S2", 1)],
        ..Script::default()
    });
    let store = Arc::new(Store::default());
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());
    let _ = next(&mut harness.stream).await.unwrap();
    let event = next(&mut harness.stream).await.unwrap();
    assert!(matches!(event.dispatch, Dispatch::Unknown { ref t, .. } if t == "READY"));
    let ops = received_ops(&fake);
    assert_eq!(ops, vec![2, 2]);
    assert_eq!(store.clears(), 1);
    assert_eq!(store.session().unwrap().session_id, "S2");
}

#[tokio::test]
async fn fatal_close_ends_the_event_stream() {
    let fake = start().await;
    fake.push(Script {
        after_auth: vec![ready("S1", 1)],
        close_after_auth: Some(4004),
        ..Script::default()
    });
    let mut harness = Harness::new(
        fake.clone(),
        Arc::new(Store::default()),
        Harness::default_config(),
    );
    let _ = next(&mut harness.stream).await.unwrap();
    assert!(next(&mut harness.stream).await.is_none());
    assert_eq!(fake.connections(), 1);
}

#[tokio::test]
async fn sends_heartbeats() {
    let fake = start().await;
    fake.push(Script {
        heartbeat_ms: 20,
        after_auth: vec![ready("S1", 1)],
        ..Script::default()
    });
    let mut harness = Harness::new(
        fake.clone(),
        Arc::new(Store::default()),
        Harness::default_config(),
    );
    let _ = next(&mut harness.stream).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if fake
            .received()
            .iter()
            .any(|value| value.get("op").and_then(Value::as_u64) == Some(1))
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no heartbeat sent");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.connections(), 1);
}
