use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::any,
};
use qbridge_qq::{
    Client, Error, GroupOpenId, MediaSource, MediaUpload, MemberOpenId, MuteOp, OutgoingMessage,
    Peer, UserOpenId,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Fake(Arc<Mutex<FakeState>>);

struct FakeResponse {
    status: u16,
    body: Option<Value>,
    headers: Vec<(String, String)>,
}

struct FakeState {
    token_calls: usize,
    token_status: u16,
    token_body: Value,
    responses: VecDeque<FakeResponse>,
    requests: Vec<Recorded>,
}

#[derive(Clone)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    headers: HeaderMap,
    body: Value,
    raw: Vec<u8>,
}

impl Fake {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            token_calls: 0,
            token_status: 200,
            token_body: json!({"access_token": "TOKEN", "expires_in": 7200}),
            responses: VecDeque::new(),
            requests: Vec::new(),
        })))
    }

    fn respond(&self, status: u16, body: Value) {
        self.0.lock().unwrap().responses.push_back(FakeResponse {
            status,
            body: Some(body),
            headers: Vec::new(),
        });
    }

    fn respond_with_headers(&self, status: u16, body: Value, headers: Vec<(&str, &str)>) {
        self.0.lock().unwrap().responses.push_back(FakeResponse {
            status,
            body: Some(body),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        });
    }

    fn respond_empty(&self, status: u16) {
        self.0.lock().unwrap().responses.push_back(FakeResponse {
            status,
            body: None,
            headers: Vec::new(),
        });
    }

    fn set_token(&self, status: u16, body: Value) {
        let mut state = self.0.lock().unwrap();
        state.token_status = status;
        state.token_body = body;
    }

    fn token_calls(&self) -> usize {
        self.0.lock().unwrap().token_calls
    }

    fn requests(&self) -> Vec<Recorded> {
        self.0.lock().unwrap().requests.clone()
    }

    fn take_requests(&self) -> Vec<Recorded> {
        std::mem::take(&mut self.0.lock().unwrap().requests)
    }
}

async fn handle(State(fake): State<Fake>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let method = request.method().as_str().to_owned();
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .unwrap_or_default();
    if path == "/token" {
        let mut state = fake.0.lock().unwrap();
        state.token_calls += 1;
        let status = state.token_status;
        let body = state.token_body.clone();
        drop(state);
        return json_response(status, Some(body), &[]);
    }
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let mut state = fake.0.lock().unwrap();
    state.requests.push(Recorded {
        method,
        path,
        query,
        headers,
        body,
        raw: bytes.to_vec(),
    });
    let response = state.responses.pop_front().unwrap_or(FakeResponse {
        status: 404,
        body: Some(json!({"code": 404})),
        headers: Vec::new(),
    });
    drop(state);
    json_response(response.status, response.body, &response.headers)
}

fn json_response(status: u16, body: Option<Value>, headers: &[(String, String)]) -> Response {
    let status = StatusCode::from_u16(status).unwrap();
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

struct Service {
    fake: Fake,
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Service {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn service() -> Service {
    let fake = Fake::new();
    let app = Router::new().fallback(any(handle)).with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Service {
        fake,
        base: format!("http://{address}"),
        task,
    }
}

impl Service {
    fn client(&self) -> Client {
        Client::with_endpoints(
            "1234",
            "SECRET",
            &self.base,
            &format!("{}/token", self.base),
        )
        .unwrap()
    }
}

fn group() -> GroupOpenId {
    GroupOpenId::new("GROUP1").unwrap()
}

#[tokio::test]
async fn sends_text_with_group_route_and_auth_headers() {
    let service = service().await;
    service.fake.respond(
        200,
        json!({"id": "M1", "timestamp": "2026-01-01T00:00:00Z", "ext_info": {"ref_idx": "R1"}}),
    );
    let sent = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("hello"))
        .await
        .unwrap();
    assert_eq!(sent.id, "M1");
    assert_eq!(sent.ext_info.unwrap().ref_idx.as_deref(), Some("R1"));

    let requests = service.fake.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v2/groups/GROUP1/messages");
    assert_eq!(request.headers.get("authorization").unwrap(), "QQBot TOKEN");
    assert_eq!(request.headers.get("x-union-appid").unwrap(), "1234");
    assert_eq!(request.body, json!({"msg_type": 0, "content": "hello"}));
}

#[tokio::test]
async fn reuses_access_token_until_it_expires() {
    let service = service().await;
    service.fake.respond_empty(204);
    service.fake.respond_empty(204);
    let client = service.client();
    client
        .recall(&Peer::User(UserOpenId::new("USER1").unwrap()), "M1")
        .await
        .unwrap();
    client
        .recall(&Peer::User(UserOpenId::new("USER1").unwrap()), "M2")
        .await
        .unwrap();
    assert_eq!(service.fake.token_calls(), 1);
}

#[tokio::test]
async fn uploads_large_media_in_parts() {
    let service = service().await;
    let data = b"hello world";
    service.fake.respond(
        200,
        json!({
            "upload_id": "UPLOAD",
            "block_size": "5",
            "parts": [
                {"index": 0, "presigned_url": format!("{}/part/0", service.base), "block_size": "5"},
                {"index": 1, "presigned_url": format!("{}/part/1", service.base), "block_size": "6"}
            ]
        }),
    );
    service.fake.respond_empty(200);
    service.fake.respond_empty(200);
    service.fake.respond_empty(200);
    service.fake.respond_empty(200);
    service.fake.respond(200, json!({"file_info": "FILE"}));

    let uploaded = service
        .client()
        .upload_media_chunked(&Peer::Group(group()), 2, "demo.mp4", data)
        .await
        .unwrap();
    assert_eq!(uploaded.file_info, "FILE");

    let requests = service.fake.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0].path, "/v2/groups/GROUP1/upload_prepare");
    assert_eq!(requests[0].body["file_type"], 2);
    assert_eq!(requests[0].body["file_size"], "11");
    assert_eq!(requests[0].body["file_name"], "demo.mp4");
    assert_eq!(requests[0].body["md5"], "5eb63bbbe01eeed093cb22bb8f5acdc3");
    assert_eq!(
        requests[0].body["sha1"],
        "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"
    );
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].raw, b"hello");
    assert_eq!(requests[2].path, "/v2/groups/GROUP1/upload_part_finish");
    assert_eq!(requests[2].body["part_index"], 0);
    assert_eq!(requests[2].body["block_size"], "5");
    assert_eq!(requests[2].body["md5"], "5d41402abc4b2a76b9719d911017c592");
    assert_eq!(requests[3].raw, b" world");
    assert_eq!(requests[4].body["part_index"], 1);
    assert_eq!(requests[4].body["block_size"], "6");
    assert_eq!(requests[5].path, "/v2/groups/GROUP1/files");
    assert_eq!(requests[5].body["upload_id"], "UPLOAD");
    assert_eq!(requests[5].body["file_name"], "demo.mp4");
    assert_eq!(requests[5].body["file_size"], "11");
}

#[tokio::test]
async fn retries_transient_chunk_upload_failures() {
    let service = service().await;
    let path = std::env::temp_dir().join(format!("qbridge-retry-{}.bin", std::process::id()));
    std::fs::write(&path, b"hello world").unwrap();
    service.fake.respond(
        200,
        json!({
            "upload_id": "UPLOAD",
            "block_size": "11",
            "parts": [{
                "index": 0,
                "presigned_url": format!("{}/part/0", service.base),
                "block_size": "11"
            }],
            "upload_config": {"concurrency": 1, "retry_delay": "0.01"}
        }),
    );
    service.fake.respond(500, json!({"message": "retry"}));
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({"file_info": "FILE"}));
    let uploaded = service
        .client()
        .upload_file_chunked(&Peer::Group(group()), 2, "demo.bin", &path)
        .await
        .unwrap();
    assert_eq!(uploaded.file_info, "FILE");
    let requests = service.fake.requests();
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[2].method, "PUT");
    assert_eq!(requests[1].raw, b"hello world");
    assert_eq!(requests[2].raw, b"hello world");
    assert_eq!(requests.len(), 5);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn manages_group_members_and_join_requests() {
    let service = service().await;
    service.fake.respond(200, json!({}));
    service.fake.respond(200, json!({}));
    let client = service.client();
    let member = MemberOpenId::new("MEMBER").unwrap();
    client
        .remove_group_members(&group(), std::slice::from_ref(&member), false)
        .await
        .unwrap();
    client
        .approve_join_request(&group(), &member, "JOIN", false, Some("no thanks"))
        .await
        .unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP1/batch_remove_members");
    assert_eq!(requests[0].body["member_openids"], json!(["MEMBER"]));
    assert_eq!(
        requests[1].path,
        "/v2/groups/GROUP1/approval_join_request/MEMBER"
    );
    assert_eq!(requests[1].body["op"], "decline");
    assert_eq!(requests[1].body["join_request_id"], "JOIN");
    assert_eq!(requests[1].body["reject_reason"], "no thanks");
}

#[tokio::test]
async fn reports_auth_error_when_token_endpoint_rejects_credentials() {
    let service = service().await;
    service.fake.set_token(
        401,
        json!({"code": 100016, "message": "invalid app secret"}),
    );
    let error = service.client().access_token().await.unwrap_err();
    assert!(error.is_auth(), "{error:?}");
}

#[tokio::test]
async fn clears_token_after_unauthorized_response() {
    let service = service().await;
    service.fake.respond(401, json!({"message": "expired"}));
    service.fake.respond(200, json!({"id": "M1"}));
    let client = service.client();
    let peer = Peer::Group(group());
    let error = client
        .send_message(&peer, &OutgoingMessage::text("hello"))
        .await
        .unwrap_err();
    assert!(error.is_auth(), "{error:?}");
    client
        .send_message(&peer, &OutgoingMessage::text("hello"))
        .await
        .unwrap();
    assert_eq!(service.fake.token_calls(), 2);
}

#[tokio::test]
async fn reports_api_error_with_status_and_code() {
    let service = service().await;
    service
        .fake
        .respond(400, json!({"code": 11244, "message": "invalid request"}));
    let error = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("hello"))
        .await
        .unwrap_err();
    match &error {
        Error::Api {
            http_status,
            code,
            message,
            trace_id,
        } => {
            assert_eq!(*http_status, 400);
            assert_eq!(*code, Some(11244));
            assert_eq!(message, "invalid request");
            assert_eq!(trace_id.as_deref(), None);
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let display = error.to_string();
    assert!(display.contains("400"));
    assert!(display.contains("11244"));
}

#[tokio::test]
async fn accepts_err_code_as_string() {
    let service = service().await;
    service
        .fake
        .respond(200, json!({"err_code": "11244", "err_msg": "nope"}));
    let error = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("x"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Api {
            code: Some(11244),
            ..
        }
    ));
}

#[tokio::test]
async fn uploads_media_and_returns_file_info() {
    let service = service().await;
    service
        .fake
        .respond(200, json!({"file_info": "FILE", "ttl": 300}));
    let uploaded = service
        .client()
        .upload_media(
            &Peer::Group(group()),
            &MediaUpload {
                file_type: 1,
                source: MediaSource::Url {
                    url: "https://example.com/a.png".into(),
                    file_name: None,
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(uploaded.file_info, "FILE");
    assert_eq!(uploaded.ttl, Some(300));
    let request = service.fake.take_requests().remove(0);
    assert_eq!(request.path, "/v2/groups/GROUP1/files");
    assert_eq!(
        request.body,
        json!({"file_type": 1, "srv_send_msg": false, "url": "https://example.com/a.png"})
    );
}

#[tokio::test]
async fn paginates_group_members_by_cursor() {
    let service = service().await;
    service.fake.respond(
        200,
        json!({
            "members": [{"member_openid": "M1", "username": "a"}],
            "next_cursor": "NEXT",
        }),
    );
    service.fake.respond(
        200,
        json!({
            "members": [{"member_openid": "M2", "username": "b"}],
            "next_cursor": "",
        }),
    );
    let client = service.client();
    let first = client.group_members(&group(), None).await.unwrap();
    assert_eq!(first.members[0].member_openid, "M1");
    assert_eq!(first.next_cursor, "NEXT");
    let second = client
        .group_members(&group(), Some(&first.next_cursor))
        .await
        .unwrap();
    assert_eq!(second.members[0].member_openid, "M2");
    let requests = service.fake.requests();
    assert_eq!(requests[0].query, "");
    assert_eq!(requests[1].query, "cursor=NEXT");
}

#[tokio::test]
async fn sets_member_mute_and_acknowledges_interactions() {
    let service = service().await;
    service.fake.respond_empty(200);
    service.fake.respond_empty(200);
    let client = service.client();
    client
        .set_member_mute(
            &group(),
            &[
                MuteOp::add(MemberOpenId::new("M1").unwrap(), "2026-01-01T00:00:00Z"),
                MuteOp::delete(MemberOpenId::new("M2").unwrap()),
            ],
        )
        .await
        .unwrap();
    client.ack_interaction("I1", 0).await.unwrap();
    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/groups/GROUP1/restrict_chat_setting");
    assert_eq!(
        requests[0].body,
        json!({"members": [
            {"op": "add", "member_openid": "M1", "mute_expire_at": "2026-01-01T00:00:00Z"},
            {"op": "del", "member_openid": "M2", "mute_expire_at": ""},
        ]})
    );
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].path, "/interactions/I1");
    assert_eq!(requests[1].body, json!({"code": 0}));
}

#[tokio::test]
async fn queries_group_info_member_and_gateway() {
    let service = service().await;
    service
        .fake
        .respond(200, json!({"group_name": "G", "group_member_num": 3}));
    service
        .fake
        .respond(200, json!({"member_openid": "M1", "username": "u"}));
    service
        .fake
        .respond(200, json!({"url": "wss://gateway.example/ws"}));
    let client = service.client();
    let info = client.group_info(&group()).await.unwrap();
    assert_eq!(info.group_name, "G");
    assert_eq!(info.group_member_num, Some(3));
    let member = client
        .group_member(&group(), &MemberOpenId::new("M1").unwrap())
        .await
        .unwrap();
    assert_eq!(member.username.as_deref(), Some("u"));
    assert_eq!(
        client.gateway_url().await.unwrap(),
        "wss://gateway.example/ws"
    );
}

#[tokio::test]
async fn handles_empty_success_body() {
    let service = service().await;
    service.fake.respond_empty(204);
    service
        .client()
        .recall(&Peer::Group(group()), "M1")
        .await
        .unwrap();
}

#[tokio::test]
async fn accepts_qq_message_ids_with_dots() {
    let service = service().await;
    service.fake.respond_empty(204);
    service
        .client()
        .recall(&Peer::Group(group()), "ROBOT1.0_ABC")
        .await
        .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(request.path, "/v2/groups/GROUP1/messages/ROBOT1.0_ABC");
}

#[tokio::test]
async fn encodes_unusual_message_id_characters_in_the_path() {
    let service = service().await;
    service.fake.respond_empty(204);
    service
        .client()
        .recall(&Peer::Group(group()), "ROBOT1.0_A+B=C")
        .await
        .unwrap();
    let request = service.fake.requests().remove(0);
    assert_eq!(
        request.path,
        "/v2/groups/GROUP1/messages/ROBOT1.0_A%2BB%3DC"
    );
}

#[tokio::test]
async fn reports_decode_error_for_malformed_json() {
    let service = service().await;
    service.fake.respond(200, json!("not an object"));
    let error = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("x"))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Decode(_)), "{error:?}");
}

#[tokio::test]
async fn rejects_empty_path_segments_before_requesting_a_token() {
    let service = service().await;
    let error = service
        .client()
        .recall(&Peer::Group(group()), "")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Decode(_)), "{error:?}");
    assert_eq!(service.fake.token_calls(), 0);
    assert!(service.fake.requests().is_empty());
}

#[test]
fn rejects_insecure_or_incomplete_configuration() {
    assert!(
        Client::with_endpoints("", "s", "https://api.example", "https://token.example").is_err()
    );
    assert!(
        Client::with_endpoints("app", "s", "http://api.example", "https://token.example").is_err()
    );
    assert!(
        Client::with_endpoints("app", "s", "https://api.example", "https://token.example").is_ok()
    );
}

#[tokio::test]
async fn send_message_accepts_an_empty_object_response() {
    let service = service().await;
    service.fake.respond(200, json!({}));
    let sent = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("hello"))
        .await
        .unwrap();
    assert!(sent.id.is_empty());
}

#[tokio::test]
async fn gateway_bot_parses_shards_and_session_start_limit() {
    let service = service().await;
    service.fake.respond(
        200,
        json!({
            "url": "wss://api.sgroup.qq.com/websocket",
            "shards": 4,
            "session_start_limit": {
                "total": 1000,
                "remaining": 998,
                "reset_after": 14400000,
                "max_concurrency": 2
            }
        }),
    );
    let bot = service.client().gateway_bot().await.unwrap();
    assert_eq!(bot.url, "wss://api.sgroup.qq.com/websocket");
    assert_eq!(bot.shards, 4);
    let limit = bot.session_start_limit.unwrap();
    assert_eq!(limit.total, 1000);
    assert_eq!(limit.remaining, 998);
    assert_eq!(limit.reset_after, 14_400_000);
    assert_eq!(limit.max_concurrency, 2);
    assert_eq!(service.fake.requests()[0].path, "/gateway/bot");
}

#[tokio::test]
async fn gateway_bot_defaults_shards_and_rejects_invalid_values() {
    let service = service().await;
    service
        .fake
        .respond(200, json!({"url": "wss://api.sgroup.qq.com/websocket"}));
    let bot = service.client().gateway_bot().await.unwrap();
    assert_eq!(bot.shards, 1);
    assert!(bot.session_start_limit.is_none());

    service.fake.respond(
        200,
        json!({"url": "wss://api.sgroup.qq.com/websocket", "shards": 0}),
    );
    assert!(matches!(
        service.client().gateway_bot().await,
        Err(Error::Decode(_))
    ));

    service.fake.respond(200, json!({"url": "", "shards": 1}));
    assert!(matches!(
        service.client().gateway_bot().await,
        Err(Error::Decode(_))
    ));
}

#[tokio::test]
async fn queries_group_mute_setting() {
    let service = service().await;
    service.fake.respond(
        200,
        json!({"members": [{"member_openid": "M1", "mute_end_timestamp": "2099-01-01T00:00:00Z"}]}),
    );
    let response = service.client().group_mute_setting(&group()).await.unwrap();
    assert_eq!(response["members"][0]["member_openid"], "M1");
    let request = service.fake.requests().remove(0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/v2/groups/GROUP1/restrict_chat_setting");
}

#[tokio::test]
async fn queries_bot_profile_and_user_info() {
    let service = service().await;
    service.fake.respond(
        200,
        json!({
            "id": "BOT-ID",
            "username": "MyBot",
            "avatar": "https://example.com/bot.png",
            "bot": true
        }),
    );
    let me = service.client().me().await.unwrap();
    assert_eq!(me.id, "BOT-ID");
    assert_eq!(me.username, "MyBot");
    assert_eq!(me.avatar.as_deref(), Some("https://example.com/bot.png"));

    service.fake.respond(
        200,
        json!({
            "nickname": "Alice",
            "avatar": "https://example.com/alice.png"
        }),
    );
    let user = qbridge_qq::UserOpenId::new("USER1").unwrap();
    let info = service.client().user_info(&user).await.unwrap();
    assert_eq!(info.display_name(), Some("Alice"));
    assert_eq!(
        info.avatar.as_deref(),
        Some("https://example.com/alice.png")
    );

    let requests = service.fake.requests();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/users/@me");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/v2/users/USER1/info");
}

#[tokio::test]
async fn lists_uploads_and_deletes_media_images() {
    let service = service().await;
    let upload_url = format!("{}/upload-slot", service.base);
    service.fake.respond(
        200,
        json!({"upload_id": "UP-1", "presigned_url": upload_url}),
    );
    service.fake.respond(200, json!({}));
    service.fake.respond(
        200,
        json!({"image_id": "IMG-1", "url": "qqbot-media://image/v1/abc"}),
    );
    let uploaded = service
        .client()
        .upload_media_image(b"PNGDATA", "a.png", "image/png")
        .await
        .unwrap();
    assert_eq!(uploaded.image_id, "IMG-1");
    assert_eq!(uploaded.url, "qqbot-media://image/v1/abc");

    service.fake.respond(
        200,
        json!({
            "images": [{"image_id": "IMG-1", "url": "qqbot-media://image/v1/abc"}],
            "next_cursor": "",
            "total": 1
        }),
    );
    let list = service.client().media_images(Some("CUR-1")).await.unwrap();
    assert_eq!(list["total"], 1);

    service.fake.respond(200, json!({}));
    service.client().delete_media_image("IMG-1").await.unwrap();

    let requests = service.fake.requests();
    assert_eq!(requests[0].path, "/v2/media-images/prepare");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(requests[1].path, "/upload-slot");
    assert_eq!(requests[2].path, "/v2/media-images/complete");
    assert_eq!(requests[3].method, "GET");
    assert_eq!(requests[3].path, "/v2/media-images");
    assert_eq!(requests[3].query, "cursor=CUR-1");
    assert_eq!(requests[4].method, "DELETE");
    assert_eq!(requests[4].path, "/v2/media-images/IMG-1/delete");
}

#[tokio::test]
async fn passes_through_trace_id_and_http_401_403_raw_details() {
    let service = service().await;
    let client = service.client();

    // 1) 响应体 trace_id 原样透传
    service.fake.respond(
        400,
        json!({
            "err_code": 40034005,
            "message": "回复消息msg_id已过期",
            "trace_id": "body-trace-001"
        }),
    );
    let err = client
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("hi"))
        .await
        .unwrap_err();
    assert_eq!(err.trace_id(), Some("body-trace-001"));
    assert_eq!(
        err.to_string(),
        "QQ API error (HTTP 400, code 40034005, trace_id body-trace-001): 回复消息msg_id已过期"
    );

    // 2) HTTP 403 携带非 token 业务错误码（如 11253）：原样返回 Error::Api，不丢码也不清 token，且提取 X-Tps-trace-ID
    service.fake.respond_with_headers(
        403,
        json!({
            "err_code": 11253,
            "message": "check app privilege not pass"
        }),
        vec![("X-Tps-trace-ID", "header-trace-002")],
    );
    let perm_err = client.group_info(&group()).await.unwrap_err();
    assert!(matches!(
        &perm_err,
        Error::Api {
            http_status: 403,
            code: Some(11253),
            message,
            trace_id: Some(trace_id),
        } if message == "check app privilege not pass" && trace_id == "header-trace-002"
    ));
    assert_eq!(service.fake.token_calls(), 1);

    // 3) HTTP 401 携带业务码与 message：仍判为 Auth 并清 token，但原样保留 code/message/trace_id
    service.fake.respond(
        401,
        json!({
            "err_code": 11243,
            "message": "check token not pass",
            "trace_id": "trace-401"
        }),
    );
    let auth_err = client.group_info(&group()).await.unwrap_err();
    assert!(auth_err.is_auth(), "{auth_err:?}");
    let auth_text = auth_err.to_string();
    assert!(auth_text.contains("HTTP 401"));
    assert!(auth_text.contains("11243"));
    assert!(auth_text.contains("check token not pass"));
    assert!(auth_text.contains("trace-401"));

    // 下次请求会重新获取 token
    service.fake.respond(200, json!({"group_name": "G"}));
    client.group_info(&group()).await.unwrap();
    assert_eq!(service.fake.token_calls(), 2);
}

#[tokio::test]
async fn passes_through_token_endpoint_and_err_code_priority() {
    let service = service().await;

    // token 接口 401 保留原始 code / message / trace_id
    service.fake.set_token(
        401,
        json!({
            "code": 100016,
            "message": "invalid appid or secret",
            "trace_id": "tok-trace-1"
        }),
    );
    let err = service.client().access_token().await.unwrap_err();
    assert!(err.is_auth(), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("100016"));
    assert!(text.contains("invalid appid or secret"));
    assert!(text.contains("tok-trace-1"));

    // token 接口 200 带非 0 code 保留原始 message
    service
        .fake
        .set_token(200, json!({"code": 10004, "message": "机器人不存在"}));
    let err = service.client().access_token().await.unwrap_err();
    assert!(err.is_auth(), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("10004"));
    assert!(text.contains("机器人不存在"));

    // 正常响应中 code=0 但 err_code!=0、message="" 但 err_msg 非空时，优先取非零 err_code 与非空 err_msg
    service
        .fake
        .set_token(200, json!({"access_token": "TOKEN", "expires_in": 7200}));
    service.fake.respond(
        400,
        json!({
            "code": 0,
            "err_code": 40034005,
            "message": "",
            "err_msg": "msg_id expired"
        }),
    );
    let err = service
        .client()
        .send_message(&Peer::Group(group()), &OutgoingMessage::text("x"))
        .await
        .unwrap_err();
    assert!(matches!(
        &err,
        Error::Api {
            http_status: 400,
            code: Some(40034005),
            message,
            ..
        } if message == "msg_id expired"
    ));
}
