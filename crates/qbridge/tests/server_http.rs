mod common;

use common::*;
use futures_util::{SinkExt, StreamExt};
use qbridge::server::Hub;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn serves_get_json_and_form_actions_with_echo() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler.clone(), hub_options());
    let server = start_http(hub.clone()).await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("http://{}/get_login_info?echo=a", server.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["retcode"], 0);
    assert_eq!(body["echo"], "a");

    let response = client
        .post(format!("http://{}/send_msg", server.addr))
        .json(&json!({
            "message_type": "private",
            "user_id": 1,
            "message": "hi"
        }))
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(handler.calls(), vec!["get_login_info", "send_msg"]);

    let response = client
        .post(format!("http://{}/get_login_info", server.addr))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("echo=form")
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["echo"], "form");
}

#[tokio::test]
async fn enforces_auth_and_http_status_contract() {
    let handler = Arc::new(StubHandler::new(10_000));
    let mut options = hub_options();
    options.access_token = Some("TOKEN".into());
    let hub = Hub::new(handler, options);
    let server = start_http(hub).await;
    let client = reqwest::Client::new();
    let url = format!("http://{}/get_login_info", server.addr);

    assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .get(&url)
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&url)
            .bearer_auth("TOKEN")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{url}?access_token=TOKEN"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let mut options = hub_options();
    options.access_token = Some("TOKEN".into());
    let hub = Hub::new(Arc::new(StubHandler::new(10_000)), options);
    let server2 = start_http(hub.clone()).await;
    let server_url = format!("http://{}", server2.addr);
    assert_eq!(
        client
            .post(format!("{server_url}/get_login_info"))
            .bearer_auth("TOKEN")
            .header("content-type", "text/plain")
            .body("x")
            .send()
            .await
            .unwrap()
            .status(),
        406
    );
    assert_eq!(
        client
            .post(format!("{server_url}/get_login_info"))
            .bearer_auth("TOKEN")
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .put(format!("{server_url}/get_login_info"))
            .bearer_auth("TOKEN")
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        client
            .get(format!("{server_url}/a/b"))
            .bearer_auth("TOKEN")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let _ = server_url;
}

#[tokio::test]
async fn unknown_actions_return_a_business_failure_with_http_200() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http(hub).await;
    let response = reqwest::get(format!("http://{}/not_an_action", server.addr))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["retcode"], 1404);
    assert_eq!(body["status"], "failed");
    assert_eq!(body["reason"], "UNKNOWN_ACTION");
}

#[tokio::test]
async fn async_and_rate_limited_modes_queue_actions() {
    let handler = Arc::new(StubHandler::new(10_000));
    let mut options = hub_options();
    options.queue_capacity = 1;
    options.rate_limit = Duration::from_millis(100);
    let hub = Hub::new(handler.clone(), options);
    let server = start_http(hub).await;
    let client = reqwest::Client::new();
    let base = format!("http://{}", server.addr);

    let body: Value = client
        .post(format!("{base}/get_login_info_async"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["status"], "async");
    assert_eq!(body["retcode"], 1);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(handler.call_count(), 1);

    let first: Value = client
        .post(format!("{base}/get_login_info_rate_limited"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["retcode"], 1);
    let second: Value = client
        .post(format!("{base}/get_login_info_rate_limited"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["retcode"], 1);
    let third: Value = client
        .post(format!("{base}/get_login_info_rate_limited"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(third["retcode"], 102);
    assert_eq!(third["reason"], "QUEUE_FULL");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(handler.call_count(), 3);
}

#[tokio::test]
async fn websocket_sends_lifecycle_actions_and_events() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler.clone(), hub_options());
    let server = start_http(hub.clone()).await;
    let url = format!("ws://{}/", server.addr);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    let connect = next_text(&mut socket).await;
    assert_eq!(connect["post_type"], "meta_event");
    assert_eq!(connect["meta_event_type"], "lifecycle");
    assert_eq!(connect["sub_type"], "connect");

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"action": "get_login_info", "params": {}, "echo": 7})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let response = next_text(&mut socket).await;
    assert_eq!(response["echo"], 7);
    assert_eq!(response["retcode"], 0);

    hub.publish(sample_event());
    let event = next_text(&mut socket).await;
    assert_eq!(event["post_type"], "notice");
    assert_eq!(event["notice_type"], "test_notice");
}

#[tokio::test]
async fn can_serve_http_api_without_forward_websocket() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http_routes(hub, qbridge::server::http::Routes::HTTP).await;
    let base = format!("http://{}", server.addr);
    assert_eq!(
        reqwest::get(format!("{base}/")).await.unwrap().status(),
        404
    );
    let response: Value = reqwest::Client::new()
        .post(format!("{base}/get_status"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["retcode"], 0);
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{}/", server.addr))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn can_serve_only_forward_websocket() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http_routes(hub, qbridge::server::http::Routes::FORWARD_WS).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{}/", server.addr))
        .await
        .unwrap();
    let connect = next_text(&mut socket).await;
    assert_eq!(connect["sub_type"], "connect");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"action": "get_status", "params": {}, "echo": 1})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let response = next_text(&mut socket).await;
    assert_eq!(response["retcode"], 0);
    assert_eq!(
        reqwest::get(format!("http://{}/get_status", server.addr))
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn broadcasts_heartbeat_events() {
    let handler = Arc::new(StubHandler::new(10_000));
    let mut options = hub_options();
    options.heartbeat_ms = 20;
    let hub = Hub::new(handler, options);
    let server = start_http(hub).await;
    let url = format!("ws://{}/", server.addr);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let connect = next_text(&mut socket).await;
    assert_eq!(connect["sub_type"], "connect");
    let heartbeat = next_text(&mut socket).await;
    assert_eq!(heartbeat["post_type"], "meta_event");
    assert_eq!(heartbeat["meta_event_type"], "heartbeat");
    assert_eq!(heartbeat["interval"], 20);
    assert_eq!(heartbeat["status"]["online"], true);
}

#[tokio::test]
async fn set_restart_is_acknowledged_as_async() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http(hub).await;
    let response: Value = reqwest::Client::new()
        .post(format!("http://{}/set_restart", server.addr))
        .json(&json!({"delay": 0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["status"], "async");
    assert_eq!(response["retcode"], 1);
}

#[tokio::test]
async fn hub_delegates_action_outcomes_and_status_fields_to_handler() {
    struct CustomHandler;

    #[async_trait::async_trait]
    impl qbridge::server::Handler for CustomHandler {
        fn self_id(&self) -> i64 {
            10_000
        }

        async fn call(
            &self,
            call: qbridge_ob11::Call,
            _context: qbridge::server::CallContext,
        ) -> Result<qbridge::server::ActionOutcome, qbridge::error::ActionError> {
            match call.action {
                qbridge_ob11::Action::GetLoginInfo => Ok(qbridge::server::ActionOutcome::Accepted),
                qbridge_ob11::Action::GetStatus => {
                    Ok(qbridge::server::ActionOutcome::Ok(json!({"custom": true})))
                }
                qbridge_ob11::Action::SetRestart(_) => Ok(qbridge::server::ActionOutcome::Ok(
                    json!({"restarted": false}),
                )),
                _ => Ok(qbridge::server::ActionOutcome::Ok(Value::Null)),
            }
        }
    }

    let hub = Hub::new(Arc::new(CustomHandler), hub_options());

    let accepted = hub
        .dispatch(qbridge_ob11::Call::parse("get_login_info", Default::default()).unwrap())
        .await;
    assert_eq!(accepted.status, qbridge_ob11::Status::Async);
    assert_eq!(accepted.retcode, 1);

    let status = hub
        .dispatch(qbridge_ob11::Call::parse("get_status", Default::default()).unwrap())
        .await;
    assert_eq!(status.status, qbridge_ob11::Status::Ok);
    assert_eq!(status.data, json!({"custom": true}));

    let restart = hub
        .dispatch(qbridge_ob11::Call::parse("set_restart", Default::default()).unwrap())
        .await;
    assert_eq!(restart.status, qbridge_ob11::Status::Ok);
    assert_eq!(restart.retcode, 0);
    assert_eq!(restart.data, json!({"restarted": false}));
}

#[tokio::test]
async fn websocket_api_role_does_not_receive_events() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http(hub.clone()).await;
    let url = format!("ws://{}/api", server.addr);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _ = next_text(&mut socket).await;
    hub.publish(sample_event());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), socket.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn websocket_event_role_rejects_actions() {
    let handler = Arc::new(StubHandler::new(10_000));
    let hub = Hub::new(handler, hub_options());
    let server = start_http(hub).await;
    let url = format!("ws://{}/event", server.addr);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _ = next_text(&mut socket).await;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"action": "get_login_info", "params": {}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let response = next_text(&mut socket).await;
    assert_eq!(response["retcode"], 1404);
}

async fn next_text(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("timed out waiting for a WebSocket frame")
            .expect("WebSocket closed")
            .expect("WebSocket error");
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}
