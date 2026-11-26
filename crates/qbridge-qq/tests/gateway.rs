use async_trait::async_trait;
use axum::{
    Router,
    extract::{
        State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use qbridge_qq::{
    Client, Dispatch, Envelope, Error, GatewayConfig, GatewayStream, Intents, Session,
    SessionStore, ShardConfig, gateway::connect,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
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
    bot_shards: Option<u32>,
    max_concurrency: u32,
}

impl FakeGateway {
    fn set_bot_shards(&self, shards: u32, max_concurrency: u32) {
        let mut inner = self.0.lock().unwrap();
        inner.bot_shards = Some(shards);
        inner.max_concurrency = max_concurrency;
    }

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
        bot_shards: None,
        max_concurrency: 1,
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
            "/gateway/bot",
            get(|State(fake): State<FakeGateway>| async move {
                let (url, shards, max_concurrency) = {
                    let inner = fake.0.lock().unwrap();
                    (
                        inner.ws_url.clone(),
                        inner.bot_shards,
                        inner.max_concurrency,
                    )
                };
                match shards {
                    Some(shards) => axum::Json(json!({
                        "url": url,
                        "shards": shards,
                        "session_start_limit": {
                            "total": 1000,
                            "remaining": 999,
                            "reset_after": 14400000,
                            "max_concurrency": max_concurrency
                        }
                    }))
                    .into_response(),
                    None => (StatusCode::NOT_FOUND, "not found").into_response(),
                }
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
    sessions: Mutex<HashMap<u32, Session>>,
    saves: AtomicUsize,
    clears: AtomicUsize,
}

#[async_trait]
impl SessionStore for Store {
    async fn load(&self, shard_id: u32) -> Result<Option<Session>, Error> {
        Ok(self.sessions.lock().unwrap().get(&shard_id).cloned())
    }

    async fn save(&self, shard_id: u32, session: &Session) -> Result<(), Error> {
        self.saves.fetch_add(1, Ordering::Relaxed);
        self.sessions
            .lock()
            .unwrap()
            .insert(shard_id, session.clone());
        Ok(())
    }

    async fn clear(&self, shard_id: u32) -> Result<(), Error> {
        self.clears.fetch_add(1, Ordering::Relaxed);
        self.sessions.lock().unwrap().remove(&shard_id);
        Ok(())
    }
}

impl Store {
    fn with_session(session: Session) -> Self {
        Self::with_shard_session(0, session)
    }

    fn with_shard_session(shard_id: u32, session: Session) -> Self {
        let mut sessions = HashMap::new();
        sessions.insert(shard_id, session);
        Self {
            sessions: Mutex::new(sessions),
            ..Self::default()
        }
    }

    fn session(&self) -> Option<Session> {
        self.shard_session(0)
    }

    fn shard_session(&self, shard_id: u32) -> Option<Session> {
        self.sessions.lock().unwrap().get(&shard_id).cloned()
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
            shard: ShardConfig::Auto,
            reconnect_initial: Duration::from_millis(10),
            reconnect_max: Duration::from_millis(50),
            ready_timeout: Duration::from_secs(2),
            identify_delay: Duration::from_millis(5),
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
        shard_count: 1,
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
        shard_count: 1,
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
        shard_count: 1,
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

#[tokio::test]
async fn auto_discovers_multiple_shards_from_gateway_bot_and_tracks_sessions_separately() {
    let fake = start().await;
    fake.set_bot_shards(2, 2);
    fake.push(Script {
        after_auth: vec![ready("SHARD-A", 1), group_message(2, "from-a")],
        ..Script::default()
    });
    fake.push(Script {
        after_auth: vec![ready("SHARD-B", 1), group_message(2, "from-b")],
        ..Script::default()
    });
    let store = Arc::new(Store::default());
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());

    let mut contents = Vec::new();
    for _ in 0..4 {
        let event = next(&mut harness.stream).await.unwrap();
        if let Dispatch::GroupAtMessage(message) = event.dispatch {
            contents.push(message.content);
        }
    }
    contents.sort();
    assert_eq!(contents, vec!["from-a", "from-b"]);
    assert_eq!(fake.connections(), 2);

    let mut shards = fake
        .received()
        .into_iter()
        .filter(|value| value["op"] == 2)
        .map(|value| value["d"]["shard"].clone())
        .collect::<Vec<_>>();
    shards.sort_by_key(|value| value.to_string());
    assert_eq!(shards, vec![json!([0, 2]), json!([1, 2])]);

    let shard0 = store.shard_session(0).unwrap();
    let shard1 = store.shard_session(1).unwrap();
    assert_eq!(shard0.shard_count, 2);
    assert_eq!(shard1.shard_count, 2);
    assert_ne!(shard0.session_id, shard1.session_id);
    assert_eq!(shard0.seq, 2);
    assert_eq!(shard1.seq, 2);
    assert!(harness.stream.online().load(Ordering::Acquire));
}

#[tokio::test]
async fn invalidates_persisted_session_when_shard_count_changes() {
    let fake = start().await;
    fake.set_bot_shards(2, 2);
    fake.push(Script {
        after_auth: vec![ready("NEW-0", 1)],
        ..Script::default()
    });
    fake.push(Script {
        after_auth: vec![ready("NEW-1", 1)],
        ..Script::default()
    });
    let store = Arc::new(Store::with_shard_session(
        0,
        Session {
            session_id: "OLD-SINGLE".into(),
            seq: 9,
            intents: Intents::GROUP_AND_C2C.bits(),
            shard_count: 1,
        },
    ));
    let mut harness = Harness::new(fake.clone(), store.clone(), Harness::default_config());
    let _ = next(&mut harness.stream).await.unwrap();
    let _ = next(&mut harness.stream).await.unwrap();

    assert_eq!(received_ops(&fake), vec![2, 2]);
    assert_eq!(store.clears(), 1);
    assert_eq!(store.shard_session(0).unwrap().shard_count, 2);
}

#[tokio::test]
async fn single_shard_config_runs_only_requested_shard() {
    let fake = start().await;
    fake.set_bot_shards(4, 1);
    fake.push(Script {
        after_auth: vec![ready("S-2", 1)],
        ..Script::default()
    });
    let store = Arc::new(Store::default());
    let mut config = Harness::default_config();
    config.shard = ShardConfig::Single { id: 2, count: 4 };
    let mut harness = Harness::new(fake.clone(), store.clone(), config);

    let _ = next(&mut harness.stream).await.unwrap();
    assert_eq!(fake.connections(), 1);
    let identify = &fake.received()[0];
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["shard"], json!([2, 4]));
    assert!(store.shard_session(0).is_none());
    let saved = store.shard_session(2).unwrap();
    assert_eq!(saved.session_id, "S-2");
    assert_eq!(saved.shard_count, 4);
}

#[tokio::test]
async fn fatal_close_on_one_shard_stops_all_shards() {
    let fake = start().await;
    fake.set_bot_shards(2, 2);
    fake.push(Script {
        after_auth: vec![ready("S0", 1)],
        close_after_auth: Some(4010),
        ..Script::default()
    });
    fake.push(Script {
        after_auth: vec![ready("S1", 1)],
        ..Script::default()
    });
    let mut harness = Harness::new(
        fake.clone(),
        Arc::new(Store::default()),
        Harness::default_config(),
    );
    let mut ended = false;
    for _ in 0..4 {
        if next(&mut harness.stream).await.is_none() {
            ended = true;
            break;
        }
    }
    assert!(ended);
    assert!(!harness.stream.online().load(Ordering::Acquire));
}
