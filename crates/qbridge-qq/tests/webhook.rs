use ed25519_dalek::{Signer, SigningKey};
use futures_util::StreamExt;
use qbridge_qq::{Dispatch, Envelope, Webhook, WebhookConfig, WebhookStream};
use reqwest::{
    Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::time::Duration;

const SECRET: &str = "DG5g3B4j9X2KOErG";

async fn start(capacity: usize) -> (WebhookStream, String) {
    let webhook = Webhook::new(SECRET).unwrap();
    let stream = webhook
        .serve(
            WebhookConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                path: "/qq/webhook".into(),
            },
            capacity,
        )
        .await
        .unwrap();
    let url = format!("http://{}/qq/webhook", stream.address());
    (stream, url)
}

fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut seed = secret.as_bytes().to_vec();
    while seed.len() < 32 {
        seed.extend_from_slice(secret.as_bytes());
    }
    seed.truncate(32);
    let key = SigningKey::from_bytes(&seed.try_into().unwrap());
    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    key.sign(&message)
        .to_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn headers(secret: &str, timestamp: &str, body: &[u8]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Signature-Ed25519",
        HeaderValue::from_str(&sign(secret, timestamp, body)).unwrap(),
    );
    headers.insert(
        "X-Signature-Timestamp",
        HeaderValue::from_str(timestamp).unwrap(),
    );
    headers
}

async fn send(client: &Client, url: &str, body: &Value, headers: HeaderMap) -> reqwest::Response {
    client
        .post(url)
        .headers(headers)
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

fn friend_add(id: &str) -> Value {
    json!({"id": id, "op": 0, "t": "FRIEND_ADD", "d": {"openid": "U1"}})
}

async fn next(stream: &mut WebhookStream) -> Option<Envelope> {
    tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out waiting for a webhook event")
}

#[tokio::test]
async fn answers_callback_validation_with_the_official_vector() {
    let (_stream, url) = start(4).await;
    let client = Client::new();
    let response = send(
        &client,
        &url,
        &json!({
            "op": 13,
            "d": {"plain_token": "Arq0D5A61EgUu4OxUvOp", "event_ts": "1725442341"}
        }),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["plain_token"], "Arq0D5A61EgUu4OxUvOp");
    assert_eq!(
        body["signature"],
        "87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706"
    );
}

#[tokio::test]
async fn rejects_events_without_a_valid_signature() {
    let (_stream, url) = start(4).await;
    let client = Client::new();
    let body = friend_add("E1");
    let unsigned = client
        .post(&url)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);

    let wrong = headers("another-secret", "1725442341", body.to_string().as_bytes());
    let rejected = send(&client, &url, &body, wrong).await;
    assert_eq!(rejected.status(), 401);

    let mut tampered = headers(SECRET, "1725442341", body.to_string().as_bytes());
    tampered.insert(
        "X-Signature-Ed25519",
        HeaderValue::from_static(
            "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        ),
    );
    assert_eq!(send(&client, &url, &body, tampered).await.status(), 401);
}

#[tokio::test]
async fn streams_signed_events() {
    let (mut stream, url) = start(4).await;
    let client = Client::new();
    let body = friend_add("E1");
    let response = send(
        &client,
        &url,
        &body,
        headers(SECRET, "1725442341", body.to_string().as_bytes()),
    )
    .await;
    assert_eq!(response.status(), 200);
    let event = next(&mut stream).await.unwrap();
    assert_eq!(event.id.as_deref(), Some("E1"));
    assert!(matches!(event.dispatch, Dispatch::FriendAdd(_)));
}

#[tokio::test]
async fn acknowledges_duplicate_event_ids_only_once() {
    let (mut stream, url) = start(4).await;
    let client = Client::new();
    let body = friend_add("E1");
    let request_headers = headers(SECRET, "1725442341", body.to_string().as_bytes());
    assert_eq!(
        send(&client, &url, &body, request_headers.clone())
            .await
            .status(),
        200
    );
    assert_eq!(
        send(&client, &url, &body, request_headers).await.status(),
        200
    );
    assert!(next(&mut stream).await.is_some());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn asks_the_platform_to_retry_when_the_queue_is_full() {
    let (mut stream, url) = start(1).await;
    let client = Client::new();
    for (id, expected) in [("E1", 200), ("E2", 503)] {
        let body = friend_add(id);
        let response = send(
            &client,
            &url,
            &body,
            headers(SECRET, "1725442341", body.to_string().as_bytes()),
        )
        .await;
        assert_eq!(response.status(), expected);
    }
    let event = next(&mut stream).await.unwrap();
    assert_eq!(event.id.as_deref(), Some("E1"));
}

#[tokio::test]
async fn acknowledges_unknown_event_types_without_dispatch() {
    let (mut stream, url) = start(4).await;
    let client = Client::new();
    let body = json!({"id": "E9", "op": 0, "t": "SOMETHING_NEW", "d": {}});
    let response = send(
        &client,
        &url,
        &body,
        headers(SECRET, "1725442341", body.to_string().as_bytes()),
    )
    .await;
    assert_eq!(response.status(), 200);
    let event = next(&mut stream).await.unwrap();
    assert!(matches!(event.dispatch, Dispatch::Unknown { ref t, .. } if t == "SOMETHING_NEW"));
}

#[tokio::test]
async fn reports_bind_failure() {
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = occupied.local_addr().unwrap();
    let webhook = Webhook::new(SECRET).unwrap();
    let error = match webhook
        .serve(
            WebhookConfig {
                bind: address,
                path: "/qq/webhook".into(),
            },
            4,
        )
        .await
    {
        Ok(_) => panic!("expected the occupied port to be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, qbridge_qq::Error::Network(_)));
}
