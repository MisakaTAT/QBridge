mod common;

use axum::{Router, body::Bytes, extract::State, http::StatusCode, routing::any};
use common::*;
use ed25519_dalek::{Signer, SigningKey};
use qbridge::app::{self, Endpoints};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "E2E-WEBHOOK-SECRET";

#[derive(Clone)]
struct PostReceiver(Arc<Mutex<Vec<Value>>>);

async fn post_handler(State(state): State<PostReceiver>, body: Bytes) -> StatusCode {
    if let Ok(value) = serde_json::from_slice::<Value>(&body) {
        state.0.lock().unwrap().push(value);
    }
    StatusCode::NO_CONTENT
}

async fn start_post_receiver() -> (PostReceiver, SocketAddr, tokio::task::JoinHandle<()>) {
    let state = PostReceiver(Arc::new(Mutex::new(Vec::new())));
    let app = Router::new()
        .fallback(any(post_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, address, task)
}

async fn free_address() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

fn signature(secret: &str, timestamp: &str, body: &[u8]) -> HeaderMap {
    let mut seed = secret.as_bytes().to_vec();
    while seed.len() < 32 {
        seed.extend_from_slice(secret.as_bytes());
    }
    seed.truncate(32);
    let key = SigningKey::from_bytes(&seed.try_into().unwrap());
    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    let hex = key
        .sign(&message)
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut headers = HeaderMap::new();
    headers.insert("X-Signature-Ed25519", HeaderValue::from_str(&hex).unwrap());
    headers.insert(
        "X-Signature-Timestamp",
        HeaderValue::from_static("1725442341"),
    );
    headers
}

async fn wait_for_post_notice(receiver: &PostReceiver, event_type: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = receiver
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|event| {
                event.get("post_type").and_then(Value::as_str) == Some("notice")
                    && event
                        .get("qq")
                        .and_then(|qq| qq.get("event_type"))
                        .and_then(Value::as_str)
                        == Some(event_type)
            })
            .cloned()
        {
            return event;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no {event_type} notice was delivered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_post_event(receiver: &PostReceiver, official_id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = receiver
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|event| {
                event.get("post_type").and_then(Value::as_str) == Some("message")
                    && event
                        .get("qq")
                        .and_then(|qq| qq.get("message_id"))
                        .and_then(Value::as_str)
                        == Some(official_id)
            })
            .cloned()
        {
            return event;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no message event was delivered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn timestamp() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[tokio::test]
async fn end_to_end_webhook_receive_reply_quote_recall_and_group_member_events() {
    let (post_receiver, post_address, post_task) = start_post_receiver().await;
    let qq = service().await;
    let webhook_address = free_address().await;
    let http_address = free_address().await;
    let storage = std::env::temp_dir().join(format!(
        "qbridge-e2e-{}-{}.sqlite",
        std::process::id(),
        webhook_address.port()
    ));
    let config_path = std::env::temp_dir().join(format!(
        "qbridge-e2e-{}-{}.toml",
        std::process::id(),
        webhook_address.port()
    ));
    let config = format!(
        r#"
self_id = 10000
access_token = "OB11TOKEN"
message_format = "array"
heartbeat_ms = 0

[qq]
app_id = "1234"
secret = "{SECRET}"
source = "webhook"
intents = ["group_and_c2c", "group_member", "interaction"]

[qq.webhook]
bind = "{webhook_address}"
path = "/qq/webhook"

[storage]
path = "{storage}"

[http]
bind = "{http_address}"

[[http_post]]
url = "http://{post_address}/"
timeout_ms = 2000

[limits]
max_body_bytes = 1048576
max_ws_bytes = 1048576
queue_capacity = 64
rate_limit_ms = 10
"#,
        storage = storage.display().to_string().replace('\\', "\\\\"),
    );
    std::fs::write(&config_path, &config).unwrap();

    let endpoints = Endpoints {
        api_base: Some(qq.base.clone()),
        token_url: Some(format!("{}/token", qq.base)),
    };
    let cancel = CancellationToken::new();
    let app_task = tokio::spawn(app::run_until(
        config_path.clone(),
        endpoints,
        cancel.clone(),
    ));
    let http = Client::new();
    let api_url = format!("http://{http_address}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = http
            .get(format!("{api_url}/get_version_info"))
            .query(&[("access_token", "OB11TOKEN")])
            .send()
            .await
            .is_ok_and(|response| response.status() == 200);
        if ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the application did not start"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let body = json!({
        "id": "EVENT-1",
        "op": 0,
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "IN-1",
            "content": "hello",
            "timestamp": timestamp(),
            "author": {"user_openid": "USER", "username": "Alice"},
            "message_scene": {"ext": ["msg_idx=REF-IN"]}
        }
    })
    .to_string();
    let response = http
        .post(format!("http://{webhook_address}/qq/webhook"))
        .headers(signature(SECRET, "1725442341", body.as_bytes()))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let event = wait_for_post_event(&post_receiver, "IN-1").await;
    let user_id = event["user_id"].as_i64().unwrap();
    let message_id = event["message_id"].as_i64().unwrap();
    assert_eq!(event["message"][0]["data"]["text"], "hello");

    qq.fake.respond(
        200,
        json!({"id": "OUT-1", "ext_info": {"ref_idx": "REF-OUT"}}),
    );
    let reply: Value = http
        .post(format!("{api_url}/send_private_msg"))
        .query(&[("access_token", "OB11TOKEN")])
        .json(&json!({
            "user_id": user_id,
            "message": [
                {"type": "reply", "data": {"id": message_id}},
                {"type": "text", "data": {"text": "pong"}}
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["status"], "ok");
    let outbound = reply["data"]["message_id"].as_i64().unwrap();
    let requests = qq.fake.requests();
    let sent = requests
        .iter()
        .find(|request| request.path == "/v2/users/USER/messages")
        .expect("the QQ send request was not observed");
    assert_eq!(sent.body["content"], "pong");
    assert_eq!(sent.body["msg_id"], "IN-1");
    assert_eq!(sent.body["msg_seq"], 1);
    assert_eq!(sent.body["message_reference"]["message_id"], "REF-IN");

    qq.fake.respond(200, json!({}));
    let recalled: Value = http
        .post(format!("{api_url}/delete_msg"))
        .query(&[("access_token", "OB11TOKEN")])
        .json(&json!({"message_id": outbound}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recalled["status"], "ok");
    assert!(qq.fake.requests().iter().any(
        |request| request.method == "DELETE" && request.path == "/v2/users/USER/messages/OUT-1"
    ));

    let add = json!({
        "id": "EVENT-ADD",
        "op": 0,
        "t": "GROUP_MEMBER_ADD",
        "d": {
            "group_openid": "GROUP-MEMBERS",
            "member_openid": "NEW-MEMBER",
            "timestamp": timestamp()
        }
    })
    .to_string();
    let response = http
        .post(format!("http://{webhook_address}/qq/webhook"))
        .headers(signature(SECRET, "1725442341", add.as_bytes()))
        .body(add)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let added = wait_for_post_notice(&post_receiver, "GROUP_MEMBER_ADD").await;
    assert_eq!(added["notice_type"], "group_increase");
    assert_eq!(added["sub_type"], "approve");
    let group_id = added["group_id"].as_i64().unwrap();
    let member_id = added["user_id"].as_i64().unwrap();
    assert!(added["operator_id"].is_null());
    assert_eq!(added["qq"]["event_type"], "GROUP_MEMBER_ADD");

    let remove = json!({
        "id": "EVENT-REMOVE",
        "op": 0,
        "t": "GROUP_MEMBER_REMOVE",
        "d": {
            "group_openid": "GROUP-MEMBERS",
            "member_openid": "NEW-MEMBER",
            "timestamp": timestamp()
        }
    })
    .to_string();
    let response = http
        .post(format!("http://{webhook_address}/qq/webhook"))
        .headers(signature(SECRET, "1725442341", remove.as_bytes()))
        .body(remove)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let removed = wait_for_post_notice(&post_receiver, "GROUP_MEMBER_REMOVE").await;
    assert_eq!(removed["notice_type"], "group_decrease");
    assert_eq!(removed["sub_type"], "leave");
    assert_eq!(removed["group_id"], group_id);
    assert_eq!(removed["user_id"], member_id);
    assert!(removed["operator_id"].is_null());

    let restart: Value = http
        .post(format!("{api_url}/set_restart"))
        .query(&[("access_token", "OB11TOKEN")])
        .json(&json!({"delay": 100}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restart["status"], "async");
    assert_eq!(restart["retcode"], 1);
    let restarted_config =
        config.replace("message_format = \"array\"", "message_format = \"string\"");
    assert_ne!(restarted_config, config);
    std::fs::write(&config_path, restarted_config).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ready = http
            .get(format!("{api_url}/get_version_info"))
            .query(&[("access_token", "OB11TOKEN")])
            .send()
            .await
            .is_ok_and(|response| response.status() == 200);
        if ready {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the application did not restart"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let second = json!({
        "id": "EVENT-2",
        "op": 0,
        "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "IN-2",
            "content": "after restart",
            "timestamp": timestamp(),
            "author": {"user_openid": "USER", "username": "Alice"}
        }
    })
    .to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let response = http
            .post(format!("http://{webhook_address}/qq/webhook"))
            .headers(signature(SECRET, "1725442341", second.as_bytes()))
            .body(second.clone())
            .send()
            .await;
        if response.is_ok_and(|response| response.status() == 200) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the restarted webhook did not accept an event"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after_restart = wait_for_post_event(&post_receiver, "IN-2").await;
    assert!(
        after_restart["message"].is_string(),
        "message_format did not change after restart: {after_restart}"
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), app_task)
        .await
        .expect("the application did not stop")
        .expect("the application task panicked");
    result.expect("the application returned an error");
    post_task.abort();
    let _ = std::fs::remove_file(&config_path);
    let _ = std::fs::remove_file(&storage);
}
